//! Durable composition for Candidate, Quality, and Architect transitions.
//!
//! The actor packet names only the immutable assignment target.  This module
//! resolves the rest from PostgreSQL, re-verifies every referenced CAS object,
//! and obtains Git/worktree facts from daemon-owned custody.  It deliberately
//! is one direct composition object, not a repository/service framework.

use std::sync::Arc;

use factory_protocol::{
    AggregateRevision, ApplicationBundleV2, ApplicationRevisionId, ArtifactId, AssignmentPacketV2,
    AssignmentRole, CandidateId, CandidatePacketV2, ContentDigest, ExpectedRevision, KernelBuildId,
    RepositoryObjectIdV2, RequiredReadV2, ReviewId, SealedArtifactReferenceV2, SessionId,
    TicketAttemptId, TicketContractReadV2, TicketId, TicketRevisionId, parse_application_bundle_v2,
    parse_command_profile_v2, parse_product_ticket_proposal_v2,
};
use miniserde::{Serialize, json};

use crate::{
    candidate_runtime::{
        CandidateCommitPolicy, CandidateTicketBinding, QualityFullSuiteOutcome,
        ResolvedEngineeringCandidateAuthority, ResolvedQualityCandidateAuthority,
        ResumeCandidateCommitAttachAuthority, ResumedHardValidationAuthority,
        ResumedHardValidationOutcome, resume_candidate_commit_attach,
        resume_candidate_hard_validation,
    },
    cas::CasStore,
    command_supervision::{
        CommandExpectation, CommandReceipt, CommandRunner, CommandStdin, CommandWorkspace,
        ComparisonRevision, DeterministicCommand, ExactBytes,
    },
    decision_store::{CandidateReceipt, DeliveryReceipt, RecordDelivery},
    git::{
        CandidateRefName, DefaultBranchName, GitCommitId, GitCustody, GitIdentity, GitTreeId,
        QualifiedRepository, WorktreeName,
    },
    operator_rpc::{
        ArchitectTransitionFuture, ArchitectTransitionResolutionError, ArchitectTransitionResolver,
        ResolvedCandidateDecisionTransition, ResolvedReleaseTransition,
    },
    product_runtime::product_observation_manifest_bytes,
    scheduler::ClaimReadyTicketAction,
    session_runtime::{
        CandidateQualityAuthorityFuture, CandidateQualityAuthorityResolutionError,
        CandidateQualityAuthorityResolver,
    },
    storage::KernelStore,
    ticket_store::{DownstreamActionContext, DownstreamActionStage},
};

const EXACT_OBSERVATION_COMPARISON: &str = "exact-observation-v1";
const FULL_SUITE_IDENTITY: &str = "full";
const CANDIDATE_SUBMITTED: i16 = 0;
const CANDIDATE_VALIDATED: i16 = 1;
const ATTEMPT_HARD_VALIDATION: i16 = 1;
const ATTEMPT_REWORK_VALIDATION: i16 = 5;
const ATTEMPT_QUALITY: i16 = 2;
const ATTEMPT_REWORK_QUALITY: i16 = 6;
const CLAIM_REQUALIFICATION_PRINCIPAL: &str = "kernel-ticket-claim-requalification";
const REGISTER_ARTIFACT_OPERATION: &str = "artifact.register";
const ARTIFACT_AUDIT_SUBJECT: i16 = 3;

/// The daemon's concrete trusted resolver.  It owns only kernel handles and
/// has no API that accepts actor-provided repository, tree, commit, ticket,
/// candidate, or validation identities.
#[derive(Clone)]
pub struct DurableAuthorityResolver {
    store: KernelStore,
    cas: CasStore,
    runner: CommandRunner,
    git: Arc<GitCustody>,
}

/// Scheduler-selected durable target, before packet construction. The shape
/// mirrors the immutable assignment relation and carries no actor-provided
/// repository, ticket, tree, candidate, or application identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DurableAssignmentTarget {
    Product,
    Engineering {
        ticket_attempt_id: TicketAttemptId,
    },
    Quality {
        ticket_attempt_id: TicketAttemptId,
        candidate_id: CandidateId,
    },
}

impl DurableAssignmentTarget {
    fn from_packet(packet: &AssignmentPacketV2) -> Result<Self, String> {
        match (
            packet.assignment_role,
            packet.ticket_attempt_id,
            packet.candidate_id,
        ) {
            (AssignmentRole::ProductResearch, None, None) => Ok(Self::Product),
            (AssignmentRole::Engineering, Some(ticket_attempt_id), None) => {
                Ok(Self::Engineering { ticket_attempt_id })
            }
            (AssignmentRole::Quality, Some(ticket_attempt_id), Some(candidate_id)) => {
                Ok(Self::Quality {
                    ticket_attempt_id,
                    candidate_id,
                })
            }
            _ => Err("assignment packet has an invalid durable target shape".to_owned()),
        }
    }
}

/// Trusted input to the pre-assignment workspace resolver. The scheduler
/// derives it from one campaign/office transition; it is not a wire DTO.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DurableAssignmentLaunchRequest {
    pub campaign_id: factory_protocol::CampaignId,
    pub application_revision_id: ApplicationRevisionId,
    pub target: DurableAssignmentTarget,
}

impl DurableAuthorityResolver {
    #[must_use]
    pub fn new(
        store: KernelStore,
        cas: CasStore,
        runner: CommandRunner,
        git: Arc<GitCustody>,
    ) -> Self {
        Self {
            store,
            cas,
            runner,
            git,
        }
    }

    /// Replays the exact kernel-owned hard validation for a durable
    /// `HardValidation` or `ReworkValidation` scheduler head.  The caller
    /// provides only the scheduler's revision-fenced context; this resolver
    /// rehydrates every candidate, ticket, application, session, artifact,
    /// command, and Git provenance fact before it can run anything.
    pub async fn resume_hard_validation(
        &self,
        action: DownstreamActionContext,
    ) -> Result<ResumedHardValidationOutcome, String> {
        let recovery = self
            .load_candidate_recovery(
                action,
                CANDIDATE_SUBMITTED,
                match action.stage {
                    DownstreamActionStage::HardValidation => ATTEMPT_HARD_VALIDATION,
                    DownstreamActionStage::ReworkValidation => ATTEMPT_REWORK_VALIDATION,
                    _ => {
                        return Err(
                            "hard-validation recovery requires a hard-validation scheduler action"
                                .to_owned(),
                        );
                    }
                },
            )
            .await?;
        let authority = ResumedHardValidationAuthority {
            principal: "kernel-candidate-hard-validation-recovery".to_owned(),
            command_id: format!(
                "candidate-{}-hard-validation-r{}",
                action.candidate_id.get(),
                action.candidate_revision.get(),
            ),
            application: recovery.application,
            repository: recovery.repository,
            ticket: recovery.ticket,
            candidate_id: action.candidate_id,
            expected_candidate_revision: ExpectedRevision::new(action.candidate_revision),
            engineering_session_id: recovery.engineering_session_id,
            kernel_build_id: recovery.kernel_build_id,
            campaign_id: recovery.campaign_id,
            application_revision_id: recovery.application_revision_id,
            candidate_tree: recovery.candidate_tree,
            regression_tree: recovery.regression_tree,
            candidate_patch_digest: recovery.candidate_patch.digest,
            submission: recovery.submission,
            product_reproducer: recovery.product_reproducer,
            full_suite_identity: FULL_SUITE_IDENTITY.to_owned(),
            full_suite: recovery.full_suite,
            validation_worktree_name: worktree_name(
                "resume-hard-validation",
                action.candidate_id.get(),
            )?,
        };
        resume_candidate_hard_validation(
            &self.store.process_store(),
            &self.store.decision_store(),
            &self.cas,
            &self.runner,
            &self.git,
            &authority,
        )
        .await
        .map_err(|error| format!("hard-validation recovery failed: {error}"))
    }

    /// Completes the Git/candidate attachment half of a passed hard
    /// validation.  It is only legal for the scheduler's explicit
    /// `CandidateCommitAttachRequired` head and never re-runs validation.
    pub async fn resume_candidate_commit_attach(
        &self,
        action: DownstreamActionContext,
    ) -> Result<CandidateReceipt, String> {
        if action.stage != DownstreamActionStage::CandidateCommitAttachRequired {
            return Err(
                "candidate commit recovery requires a commit-attach scheduler action".to_owned(),
            );
        }
        let recovery = match self
            .load_candidate_recovery(action, CANDIDATE_VALIDATED, ATTEMPT_QUALITY)
            .await
        {
            Ok(recovery) => recovery,
            Err(regular_error) => self
                .load_candidate_recovery(action, CANDIDATE_VALIDATED, ATTEMPT_REWORK_QUALITY)
                .await
                .map_err(|_| regular_error)?,
        };
        let hard_validation_id = recovery.hard_validation_id.ok_or_else(|| {
            "validated candidate is missing its exact passed hard validation".to_owned()
        })?;
        let authority = ResumeCandidateCommitAttachAuthority {
            principal: "kernel-candidate-commit-attach-recovery".to_owned(),
            command_id: format!(
                "candidate-{}-commit-attach-r{}",
                action.candidate_id.get(),
                action.candidate_revision.get(),
            ),
            application: recovery.application,
            repository: recovery.repository,
            ticket: recovery.ticket,
            candidate_id: action.candidate_id,
            expected_candidate_revision: ExpectedRevision::new(action.candidate_revision),
            hard_validation_id,
            kernel_build_id: recovery.kernel_build_id,
            campaign_id: recovery.campaign_id,
            application_revision_id: recovery.application_revision_id,
            candidate_tree: recovery.candidate_tree,
            regression_tree: recovery.regression_tree,
            candidate_patch_digest: recovery.candidate_patch.digest,
            submission: recovery.submission,
            commit: self
                .terminal_commit_policy(
                    recovery.engineering_session_id,
                    recovery.campaign_id,
                    recovery.application_revision_id,
                    recovery.engineering_started_at_seconds,
                )
                .await?,
        };
        resume_candidate_commit_attach(&self.store.decision_store(), &self.git, &authority)
            .await
            .map_err(|error| format!("candidate commit attachment recovery failed: {error}"))
    }

