//! Durable composition for Candidate, Quality, and Architect transitions.
//!
//! The actor packet names only the immutable assignment target.  This module
//! resolves the rest from PostgreSQL, re-verifies every referenced CAS object,
//! and obtains Git/worktree facts from daemon-owned custody.  It deliberately
//! is one direct composition object, not a repository/service framework.

use std::sync::Arc;

use factory_protocol::{
    AggregateRevision, ApplicationBundleV1, ApplicationRevisionId, ArtifactId, AssignmentPacketV1,
    CandidateId, CandidatePacketV1, ContentDigest, ExpectedRevision, KernelBuildId, Office,
    RepositoryObjectIdV1, RequiredReadV1, ReviewId, SealedArtifactReferenceV1, SessionId,
    TicketAttemptId, TicketContractReadV1, TicketId, TicketRevisionId, parse_application_bundle_v1,
    parse_command_profile_v1, parse_product_ticket_proposal_v1,
};
use miniserde::{Serialize, json};
use sqlx::{Row, postgres::PgRow};

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
        CommandExpectation, CommandReceipt, CommandRunner, CommandStdin, CommandTerminal,
        CommandWorkspace, ComparisonRevision, DeterministicCommand, ExactBytes,
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
    fn from_packet(packet: &AssignmentPacketV1) -> Result<Self, String> {
        match (packet.office, packet.ticket_attempt_id, packet.candidate_id) {
            (Office::ProductResearch, None, None) => Ok(Self::Product),
            (Office::Engineering, Some(ticket_attempt_id), None) => {
                Ok(Self::Engineering { ticket_attempt_id })
            }
            (Office::Quality, Some(ticket_attempt_id), Some(candidate_id)) => Ok(Self::Quality {
                ticket_attempt_id,
                candidate_id,
            }),
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
            commit: recovery.commit,
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
            commit: recovery.commit,
        };
        resume_candidate_commit_attach(&self.store.decision_store(), &self.git, &authority)
            .await
            .map_err(|error| format!("candidate commit attachment recovery failed: {error}"))
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
        let row = sqlx::query(
            "SELECT c.ticket_attempt_id, c.base_commit, c.candidate_tree, c.candidate_commit,
                    c.revision AS candidate_revision, ta.revision AS attempt_revision,
                    tr.revision AS ticket_revision, tr.ticket_id, tr.application_revision_id,
                    camp.revision AS campaign_revision, kb.build_digest
               FROM factory.candidates c
               JOIN factory.ticket_attempts ta ON ta.id = c.ticket_attempt_id
               JOIN factory.ticket_revisions tr ON tr.id = ta.ticket_revision_id
               JOIN factory.campaigns camp ON camp.id = ta.campaign_id
               JOIN factory.kernel_builds kb ON kb.id = camp.kernel_build_id
              WHERE c.id = $1 AND c.lifecycle = 3 AND ta.stage = 3 AND camp.lifecycle = 0",
        )
        .bind(command.candidate_id.get())
        .fetch_optional(&self.store.pool_for_authority())
        .await
        .map_err(db_error)?
        .ok_or_else(|| "candidate is not accepted and awaiting local delivery".to_owned())?;
        let application_revision_id =
            ApplicationRevisionId::new(field(&row, "application_revision_id")?)
                .map_err(|error| error.to_string())?;
        let repository = self
            .load_application(application_revision_id)
            .await?
            .repository;
        let ticket_id =
            TicketId::new(field(&row, "ticket_id")?).map_err(|error| error.to_string())?;
        let candidate_commit = GitCommitId::parse(field::<String>(&row, "candidate_commit")?)
            .map_err(|error| format!("stored candidate commit is invalid: {error}"))?;
        let candidate_tree = tree(&row, "candidate_tree")?;
        let recovered = self
            .git
            .recover_candidate_commit(
                &repository,
                CandidateRefName::new(ticket_id, command.candidate_id),
                candidate_commit,
                candidate_tree,
            )
            .map_err(|error| format!("stored candidate commit cannot be delivered: {error}"))?;
        let delivery = self
            .git
            .guarded_local_fast_forward(&repository, &recovered)
            .map_err(|error| format!("guarded local delivery failed: {error}"))?;
        let kernel_build_id = kernel_build_id(&row, "build_digest")?;
        let receipt_bytes = local_delivery_receipt_bytes(
            command.candidate_id,
            &delivery.previous_commit,
            &delivery.delivered_commit,
            &delivery.delivered_tree,
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
                expected_candidate_revision: ExpectedRevision::new(revision(
                    &row,
                    "candidate_revision",
                )?),
                expected_attempt_revision: ExpectedRevision::new(revision(
                    &row,
                    "attempt_revision",
                )?),
                expected_ticket_revision: ExpectedRevision::new(revision(&row, "ticket_revision")?),
                expected_campaign_revision: ExpectedRevision::new(revision(
                    &row,
                    "campaign_revision",
                )?),
                expected_old_commit: object(&row, "base_commit")?,
                resulting_commit: RepositoryObjectIdV1::parse(
                    delivery.delivered_commit.to_string(),
                )
                .map_err(|error| error.to_string())?,
                resulting_tree: RepositoryObjectIdV1::parse(delivery.delivered_tree.to_string())
                    .map_err(|error| error.to_string())?,
                receipt: SealedArtifactReferenceV1 {
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
        packet: &AssignmentPacketV1,
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
        let row = sqlx::query(
            "SELECT application_revision_id
               FROM factory.campaigns
              WHERE id = $1 AND lifecycle = 0",
        )
        .bind(request.campaign_id.get())
        .fetch_optional(&self.store.pool_for_authority())
        .await
        .map_err(db_error)?
        .ok_or_else(|| "campaign is absent or no longer running".to_owned())?;
        let actual_application =
            ApplicationRevisionId::new(field(&row, "application_revision_id")?)
                .map_err(|error| error.to_string())?;
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
                let row = sqlx::query(
                    "SELECT claimed_commit, claimed_tree, tr.proposal_artifact_id,
                            tr.ticket_id, tr.id AS ticket_revision_id
                       FROM factory.ticket_attempts ta
                       JOIN factory.ticket_revisions tr ON tr.id = ta.ticket_revision_id
                      WHERE ta.id = $1 AND ta.campaign_id = $2
                        AND tr.application_revision_id = $3 AND ta.stage IN (0, 4)",
                )
                .bind(ticket_attempt_id.get())
                .bind(assignment.campaign_id.get())
                .bind(assignment.application_revision_id.get())
                .fetch_optional(&self.store.pool_for_authority())
                .await
                .map_err(db_error)?
                .ok_or_else(|| "Engineering attempt is not launchable".to_owned())?;
                Ok(DurableAssignmentLaunchContext {
                    application_revision_id: assignment.application_revision_id,
                    target: DurableAssignmentTarget::Engineering { ticket_attempt_id },
                    repository: application.repository,
                    materialize_commit: commit(&row, "claimed_commit")?,
                    materialize_tree: tree(&row, "claimed_tree")?,
                    ticket_id: Some(
                        TicketId::new(field(&row, "ticket_id")?)
                            .map_err(|error| error.to_string())?,
                    ),
                    ticket_revision_id: Some(
                        TicketRevisionId::new(field(&row, "ticket_revision_id")?)
                            .map_err(|error| error.to_string())?,
                    ),
                    validation_id: None,
                    proposal: Some(
                        self.reference(artifact_id(&row, "proposal_artifact_id")?)
                            .await?,
                    ),
                    application_required_reads: application.bundle.required_reads.clone(),
                    ticket_contract_reads: self
                        .ticket_contract_reads(
                            &application.bundle,
                            artifact_id(&row, "proposal_artifact_id")?,
                        )
                        .await?,
                })
            }
            DurableAssignmentTarget::Quality {
                ticket_attempt_id,
                candidate_id,
            } => {
                let row = sqlx::query(
                    "SELECT c.base_commit, c.candidate_tree, tr.proposal_artifact_id,
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
                )
                .bind(candidate_id.get())
                .bind(ticket_attempt_id.get())
                .bind(assignment.campaign_id.get())
                .bind(assignment.application_revision_id.get())
                .fetch_optional(&self.store.pool_for_authority())
                .await
                .map_err(db_error)?
                .ok_or_else(|| "Quality candidate is not launchable".to_owned())?;
                Ok(DurableAssignmentLaunchContext {
                    application_revision_id: assignment.application_revision_id,
                    target: DurableAssignmentTarget::Quality {
                        ticket_attempt_id,
                        candidate_id,
                    },
                    repository: application.repository,
                    materialize_commit: commit(&row, "base_commit")?,
                    materialize_tree: tree(&row, "candidate_tree")?,
                    ticket_id: Some(
                        TicketId::new(field(&row, "ticket_id")?)
                            .map_err(|error| error.to_string())?,
                    ),
                    ticket_revision_id: Some(
                        TicketRevisionId::new(field(&row, "ticket_revision_id")?)
                            .map_err(|error| error.to_string())?,
                    ),
                    validation_id: Some(
                        factory_protocol::ValidationId::new(field(&row, "validation_id")?)
                            .map_err(|error| error.to_string())?,
                    ),
                    proposal: Some(
                        self.reference(artifact_id(&row, "proposal_artifact_id")?)
                            .await?,
                    ),
                    application_required_reads: application.bundle.required_reads.clone(),
                    ticket_contract_reads: self
                        .ticket_contract_reads(
                            &application.bundle,
                            artifact_id(&row, "proposal_artifact_id")?,
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
                proposal: None,
                application_required_reads: application.bundle.required_reads,
                ticket_contract_reads: Vec::new(),
            }),
        }
    }

    async fn resolve_engineering_inner(
        &self,
        session_id: SessionId,
        packet: &AssignmentPacketV1,
    ) -> Result<ResolvedEngineeringCandidateAuthority, String> {
        if packet.office != Office::Engineering {
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
        let proposal = parse_product_ticket_proposal_v1(
            &proposal_bytes,
            &application.bundle.ticket_policy.ticket_bounds,
        )
        .map_err(|error| format!("stored ticket proposal is invalid: {error}"))?;
        self.verify_proposal_artifacts(&proposal).await?;
        let profile_bytes = self.artifact_bytes(ticket.reproducer_artifact_id).await?;
        let stored_profile = parse_command_profile_v1(&profile_bytes)
            .map_err(|error| format!("stored ticket reproducer profile is invalid: {error}"))?;
        let source_profile_bytes = self
            .artifact_bytes(proposal.reproducer.command.artifact_id)
            .await?;
        let source_profile = parse_command_profile_v1(&source_profile_bytes)
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
        let commit_identity = GitIdentity::new("Factory Kernel", "factory-kernel@local")
            .map_err(|error| format!("kernel Git identity is invalid: {error}"))?;
        let full_suite = full_suite_commands(&application.bundle)?;
        let timestamp_unix_seconds = self
            .engineering_commit_timestamp(session_id, packet, &assignment)
            .await?;
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
                "ticket-attempt-{}-reproducer",
                ticket_attempt_id.get()
            ),
            regression_worktree_name: worktree_name("engineering-regression", session_suffix)?,
            product_reproducer: reproducer,
            full_suite_identity: FULL_SUITE_IDENTITY.to_owned(),
            full_suite,
            validation_worktree_name: worktree_name("engineering-validation", session_suffix)?,
            commit: CandidateCommitPolicy {
                author: commit_identity.clone(),
                committer: commit_identity,
                timestamp_unix_seconds,
                // The transcript does not exist until the one actor session
                // terminates. The immutable packet seal is the only session
                // evidence available before terminal submission; the durable
                // session row still binds the actual actor session exactly.
                engineering_session_digest: packet.packet_digest,
            },
        })
    }

    async fn resolve_quality_inner(
        &self,
        session_id: SessionId,
        packet: &AssignmentPacketV1,
    ) -> Result<ResolvedQualityCandidateAuthority, String> {
        if packet.office != Office::Quality {
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
        let row = sqlx::query(
            "SELECT ar.bundle_artifact_id, r.canonical_local_path, r.default_branch
               FROM factory.application_revisions ar
               JOIN factory.repositories r ON r.id = ar.repository_id
              WHERE ar.id = $1",
        )
        .bind(application_revision_id.get())
        .fetch_optional(&self.store.pool_for_authority())
        .await
        .map_err(db_error)?
        .ok_or_else(|| {
            format!(
                "application revision {} is absent",
                application_revision_id.get()
            )
        })?;
        let bundle_artifact_id = artifact_id(&row, "bundle_artifact_id")?;
        let bundle_bytes = self.artifact_bytes(bundle_artifact_id).await?;
        let bundle = parse_application_bundle_v1(&bundle_bytes)
            .map_err(|error| format!("admitted application bundle is invalid: {error}"))?;
        let repository_path: String = field(&row, "canonical_local_path")?;
        let default_branch: String = field(&row, "default_branch")?;
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
        packet: &AssignmentPacketV1,
    ) -> Result<AssignmentContext, String> {
        let row = sqlx::query(
            "SELECT campaign_id, application_revision_id, office, ticket_attempt_id, candidate_id
               FROM factory.assignments
              WHERE id = $1",
        )
        .bind(packet.assignment_id.get())
        .fetch_optional(&self.store.pool_for_authority())
        .await
        .map_err(db_error)?
        .ok_or_else(|| "assignment is absent from durable authority".to_owned())?;
        let campaign_id: i64 = field(&row, "campaign_id")?;
        let application_revision_id =
            ApplicationRevisionId::new(field(&row, "application_revision_id")?)
                .map_err(|error| error.to_string())?;
        let office: i16 = field(&row, "office")?;
        let ticket_attempt_id = optional_positive(&row, "ticket_attempt_id", TicketAttemptId::new)?;
        let candidate_id = optional_positive(&row, "candidate_id", CandidateId::new)?;
        let expected_office = match packet.office {
            Office::ProductResearch => 0,
            Office::Engineering => 1,
            Office::Quality => 2,
        };
        if campaign_id != packet.campaign_id.get()
            || application_revision_id != packet.application_revision_id
            || office != expected_office
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

    async fn exact_bytes(
        &self,
        reference: &SealedArtifactReferenceV1,
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
        proposal: &factory_protocol::ProductTicketProposalV1,
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
        bundle: &ApplicationBundleV1,
        proposal_artifact_id: ArtifactId,
    ) -> Result<Vec<TicketContractReadV1>, String> {
        let proposal_bytes = self.artifact_bytes(proposal_artifact_id).await?;
        let proposal =
            parse_product_ticket_proposal_v1(&proposal_bytes, &bundle.ticket_policy.ticket_bounds)
                .map_err(|error| format!("stored ticket proposal is invalid: {error}"))?;
        self.verify_proposal_artifacts(&proposal).await?;
        Ok(proposal.contract_reads)
    }

    async fn command_from_reproducer(
        &self,
        stored_profile: &factory_protocol::CommandProfileV1,
        proposal: &factory_protocol::ProductTicketProposalV1,
    ) -> Result<DeterministicCommand, String> {
        let mut profile = stored_profile.clone();
        profile.expected_exit_status = proposal.reproducer.expected_observation.exit_status;
        let stdin = match &proposal.reproducer.stdin {
            Some(reference) => CommandStdin::Artifact(self.exact_bytes(reference).await?),
            None => CommandStdin::Empty,
        };
        let stdout = self
            .exact_bytes(&proposal.reproducer.expected_observation.stdout)
            .await?;
        let stderr = self
            .exact_bytes(&proposal.reproducer.expected_observation.stderr)
            .await?;
        DeterministicCommand::new(
            profile,
            stdin,
            CommandExpectation::new(
                ComparisonRevision::parse(EXACT_OBSERVATION_COMPARISON)
                    .map_err(|error| error.to_string())?,
                Some(stdout),
                Some(stderr),
            ),
        )
        .map_err(|error| format!("ticket reproducer command is invalid: {error}"))
    }

    async fn load_engineering_ticket(
        &self,
        ticket_attempt_id: TicketAttemptId,
        assignment: &AssignmentContext,
    ) -> Result<EngineeringTicket, String> {
        let row = sqlx::query(
            "SELECT ta.revision AS attempt_revision, tr.id AS ticket_revision_id,
                    tr.revision AS ticket_revision, tr.proposal_artifact_id,
                    tr.reproducer_artifact_id, tr.expected_observation_artifact_id,
                    tr.discovery_observation_artifact_id, t.id AS ticket_id
               FROM factory.ticket_attempts ta
               JOIN factory.ticket_revisions tr ON tr.id = ta.ticket_revision_id
               JOIN factory.tickets t ON t.id = tr.ticket_id
              WHERE ta.id = $1 AND ta.campaign_id = $2 AND tr.application_revision_id = $3
                AND ta.stage IN (0, 4)",
        )
        .bind(ticket_attempt_id.get())
        .bind(assignment.campaign_id.get())
        .bind(assignment.application_revision_id.get())
        .fetch_optional(&self.store.pool_for_authority())
        .await
        .map_err(db_error)?
        .ok_or_else(|| "Engineering assignment target is not an active attempt".to_owned())?;
        Ok(EngineeringTicket {
            ticket_id: TicketId::new(field(&row, "ticket_id")?)
                .map_err(|error| error.to_string())?,
            ticket_revision_id: TicketRevisionId::new(field(&row, "ticket_revision_id")?)
                .map_err(|error| error.to_string())?,
            ticket_revision: revision(&row, "ticket_revision")?,
            attempt_revision: revision(&row, "attempt_revision")?,
            proposal_artifact_id: artifact_id(&row, "proposal_artifact_id")?,
            reproducer_artifact_id: artifact_id(&row, "reproducer_artifact_id")?,
            expected_observation_artifact_id: artifact_id(
                &row,
                "expected_observation_artifact_id",
            )?,
            discovery_observation_artifact_id: artifact_id(
                &row,
                "discovery_observation_artifact_id",
            )?,
        })
    }

    async fn load_candidate_recovery(
        &self,
        action: DownstreamActionContext,
        required_candidate_lifecycle: i16,
        required_attempt_stage: i16,
    ) -> Result<CandidateRecovery, String> {
        let row = sqlx::query(
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
                    camp.id AS campaign_id, kb.build_digest, a.packet_digest,
                    hv.id AS hard_validation_id
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
                AND camp.lifecycle = 0 AND es.campaign_id = camp.id
                AND es.office = 1 AND a.office = 1
                AND a.campaign_id = camp.id
                AND a.application_revision_id = tr.application_revision_id",
        )
        .bind(action.candidate_id.get())
        .bind(action.ticket_attempt_id.get())
        .bind(required_candidate_lifecycle)
        .bind(required_attempt_stage)
        .fetch_optional(&self.store.pool_for_authority())
        .await
        .map_err(db_error)?
        .ok_or_else(|| {
            "candidate recovery action is no longer at its exact durable stage".to_owned()
        })?;
        let candidate_revision = revision(&row, "candidate_revision")?;
        let attempt_revision = revision(&row, "attempt_revision")?;
        if candidate_revision != action.candidate_revision
            || attempt_revision != action.ticket_attempt_revision
        {
            return Err("candidate recovery action has stale aggregate revisions".to_owned());
        }
        let application_revision_id =
            ApplicationRevisionId::new(field(&row, "application_revision_id")?)
                .map_err(|error| error.to_string())?;
        let application = self.load_application(application_revision_id).await?;
        let proposal_artifact_id = artifact_id(&row, "proposal_artifact_id")?;
        let proposal_bytes = self.artifact_bytes(proposal_artifact_id).await?;
        let proposal = parse_product_ticket_proposal_v1(
            &proposal_bytes,
            &application.bundle.ticket_policy.ticket_bounds,
        )
        .map_err(|error| format!("stored ticket proposal is invalid: {error}"))?;
        self.verify_proposal_artifacts(&proposal).await?;
        let stored_profile = parse_command_profile_v1(
            &self
                .artifact_bytes(artifact_id(&row, "reproducer_artifact_id")?)
                .await?,
        )
        .map_err(|error| format!("stored ticket reproducer profile is invalid: {error}"))?;
        let source_profile = parse_command_profile_v1(
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
            .artifact_bytes(artifact_id(&row, "expected_observation_artifact_id")?)
            .await?;
        let _ = self
            .artifact_bytes(artifact_id(&row, "discovery_observation_artifact_id")?)
            .await?;
        let _ = self
            .reference(artifact_id(&row, "changed_paths_artifact_id")?)
            .await?;
        let _ = self
            .reference(artifact_id(&row, "regression_patch_artifact_id")?)
            .await?;
        let _ = self
            .reference(artifact_id(&row, "regression_command_set_artifact_id")?)
            .await?;
        let _ = self
            .reference(artifact_id(&row, "regression_log_artifact_id")?)
            .await?;
        let candidate_patch = self
            .reference(artifact_id(&row, "patch_artifact_id")?)
            .await?;
        let engineering_report = self
            .reference(artifact_id(&row, "engineering_report_artifact_id")?)
            .await?;
        let risks = self
            .reference(artifact_id(&row, "risks_artifact_id")?)
            .await?;
        let ticket_id =
            TicketId::new(field(&row, "ticket_id")?).map_err(|error| error.to_string())?;
        let ticket_revision_id = TicketRevisionId::new(field(&row, "ticket_revision_id")?)
            .map_err(|error| error.to_string())?;
        let kernel_build_id = kernel_build_id(&row, "build_digest")?;
        let packet_digest = digest(&row, "packet_digest")?;
        let commit_identity = GitIdentity::new("Factory Kernel", "factory-kernel@local")
            .map_err(|error| format!("kernel Git identity is invalid: {error}"))?;
        let timestamp_unix_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "system clock predates the Unix epoch".to_owned())?
            .as_secs()
            .try_into()
            .map_err(|_| "current commit timestamp is out of range".to_owned())?;
        Ok(CandidateRecovery {
            application: application.bundle,
            repository: application.repository,
            ticket: CandidateTicketBinding {
                ticket_id,
                ticket_attempt_id: action.ticket_attempt_id,
                ticket_revision_id,
                expected_attempt_revision: ExpectedRevision::new(attempt_revision),
                expected_ticket_revision: ExpectedRevision::new(revision(&row, "ticket_revision")?),
                ticket_revision_digest: ContentDigest::of_bytes(&proposal_bytes),
            },
            engineering_session_id: SessionId::new(field(&row, "engineering_session_id")?)
                .map_err(|error| error.to_string())?,
            kernel_build_id,
            campaign_id: factory_protocol::CampaignId::new(field(&row, "campaign_id")?)
                .map_err(|error| error.to_string())?,
            application_revision_id,
            candidate_tree: tree(&row, "candidate_tree")?,
            regression_tree: tree(&row, "regression_tree")?,
            candidate_patch,
            submission: factory_protocol::CandidateSubmissionV1 {
                engineering_report,
                commit_subject: field(&row, "commit_subject")?,
                commit_body: field(&row, "commit_body")?,
                regression_test_identity: field(&row, "regression_test_identity")?,
                risks,
            },
            product_reproducer: self
                .command_from_reproducer(&stored_profile, &proposal)
                .await?,
            full_suite,
            hard_validation_id: optional_positive(
                &row,
                "hard_validation_id",
                factory_protocol::ValidationId::new,
            )?,
            commit: CandidateCommitPolicy {
                author: commit_identity.clone(),
                committer: commit_identity,
                timestamp_unix_seconds,
                engineering_session_digest: packet_digest,
            },
        })
    }

    async fn load_quality_candidate(
        &self,
        ticket_attempt_id: TicketAttemptId,
        candidate_id: CandidateId,
        assignment: &AssignmentContext,
    ) -> Result<QualityCandidate, String> {
        let row = sqlx::query(
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
        )
        .bind(candidate_id.get())
        .bind(ticket_attempt_id.get())
        .bind(assignment.campaign_id.get())
        .bind(assignment.application_revision_id.get())
        .fetch_optional(&self.store.pool_for_authority())
        .await
        .map_err(db_error)?
        .ok_or_else(|| {
            "Quality assignment target is not an exact validated candidate".to_owned()
        })?;
        let candidate_commit: String = field(&row, "candidate_commit")?;
        let packet = CandidatePacketV1 {
            candidate_id,
            ticket_attempt_id,
            ticket_revision_id: TicketRevisionId::new(field(&row, "ticket_revision_id")?)
                .map_err(|error| error.to_string())?,
            base_commit: object(&row, "base_commit")?,
            base_tree: object(&row, "base_tree")?,
            regression_tree: object(&row, "regression_tree")?,
            candidate_tree: object(&row, "candidate_tree")?,
            regression_patch: self
                .reference(artifact_id(&row, "regression_patch_artifact_id")?)
                .await?,
            regression_command_set: self
                .reference(artifact_id(&row, "regression_command_set_artifact_id")?)
                .await?,
            regression_log: self
                .reference(artifact_id(&row, "regression_log_artifact_id")?)
                .await?,
            candidate_patch: self
                .reference(artifact_id(&row, "patch_artifact_id")?)
                .await?,
            engineering_session_id: SessionId::new(field(&row, "engineering_session_id")?)
                .map_err(|error| error.to_string())?,
            engineering_report: self
                .reference(artifact_id(&row, "engineering_report_artifact_id")?)
                .await?,
            hard_validation_id: factory_protocol::ValidationId::new(field(
                &row,
                "hard_validation_id",
            )?)
            .map_err(|error| error.to_string())?,
            candidate_commit: RepositoryObjectIdV1::parse(candidate_commit)
                .map_err(|error| error.to_string())?,
            candidate_revision: revision(&row, "candidate_revision")?,
        };
        packet.validate().map_err(|error| error.to_string())?;
        let prior_full_suite = match optional_positive(
            &row,
            "quality_validation_id",
            factory_protocol::ValidationId::new,
        )? {
            Some(validation_id) => {
                let validation_tree = object(&row, "quality_validation_tree")?;
                if validation_tree != packet.candidate_tree {
                    return Err(
                        "persisted Quality validation tree differs from candidate tree".to_owned(),
                    );
                }
                let audit_log_id: i64 = field(&row, "quality_validation_audit_log_id")?;
                Some(QualityFullSuiteOutcome {
                    receipt: factory_protocol::QualityValidationReceiptV1 {
                        validation_id,
                        candidate_id,
                        candidate_tree: packet.candidate_tree.clone(),
                        log_artifact: self
                            .reference(artifact_id(&row, "quality_validation_log_artifact_id")?)
                            .await?,
                        revision: packet.candidate_revision,
                    },
                    result: crate::decision_store::ValidationResult::Passed,
                    resulting_attempt_revision: revision(&row, "attempt_revision")?,
                    audit_log_id,
                })
            }
            None => None,
        };
        Ok(QualityCandidate {
            packet,
            attempt_revision: revision(&row, "attempt_revision")?,
            prior_full_suite,
        })
    }

    async fn reference(
        &self,
        artifact_id: ArtifactId,
    ) -> Result<SealedArtifactReferenceV1, String> {
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
        Ok(SealedArtifactReferenceV1 {
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
        packet: &'a AssignmentPacketV1,
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
        packet: &'a AssignmentPacketV1,
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
            let row = sqlx::query(
                "SELECT ta.revision AS attempt_revision, tr.revision AS ticket_revision,
                        tr.application_revision_id, tr.proposal_artifact_id,
                        tr.reproducer_artifact_id, c.kernel_build_id,
                        kb.build_digest
                   FROM factory.ticket_attempts ta
                   JOIN factory.ticket_revisions tr ON tr.id = ta.ticket_revision_id
                   JOIN factory.campaigns c ON c.id = ta.campaign_id
                   JOIN factory.kernel_builds kb ON kb.id = c.kernel_build_id
                  WHERE ta.id = $1 AND ta.stage IN (8, 9) AND ta.released_at IS NULL",
            )
            .bind(ticket_attempt_id.get())
            .fetch_optional(&self.store.pool_for_authority())
            .await
            .map_err(|error| ArchitectTransitionResolutionError::Precondition {
                message: db_error(error),
            })?
            .ok_or_else(|| ArchitectTransitionResolutionError::Precondition {
                message: "ticket attempt is not a failed or cancelled unreleased attempt"
                    .to_owned(),
            })?;
            let attempt_revision = revision(&row, "attempt_revision").map_err(precondition)?;
            if caller_expected_attempt_revision.get() != attempt_revision {
                return Err(ArchitectTransitionResolutionError::RevisionConflict {
                    expected: caller_expected_attempt_revision.get().get(),
                    current: attempt_revision.get(),
                });
            }
            let application_revision_id = ApplicationRevisionId::new(
                field(&row, "application_revision_id").map_err(precondition)?,
            )
            .map_err(|error| precondition(error.to_string()))?;
            let application = self
                .load_application(application_revision_id)
                .await
                .map_err(precondition)?;
            let proposal_bytes = self
                .artifact_bytes(artifact_id(&row, "proposal_artifact_id").map_err(precondition)?)
                .await
                .map_err(precondition)?;
            let proposal = parse_product_ticket_proposal_v1(
                &proposal_bytes,
                &application.bundle.ticket_policy.ticket_bounds,
            )
            .map_err(|error| precondition(format!("stored ticket proposal is invalid: {error}")))?;
            self.verify_proposal_artifacts(&proposal)
                .await
                .map_err(precondition)?;
            let profile_bytes = self
                .artifact_bytes(artifact_id(&row, "reproducer_artifact_id").map_err(precondition)?)
                .await
                .map_err(precondition)?;
            let stored_profile = parse_command_profile_v1(&profile_bytes).map_err(|error| {
                precondition(format!(
                    "stored ticket reproducer profile is invalid: {error}"
                ))
            })?;
            let source_profile_bytes = self
                .artifact_bytes(proposal.reproducer.command.artifact_id)
                .await
                .map_err(precondition)?;
            let source_profile =
                parse_command_profile_v1(&source_profile_bytes).map_err(|error| {
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
            let kernel_build_id = kernel_build_id(&row, "build_digest").map_err(precondition)?;
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
            Ok(ResolvedReleaseTransition {
                expected_attempt_revision: ExpectedRevision::new(attempt_revision),
                expected_ticket_revision: ExpectedRevision::new(
                    revision(&row, "ticket_revision").map_err(precondition)?,
                ),
                requalification: crate::ticket_store::CurrentHeadRequalification {
                    current_head_commit: application
                        .repository
                        .snapshot()
                        .base_commit()
                        .to_string(),
                    current_head_tree: application.repository.snapshot().base_tree().to_string(),
                    first_actual_observation_artifact_id: first,
                    second_actual_observation_artifact_id: second,
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
            let row = sqlx::query(
                "SELECT c.revision AS candidate_revision, ta.revision AS attempt_revision,
                        tr.revision AS ticket_revision
                   FROM factory.candidates c
                   JOIN factory.ticket_attempts ta ON ta.id = c.ticket_attempt_id
                   JOIN factory.ticket_revisions tr ON tr.id = ta.ticket_revision_id
                   JOIN factory.reviews r ON r.candidate_id = c.id
                  WHERE c.id = $1 AND r.id = $2",
            )
            .bind(candidate_id.get())
            .bind(review_id.get())
            .fetch_optional(&self.store.pool_for_authority())
            .await
            .map_err(|error| ArchitectTransitionResolutionError::Precondition {
                message: db_error(error),
            })?
            .ok_or_else(|| ArchitectTransitionResolutionError::Precondition {
                message: "candidate decision does not name its exact persisted review".to_owned(),
            })?;
            let candidate_revision = revision(&row, "candidate_revision").map_err(precondition)?;
            if caller_expected_candidate_revision.get() != candidate_revision {
                return Err(ArchitectTransitionResolutionError::RevisionConflict {
                    expected: caller_expected_candidate_revision.get().get(),
                    current: candidate_revision.get(),
                });
            }
            Ok(ResolvedCandidateDecisionTransition {
                expected_candidate_revision: ExpectedRevision::new(candidate_revision),
                expected_attempt_revision: ExpectedRevision::new(
                    revision(&row, "attempt_revision").map_err(precondition)?,
                ),
                expected_ticket_revision: ExpectedRevision::new(
                    revision(&row, "ticket_revision").map_err(precondition)?,
                ),
            })
        })
    }
}

#[derive(Clone)]
struct ApplicationContext {
    bundle: ApplicationBundleV1,
    repository: QualifiedRepository,
}

/// Exact daemon-owned materialization input for one non-Product assignment.
/// `materialize_commit` remains the detached worktree `HEAD`; the selected
/// tree is written with Git's temporary-index custody rather than checked out
/// as an actor-controlled branch.
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
    /// Exact sealed Product contract upstream of an Engineering/Quality
    /// assignment. Product has no preceding ticket proposal.
    pub proposal: Option<SealedArtifactReferenceV1>,
    /// Application-required paths, exactly as admitted with the application.
    pub application_required_reads: Vec<RequiredReadV1>,
    /// Ticket-specific contract reads, parsed from the sealed admitted
    /// proposal. Product has none because it has no upstream ticket target.
    pub ticket_contract_reads: Vec<TicketContractReadV1>,
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
    method: &'static str,
}

fn local_delivery_receipt_bytes(
    candidate_id: CandidateId,
    expected_old_commit: &GitCommitId,
    resulting_commit: &GitCommitId,
    resulting_tree: &GitTreeId,
) -> Vec<u8> {
    json::to_string(&LocalDeliveryReceiptBytes {
        candidate_id: candidate_id.get(),
        expected_old_commit: expected_old_commit.as_str(),
        resulting_commit: resulting_commit.as_str(),
        resulting_tree: resulting_tree.as_str(),
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
    packet: CandidatePacketV1,
    attempt_revision: AggregateRevision,
    prior_full_suite: Option<QualityFullSuiteOutcome>,
}

/// Private decoded durable state shared by the two candidate recovery
/// operations. It is assembled only after every persisted artifact is
/// re-verified from CAS and all scheduler revisions still match.
struct CandidateRecovery {
    application: ApplicationBundleV1,
    repository: QualifiedRepository,
    ticket: CandidateTicketBinding,
    engineering_session_id: SessionId,
    kernel_build_id: KernelBuildId,
    campaign_id: factory_protocol::CampaignId,
    application_revision_id: ApplicationRevisionId,
    candidate_tree: GitTreeId,
    regression_tree: GitTreeId,
    candidate_patch: SealedArtifactReferenceV1,
    submission: factory_protocol::CandidateSubmissionV1,
    product_reproducer: DeterministicCommand,
    full_suite: Vec<DeterministicCommand>,
    hard_validation_id: Option<factory_protocol::ValidationId>,
    commit: CandidateCommitPolicy,
}

#[derive(Serialize)]
struct ObservationManifest<'a> {
    exit_status: i32,
    stdout_digest: &'a str,
    stdout_byte_length: u64,
    stderr_digest: &'a str,
    stderr_byte_length: u64,
}

/// Stores the same closed observation-manifest bytes used at Product
/// admission. DecisionStore compares only that manifest digest, preventing a
/// release resolver from redefining equality for current-head reproduction.
async fn seal_command_observation_manifest(
    process: &crate::process::ProcessStore,
    cas: &CasStore,
    principal: &str,
    command_prefix: &str,
    kernel_build_id: KernelBuildId,
    receipt: &CommandReceipt,
) -> Result<ArtifactId, String> {
    let exit_status = match receipt.terminal() {
        CommandTerminal::Exited { exit_code } => exit_code,
        other => {
            return Err(format!(
                "current-head reproducer did not exit normally: {other:?}"
            ));
        }
    };
    let (stdout, _) = process
        .adopt_and_register_kernel_bytes(
            cas,
            principal,
            &format!("{command_prefix}-stdout"),
            kernel_build_id,
            receipt.stdout(),
        )
        .await
        .map_err(|error| format!("could not seal current-head stdout: {error}"))?;
    let (stderr, _) = process
        .adopt_and_register_kernel_bytes(
            cas,
            principal,
            &format!("{command_prefix}-stderr"),
            kernel_build_id,
            receipt.stderr(),
        )
        .await
        .map_err(|error| format!("could not seal current-head stderr: {error}"))?;
    let stdout_digest = stdout.digest().to_hex();
    let stderr_digest = stderr.digest().to_hex();
    let bytes = json::to_string(&ObservationManifest {
        exit_status,
        stdout_digest: &stdout_digest,
        stdout_byte_length: stdout.byte_length(),
        stderr_digest: &stderr_digest,
        stderr_byte_length: stderr.byte_length(),
    })
    .into_bytes();
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

fn full_suite_commands(bundle: &ApplicationBundleV1) -> Result<Vec<DeterministicCommand>, String> {
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

fn field<T>(row: &PgRow, name: &str) -> Result<T, String>
where
    for<'r> T: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
    for<'r> &'r str: sqlx::ColumnIndex<PgRow>,
{
    row.try_get(name)
        .map_err(|error| format!("durable field {name} is corrupt: {error}"))
}

fn optional_positive<T>(
    row: &PgRow,
    name: &str,
    parse: impl FnOnce(i64) -> Result<T, factory_protocol::ContractError>,
) -> Result<Option<T>, String> {
    let value: Option<i64> = field(row, name)?;
    value
        .map(parse)
        .transpose()
        .map_err(|error| format!("durable field {name} is invalid: {error}"))
}

fn artifact_id(row: &PgRow, name: &str) -> Result<ArtifactId, String> {
    ArtifactId::new(field(row, name)?).map_err(|error| error.to_string())
}

fn revision(row: &PgRow, name: &str) -> Result<AggregateRevision, String> {
    let value: i64 = field(row, name)?;
    let value = u64::try_from(value).map_err(|_| format!("durable revision {name} is negative"))?;
    Ok(AggregateRevision::from_persisted(value))
}

fn object(row: &PgRow, name: &str) -> Result<RepositoryObjectIdV1, String> {
    RepositoryObjectIdV1::parse(field::<String>(row, name)?).map_err(|error| error.to_string())
}

fn commit(row: &PgRow, name: &str) -> Result<GitCommitId, String> {
    GitCommitId::parse(field::<String>(row, name)?).map_err(|error| error.to_string())
}

fn tree(row: &PgRow, name: &str) -> Result<GitTreeId, String> {
    GitTreeId::parse(field::<String>(row, name)?).map_err(|error| error.to_string())
}

fn kernel_build_id(row: &PgRow, name: &str) -> Result<KernelBuildId, String> {
    let bytes: Vec<u8> = field(row, name)?;
    let bytes: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| format!("durable field {name} is not a BLAKE3 digest"))?;
    Ok(KernelBuildId::new(ContentDigest::from_bytes(bytes)))
}

fn digest(row: &PgRow, name: &str) -> Result<ContentDigest, String> {
    let bytes: Vec<u8> = field(row, name)?;
    let bytes: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| format!("durable field {name} is not a BLAKE3 digest"))?;
    Ok(ContentDigest::from_bytes(bytes))
}

fn db_error(error: sqlx::Error) -> String {
    format!("durable authority read failed: {error}")
}

fn precondition(message: String) -> ArchitectTransitionResolutionError {
    ArchitectTransitionResolutionError::Precondition { message }
}