    /// Re-runs the exact persisted Product reproducer twice on the currently
    /// qualified default head before an otherwise-ready ticket is claimed.
    /// The scheduler action is only a revision fence; all ticket, campaign,
    /// application, command, and expected-observation identities come from
    /// the durable relation and verified CAS artifacts.
    pub async fn requalify_sponsored_ticket(
        &self,
        action: ClaimReadyTicketAction,
    ) -> Result<crate::ticket_store::CurrentHeadRequalification, String> {
        let row = sqlx::query!(
            "SELECT tr.revision AS ticket_revision, tr.application_revision_id,
                    tr.proposal_artifact_id, tr.reproducer_artifact_id,
                    camp.revision AS campaign_revision, kb.build_digest
               FROM factory.ticket_revisions tr
               JOIN factory.tickets t ON t.id = tr.ticket_id
               JOIN factory.campaigns camp ON camp.application_revision_id = tr.application_revision_id
               JOIN factory.kernel_builds kb ON kb.id = camp.kernel_build_id
              WHERE tr.id = $1 AND tr.lifecycle = 1 AND t.current_ticket_revision_id = tr.id
                AND camp.id = $2 AND camp.lifecycle = 0",
            action.ticket.ticket_revision_id.get(),
            action.campaign_id.get(),
        )
        .fetch_optional(&self.store.pool_for_authority())
        .await
        .map_err(db_error)?
        .ok_or_else(|| "sponsored ticket is not claimable in this running campaign".to_owned())?;
        let ticket_revision = persisted_revision(row.ticket_revision, "ticket revision")?;
        let campaign_revision = persisted_revision(row.campaign_revision, "campaign revision")?;
        if ticket_revision != action.ticket.revision
            || campaign_revision != action.expected_campaign_revision.get()
        {
            return Err("sponsored ticket claim action has stale aggregate revisions".to_owned());
        }
        let application_revision_id = ApplicationRevisionId::new(row.application_revision_id)
            .map_err(|error| error.to_string())?;
        let application = self.load_application(application_revision_id).await?;
        let prefix = claim_requalification_command_prefix(
            action.campaign_id.get(),
            action.ticket.ticket_revision_id.get(),
            action.ticket.revision.get(),
        );
        if let Some((first_actual_observation_artifact_id, second_actual_observation_artifact_id)) =
            self.requalification_manifest_pair(&prefix).await?
        {
            return Ok(crate::ticket_store::CurrentHeadRequalification {
                current_head_commit: application.repository.snapshot().base_commit().to_string(),
                current_head_tree: application.repository.snapshot().base_tree().to_string(),
                first_actual_observation_artifact_id,
                second_actual_observation_artifact_id,
            });
        }
        let proposal_bytes = self
            .artifact_bytes(
                ArtifactId::new(row.proposal_artifact_id).map_err(|error| error.to_string())?,
            )
            .await?;
        let proposal = parse_product_ticket_proposal_v2(
            &proposal_bytes,
            &application.bundle.ticket_policy.ticket_bounds,
        )
        .map_err(|error| format!("stored ticket proposal is invalid: {error}"))?;
        self.verify_proposal_artifacts(&proposal).await?;
        let stored_profile = parse_command_profile_v2(
            &self
                .artifact_bytes(
                    ArtifactId::new(row.reproducer_artifact_id)
                        .map_err(|error| error.to_string())?,
                )
                .await?,
        )
        .map_err(|error| format!("stored ticket reproducer profile is invalid: {error}"))?;
        let proposal_profile = parse_command_profile_v2(
            &self
                .artifact_bytes(proposal.reproducer.command.artifact_id)
                .await?,
        )
        .map_err(|error| format!("stored ticket reproducer profile is invalid: {error}"))?;
        if stored_profile != proposal_profile
            || application
                .bundle
                .reproducer_profiles
                .iter()
                .find(|profile| profile.name == proposal.reproducer_profile)
                != Some(&stored_profile)
        {
            return Err(
                "stored ticket reproducer no longer matches its admitted profile".to_owned(),
            );
        }
        let reproducer = self
            .command_from_reproducer(&stored_profile, &proposal)
            .await?;
        let workspace = CommandWorkspace::open(application.repository.root()).map_err(|error| {
            format!("qualified repository cannot be used as command workspace: {error}")
        })?;
        let reproduction = self
            .runner
            .run_discovery_reproducer(&workspace, &reproducer)
            .map_err(|error| format!("current-head reproducer failed to run: {error}"))?;
        let process = self.store.process_store();
        let kernel_build_id = kernel_build_id_bytes(row.build_digest, "build digest")?;
        // A claim retry must reuse its sealed observations, while a later
        // campaign that retries the same ticket must record its own
        // requalification under that campaign's installed build. Scope the
        // idempotency namespace to both durable identities so the latter does
        // not collide with the former.
        let first = seal_command_observation_manifest(
            &process,
            &self.cas,
            CLAIM_REQUALIFICATION_PRINCIPAL,
            &format!("{prefix}-first"),
            kernel_build_id,
            reproduction.first(),
        )
        .await?;
        let second = seal_command_observation_manifest(
            &process,
            &self.cas,
            CLAIM_REQUALIFICATION_PRINCIPAL,
            &format!("{prefix}-second"),
            kernel_build_id,
            reproduction.second(),
        )
        .await?;
        // Product admitted this ticket under the narrowly scoped status-only
        // discovery rule. Requalification still runs and seals both raw
        // receipts, but a process-local diagnostic in the second receipt is
        // not a new product state. Persist the first canonical receipt in the
        // ticket's replay slots, just as Product admission did.
        let (first_actual_observation_artifact_id, second_actual_observation_artifact_id) =
            canonical_requalification_observations(first, second);
        Ok(crate::ticket_store::CurrentHeadRequalification {
            current_head_commit: application.repository.snapshot().base_commit().to_string(),
            current_head_tree: application.repository.snapshot().base_tree().to_string(),
            first_actual_observation_artifact_id,
            second_actual_observation_artifact_id,
        })
    }

    /// Completes an already-accepted candidate delivery from durable state.
    /// A daemon driver may name a candidate and its idempotency namespace, but
    /// it cannot claim a Git result, choose a ref, or provide revisions. The
    /// kernel rereads all commit/base/tree/campaign facts, performs the only
    /// local fast-forward, seals its receipt, then lets DecisionStore make the
    /// terminal durable transition.
    pub async fn deliver_accepted_candidate(
        &self,
        command: DeliverAcceptedCandidate,
    ) -> Result<DeliveryReceipt, String> {
        let row = sqlx::query!(
            "SELECT c.ticket_attempt_id, c.base_commit, c.candidate_tree, c.candidate_commit,
                    c.candidate_ref,
                    c.revision AS candidate_revision, ta.revision AS attempt_revision,
                    tr.revision AS ticket_revision, tr.application_revision_id,
                    camp.revision AS campaign_revision, camp.cost_state,
                    camp.measured_cost_micro_usd, kb.build_digest
               FROM factory.candidates c
               JOIN factory.ticket_attempts ta ON ta.id = c.ticket_attempt_id
               JOIN factory.ticket_revisions tr ON tr.id = ta.ticket_revision_id
               JOIN factory.campaigns camp ON camp.id = ta.campaign_id
               JOIN factory.kernel_builds kb ON kb.id = camp.kernel_build_id
              WHERE c.id = $1 AND c.lifecycle = 3 AND ta.stage = 3 AND camp.lifecycle = 0",
            command.candidate_id.get(),
        )
        .fetch_optional(&self.store.pool_for_authority())
        .await
        .map_err(db_error)?
        .ok_or_else(|| "candidate is not accepted and awaiting local delivery".to_owned())?;
        let application_revision_id = ApplicationRevisionId::new(row.application_revision_id)
            .map_err(|error| error.to_string())?;
        let repository = self
            .load_application(application_revision_id)
            .await?
            .repository;
        let candidate_commit = GitCommitId::parse(
            row.candidate_commit
                .ok_or_else(|| "accepted candidate is missing its local commit".to_owned())?,
        )
        .map_err(|error| format!("stored candidate commit is invalid: {error}"))?;
        let candidate_commit_object = RepositoryObjectIdV2::parse(candidate_commit.to_string())
            .map_err(|error| error.to_string())?;
        let candidate_tree = GitTreeId::parse(row.candidate_tree)
            .map_err(|error| format!("stored candidate tree is invalid: {error}"))?;
        let candidate_ref = CandidateRefName::parse(
            row.candidate_ref
                .ok_or_else(|| "accepted candidate is missing its local ref".to_owned())?,
        )
        .map_err(|error| format!("stored candidate ref is invalid: {error}"))?;
        let expected_old_commit_object = RepositoryObjectIdV2::parse(row.base_commit.clone())
            .map_err(|error| format!("stored candidate base object is invalid: {error}"))?;
        let expected_old_commit = GitCommitId::parse(row.base_commit)
            .map_err(|error| format!("stored candidate base commit is invalid: {error}"))?;
        let factory_cost_micro_usd = if row.cost_state == 0 {
            u64::try_from(row.measured_cost_micro_usd)
                .map_err(|_| "stored campaign Factory-Cost is invalid".to_owned())?
        } else {
            return Err("campaign Factory-Cost is not known".to_owned());
        };
        let delivery = if repository.snapshot().base_commit() == &candidate_commit
            && repository.snapshot().base_tree() == &candidate_tree
        {
            self.git
                .recover_completed_local_fast_forward(
                    &repository,
                    expected_old_commit,
                    candidate_ref.clone(),
                    candidate_commit,
                    candidate_tree,
                )
                .map_err(|error| format!("completed local delivery cannot be recovered: {error}"))?
        } else if repository.snapshot().base_tree() == &candidate_tree
            && repository.snapshot().base_commit() != &expected_old_commit
        {
            self.git
                .recover_completed_local_fast_forward_with_factory_cost(
                    &repository,
                    expected_old_commit,
                    candidate_ref.clone(),
                    candidate_commit,
                    candidate_tree,
                    factory_cost_micro_usd,
                )
                .map_err(|error| {
                    format!("completed cost-visible delivery cannot be recovered: {error}")
                })?
        } else {
            let recovered = self
                .git
                .recover_candidate_commit(
                    &repository,
                    candidate_ref,
                    candidate_commit,
                    candidate_tree,
                )
                .map_err(|error| format!("stored candidate commit cannot be delivered: {error}"))?;
            self.git
                .guarded_local_fast_forward_with_factory_cost(
                    &repository,
                    &recovered,
                    factory_cost_micro_usd,
                )
                .map_err(|error| format!("guarded local delivery failed: {error}"))?
        };
        let kernel_build_id = kernel_build_id_bytes(row.build_digest, "build digest")?;
        let receipt_bytes = local_delivery_receipt_bytes(
            command.candidate_id,
            &delivery.previous_commit,
            &delivery.delivered_commit,
            &delivery.delivered_tree,
            factory_cost_micro_usd,
        );
        let (seal, receipt) = self
            .store
            .process_store()
            .adopt_and_register_kernel_bytes(
                &self.cas,
                "kernel-local-delivery",
                &command.command_id,
                kernel_build_id,
                &receipt_bytes,
            )
            .await
            .map_err(|error| format!("could not seal local delivery receipt: {error}"))?;
        self.store
            .decision_store()
            .record_delivery(&RecordDelivery {
                principal: command.principal,
                command_id: command.command_id,
                candidate_id: command.candidate_id,
                expected_candidate_revision: ExpectedRevision::new(persisted_revision(
                    row.candidate_revision,
                    "candidate revision",
                )?),
                expected_attempt_revision: ExpectedRevision::new(persisted_revision(
                    row.attempt_revision,
                    "attempt revision",
                )?),
                expected_ticket_revision: ExpectedRevision::new(persisted_revision(
                    row.ticket_revision,
                    "ticket revision",
                )?),
                expected_campaign_revision: ExpectedRevision::new(persisted_revision(
                    row.campaign_revision,
                    "campaign revision",
                )?),
                expected_old_commit: expected_old_commit_object,
                resulting_commit: RepositoryObjectIdV2::parse(
                    delivery.delivered_commit.to_string(),
                )
                .map_err(|error| error.to_string())?,
                candidate_commit: candidate_commit_object,
                resulting_tree: RepositoryObjectIdV2::parse(delivery.delivered_tree.to_string())
                    .map_err(|error| error.to_string())?,
                factory_cost_micro_usd,
                receipt: SealedArtifactReferenceV2 {
                    artifact_id: receipt.artifact_id,
                    digest: seal.digest(),
                    byte_length: seal.byte_length(),
                },
            })
            .await
            .map_err(|error| format!("delivery transition was rejected after Git custody: {error}"))
    }

    /// Resolves daemon-owned workspace materialization facts before an actor
    /// session exists. Composition uses this once to create the actor or
    /// review worktree; actors never receive a chance to select these IDs.
    pub async fn resolve_assignment_launch(
        &self,
        packet: &AssignmentPacketV2,
    ) -> Result<DurableAssignmentLaunchContext, String> {
        let assignment = self.load_assignment_context(packet).await?;
        let target = DurableAssignmentTarget::from_packet(packet)?;
        self.resolve_launch_context(assignment, target).await
    }

    /// Resolves a worktree before an assignment packet exists. This is the
    /// only seam needed to calculate required-read digests before packet
    /// sealing: the caller supplies scheduler-selected durable identities,
    /// never actor payload, and the resolver verifies the running campaign,
    /// admitted application, and exact target stage itself.
    pub async fn resolve_pre_assignment_launch(
        &self,
        request: DurableAssignmentLaunchRequest,
    ) -> Result<DurableAssignmentLaunchContext, String> {
        let actual_application = sqlx::query_scalar!(
            "SELECT application_revision_id
               FROM factory.campaigns
              WHERE id = $1 AND lifecycle = 0",
            request.campaign_id.get(),
        )
        .fetch_optional(&self.store.pool_for_authority())
        .await
        .map_err(db_error)?
        .ok_or_else(|| "campaign is absent or no longer running".to_owned())?;
        let actual_application =
            ApplicationRevisionId::new(actual_application).map_err(|error| error.to_string())?;
        if actual_application != request.application_revision_id {
            return Err("campaign application revision differs from launch request".to_owned());
        }
        self.resolve_launch_context(
            AssignmentContext {
                campaign_id: request.campaign_id,
                application_revision_id: request.application_revision_id,
            },
            request.target,
        )
        .await
    }

    async fn resolve_launch_context(
        &self,
        assignment: AssignmentContext,
        target: DurableAssignmentTarget,
    ) -> Result<DurableAssignmentLaunchContext, String> {
        let application = self
            .load_application(assignment.application_revision_id)
            .await?;
        match target {
            DurableAssignmentTarget::Engineering { ticket_attempt_id } => {
                let row = sqlx::query!(
                    "SELECT claimed_commit, claimed_tree, tr.proposal_artifact_id,
                            tr.reproducer_artifact_id, tr.ticket_id, tr.id AS ticket_revision_id
                       FROM factory.ticket_attempts ta
                       JOIN factory.ticket_revisions tr ON tr.id = ta.ticket_revision_id
                      WHERE ta.id = $1 AND ta.campaign_id = $2
                        AND tr.application_revision_id = $3 AND ta.stage IN (0, 4)",
                    ticket_attempt_id.get(),
                    assignment.campaign_id.get(),
                    assignment.application_revision_id.get(),
                )
                .fetch_optional(&self.store.pool_for_authority())
                .await
                .map_err(db_error)?
                .ok_or_else(|| "Engineering attempt is not launchable".to_owned())?;
                let claimed_commit =
                    GitCommitId::parse(row.claimed_commit).map_err(|error| error.to_string())?;
                let claimed_tree =
                    GitTreeId::parse(row.claimed_tree).map_err(|error| error.to_string())?;
                let proposal_artifact_id =
                    ArtifactId::new(row.proposal_artifact_id).map_err(|error| error.to_string())?;
                if application.repository.snapshot().base_commit() != &claimed_commit
                    || application.repository.snapshot().base_tree() != &claimed_tree
                {
                    return Err(
                        "qualified repository head no longer matches the claimed Engineering base"
                            .to_owned(),
                    );
                }
                let engineering_checkpoint = self
                    .engineering_checkpoint_contract(
                        &application.bundle,
                        ticket_attempt_id,
                        proposal_artifact_id,
                        ArtifactId::new(row.reproducer_artifact_id)
                            .map_err(|error| error.to_string())?,
                    )
                    .await?;
                Ok(DurableAssignmentLaunchContext {
                    application_revision_id: assignment.application_revision_id,
                    target: DurableAssignmentTarget::Engineering { ticket_attempt_id },
                    repository: application.repository,
                    materialize_commit: claimed_commit,
                    materialize_tree: claimed_tree,
                    ticket_id: Some(
                        TicketId::new(row.ticket_id).map_err(|error| error.to_string())?,
                    ),
                    ticket_revision_id: Some(
                        TicketRevisionId::new(row.ticket_revision_id)
                            .map_err(|error| error.to_string())?,
                    ),
                    validation_id: None,
                    engineering_checkpoint: Some(engineering_checkpoint),
                    proposal: Some(self.reference(proposal_artifact_id).await?),
                    evidence: DurableAssignmentEvidence {
                        proposal: Some(
                            self.proposal_evidence(&application.bundle, proposal_artifact_id)
                                .await?,
                        ),
                        candidate: None,
                    },
                    application_required_reads: application.bundle.required_reads.clone(),
                    ticket_contract_reads: self
                        .ticket_contract_reads(&application.bundle, proposal_artifact_id)
                        .await?,
                })
            }
            DurableAssignmentTarget::Quality {
                ticket_attempt_id,
                candidate_id,
            } => {
                let row = sqlx::query!(
                    "SELECT c.candidate_tree, c.candidate_commit, tr.proposal_artifact_id,
                            tr.ticket_id, tr.id AS ticket_revision_id, v.id AS validation_id
                       FROM factory.candidates c
                       JOIN factory.ticket_attempts ta ON ta.id = c.ticket_attempt_id
                       JOIN factory.ticket_revisions tr ON tr.id = ta.ticket_revision_id
                       JOIN factory.validations v ON v.candidate_id = c.id
                            AND v.validation_scope = 0 AND v.lifecycle = 1
                       LEFT JOIN factory.validations qv ON qv.candidate_id = c.id
                            AND qv.validation_scope = 1 AND qv.lifecycle = 1
                       LEFT JOIN factory.reviews qr ON qr.candidate_id = c.id
                      WHERE c.id = $1 AND c.ticket_attempt_id = $2
                        AND ta.campaign_id = $3 AND tr.application_revision_id = $4
                        AND c.lifecycle = 1 AND c.candidate_commit IS NOT NULL
                        AND (ta.stage IN (2, 6)
                             OR (ta.stage = 3 AND qv.id IS NOT NULL AND qr.id IS NULL))",
                    candidate_id.get(),
                    ticket_attempt_id.get(),
                    assignment.campaign_id.get(),
                    assignment.application_revision_id.get(),
                )
                .fetch_optional(&self.store.pool_for_authority())
                .await
                .map_err(db_error)?
                .ok_or_else(|| "Quality candidate is not launchable".to_owned())?;
                let candidate_commit =
                    GitCommitId::parse(row.candidate_commit.ok_or_else(|| {
                        "Quality candidate is missing its attached commit".to_owned()
                    })?)
                    .map_err(|error| error.to_string())?;
                Ok(DurableAssignmentLaunchContext {
                    application_revision_id: assignment.application_revision_id,
                    target: DurableAssignmentTarget::Quality {
                        ticket_attempt_id,
                        candidate_id,
                    },
                    repository: application.repository,
                    materialize_commit: candidate_commit,
                    materialize_tree: GitTreeId::parse(row.candidate_tree)
                        .map_err(|error| error.to_string())?,
                    ticket_id: Some(
                        TicketId::new(row.ticket_id).map_err(|error| error.to_string())?,
                    ),
                    ticket_revision_id: Some(
                        TicketRevisionId::new(row.ticket_revision_id)
                            .map_err(|error| error.to_string())?,
                    ),
                    validation_id: Some(
                        factory_protocol::ValidationId::new(row.validation_id)
                            .map_err(|error| error.to_string())?,
                    ),
                    engineering_checkpoint: None,
                    proposal: Some(
                        self.reference(
                            ArtifactId::new(row.proposal_artifact_id)
                                .map_err(|error| error.to_string())?,
                        )
                        .await?,
                    ),
                    evidence: DurableAssignmentEvidence {
                        proposal: Some(
                            self.proposal_evidence(
                                &application.bundle,
                                ArtifactId::new(row.proposal_artifact_id)
                                    .map_err(|error| error.to_string())?,
                            )
                            .await?,
                        ),
                        candidate: Some(
                            self.candidate_evidence(ticket_attempt_id, candidate_id)
                                .await?,
                        ),
                    },
                    application_required_reads: application.bundle.required_reads.clone(),
                    ticket_contract_reads: self
                        .ticket_contract_reads(
                            &application.bundle,
                            ArtifactId::new(row.proposal_artifact_id)
                                .map_err(|error| error.to_string())?,
                        )
                        .await?,
                })
            }
            DurableAssignmentTarget::Product => Ok(DurableAssignmentLaunchContext {
                application_revision_id: assignment.application_revision_id,
                target: DurableAssignmentTarget::Product,
                materialize_commit: application.repository.snapshot().base_commit().clone(),
                materialize_tree: application.repository.snapshot().base_tree().clone(),
                repository: application.repository,
                ticket_id: None,
                ticket_revision_id: None,
                validation_id: None,
                engineering_checkpoint: None,
                proposal: None,
                evidence: DurableAssignmentEvidence {
                    proposal: None,
                    candidate: None,
                },
                application_required_reads: application.bundle.required_reads,
                ticket_contract_reads: Vec::new(),
            }),
        }
    }

    async fn resolve_engineering_inner(
        &self,
        session_id: SessionId,
        packet: &AssignmentPacketV2,
    ) -> Result<ResolvedEngineeringCandidateAuthority, String> {
        if packet.assignment_role != AssignmentRole::Engineering {
            return Err("Engineering resolver received a non-Engineering packet".to_owned());
        }
        let ticket_attempt_id = packet
            .ticket_attempt_id
            .ok_or_else(|| "Engineering packet has no ticket attempt target".to_owned())?;
        if packet.candidate_id.is_some() {
            return Err("Engineering packet unexpectedly names a candidate".to_owned());
        }
        let assignment = self.load_assignment_context(packet).await?;
        let application = self
            .load_application(assignment.application_revision_id)
            .await?;
        let ticket = self
            .load_engineering_ticket(ticket_attempt_id, &assignment)
            .await?;
        let proposal_bytes = self.artifact_bytes(ticket.proposal_artifact_id).await?;
        let proposal = parse_product_ticket_proposal_v2(
            &proposal_bytes,
            &application.bundle.ticket_policy.ticket_bounds,
        )
        .map_err(|error| format!("stored ticket proposal is invalid: {error}"))?;
        self.verify_proposal_artifacts(&proposal).await?;
        let profile_bytes = self.artifact_bytes(ticket.reproducer_artifact_id).await?;
        let stored_profile = parse_command_profile_v2(&profile_bytes)
            .map_err(|error| format!("stored ticket reproducer profile is invalid: {error}"))?;
        let source_profile_bytes = self
            .artifact_bytes(proposal.reproducer.command.artifact_id)
            .await?;
        let source_profile = parse_command_profile_v2(&source_profile_bytes)
            .map_err(|error| format!("ticket proposal reproducer command is invalid: {error}"))?;
        if stored_profile != source_profile
            || application
                .bundle
                .reproducer_profiles
                .iter()
                .find(|profile| profile.name == proposal.reproducer_profile)
                != Some(&stored_profile)
        {
            return Err(
                "stored ticket reproducer no longer matches its admitted profile".to_owned(),
            );
        }
        // These manifests are also part of the immutable ticket contract.
        let _ = self
            .artifact_bytes(ticket.expected_observation_artifact_id)
            .await?;
        let _ = self
            .artifact_bytes(ticket.discovery_observation_artifact_id)
            .await?;
        let reproducer = self
            .command_from_reproducer(&stored_profile, &proposal)
            .await?;
        let actor_worktree = self
            .git
            .adopt_actor_worktree(&application.repository, packet.workspace_root.as_str())
            .map_err(|error| format!("Engineering worktree custody failed: {error}"))?;
        let session_suffix = session_id.get();
        let full_suite = full_suite_commands(&application.bundle)?;
        Ok(ResolvedEngineeringCandidateAuthority {
            application: application.bundle.clone(),
            repository: application.repository.clone(),
            actor_worktree,
            ticket: CandidateTicketBinding {
                ticket_id: ticket.ticket_id,
                ticket_attempt_id,
                ticket_revision_id: ticket.ticket_revision_id,
                expected_attempt_revision: ExpectedRevision::new(ticket.attempt_revision),
                expected_ticket_revision: ExpectedRevision::new(ticket.ticket_revision),
                ticket_revision_digest: ContentDigest::of_bytes(&proposal_bytes),
            },
            regression_command: reproducer.clone(),
            regression_expected_failure: format!(
                "ticket-attempt-{}-{}",
                ticket_attempt_id.get(),
                stored_profile.name,
            ),
            regression_worktree_name: worktree_name("engineering-regression", session_suffix)?,
            product_reproducer: reproducer,
            full_suite_identity: FULL_SUITE_IDENTITY.to_owned(),
            full_suite,
            validation_worktree_name: worktree_name("engineering-validation", session_suffix)?,
        })
    }

    async fn resolve_quality_inner(
        &self,
        session_id: SessionId,
        packet: &AssignmentPacketV2,
    ) -> Result<ResolvedQualityCandidateAuthority, String> {
        if packet.assignment_role != AssignmentRole::Quality {
            return Err("Quality resolver received a non-Quality packet".to_owned());
        }
        let ticket_attempt_id = packet
            .ticket_attempt_id
            .ok_or_else(|| "Quality packet has no ticket attempt target".to_owned())?;
        let candidate_id = packet
            .candidate_id
            .ok_or_else(|| "Quality packet has no candidate target".to_owned())?;
        let assignment = self.load_assignment_context(packet).await?;
        let application = self
            .load_application(assignment.application_revision_id)
            .await?;
        let full_suite = full_suite_commands(&application.bundle)?;
        let candidate = self
            .load_quality_candidate(ticket_attempt_id, candidate_id, &assignment)
            .await?;
        Ok(ResolvedQualityCandidateAuthority {
            application: application.bundle,
            repository: application.repository,
            candidate: candidate.packet,
            expected_attempt_revision: ExpectedRevision::new(candidate.attempt_revision),
            full_suite_identity: FULL_SUITE_IDENTITY.to_owned(),
            full_suite,
            validation_worktree_name: worktree_name("quality-validation", session_id.get())?,
            prior_full_suite: candidate.prior_full_suite,
        })
    }

    async fn load_application(
        &self,
        application_revision_id: ApplicationRevisionId,
    ) -> Result<ApplicationContext, String> {
        let row = sqlx::query!(
            "SELECT ar.bundle_artifact_id, r.canonical_local_path, r.default_branch
               FROM factory.application_revisions ar
               JOIN factory.repositories r ON r.id = ar.repository_id
              WHERE ar.id = $1",
            application_revision_id.get(),
        )
        .fetch_optional(&self.store.pool_for_authority())
        .await
        .map_err(db_error)?
        .ok_or_else(|| {
            format!(
                "application revision {} is absent",
                application_revision_id.get()
            )
        })?;
        let bundle_artifact_id =
            ArtifactId::new(row.bundle_artifact_id).map_err(|error| error.to_string())?;
        let bundle_bytes = self.artifact_bytes(bundle_artifact_id).await?;
        let bundle = parse_application_bundle_v2(&bundle_bytes)
            .map_err(|error| format!("admitted application bundle is invalid: {error}"))?;
        let repository_path = row.canonical_local_path;
        let default_branch = row.default_branch;
        if bundle.repository.canonical_local_path.as_str() != repository_path
            || bundle.repository.default_branch != default_branch
        {
            return Err(
                "application bundle repository binding differs from durable repository".to_owned(),
            );
        }
        let repository = self
            .git
            .qualify_repository(
                &repository_path,
                DefaultBranchName::parse(default_branch).map_err(|error| error.to_string())?,
            )
            .map_err(|error| format!("repository qualification failed: {error}"))?;
        Ok(ApplicationContext { bundle, repository })
    }

    /// The packet is sealed transport evidence, not a lookup authority. Re-read
    /// its immutable target tuple from the assignment before following any
    /// campaign, application, attempt, or candidate relationship.
    async fn load_assignment_context(
        &self,
        packet: &AssignmentPacketV2,
    ) -> Result<AssignmentContext, String> {
        let row = sqlx::query!(
            "SELECT campaign_id, application_revision_id, assignment_role, ticket_attempt_id, candidate_id
               FROM factory.assignments
              WHERE id = $1",
            packet.assignment_id.get(),
        )
        .fetch_optional(&self.store.pool_for_authority())
        .await
        .map_err(db_error)?
        .ok_or_else(|| "assignment is absent from durable authority".to_owned())?;
        let campaign_id = row.campaign_id;
        let application_revision_id = ApplicationRevisionId::new(row.application_revision_id)
            .map_err(|error| error.to_string())?;
        let assignment_role = row.assignment_role;
        let ticket_attempt_id = row
            .ticket_attempt_id
            .map(TicketAttemptId::new)
            .transpose()
            .map_err(|error| error.to_string())?;
        let candidate_id = row
            .candidate_id
            .map(CandidateId::new)
            .transpose()
            .map_err(|error| error.to_string())?;
        let expected_office = match packet.assignment_role {
            AssignmentRole::ProductResearch => 0,
            AssignmentRole::Engineering => 1,
            AssignmentRole::Quality => 2,
        };
        if campaign_id != packet.campaign_id.get()
            || application_revision_id != packet.application_revision_id
            || assignment_role != expected_office
            || ticket_attempt_id != packet.ticket_attempt_id
            || candidate_id != packet.candidate_id
        {
            return Err("assignment packet identity differs from durable assignment".to_owned());
        }
        Ok(AssignmentContext {
            campaign_id: packet.campaign_id,
            application_revision_id,
        })
    }

    async fn artifact_bytes(&self, artifact_id: ArtifactId) -> Result<Vec<u8>, String> {
        let process = self.store.process_store();
        let seal = process
            .registered_artifact(&self.cas, artifact_id)
            .await
            .map_err(|error| {
                format!(
                    "registered artifact {} is unavailable: {error}",
                    artifact_id.get()
                )
            })?;
        self.cas.read_verified(seal.digest()).map_err(|error| {
            format!(
                "registered artifact {} failed CAS verification: {error}",
                artifact_id.get()
            )
        })
    }

    /// A daemon can stop after sealing both current-head replay manifests but
    /// before it persists the ticket claim. A restarted driver must reuse that
    /// complete, verified pair: raw command diagnostics can legitimately vary
    /// between invocations, whereas the sealed status-only manifests are the
    /// ticket's replay identity. A partial pair is a durable fault rather than
    /// permission to mix old and new observations.
    async fn requalification_manifest_pair(
        &self,
        command_prefix: &str,
    ) -> Result<Option<(ArtifactId, ArtifactId)>, String> {
        let first = self
            .registered_claim_requalification_manifest(&format!("{command_prefix}-first-manifest"))
            .await?;
        let second = self
            .registered_claim_requalification_manifest(&format!("{command_prefix}-second-manifest"))
            .await?;
        requalification_manifest_pair(first, second).map_err(str::to_owned)
    }

    async fn registered_claim_requalification_manifest(
        &self,
        command_id: &str,
    ) -> Result<Option<ArtifactId>, String> {
        let row = sqlx::query!(
            "SELECT subject_id
               FROM factory.audit_log
              WHERE principal = $1 AND command_id = $2
                AND operation = $3 AND subject_kind = $4",
            CLAIM_REQUALIFICATION_PRINCIPAL,
            command_id,
            REGISTER_ARTIFACT_OPERATION,
            ARTIFACT_AUDIT_SUBJECT,
        )
        .fetch_optional(&self.store.pool_for_authority())
        .await
        .map_err(db_error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let artifact_id = ArtifactId::new(row.subject_id).map_err(|error| error.to_string())?;
        self.store
            .process_store()
            .registered_artifact(&self.cas, artifact_id)
            .await
            .map_err(|error| {
                format!(
                    "requalification manifest artifact {} is unavailable: {error}",
                    artifact_id.get()
                )
            })?;
        Ok(Some(artifact_id))
    }

    async fn exact_bytes(
        &self,
        reference: &SealedArtifactReferenceV2,
    ) -> Result<ExactBytes, String> {
        let bytes = self.artifact_bytes(reference.artifact_id).await?;
        ExactBytes::from_artifact(reference.digest, bytes).map_err(|error| {
            format!(
                "artifact {} does not match its sealed reference: {error}",
                reference.artifact_id.get()
            )
        })
    }

    async fn verify_proposal_artifacts(
        &self,
        proposal: &factory_protocol::ProductTicketProposalV2,
    ) -> Result<(), String> {
        let mut references = vec![
            &proposal.narrative,
            &proposal.evidence,
            &proposal.reproducer.command,
            &proposal.reproducer.expected_observation.stdout,
            &proposal.reproducer.expected_observation.stderr,
            &proposal.reproducer.first_observation.stdout,
            &proposal.reproducer.first_observation.stderr,
            &proposal.reproducer.second_observation.stdout,
            &proposal.reproducer.second_observation.stderr,
        ];
        if let Some(stdin) = &proposal.reproducer.stdin {
            references.push(stdin);
        }
        for reference in references {
            let _ = self.exact_bytes(reference).await?;
        }
        Ok(())
    }

    async fn ticket_contract_reads(
        &self,
        bundle: &ApplicationBundleV2,
        proposal_artifact_id: ArtifactId,
    ) -> Result<Vec<TicketContractReadV2>, String> {
        let proposal_bytes = self.artifact_bytes(proposal_artifact_id).await?;
        let proposal =
            parse_product_ticket_proposal_v2(&proposal_bytes, &bundle.ticket_policy.ticket_bounds)
                .map_err(|error| format!("stored ticket proposal is invalid: {error}"))?;
        self.verify_proposal_artifacts(&proposal).await?;
        Ok(proposal.contract_reads)
    }

    async fn command_from_reproducer(
        &self,
        stored_profile: &factory_protocol::CommandProfileV2,
        proposal: &factory_protocol::ProductTicketProposalV2,
    ) -> Result<DeterministicCommand, String> {
        let mut profile = stored_profile.clone();
        profile.expected_exit_status = proposal.reproducer.expected_observation.exit_status;
        let stdin = match &proposal.reproducer.stdin {
            Some(reference) => CommandStdin::Artifact(self.exact_bytes(reference).await?),
            None => CommandStdin::Empty,
        };
        DeterministicCommand::new(
            profile,
            stdin,
            CommandExpectation::new(
                // Product tickets establish their public process contract by
                // exit status. Raw expected stream artifacts remain durable
                // diagnostic evidence, but cannot be used for exact replay
                // when a pre-fix host panic embeds a process-local id.
                ComparisonRevision::parse("status-only-v1").map_err(|error| error.to_string())?,
                None,
                None,
            ),
        )
        .map_err(|error| format!("ticket reproducer command is invalid: {error}"))
    }

    async fn load_engineering_ticket(
        &self,
        ticket_attempt_id: TicketAttemptId,
        assignment: &AssignmentContext,
    ) -> Result<EngineeringTicket, String> {
        let row = sqlx::query!(
            "SELECT ta.revision AS attempt_revision, tr.id AS ticket_revision_id,
                    tr.revision AS ticket_revision, tr.proposal_artifact_id,
                    tr.reproducer_artifact_id, tr.expected_observation_artifact_id,
                    tr.discovery_observation_artifact_id, t.id AS ticket_id
               FROM factory.ticket_attempts ta
               JOIN factory.ticket_revisions tr ON tr.id = ta.ticket_revision_id
               JOIN factory.tickets t ON t.id = tr.ticket_id
              WHERE ta.id = $1 AND ta.campaign_id = $2 AND tr.application_revision_id = $3
                AND ta.stage IN (0, 4)",
            ticket_attempt_id.get(),
            assignment.campaign_id.get(),
            assignment.application_revision_id.get(),
        )
        .fetch_optional(&self.store.pool_for_authority())
        .await
        .map_err(db_error)?
        .ok_or_else(|| "Engineering assignment target is not an active attempt".to_owned())?;
        Ok(EngineeringTicket {
            ticket_id: TicketId::new(row.ticket_id).map_err(|error| error.to_string())?,
            ticket_revision_id: TicketRevisionId::new(row.ticket_revision_id)
                .map_err(|error| error.to_string())?,
            ticket_revision: persisted_revision(row.ticket_revision, "ticket revision")?,
            attempt_revision: persisted_revision(row.attempt_revision, "attempt revision")?,
            proposal_artifact_id: ArtifactId::new(row.proposal_artifact_id)
                .map_err(|error| error.to_string())?,
            reproducer_artifact_id: ArtifactId::new(row.reproducer_artifact_id)
                .map_err(|error| error.to_string())?,
            expected_observation_artifact_id: ArtifactId::new(row.expected_observation_artifact_id)
                .map_err(|error| error.to_string())?,
            discovery_observation_artifact_id: ArtifactId::new(
                row.discovery_observation_artifact_id,
            )
            .map_err(|error| error.to_string())?,
        })
    }

    /// Loads the only digest eligible for the Engineering provenance trailer.
    /// Packet bytes prove admission, but only the terminal transcript proves
    /// the actual actor session that submitted the candidate.  Requiring the
    /// completed candidate terminal operation, known cost, and complete
    /// required-read assertion makes a failed/interrupted Engineering session
    /// unrecoverable for Quality or commit attachment.
    async fn terminal_commit_policy(
        &self,
        session_id: SessionId,
        campaign_id: factory_protocol::CampaignId,
        application_revision_id: ApplicationRevisionId,
        timestamp_unix_seconds: i64,
    ) -> Result<CandidateCommitPolicy, String> {
        let engineering_session_digest = sqlx::query_scalar!(
            "SELECT artifact.digest
               FROM factory.sessions s
               JOIN factory.assignments a ON a.id = s.assignment_id
               JOIN factory.artifacts artifact ON artifact.id = s.transcript_artifact_id
              WHERE s.id = $1 AND s.campaign_id = $2 AND s.application_revision_id = $3
                AND s.assignment_role = 1 AND a.assignment_role = 1 AND a.campaign_id = $2
                AND a.application_revision_id = $3
                AND s.lifecycle = 2 AND s.cost_state = 0
                AND s.terminal_operation = 1
                AND s.required_read_satisfied_count = s.required_read_expected_count",
            session_id.get(),
            campaign_id.get(),
            application_revision_id.get(),
        )
        .fetch_optional(&self.store.pool_for_authority())
        .await
        .map_err(db_error)?
        .ok_or_else(|| {
            "candidate commit attachment requires a succeeded Engineering candidate terminal with known cost and complete required reads".to_owned()
        })?;
        let engineering_session_digest: [u8; 32] = engineering_session_digest
            .try_into()
            .map_err(|_| "terminal transcript digest has an invalid length".to_owned())?;
        let engineering_session_digest = ContentDigest::from_bytes(engineering_session_digest);
        let identity = GitIdentity::new("Factory Kernel", "factory-kernel@local")
            .map_err(|error| format!("kernel Git identity is invalid: {error}"))?;
        Ok(CandidateCommitPolicy {
            author: identity.clone(),
            committer: identity,
            timestamp_unix_seconds,
            engineering_session_digest,
        })
    }

    async fn load_candidate_recovery(
        &self,
        action: DownstreamActionContext,
        required_candidate_lifecycle: i16,
        required_attempt_stage: i16,
    ) -> Result<CandidateRecovery, String> {
        let row = sqlx::query!(
            "SELECT c.revision AS candidate_revision, c.base_commit, c.base_tree,
                    c.regression_tree, c.candidate_tree, c.changed_paths_artifact_id,
                    c.regression_patch_artifact_id, c.regression_command_set_artifact_id,
                    c.regression_log_artifact_id, c.patch_artifact_id,
                    c.engineering_session_id, c.engineering_report_artifact_id,
                    c.commit_subject, c.commit_body, c.regression_test_identity,
                    c.risks_artifact_id,
                    ta.revision AS attempt_revision, tr.id AS ticket_revision_id,
                    tr.revision AS ticket_revision, tr.ticket_id,
                    tr.application_revision_id, tr.proposal_artifact_id,
                    tr.reproducer_artifact_id, tr.expected_observation_artifact_id,
                    tr.discovery_observation_artifact_id,
                    camp.id AS campaign_id, kb.build_digest,
                    FLOOR(EXTRACT(EPOCH FROM es.started_at))::BIGINT
                        AS engineering_started_at_seconds,
                    hv.id AS \"hard_validation_id?\"
               FROM factory.candidates c
               JOIN factory.ticket_attempts ta ON ta.id = c.ticket_attempt_id
               JOIN factory.ticket_revisions tr ON tr.id = ta.ticket_revision_id
               JOIN factory.campaigns camp ON camp.id = ta.campaign_id
               JOIN factory.kernel_builds kb ON kb.id = camp.kernel_build_id
               JOIN factory.sessions es ON es.id = c.engineering_session_id
               JOIN factory.assignments a ON a.id = es.assignment_id
               LEFT JOIN factory.validations hv ON hv.candidate_id = c.id
                    AND hv.validation_scope = 0 AND hv.lifecycle = 1
              WHERE c.id = $1 AND c.ticket_attempt_id = $2
                AND c.lifecycle = $3 AND ta.stage = $4
                AND c.candidate_commit IS NULL
                AND camp.lifecycle = 0 AND es.campaign_id = camp.id
                AND es.assignment_role = 1 AND a.assignment_role = 1
                AND a.campaign_id = camp.id
                AND a.application_revision_id = tr.application_revision_id",
            action.candidate_id.get(),
            action.ticket_attempt_id.get(),
            required_candidate_lifecycle,
            required_attempt_stage,
        )
        .fetch_optional(&self.store.pool_for_authority())
        .await
        .map_err(db_error)?
        .ok_or_else(|| {
            "candidate recovery action is no longer at its exact durable stage".to_owned()
        })?;
        let candidate_revision = persisted_revision(row.candidate_revision, "candidate revision")?;
        let attempt_revision = persisted_revision(row.attempt_revision, "attempt revision")?;
        if candidate_revision != action.candidate_revision
            || attempt_revision != action.ticket_attempt_revision
        {
            return Err("candidate recovery action has stale aggregate revisions".to_owned());
        }
        let application_revision_id = ApplicationRevisionId::new(row.application_revision_id)
            .map_err(|error| error.to_string())?;
        let application = self.load_application(application_revision_id).await?;
        let persisted_base_commit = GitCommitId::parse(row.base_commit)
            .map_err(|error| format!("stored candidate base commit is invalid: {error}"))?;
        let persisted_base_tree = GitTreeId::parse(row.base_tree)
            .map_err(|error| format!("stored candidate base tree is invalid: {error}"))?;
        if application.repository.snapshot().base_commit() != &persisted_base_commit
            || application.repository.snapshot().base_tree() != &persisted_base_tree
        {
            return Err(
                "qualified repository head no longer matches the candidate's persisted base"
                    .to_owned(),
            );
        }
        let proposal_artifact_id =
            ArtifactId::new(row.proposal_artifact_id).map_err(|error| error.to_string())?;
        let proposal_bytes = self.artifact_bytes(proposal_artifact_id).await?;
        let proposal = parse_product_ticket_proposal_v2(
            &proposal_bytes,
            &application.bundle.ticket_policy.ticket_bounds,
        )
        .map_err(|error| format!("stored ticket proposal is invalid: {error}"))?;
        self.verify_proposal_artifacts(&proposal).await?;
        let stored_profile = parse_command_profile_v2(
            &self
                .artifact_bytes(
                    ArtifactId::new(row.reproducer_artifact_id)
                        .map_err(|error| error.to_string())?,
                )
                .await?,
        )
        .map_err(|error| format!("stored ticket reproducer profile is invalid: {error}"))?;
        let source_profile = parse_command_profile_v2(
            &self
                .artifact_bytes(proposal.reproducer.command.artifact_id)
                .await?,
        )
        .map_err(|error| format!("ticket proposal reproducer command is invalid: {error}"))?;
        if stored_profile != source_profile
            || application
                .bundle
                .reproducer_profiles
                .iter()
                .find(|profile| profile.name == proposal.reproducer_profile)
                != Some(&stored_profile)
        {
            return Err(
                "stored ticket reproducer no longer matches its admitted profile".to_owned(),
            );
        }
        // Preserve the complete admitted ticket/candidate evidence closure
        // across recovery. These reads re-verify CAS rather than trusting a
        // foreign key or artifact ID alone.
        let _ = self
            .artifact_bytes(
                ArtifactId::new(row.expected_observation_artifact_id)
                    .map_err(|error| error.to_string())?,
            )
            .await?;
        let _ = self
            .artifact_bytes(
                ArtifactId::new(row.discovery_observation_artifact_id)
                    .map_err(|error| error.to_string())?,
            )
            .await?;
        let _ = self
            .reference(
                ArtifactId::new(row.changed_paths_artifact_id)
                    .map_err(|error| error.to_string())?,
            )
            .await?;
        let _ = self
            .reference(
                ArtifactId::new(row.regression_patch_artifact_id)
                    .map_err(|error| error.to_string())?,
            )
            .await?;
        let _ = self
            .reference(
                ArtifactId::new(row.regression_command_set_artifact_id)
                    .map_err(|error| error.to_string())?,
            )
            .await?;
        let _ = self
            .reference(
                ArtifactId::new(row.regression_log_artifact_id)
                    .map_err(|error| error.to_string())?,
            )
            .await?;
        let candidate_patch = self
            .reference(ArtifactId::new(row.patch_artifact_id).map_err(|error| error.to_string())?)
            .await?;
        let _ = self
            .reference(
                ArtifactId::new(row.engineering_report_artifact_id)
                    .map_err(|error| error.to_string())?,
            )
            .await?;
        let _ = self
            .reference(ArtifactId::new(row.risks_artifact_id).map_err(|error| error.to_string())?)
            .await?;
        let ticket_id = TicketId::new(row.ticket_id).map_err(|error| error.to_string())?;
        let ticket_revision_id =
            TicketRevisionId::new(row.ticket_revision_id).map_err(|error| error.to_string())?;
        let kernel_build_id = kernel_build_id_bytes(row.build_digest, "build digest")?;
        let full_suite = full_suite_commands(&application.bundle)?;
        let timestamp_unix_seconds = row.engineering_started_at_seconds.ok_or_else(|| {
            "Engineering session start timestamp is missing from candidate recovery".to_owned()
        })?;
        Ok(CandidateRecovery {
            application: application.bundle,
            repository: application.repository,
            ticket: CandidateTicketBinding {
                ticket_id,
                ticket_attempt_id: action.ticket_attempt_id,
                ticket_revision_id,
                expected_attempt_revision: ExpectedRevision::new(attempt_revision),
                expected_ticket_revision: ExpectedRevision::new(persisted_revision(
                    row.ticket_revision,
                    "ticket revision",
                )?),
                ticket_revision_digest: ContentDigest::of_bytes(&proposal_bytes),
            },
            engineering_session_id: SessionId::new(row.engineering_session_id)
                .map_err(|error| error.to_string())?,
            kernel_build_id,
            campaign_id: factory_protocol::CampaignId::new(row.campaign_id)
                .map_err(|error| error.to_string())?,
            application_revision_id,
            candidate_tree: GitTreeId::parse(row.candidate_tree)
                .map_err(|error| format!("stored candidate tree is invalid: {error}"))?,
            regression_tree: GitTreeId::parse(row.regression_tree)
                .map_err(|error| format!("stored regression tree is invalid: {error}"))?,
            candidate_patch,
            submission: factory_protocol::CandidateSubmissionV2 {
                commit_subject: row.commit_subject,
                commit_body: row.commit_body,
                regression_test_identity: row.regression_test_identity,
            },
            product_reproducer: self
                .command_from_reproducer(&stored_profile, &proposal)
                .await?,
            full_suite,
            hard_validation_id: row
                .hard_validation_id
                .map(factory_protocol::ValidationId::new)
                .transpose()
                .map_err(|error| error.to_string())?,
            engineering_started_at_seconds: timestamp_unix_seconds,
        })
    }

    async fn load_quality_candidate(
        &self,
        ticket_attempt_id: TicketAttemptId,
        candidate_id: CandidateId,
        assignment: &AssignmentContext,
    ) -> Result<QualityCandidate, String> {
        let row = sqlx::query!(
            "SELECT ta.revision AS attempt_revision,
                    c.ticket_attempt_id, c.base_commit, c.base_tree, c.regression_tree,
                    c.candidate_tree, c.regression_patch_artifact_id,
                    c.regression_command_set_artifact_id, c.regression_log_artifact_id,
                    c.patch_artifact_id, c.engineering_session_id,
                    c.engineering_report_artifact_id, v.id AS hard_validation_id,
                    c.candidate_commit, c.revision AS candidate_revision,
                    tr.id AS ticket_revision_id,
                    qv.id AS quality_validation_id,
                    qv.pristine_tree AS quality_validation_tree,
                    qv.log_artifact_id AS quality_validation_log_artifact_id,
                    qva.id AS quality_validation_audit_log_id
               FROM factory.candidates c
               JOIN factory.ticket_attempts ta ON ta.id = c.ticket_attempt_id
               JOIN factory.ticket_revisions tr ON tr.id = ta.ticket_revision_id
               JOIN factory.validations v ON v.candidate_id = c.id
                    AND v.validation_scope = 0 AND v.lifecycle = 1
               LEFT JOIN factory.validations qv ON qv.candidate_id = c.id
                    AND qv.validation_scope = 1 AND qv.lifecycle = 1
               LEFT JOIN factory.audit_log qva ON qva.subject_kind = 41
                    AND qva.subject_id = qv.id AND qva.operation = 'validation.record'
               LEFT JOIN factory.reviews qr ON qr.candidate_id = c.id
              WHERE c.id = $1 AND c.ticket_attempt_id = $2
                AND ta.campaign_id = $3 AND tr.application_revision_id = $4
                AND c.lifecycle = 1 AND c.candidate_commit IS NOT NULL
                AND (ta.stage IN (2, 6)
                     OR (ta.stage = 3 AND qv.id IS NOT NULL AND qr.id IS NULL))",
            candidate_id.get(),
            ticket_attempt_id.get(),
            assignment.campaign_id.get(),
            assignment.application_revision_id.get(),
        )
        .fetch_optional(&self.store.pool_for_authority())
        .await
        .map_err(db_error)?
        .ok_or_else(|| {
            "Quality assignment target is not an exact validated candidate".to_owned()
        })?;
        let candidate_commit = row
            .candidate_commit
            .ok_or_else(|| "validated candidate is missing its attached commit".to_owned())?;
        let packet = CandidatePacketV2 {
            candidate_id,
            ticket_attempt_id,
            ticket_revision_id: TicketRevisionId::new(row.ticket_revision_id)
                .map_err(|error| error.to_string())?,
            base_commit: RepositoryObjectIdV2::parse(row.base_commit)
                .map_err(|error| error.to_string())?,
            base_tree: RepositoryObjectIdV2::parse(row.base_tree)
                .map_err(|error| error.to_string())?,
            regression_tree: RepositoryObjectIdV2::parse(row.regression_tree)
                .map_err(|error| error.to_string())?,
            candidate_tree: RepositoryObjectIdV2::parse(row.candidate_tree)
                .map_err(|error| error.to_string())?,
            regression_patch: self
                .reference(
                    ArtifactId::new(row.regression_patch_artifact_id)
                        .map_err(|error| error.to_string())?,
                )
                .await?,
            regression_command_set: self
                .reference(
                    ArtifactId::new(row.regression_command_set_artifact_id)
                        .map_err(|error| error.to_string())?,
                )
                .await?,
            regression_log: self
                .reference(
                    ArtifactId::new(row.regression_log_artifact_id)
                        .map_err(|error| error.to_string())?,
                )
                .await?,
            candidate_patch: self
                .reference(
                    ArtifactId::new(row.patch_artifact_id).map_err(|error| error.to_string())?,
                )
                .await?,
            engineering_session_id: SessionId::new(row.engineering_session_id)
                .map_err(|error| error.to_string())?,
            engineering_report: self
                .reference(
                    ArtifactId::new(row.engineering_report_artifact_id)
                        .map_err(|error| error.to_string())?,
                )
                .await?,
            hard_validation_id: factory_protocol::ValidationId::new(row.hard_validation_id)
                .map_err(|error| error.to_string())?,
            candidate_commit: RepositoryObjectIdV2::parse(candidate_commit)
                .map_err(|error| error.to_string())?,
            candidate_revision: persisted_revision(row.candidate_revision, "candidate revision")?,
        };
        packet.validate().map_err(|error| error.to_string())?;
        let prior_full_suite = match row
            .quality_validation_id
            .map(factory_protocol::ValidationId::new)
            .transpose()
            .map_err(|error| error.to_string())?
        {
            Some(validation_id) => {
                let validation_tree =
                    RepositoryObjectIdV2::parse(row.quality_validation_tree.ok_or_else(|| {
                        "persisted Quality validation has no pristine tree".to_owned()
                    })?)
                    .map_err(|error| error.to_string())?;
                if validation_tree != packet.candidate_tree {
                    return Err(
                        "persisted Quality validation tree differs from candidate tree".to_owned(),
                    );
                }
                let audit_log_id = row.quality_validation_audit_log_id.ok_or_else(|| {
                    "persisted Quality validation has no audit receipt".to_owned()
                })?;
                Some(QualityFullSuiteOutcome {
                    receipt: factory_protocol::QualityValidationReceiptV2 {
                        validation_id,
                        candidate_id,
                        candidate_tree: packet.candidate_tree.clone(),
                        log_artifact: self
                            .reference(
                                ArtifactId::new(
                                    row.quality_validation_log_artifact_id.ok_or_else(|| {
                                        "persisted Quality validation has no log artifact"
                                            .to_owned()
                                    })?,
                                )
                                .map_err(|error| error.to_string())?,
                            )
                            .await?,
                        revision: packet.candidate_revision,
                    },
                    result: crate::decision_store::ValidationResult::Passed,
                    resulting_attempt_revision: persisted_revision(
                        row.attempt_revision,
                        "attempt revision",
                    )?,
                    audit_log_id,
                })
            }
            None => None,
        };
        Ok(QualityCandidate {
            packet,
            attempt_revision: persisted_revision(row.attempt_revision, "attempt revision")?,
            prior_full_suite,
        })
    }

    /// Rehydrates every proposal-owned artifact from the exact admitted
    /// proposal bytes.  The proposal reference alone is not sufficient for an
    /// actor to inspect the reproducer observations that define its contract.
    async fn proposal_evidence(
        &self,
        application: &ApplicationBundleV2,
        proposal_artifact_id: ArtifactId,
    ) -> Result<DurableProposalEvidence, String> {
        let proposal_bytes = self.artifact_bytes(proposal_artifact_id).await?;
        let proposal = parse_product_ticket_proposal_v2(
            &proposal_bytes,
            &application.ticket_policy.ticket_bounds,
        )
        .map_err(|error| format!("stored ticket proposal is invalid: {error}"))?;
        self.verify_proposal_artifacts(&proposal).await?;
        Ok(DurableProposalEvidence {
            proposal: self.reference(proposal_artifact_id).await?,
            narrative: proposal.narrative,
            evidence: proposal.evidence,
            reproducer_command: proposal.reproducer.command,
            reproducer_stdin: proposal.reproducer.stdin,
            expected_observation: DurableObservationEvidence {
                stdout: proposal.reproducer.expected_observation.stdout,
                stderr: proposal.reproducer.expected_observation.stderr,
            },
            first_observation: DurableObservationEvidence {
                stdout: proposal.reproducer.first_observation.stdout,
                stderr: proposal.reproducer.first_observation.stderr,
            },
            second_observation: DurableObservationEvidence {
                stdout: proposal.reproducer.second_observation.stdout,
                stderr: proposal.reproducer.second_observation.stderr,
            },
        })
    }

    /// The checkpoint action echoes two fixed ticket-bound values. Render
    /// them into the Engineering prompt from the same admitted profile that
    /// the later live resolver verifies, so the actor never has to infer an
    /// opaque string from a command line or an artifact layout.
    async fn engineering_checkpoint_contract(
        &self,
        application: &ApplicationBundleV2,
        ticket_attempt_id: TicketAttemptId,
        proposal_artifact_id: ArtifactId,
        reproducer_artifact_id: ArtifactId,
    ) -> Result<EngineeringCheckpointContract, String> {
        let proposal_bytes = self.artifact_bytes(proposal_artifact_id).await?;
        let proposal = parse_product_ticket_proposal_v2(
            &proposal_bytes,
            &application.ticket_policy.ticket_bounds,
        )
        .map_err(|error| format!("stored ticket proposal is invalid: {error}"))?;
        self.verify_proposal_artifacts(&proposal).await?;
        let stored_profile =
            parse_command_profile_v2(&self.artifact_bytes(reproducer_artifact_id).await?)
                .map_err(|error| format!("stored ticket reproducer profile is invalid: {error}"))?;
        let proposal_profile = parse_command_profile_v2(
            &self
                .artifact_bytes(proposal.reproducer.command.artifact_id)
                .await?,
        )
        .map_err(|error| format!("ticket proposal reproducer profile is invalid: {error}"))?;
        if stored_profile != proposal_profile
            || application
                .reproducer_profiles
                .iter()
                .find(|profile| profile.name == proposal.reproducer_profile)
                != Some(&stored_profile)
        {
            return Err(
                "stored ticket reproducer no longer matches its admitted profile".to_owned(),
            );
        }
        Ok(EngineeringCheckpointContract {
            regression_command: stored_profile.name.clone(),
            expected_failure: format!(
                "ticket-attempt-{}-{}",
                ticket_attempt_id.get(),
                stored_profile.name,
            ),
        })
    }

    /// Resolves the exact evidence closure already attached to one validated
    /// candidate.  Every artifact is converted through `reference`, which
    /// proves the registered CAS seal remains readable before the assignment
    /// can name it to an actor.
    async fn candidate_evidence(
        &self,
        ticket_attempt_id: TicketAttemptId,
        candidate_id: CandidateId,
    ) -> Result<DurableCandidateEvidence, String> {
        let row = sqlx::query!(
            "SELECT c.changed_paths_artifact_id, c.regression_patch_artifact_id,
                    c.regression_command_set_artifact_id, c.regression_log_artifact_id,
                    c.patch_artifact_id, c.engineering_report_artifact_id,
                    c.risks_artifact_id AS engineering_risks_artifact_id,
                    hv.command_set_artifact_id AS hard_validation_command_set_artifact_id,
                    hv.log_artifact_id AS hard_validation_log_artifact_id,
                    review.additional_probes_artifact_id AS prior_quality_additional_probes_artifact_id,
                    review.rationale_artifact_id AS prior_quality_rationale_artifact_id,
                    review.risks_artifact_id AS prior_quality_risks_artifact_id,
                    decision.rationale_artifact_id AS architect_rationale_artifact_id
               FROM factory.candidates c
               JOIN factory.validations hv ON hv.candidate_id = c.id
                    AND hv.validation_scope = 0 AND hv.lifecycle = 1
               LEFT JOIN factory.reviews review ON review.candidate_id = c.id
               LEFT JOIN LATERAL (
                    SELECT rationale_artifact_id
                      FROM factory.architect_decisions
                     WHERE candidate_id = c.id
                     ORDER BY created_at DESC, id DESC
                     LIMIT 1
               ) decision ON TRUE
              WHERE c.id = $1 AND c.ticket_attempt_id = $2 AND c.lifecycle = 1",
            candidate_id.get(),
            ticket_attempt_id.get(),
        )
        .fetch_optional(&self.store.pool_for_authority())
        .await
        .map_err(db_error)?
        .ok_or_else(|| "candidate evidence is not available at this durable stage".to_owned())?;
        let prior_quality_additional_probes = match row
            .prior_quality_additional_probes_artifact_id
            .map(ArtifactId::new)
            .transpose()
            .map_err(|error| error.to_string())?
        {
            Some(artifact_id) => Some(self.reference(artifact_id).await?),
            None => None,
        };
        let prior_quality_rationale = match row
            .prior_quality_rationale_artifact_id
            .map(ArtifactId::new)
            .transpose()
            .map_err(|error| error.to_string())?
        {
            Some(artifact_id) => Some(self.reference(artifact_id).await?),
            None => None,
        };
        let prior_quality_risks = match row
            .prior_quality_risks_artifact_id
            .map(ArtifactId::new)
            .transpose()
            .map_err(|error| error.to_string())?
        {
            Some(artifact_id) => Some(self.reference(artifact_id).await?),
            None => None,
        };
        let architect_rationale = match row
            .architect_rationale_artifact_id
            .map(ArtifactId::new)
            .transpose()
            .map_err(|error| error.to_string())?
        {
            Some(artifact_id) => Some(self.reference(artifact_id).await?),
            None => None,
        };
        Ok(DurableCandidateEvidence {
            changed_paths: self
                .reference(
                    ArtifactId::new(row.changed_paths_artifact_id)
                        .map_err(|error| error.to_string())?,
                )
                .await?,
            regression_patch: self
                .reference(
                    ArtifactId::new(row.regression_patch_artifact_id)
                        .map_err(|error| error.to_string())?,
                )
                .await?,
            regression_command_set: self
                .reference(
                    ArtifactId::new(row.regression_command_set_artifact_id)
                        .map_err(|error| error.to_string())?,
                )
                .await?,
            regression_log: self
                .reference(
                    ArtifactId::new(row.regression_log_artifact_id)
                        .map_err(|error| error.to_string())?,
                )
                .await?,
            candidate_patch: self
                .reference(
                    ArtifactId::new(row.patch_artifact_id).map_err(|error| error.to_string())?,
                )
                .await?,
            engineering_report: self
                .reference(
                    ArtifactId::new(row.engineering_report_artifact_id)
                        .map_err(|error| error.to_string())?,
                )
                .await?,
            engineering_risks: self
                .reference(
                    ArtifactId::new(row.engineering_risks_artifact_id)
                        .map_err(|error| error.to_string())?,
                )
                .await?,
            hard_validation_command_set: self
                .reference(
                    ArtifactId::new(row.hard_validation_command_set_artifact_id)
                        .map_err(|error| error.to_string())?,
                )
                .await?,
            hard_validation_log: self
                .reference(
                    ArtifactId::new(row.hard_validation_log_artifact_id)
                        .map_err(|error| error.to_string())?,
                )
                .await?,
            prior_quality_additional_probes,
            prior_quality_rationale,
            prior_quality_risks,
            architect_rationale,
        })
    }

    async fn reference(
        &self,
        artifact_id: ArtifactId,
    ) -> Result<SealedArtifactReferenceV2, String> {
        let process = self.store.process_store();
        let seal = process
            .registered_artifact(&self.cas, artifact_id)
            .await
            .map_err(|error| {
                format!(
                    "artifact reference {} is unavailable: {error}",
                    artifact_id.get()
                )
            })?;
        Ok(SealedArtifactReferenceV2 {
            artifact_id,
            digest: seal.digest(),
            byte_length: seal.byte_length(),
        })
    }
}

impl CandidateQualityAuthorityResolver for DurableAuthorityResolver {
    fn resolve_engineering<'a>(
        &'a self,
        session_id: SessionId,
        packet: &'a AssignmentPacketV2,
    ) -> CandidateQualityAuthorityFuture<'a, ResolvedEngineeringCandidateAuthority> {
        Box::pin(async move {
            self.resolve_engineering_inner(session_id, packet)
                .await
                .map_err(
                    |message| CandidateQualityAuthorityResolutionError::Precondition { message },
                )
        })
    }

    fn resolve_quality<'a>(
        &'a self,
        session_id: SessionId,
        packet: &'a AssignmentPacketV2,
    ) -> CandidateQualityAuthorityFuture<'a, ResolvedQualityCandidateAuthority> {
        Box::pin(async move {
            self.resolve_quality_inner(session_id, packet)
                .await
                .map_err(
                    |message| CandidateQualityAuthorityResolutionError::Precondition { message },
                )
        })
    }
}

impl ArchitectTransitionResolver for DurableAuthorityResolver {
    fn resolve_release<'a>(
        &'a self,
        ticket_attempt_id: TicketAttemptId,
        caller_expected_attempt_revision: ExpectedRevision,
    ) -> ArchitectTransitionFuture<'a, ResolvedReleaseTransition> {
        Box::pin(async move {
            let row = sqlx::query!(
                "SELECT ta.revision AS attempt_revision, tr.revision AS ticket_revision,
                        tr.application_revision_id, tr.proposal_artifact_id,
                        tr.reproducer_artifact_id, c.kernel_build_id,
                        kb.build_digest
                   FROM factory.ticket_attempts ta
                   JOIN factory.ticket_revisions tr ON tr.id = ta.ticket_revision_id
                   JOIN factory.campaigns c ON c.id = ta.campaign_id
                   JOIN factory.kernel_builds kb ON kb.id = c.kernel_build_id
                  WHERE ta.id = $1 AND ta.stage IN (8, 9) AND ta.released_at IS NULL",
                ticket_attempt_id.get(),
            )
            .fetch_optional(&self.store.pool_for_authority())
            .await
            .map_err(|error| ArchitectTransitionResolutionError::Precondition {
                message: db_error(error),
            })?
            .ok_or_else(|| ArchitectTransitionResolutionError::Precondition {
                message: "ticket attempt is not a failed or cancelled unreleased attempt"
                    .to_owned(),
            })?;
            let attempt_revision = persisted_revision(row.attempt_revision, "attempt revision")
                .map_err(precondition)?;
            if caller_expected_attempt_revision.get() != attempt_revision {
                return Err(ArchitectTransitionResolutionError::RevisionConflict {
                    expected: caller_expected_attempt_revision.get().get(),
                    current: attempt_revision.get(),
                });
            }
            let application_revision_id = ApplicationRevisionId::new(row.application_revision_id)
                .map_err(|error| precondition(error.to_string()))?;
            let application = self
                .load_application(application_revision_id)
                .await
                .map_err(precondition)?;
            let proposal_bytes = self
                .artifact_bytes(
                    ArtifactId::new(row.proposal_artifact_id)
                        .map_err(|error| precondition(error.to_string()))?,
                )
                .await
                .map_err(precondition)?;
            let proposal = parse_product_ticket_proposal_v2(
                &proposal_bytes,
                &application.bundle.ticket_policy.ticket_bounds,
            )
            .map_err(|error| precondition(format!("stored ticket proposal is invalid: {error}")))?;
            self.verify_proposal_artifacts(&proposal)
                .await
                .map_err(precondition)?;
            let profile_bytes = self
                .artifact_bytes(
                    ArtifactId::new(row.reproducer_artifact_id)
                        .map_err(|error| precondition(error.to_string()))?,
                )
                .await
                .map_err(precondition)?;
            let stored_profile = parse_command_profile_v2(&profile_bytes).map_err(|error| {
                precondition(format!(
                    "stored ticket reproducer profile is invalid: {error}"
                ))
            })?;
            let source_profile_bytes = self
                .artifact_bytes(proposal.reproducer.command.artifact_id)
                .await
                .map_err(precondition)?;
            let source_profile =
                parse_command_profile_v2(&source_profile_bytes).map_err(|error| {
                    precondition(format!(
                        "ticket proposal reproducer command is invalid: {error}"
                    ))
                })?;
            if stored_profile != source_profile
                || application
                    .bundle
                    .reproducer_profiles
                    .iter()
                    .find(|profile| profile.name == proposal.reproducer_profile)
                    != Some(&stored_profile)
            {
                return Err(precondition(
                    "stored ticket reproducer no longer matches its admitted profile".to_owned(),
                ));
            }
            let reproducer = self
                .command_from_reproducer(&stored_profile, &proposal)
                .await
                .map_err(precondition)?;
            let workspace =
                CommandWorkspace::open(application.repository.root()).map_err(|error| {
                    precondition(format!(
                        "qualified repository cannot be used as command workspace: {error}"
                    ))
                })?;
            let reproduction = self
                .runner
                .run_discovery_reproducer(&workspace, &reproducer)
                .map_err(|error| {
                    precondition(format!("current-head reproducer failed to run: {error}"))
                })?;
            let kernel_build_id =
                kernel_build_id_bytes(row.build_digest, "build digest").map_err(precondition)?;
            let principal = "kernel-architect-requalification";
            let command_prefix = format!("requalification-attempt-{}", ticket_attempt_id.get());
            let process = self.store.process_store();
            let first = seal_command_observation_manifest(
                &process,
                &self.cas,
                principal,
                &format!("{command_prefix}-first"),
                kernel_build_id,
                reproduction.first(),
            )
            .await
            .map_err(precondition)?;
            let second = seal_command_observation_manifest(
                &process,
                &self.cas,
                principal,
                &format!("{command_prefix}-second"),
                kernel_build_id,
                reproduction.second(),
            )
            .await
            .map_err(precondition)?;
            let (first_actual_observation_artifact_id, second_actual_observation_artifact_id) =
                canonical_requalification_observations(first, second);
            Ok(ResolvedReleaseTransition {
                expected_attempt_revision: ExpectedRevision::new(attempt_revision),
                expected_ticket_revision: ExpectedRevision::new(
                    persisted_revision(row.ticket_revision, "ticket revision")
                        .map_err(precondition)?,
                ),
                requalification: crate::ticket_store::CurrentHeadRequalification {
                    current_head_commit: application
                        .repository
                        .snapshot()
                        .base_commit()
                        .to_string(),
                    current_head_tree: application.repository.snapshot().base_tree().to_string(),
                    first_actual_observation_artifact_id,
                    second_actual_observation_artifact_id,
                },
            })
        })
    }

    fn resolve_candidate_decision<'a>(
        &'a self,
        candidate_id: CandidateId,
        review_id: ReviewId,
        caller_expected_candidate_revision: ExpectedRevision,
    ) -> ArchitectTransitionFuture<'a, ResolvedCandidateDecisionTransition> {
        Box::pin(async move {
            let row = sqlx::query!(
                "SELECT c.revision AS candidate_revision, ta.revision AS attempt_revision,
                        tr.revision AS ticket_revision
                   FROM factory.candidates c
                   JOIN factory.ticket_attempts ta ON ta.id = c.ticket_attempt_id
                   JOIN factory.ticket_revisions tr ON tr.id = ta.ticket_revision_id
                   JOIN factory.reviews r ON r.candidate_id = c.id
                  WHERE c.id = $1 AND r.id = $2",
                candidate_id.get(),
                review_id.get(),
            )
            .fetch_optional(&self.store.pool_for_authority())
            .await
            .map_err(|error| ArchitectTransitionResolutionError::Precondition {
                message: db_error(error),
            })?
            .ok_or_else(|| ArchitectTransitionResolutionError::Precondition {
                message: "candidate decision does not name its exact persisted review".to_owned(),
            })?;
            let candidate_revision =
                persisted_revision(row.candidate_revision, "candidate revision")
                    .map_err(precondition)?;
            if caller_expected_candidate_revision.get() != candidate_revision {
                return Err(ArchitectTransitionResolutionError::RevisionConflict {
                    expected: caller_expected_candidate_revision.get().get(),
                    current: candidate_revision.get(),
                });
            }
            Ok(ResolvedCandidateDecisionTransition {
                expected_candidate_revision: ExpectedRevision::new(candidate_revision),
                expected_attempt_revision: ExpectedRevision::new(
                    persisted_revision(row.attempt_revision, "attempt revision")
                        .map_err(precondition)?,
                ),
                expected_ticket_revision: ExpectedRevision::new(
                    persisted_revision(row.ticket_revision, "ticket revision")
                        .map_err(precondition)?,
                ),
            })
        })
    }
}

#[derive(Clone)]
struct ApplicationContext {
    bundle: ApplicationBundleV2,
    repository: QualifiedRepository,
}

/// Exact daemon-owned materialization input for one non-Product assignment.
/// Engineering retains the qualified base as its detached `HEAD`; Quality
/// receives the kernel-captured candidate commit as its detached `HEAD`, whose
/// tree must equal `materialize_tree`. Validation-only materialization may
/// still write an exact tree under the qualified base without creating a new
/// commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DurableAssignmentLaunchContext {
    pub application_revision_id: ApplicationRevisionId,
    pub target: DurableAssignmentTarget,
    pub repository: QualifiedRepository,
    pub materialize_commit: GitCommitId,
    pub materialize_tree: GitTreeId,
    pub ticket_id: Option<TicketId>,
    pub ticket_revision_id: Option<TicketRevisionId>,
    /// The passed hard Candidate validation that authorizes Quality. It is
    /// absent for Product and Engineering; Quality launch refuses without it.
    pub validation_id: Option<factory_protocol::ValidationId>,
    /// The exact fixed values the Engineering checkpoint must echo. This is
    /// absent outside Engineering so no unrelated assignment can render it.
    pub engineering_checkpoint: Option<EngineeringCheckpointContract>,
    /// Exact sealed Product contract upstream of an Engineering/Quality
    /// assignment. Product has no preceding ticket proposal.
    pub proposal: Option<SealedArtifactReferenceV2>,
    /// Bounded, named immutable evidence closure rendered into the assignment
    /// target.  It is intentionally not a metadata map: each reference has a
    /// stable meaning, is re-verified from CAS here, and can be allowlisted by
    /// `artifact.read` without granting arbitrary CAS navigation.
    pub evidence: DurableAssignmentEvidence,
    /// Application-required paths, exactly as admitted with the application.
    pub application_required_reads: Vec<RequiredReadV2>,
    /// Ticket-specific contract reads, parsed from the sealed admitted
    /// proposal. Product has none because it has no upstream ticket target.
    pub ticket_contract_reads: Vec<TicketContractReadV2>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineeringCheckpointContract {
    pub regression_command: String,
    pub expected_failure: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DurableAssignmentEvidence {
    pub proposal: Option<DurableProposalEvidence>,
    pub candidate: Option<DurableCandidateEvidence>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DurableProposalEvidence {
    pub proposal: SealedArtifactReferenceV2,
    pub narrative: SealedArtifactReferenceV2,
    pub evidence: SealedArtifactReferenceV2,
    pub reproducer_command: SealedArtifactReferenceV2,
    pub reproducer_stdin: Option<SealedArtifactReferenceV2>,
    pub expected_observation: DurableObservationEvidence,
    pub first_observation: DurableObservationEvidence,
    pub second_observation: DurableObservationEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DurableObservationEvidence {
    pub stdout: SealedArtifactReferenceV2,
    pub stderr: SealedArtifactReferenceV2,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DurableCandidateEvidence {
    pub changed_paths: SealedArtifactReferenceV2,
    pub regression_patch: SealedArtifactReferenceV2,
    pub regression_command_set: SealedArtifactReferenceV2,
    pub regression_log: SealedArtifactReferenceV2,
    pub candidate_patch: SealedArtifactReferenceV2,
    pub engineering_report: SealedArtifactReferenceV2,
    pub engineering_risks: SealedArtifactReferenceV2,
    pub hard_validation_command_set: SealedArtifactReferenceV2,
    pub hard_validation_log: SealedArtifactReferenceV2,
    /// Rework Quality must receive the prior additional-probes receipt as
    /// immutable evidence, rather than reconstructing it from a review row.
    pub prior_quality_additional_probes: Option<SealedArtifactReferenceV2>,
    pub prior_quality_rationale: Option<SealedArtifactReferenceV2>,
    pub prior_quality_risks: Option<SealedArtifactReferenceV2>,
    pub architect_rationale: Option<SealedArtifactReferenceV2>,
}

/// Narrow daemon-driver delivery input. All repository, tree, commit, and
/// aggregate revision facts are reread by [`DurableAuthorityResolver`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliverAcceptedCandidate {
    pub principal: String,
    pub command_id: String,
    pub candidate_id: CandidateId,
}

#[derive(Serialize)]
struct LocalDeliveryReceiptBytes<'a> {
    candidate_id: i64,
    expected_old_commit: &'a str,
    resulting_commit: &'a str,
    resulting_tree: &'a str,
    factory_cost_micro_usd: u64,
    method: &'static str,
}

fn local_delivery_receipt_bytes(
    candidate_id: CandidateId,
    expected_old_commit: &GitCommitId,
    resulting_commit: &GitCommitId,
    resulting_tree: &GitTreeId,
    factory_cost_micro_usd: u64,
) -> Vec<u8> {
    json::to_string(&LocalDeliveryReceiptBytes {
        candidate_id: candidate_id.get(),
        expected_old_commit: expected_old_commit.as_str(),
        resulting_commit: resulting_commit.as_str(),
        resulting_tree: resulting_tree.as_str(),
        factory_cost_micro_usd,
        method: "guarded-local-fast-forward-v1",
    })
    .into_bytes()
}

/// Narrow durable context shared by target-specific reads. It intentionally
/// contains no actor-selected repository or revision values.
struct AssignmentContext {
    campaign_id: factory_protocol::CampaignId,
    application_revision_id: ApplicationRevisionId,
}

struct EngineeringTicket {
    ticket_id: TicketId,
    ticket_revision_id: TicketRevisionId,
    ticket_revision: AggregateRevision,
    attempt_revision: AggregateRevision,
    proposal_artifact_id: ArtifactId,
    reproducer_artifact_id: ArtifactId,
    expected_observation_artifact_id: ArtifactId,
    discovery_observation_artifact_id: ArtifactId,
}

struct QualityCandidate {
    packet: CandidatePacketV2,
    attempt_revision: AggregateRevision,
    prior_full_suite: Option<QualityFullSuiteOutcome>,
}

/// Private decoded durable state shared by the two candidate recovery
/// operations. It is assembled only after every persisted artifact is
/// re-verified from CAS and all scheduler revisions still match.
struct CandidateRecovery {
    application: ApplicationBundleV2,
    repository: QualifiedRepository,
    ticket: CandidateTicketBinding,
    engineering_session_id: SessionId,
    kernel_build_id: KernelBuildId,
    campaign_id: factory_protocol::CampaignId,
    application_revision_id: ApplicationRevisionId,
    candidate_tree: GitTreeId,
    regression_tree: GitTreeId,
    candidate_patch: SealedArtifactReferenceV2,
    submission: factory_protocol::CandidateSubmissionV2,
    product_reproducer: DeterministicCommand,
    full_suite: Vec<DeterministicCommand>,
    hard_validation_id: Option<factory_protocol::ValidationId>,
    engineering_started_at_seconds: i64,
}

/// Stores the same status-only observation-manifest bytes used at Product
/// admission. Full raw output remains sealed alongside it; DecisionStore
/// compares the named comparison identity for current-head reproduction.
async fn seal_command_observation_manifest(
    process: &crate::process::ProcessStore,
    cas: &CasStore,
    principal: &str,
    command_prefix: &str,
    kernel_build_id: KernelBuildId,
    receipt: &CommandReceipt,
) -> Result<ArtifactId, String> {
    let (_stdout, _) = process
        .adopt_and_register_kernel_bytes(
            cas,
            principal,
            &format!("{command_prefix}-stdout"),
            kernel_build_id,
            receipt.stdout(),
        )
        .await
        .map_err(|error| format!("could not seal current-head stdout: {error}"))?;
    let (_stderr, _) = process
        .adopt_and_register_kernel_bytes(
            cas,
            principal,
            &format!("{command_prefix}-stderr"),
            kernel_build_id,
            receipt.stderr(),
        )
        .await
        .map_err(|error| format!("could not seal current-head stderr: {error}"))?;
    let bytes = product_observation_manifest_bytes(&receipt.terminal());
    let (_, manifest_receipt) = process
        .adopt_and_register_kernel_bytes(
            cas,
            principal,
            &format!("{command_prefix}-manifest"),
            kernel_build_id,
            &bytes,
        )
        .await
        .map_err(|error| format!("could not seal current-head observation manifest: {error}"))?;
    Ok(manifest_receipt.artifact_id)
}

fn claim_requalification_command_prefix(
    campaign_id: i64,
    ticket_revision_id: i64,
    ticket_revision: u64,
) -> String {
    format!(
        "claim-campaign-{campaign_id}-ticket-revision-{ticket_revision_id}-revision-{ticket_revision}"
    )
}

/// Ticket store requalification predates status-only Product discovery and
/// compares its two stored replay identities byte-for-byte. Keep both raw
/// receipts sealed for diagnosis; make the first one the closed, canonical
/// replay identity for the admitted status-only pair.
fn canonical_requalification_observations(
    first: ArtifactId,
    _second_raw_diagnostic: ArtifactId,
) -> (ArtifactId, ArtifactId) {
    (first, first)
}

fn requalification_manifest_pair(
    first: Option<ArtifactId>,
    second: Option<ArtifactId>,
) -> Result<Option<(ArtifactId, ArtifactId)>, &'static str> {
    match (first, second) {
        (Some(first), Some(second)) => Ok(Some((first, second))),
        (None, None) => Ok(None),
        _ => Err("current-head requalification recovery found only one sealed manifest"),
    }
}

fn full_suite_commands(bundle: &ApplicationBundleV2) -> Result<Vec<DeterministicCommand>, String> {
    bundle
        .validation_profiles
        .full
        .iter()
        .cloned()
        .map(|profile| {
            DeterministicCommand::new(
                profile,
                CommandStdin::Empty,
                CommandExpectation::new(
                    ComparisonRevision::parse(EXACT_OBSERVATION_COMPARISON)
                        .map_err(|error| error.to_string())?,
                    None,
                    None,
                ),
            )
            .map_err(|error| format!("full-suite command is invalid: {error}"))
        })
        .collect()
}

fn worktree_name(prefix: &str, session_id: i64) -> Result<WorktreeName, String> {
    WorktreeName::parse(format!("{prefix}-{session_id}"))
        .map_err(|error| format!("derived worktree name is invalid: {error}"))
}

fn persisted_revision(value: i64, name: &str) -> Result<AggregateRevision, String> {
    let value = u64::try_from(value).map_err(|_| format!("durable revision {name} is negative"))?;
    Ok(AggregateRevision::from_persisted(value))
}

fn kernel_build_id_bytes(bytes: Vec<u8>, name: &str) -> Result<KernelBuildId, String> {
    let bytes: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| format!("durable field {name} is not a BLAKE3 digest"))?;
    Ok(KernelBuildId::new(ContentDigest::from_bytes(bytes)))
}

fn db_error(error: sqlx::Error) -> String {
    format!("durable authority read failed: {error}")
}

fn precondition(message: String) -> ArchitectTransitionResolutionError {
    ArchitectTransitionResolutionError::Precondition { message }
}

#[cfg(test)]
mod tests {
    use factory_protocol::ArtifactId;

    use super::{
        canonical_requalification_observations, claim_requalification_command_prefix,
        requalification_manifest_pair,
    };

    #[test]
    fn claim_requalification_keys_reuse_only_for_one_exact_sponsored_revision() {
        let first = claim_requalification_command_prefix(17, 4, 8);
        assert_eq!(first, claim_requalification_command_prefix(17, 4, 8));
        assert_ne!(first, claim_requalification_command_prefix(18, 4, 8));
        assert_ne!(first, claim_requalification_command_prefix(17, 5, 8));
        assert_ne!(first, claim_requalification_command_prefix(17, 4, 9));
    }

    #[test]
    fn status_only_requalification_uses_one_canonical_replay_identity() {
        let first = ArtifactId::new(41).expect("non-zero artifact id");
        let second = ArtifactId::new(42).expect("non-zero artifact id");

        assert_eq!(
            canonical_requalification_observations(first, second),
            (first, first)
        );
    }

    #[test]
    fn requalification_recovery_reuses_only_a_complete_manifest_pair() {
        let first = ArtifactId::new(41).expect("non-zero artifact id");
        let second = ArtifactId::new(42).expect("non-zero artifact id");
        assert_eq!(
            requalification_manifest_pair(Some(first), Some(second)).expect("complete pair"),
            Some((first, second)),
        );
        assert_eq!(
            requalification_manifest_pair(None, None).expect("no prior pair"),
            None,
        );
        assert!(requalification_manifest_pair(Some(first), None).is_err());
        assert!(requalification_manifest_pair(None, Some(second)).is_err());
    }
}
