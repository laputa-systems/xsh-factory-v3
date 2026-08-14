//! Candidate and Quality execution at the kernel boundary.
//!
//! An actor supplies only a bounded request and references to artifacts it
//! sealed through its assigned connection.  This module proves those
//! references belong to that actor, obtains every Git identity from custody,
//! runs application-declared commands in fresh worktrees, seals kernel
//! evidence, and finally calls the narrow typed decision store.
//!
//! No method here offers a remote Git operation or accepts a tree, patch,
//! commit, validation result, or artifact identity as an actor assertion.

use std::path::Path;

use factory_protocol::{
    AggregateRevision, ApplicationBundleV2, AssignmentPacketV2, AssignmentRole,
    CandidateCheckpointRegressionRequest, CandidateId, CandidatePacketV2, CandidateSubmissionV2,
    CandidateSubmitRequest, ContentDigest, ExpectedRevision, QualityRunFullSuiteRequest,
    QualitySubmitReviewRequest, QualityValidationReceiptV2, RepositoryObjectIdV2,
    SealedArtifactReferenceV2, SessionId, TerminalOperationV2, TicketAttemptId, TicketId,
    TicketRevisionId, ValidationId,
};
use miniserde::{Serialize, json};
use thiserror::Error;

use crate::{
    cas::{CasArtifact, CasStore},
    command_supervision::{
        CommandReceipt, CommandRunner, CommandSupervisionError, CommandWorkspace,
        DeterministicCommand, GitTrackedTreeProbe, GitTreeIdentity, PristineWorkspace,
        ValidationInvocation, ValidationReceipt as CommandValidationReceipt, ValidationStatus,
    },
    decision_store::{
        AttachCandidateCommit, CandidateReceipt, DecisionStore, DecisionStoreError,
        RecordValidation, ReviewReceipt, SubmitCandidate, SubmitQualityReview, ValidationReceipt,
        ValidationResult, ValidationScope,
    },
    git::{
        CandidateRefName, CandidateWhitespaceCheck, CommitMessage, CommitProvenance,
        ConstructCandidateCommit, GitCustody, GitCustodyError, GitIdentity, GitTreeId,
        OwnedWorktree, QualifiedRepository, TreeCapture, WorktreeKind, WorktreeName,
    },
    process::ProcessStore,
    storage::StoreError,
};

const TREE_COMPARISON_REVISION: &str = "candidate-runtime-tree-v1";
const COMMAND_SET_FORMAT: &str = "factory-validation-command-set-v1";
const VALIDATION_LOG_FORMAT: &str = "factory-validation-log-v1";
const CHECKPOINT_LOG_FORMAT: &str = "factory-regression-checkpoint-log-v1";
const COMMAND_SET_LIMIT: usize = 256 * 1024;
const VALIDATION_LOG_LIMIT: usize = 16 * 1024 * 1024;
const TREE_PROBE_TIMEOUT_MILLIS: u64 = 30_000;
const TREE_PROBE_STREAM_LIMIT: u32 = 64 * 1024;

/// Immutable actor/session facts supplied by the local transport owner, not
/// by the actor request.  The packet remains the authority binding for every
/// operation below.
#[derive(Clone, Copy, Debug)]
pub struct ActorRequestBinding<'a> {
    pub principal: &'a str,
    pub session_id: SessionId,
    pub session_revision: ExpectedRevision,
    pub packet: &'a AssignmentPacketV2,
}

/// Ticket facts that were already claimed and snapshot-bound by the scheduler.
/// They are explicit here because the candidate decision relation needs their
/// independent optimistic revisions and commit provenance inputs.
#[derive(Clone, Copy, Debug)]
pub struct CandidateTicketBinding {
    pub ticket_id: TicketId,
    pub ticket_attempt_id: TicketAttemptId,
    pub ticket_revision_id: TicketRevisionId,
    pub expected_attempt_revision: ExpectedRevision,
    pub expected_ticket_revision: ExpectedRevision,
    pub ticket_revision_digest: ContentDigest,
}

/// Kernel-selected identity and timestamp for a constructed candidate commit.
///
/// This policy is deliberately used only after the Engineering session has
/// reached a successful terminal state.  Its transcript digest is unavailable
/// at `candidate.submit`, so constructing a commit on the actor request would
/// falsely claim packet bytes were session evidence.
#[derive(Clone, Debug)]
pub struct CandidateCommitPolicy {
    pub author: GitIdentity,
    pub committer: GitIdentity,
    pub timestamp_unix_seconds: i64,
    pub engineering_session_digest: ContentDigest,
}

/// Owned result of daemon-side Engineering context resolution.  It is the
/// only form a session dispatcher may retain between the nonterminal
/// checkpoint and the one terminal candidate submission; actor frames never
/// supply or mutate any of these fields.
#[derive(Clone, Debug)]
pub struct ResolvedEngineeringCandidateAuthority {
    pub application: ApplicationBundleV2,
    pub repository: QualifiedRepository,
    pub actor_worktree: OwnedWorktree,
    pub ticket: CandidateTicketBinding,
    pub regression_command: DeterministicCommand,
    pub regression_expected_failure: String,
    pub regression_worktree_name: WorktreeName,
    pub product_reproducer: DeterministicCommand,
    pub full_suite_identity: String,
    pub full_suite: Vec<DeterministicCommand>,
    pub validation_worktree_name: WorktreeName,
}

impl ResolvedEngineeringCandidateAuthority {
    #[must_use]
    pub fn authority<'a>(
        &'a self,
        actor: ActorRequestBinding<'a>,
    ) -> EngineeringCandidateAuthority<'a> {
        EngineeringCandidateAuthority {
            actor,
            application: &self.application,
            repository: &self.repository,
            actor_worktree: &self.actor_worktree,
            ticket: self.ticket,
            regression: RegressionCheckpointProgram {
                command: &self.regression_command,
                expected_failure: &self.regression_expected_failure,
                worktree_name: self.regression_worktree_name.clone(),
            },
            validation: CandidateValidationProgram {
                product_reproducer: &self.product_reproducer,
                full_suite_identity: &self.full_suite_identity,
                full_suite: &self.full_suite,
                worktree_name: self.validation_worktree_name.clone(),
            },
        }
    }
}

/// Owned daemon-side Quality context.  The Quality candidate packet and
/// expected attempt revision are resolved from kernel state before any actor
/// full-suite request is admitted.
#[derive(Clone, Debug)]
pub struct ResolvedQualityCandidateAuthority {
    pub application: ApplicationBundleV2,
    pub repository: QualifiedRepository,
    pub candidate: CandidatePacketV2,
    pub expected_attempt_revision: ExpectedRevision,
    pub full_suite_identity: String,
    pub full_suite: Vec<DeterministicCommand>,
    pub validation_worktree_name: WorktreeName,
    /// A passed Quality validation persisted by a prior interrupted Quality
    /// session.  Its exact sealed log is reusable only for the one missing
    /// review; a continuation must not rerun or replace it.
    pub prior_full_suite: Option<QualityFullSuiteOutcome>,
}

impl ResolvedQualityCandidateAuthority {
    #[must_use]
    pub fn authority<'a>(
        &'a self,
        actor: ActorRequestBinding<'a>,
    ) -> QualityCandidateAuthority<'a> {
        QualityCandidateAuthority {
            actor,
            application: &self.application,
            repository: &self.repository,
            candidate: &self.candidate,
            expected_attempt_revision: self.expected_attempt_revision,
            validation: QualityValidationProgram {
                full_suite_identity: &self.full_suite_identity,
                full_suite: &self.full_suite,
                worktree_name: self.validation_worktree_name.clone(),
            },
        }
    }

    #[must_use]
    pub fn review_authority<'a>(
        &'a self,
        actor: ActorRequestBinding<'a>,
        full_suite: &'a QualityFullSuiteOutcome,
    ) -> QualityReviewAuthority<'a> {
        QualityReviewAuthority {
            actor,
            candidate: &self.candidate,
            full_suite: &full_suite.receipt,
            expected_attempt_revision: ExpectedRevision::new(full_suite.resulting_attempt_revision),
        }
    }
}

/// A targeted pre-fix command that is already admitted by the application.
/// `expected_failure` is a bounded ticket-contract identity, not prose that
/// can override the actual command observation.
#[derive(Clone, Debug)]
pub struct RegressionCheckpointProgram<'a> {
    pub command: &'a DeterministicCommand,
    pub expected_failure: &'a str,
    pub worktree_name: WorktreeName,
}

/// Kernel-selected candidate hard-validation program.  The Product
/// reproducer is included before the application full suite so a passing full
/// suite cannot erase the ticket-specific proof.
#[derive(Clone, Debug)]
pub struct CandidateValidationProgram<'a> {
    pub product_reproducer: &'a DeterministicCommand,
    pub full_suite_identity: &'a str,
    pub full_suite: &'a [DeterministicCommand],
    pub worktree_name: WorktreeName,
}

/// Kernel-selected independent Quality full-suite program.
#[derive(Clone, Debug)]
pub struct QualityValidationProgram<'a> {
    pub full_suite_identity: &'a str,
    pub full_suite: &'a [DeterministicCommand],
    pub worktree_name: WorktreeName,
}

/// All trusted inputs needed to accept an Engineering checkpoint or terminal
/// candidate submission.  The actor request itself is deliberately absent.
#[derive(Clone, Debug)]
pub struct EngineeringCandidateAuthority<'a> {
    pub actor: ActorRequestBinding<'a>,
    pub application: &'a ApplicationBundleV2,
    pub repository: &'a QualifiedRepository,
    pub actor_worktree: &'a OwnedWorktree,
    pub ticket: CandidateTicketBinding,
    pub regression: RegressionCheckpointProgram<'a>,
    pub validation: CandidateValidationProgram<'a>,
}

/// Trusted inputs for an independent Quality full-suite invocation.  Quality
/// never executes the suite in its actor worktree; it is rematerialized from
/// `candidate.candidate_tree` below.
#[derive(Clone, Debug)]
pub struct QualityCandidateAuthority<'a> {
    pub actor: ActorRequestBinding<'a>,
    pub application: &'a ApplicationBundleV2,
    pub repository: &'a QualifiedRepository,
    pub candidate: &'a CandidatePacketV2,
    pub expected_attempt_revision: ExpectedRevision,
    pub validation: QualityValidationProgram<'a>,
}

/// Trusted inputs for Quality's terminal prose submission after it receives a
/// kernel-created full-suite receipt.
#[derive(Clone, Debug)]
pub struct QualityReviewAuthority<'a> {
    pub actor: ActorRequestBinding<'a>,
    pub candidate: &'a CandidatePacketV2,
    pub full_suite: &'a QualityValidationReceiptV2,
    pub expected_attempt_revision: ExpectedRevision,
}

/// Opaque capability emitted only after the kernel captured a regression tree
/// and saw the targeted command fail on a fresh worktree.  It is intentionally
/// not serializable and cannot be assembled from actor values.
#[derive(Clone, Debug)]
pub struct RegressionCheckpoint {
    ticket_attempt_id: TicketAttemptId,
    engineering_session_id: SessionId,
    base_tree: GitTreeId,
    regression: TreeCapture,
    regression_identity: String,
    expected_failure: String,
    regression_patch: SealedArtifactReferenceV2,
    command_set: SealedArtifactReferenceV2,
    log: SealedArtifactReferenceV2,
}

impl RegressionCheckpoint {
    #[must_use]
    pub fn regression_tree(&self) -> &GitTreeId {
        self.regression.tree()
    }

    #[must_use]
    pub fn regression_patch(&self) -> &SealedArtifactReferenceV2 {
        &self.regression_patch
    }

    #[must_use]
    pub fn command_set(&self) -> &SealedArtifactReferenceV2 {
        &self.command_set
    }

    #[must_use]
    pub fn log(&self) -> &SealedArtifactReferenceV2 {
        &self.log
    }
}

/// The terminal Engineering result.  Hard-validation failure is an observed
/// terminal outcome, not an infrastructure exception; no candidate commit is
/// constructed in that branch.
#[derive(Clone, Debug)]
pub enum CandidateSubmissionOutcome {
    Rejected {
        candidate: CandidateReceipt,
        hard_validation: ValidationReceipt,
    },
    Validated {
        candidate: CandidateReceipt,
        hard_validation: ValidationReceipt,
        candidate_tree: RepositoryObjectIdV2,
    },
}

/// A Quality full-suite result.  A failed/interrupted result still has a
/// sealed receipt, but it cannot be used for review submission.
#[derive(Clone, Debug)]
pub struct QualityFullSuiteOutcome {
    pub receipt: QualityValidationReceiptV2,
    pub result: ValidationResult,
    pub resulting_attempt_revision: AggregateRevision,
    /// Durable validation transition which produced this exact receipt.
    pub audit_log_id: i64,
}

/// Durable-only authority for resuming a crash-stranded hard validation. It
/// has no actor request or workspace: every field is loaded from the exact
/// submitted candidate, its Engineering session, and the admitted ticket
/// revision by the daemon resolver.
#[derive(Clone, Debug)]
pub struct ResumedHardValidationAuthority {
    pub principal: String,
    pub command_id: String,
    pub application: ApplicationBundleV2,
    pub repository: QualifiedRepository,
    pub ticket: CandidateTicketBinding,
    pub candidate_id: CandidateId,
    pub expected_candidate_revision: ExpectedRevision,
    pub engineering_session_id: SessionId,
    pub kernel_build_id: factory_protocol::KernelBuildId,
    pub campaign_id: factory_protocol::CampaignId,
    pub application_revision_id: factory_protocol::ApplicationRevisionId,
    pub candidate_tree: GitTreeId,
    pub regression_tree: GitTreeId,
    pub candidate_patch_digest: ContentDigest,
    pub submission: CandidateSubmissionV2,
    pub product_reproducer: DeterministicCommand,
    pub full_suite_identity: String,
    pub full_suite: Vec<DeterministicCommand>,
    pub validation_worktree_name: WorktreeName,
}

/// Result of a resumed hard-validation pass. A passing result deliberately
/// leaves the candidate without a Git commit until the Engineering session's
/// terminal transcript has been sealed and verified.
#[derive(Clone, Debug)]
pub struct ResumedHardValidationOutcome {
    pub validation: ValidationReceipt,
}

/// Durable-only authority for the second half of a passed hard validation.
/// `record_validation(Passed)` intentionally commits before Git ref custody;
/// a daemon interruption in that small interval can therefore recover the
/// exact commit from the persisted candidate and hard-validation rows without
/// re-running the suite or accepting a new actor result.
#[derive(Clone, Debug)]
pub struct ResumeCandidateCommitAttachAuthority {
    pub principal: String,
    pub command_id: String,
    pub application: ApplicationBundleV2,
    pub repository: QualifiedRepository,
    pub ticket: CandidateTicketBinding,
    pub candidate_id: CandidateId,
    pub expected_candidate_revision: ExpectedRevision,
    pub hard_validation_id: ValidationId,
    pub kernel_build_id: factory_protocol::KernelBuildId,
    pub campaign_id: factory_protocol::CampaignId,
    pub application_revision_id: factory_protocol::ApplicationRevisionId,
    pub candidate_tree: GitTreeId,
    pub regression_tree: GitTreeId,
    pub candidate_patch_digest: ContentDigest,
    pub submission: CandidateSubmissionV2,
    pub commit: CandidateCommitPolicy,
}

#[derive(Debug, Error)]
pub enum CandidateRuntimeError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Decision(#[from] DecisionStoreError),
    #[error(transparent)]
    Git(#[from] GitCustodyError),
    #[error(transparent)]
    Command(#[from] CommandSupervisionError),
    #[error("the admitted application bundle is invalid: {0}")]
    ApplicationContract(String),
    #[error("the actor request is invalid: {0}")]
    RequestContract(String),
    #[error("the assignment packet is not valid for this authority: {0}")]
    PacketBinding(String),
    #[error("the qualified repository is not the one pinned by the application")]
    RepositoryBindingMismatch,
    #[error("the qualified default branch differs from the admitted application branch")]
    DefaultBranchMismatch,
    #[error("the Engineering actor worktree differs from its packet workspace")]
    EngineeringWorkspaceMismatch,
    #[error("the requested command does not match the admitted application profile")]
    UndeclaredCommand,
    #[error("the requested validation profile differs from the kernel-selected profile")]
    ValidationProfileMismatch,
    #[error("the regression checkpoint request differs from the ticket-bound command contract")]
    RegressionCheckpointMismatch,
    #[error("the regression checkpoint unexpectedly passed")]
    RegressionCheckpointPassed,
    #[error("the regression checkpoint command changed its fresh source tree")]
    RegressionCheckpointChangedTree,
    #[error("the candidate changed forbidden repository path {path:?}")]
    ForbiddenChangedPath { path: String },
    #[error("the checkpoint capability belongs to a different Engineering assignment")]
    CheckpointBindingMismatch,
    #[error("the candidate capture has a different base tree from its regression checkpoint")]
    CheckpointBaseMismatch,
    #[error("the candidate submission's regression identity differs from its checkpoint")]
    RegressionIdentityMismatch,
    #[error("an actor artifact reference differs from its registered sealed identity")]
    ArtifactIdentityMismatch,
    #[error("kernel evidence {kind} is {observed} bytes, exceeding its {maximum}-byte bound")]
    EvidenceTooLarge {
        kind: &'static str,
        maximum: usize,
        observed: usize,
    },
    #[error("the candidate commit message exceeds the admitted application policy")]
    CommitMessagePolicyExceeded,
    #[error("a Quality receipt is not bound to the supplied candidate packet")]
    QualityReceiptMismatch,
    #[error("candidate validation worktree cleanup failed after command execution: {0}")]
    ValidationCleanup(#[source] GitCustodyError),
}

/// Captures one pre-fix regression tree and verifies the bound targeted
/// command fails on a fresh, exact materialization.  This is repeatable and
/// nonterminal; durable candidate state begins only at [`submit_candidate`].
pub async fn checkpoint_regression(
    process: &ProcessStore,
    cas: &CasStore,
    runner: &CommandRunner,
    git: &GitCustody,
    authority: &EngineeringCandidateAuthority<'_>,
    request: &CandidateCheckpointRegressionRequest,
) -> Result<RegressionCheckpoint, CandidateRuntimeError> {
    validate_engineering_authority(authority, None)?;
    validate_request_revision(&authority.actor, request.expected_revision)?;
    validate_regression_request(authority, request)?;

    let regression = git.capture_regression_tree(authority.actor_worktree)?;
    reject_forbidden_paths(authority.application, &regression)?;
    let tree = regression.tree().clone();
    let receipt = run_validation_in_fresh_worktree(
        git,
        authority.repository,
        tree,
        authority.regression.worktree_name.clone(),
        runner,
        std::slice::from_ref(authority.regression.command),
        ValidationInvocation::Candidate,
    )?;
    match receipt.status() {
        ValidationStatus::Passed => return Err(CandidateRuntimeError::RegressionCheckpointPassed),
        ValidationStatus::TreeChanged => {
            return Err(CandidateRuntimeError::RegressionCheckpointChangedTree);
        }
        ValidationStatus::CommandFailed => {}
    }

    let regression_patch = seal_bytes(
        process,
        cas,
        authority.actor.principal,
        &derived_command_id(&request.client_command_id, "checkpoint-patch")?,
        authority.actor.packet.kernel_build_id,
        regression.binary_patch(),
        "regression binary patch",
        VALIDATION_LOG_LIMIT,
    )
    .await?;
    let evidence = seal_validation_evidence(
        process,
        cas,
        authority.actor.principal,
        derived_command_id(&request.client_command_id, "checkpoint-command-set")?.as_str(),
        authority.actor.packet.kernel_build_id,
        CHECKPOINT_LOG_FORMAT,
        authority.regression.command.profile().name.as_str(),
        std::slice::from_ref(authority.regression.command),
        &receipt,
        None,
    )
    .await?;
    Ok(RegressionCheckpoint {
        ticket_attempt_id: authority.ticket.ticket_attempt_id,
        engineering_session_id: authority.actor.session_id,
        base_tree: regression.base_tree().clone(),
        regression,
        regression_identity: request.regression_command.clone(),
        expected_failure: request.expected_failure.clone(),
        regression_patch,
        command_set: evidence.command_set,
        log: evidence.log,
    })
}

/// Captures the actor tree, verifies sealed actor artifacts, records hard
/// validation, then constructs and attaches a local candidate commit only on
/// a passed exact tree.  No remote Git operation is reachable from this path.
pub async fn submit_candidate(
    process: &ProcessStore,
    decisions: &DecisionStore,
    cas: &CasStore,
    runner: &CommandRunner,
    git: &GitCustody,
    authority: &EngineeringCandidateAuthority<'_>,
    checkpoint: &RegressionCheckpoint,
    request: &CandidateSubmitRequest,
) -> Result<CandidateSubmissionOutcome, CandidateRuntimeError> {
    validate_engineering_authority(authority, Some(TerminalOperationV2::CandidateSubmit))?;
    validate_request_revision(&authority.actor, request.expected_revision)?;
    validate_checkpoint(authority, checkpoint)?;
    let submission = request
        .submission()
        .map_err(|error| CandidateRuntimeError::RequestContract(error.to_string()))?;
    if submission.regression_test_identity != checkpoint.regression_identity {
        return Err(CandidateRuntimeError::RegressionIdentityMismatch);
    }
    validate_commit_message_policy(authority.application, &submission)?;

    let capture = git.capture_tree(authority.actor_worktree)?;
    if capture.base_tree() != &checkpoint.base_tree {
        return Err(CandidateRuntimeError::CheckpointBaseMismatch);
    }
    reject_forbidden_paths(authority.application, &capture)?;
    let changed_paths = seal_changed_paths(
        process,
        cas,
        authority.actor.principal,
        &derived_command_id(&request.client_command_id, "candidate-changed-paths")?,
        authority.actor.packet.kernel_build_id,
        &capture,
    )
    .await?;
    let candidate_patch = seal_bytes(
        process,
        cas,
        authority.actor.principal,
        &derived_command_id(&request.client_command_id, "candidate-patch")?,
        authority.actor.packet.kernel_build_id,
        capture.binary_patch(),
        "candidate binary patch",
        VALIDATION_LOG_LIMIT,
    )
    .await?;

    let candidate_tree = repository_object(capture.tree().as_str())?;
    let base_tree = repository_object(capture.base_tree().as_str())?;
    let base_commit = repository_object(authority.repository.snapshot().base_commit().as_str())?;
    let regression_tree = repository_object(checkpoint.regression.tree().as_str())?;
    let whitespace = git.check_candidate_whitespace(authority.repository, capture.tree())?;

    let mut commands = Vec::with_capacity(authority.validation.full_suite.len() + 1);
    commands.push(authority.validation.product_reproducer.clone());
    commands.extend_from_slice(authority.validation.full_suite);
    let hard_execution = run_validation_in_fresh_worktree(
        git,
        authority.repository,
        capture.tree().clone(),
        authority.validation.worktree_name.clone(),
        runner,
        &commands,
        ValidationInvocation::Candidate,
    );
    let (hard_result, hard_evidence) = match hard_execution {
        Ok(receipt) => {
            let result = if whitespace.is_clean() {
                validation_result(receipt.status())
            } else {
                ValidationResult::Failed
            };
            let evidence = seal_validation_evidence(
                process,
                cas,
                authority.actor.principal,
                derived_command_id(&request.client_command_id, "hard-command-set")?.as_str(),
                authority.actor.packet.kernel_build_id,
                VALIDATION_LOG_FORMAT,
                authority.validation.full_suite_identity,
                &commands,
                &receipt,
                Some(&whitespace),
            )
            .await?;
            (result, evidence)
        }
        Err(CandidateRuntimeError::ValidationCleanup(error)) => {
            return Err(CandidateRuntimeError::ValidationCleanup(error));
        }
        Err(error) => {
            let evidence = seal_interrupted_validation_evidence(
                process,
                cas,
                authority.actor.principal,
                &request.client_command_id,
                authority.actor.packet.kernel_build_id,
                authority.validation.full_suite_identity,
                &commands,
                &error.to_string(),
                Some(&whitespace),
            )
            .await?;
            (ValidationResult::Interrupted, evidence)
        }
    };
    let engineering_evidence = seal_engineering_completion_evidence(
        process,
        cas,
        authority.actor.principal,
        &request.client_command_id,
        authority.actor.packet.kernel_build_id,
        checkpoint,
        &capture,
        &changed_paths,
        &candidate_patch,
        &hard_evidence,
        hard_result,
    )
    .await?;
    // All physical capture, command execution, and evidence sealing precede
    // the first durable candidate transition.  A local CAS/worktree failure
    // therefore cannot strand a submitted candidate with no validation row.
    let candidate = decisions
        .submit_candidate(&SubmitCandidate {
            principal: authority.actor.principal.to_owned(),
            command_id: request.client_command_id.clone(),
            ticket_attempt_id: authority.ticket.ticket_attempt_id,
            expected_attempt_revision: authority.ticket.expected_attempt_revision,
            expected_ticket_revision: authority.ticket.expected_ticket_revision,
            engineering_session_id: authority.actor.session_id,
            base_commit,
            base_tree,
            regression_tree,
            candidate_tree: candidate_tree.clone(),
            changed_paths,
            regression_patch: checkpoint.regression_patch.clone(),
            regression_command_set: checkpoint.command_set.clone(),
            regression_log: checkpoint.log.clone(),
            candidate_patch: candidate_patch.clone(),
            engineering_report: engineering_evidence.report,
            engineering_risks: engineering_evidence.risks,
            submission: submission.clone(),
        })
        .await?;
    let expected_attempt_revision = ExpectedRevision::new(
        authority
            .ticket
            .expected_attempt_revision
            .get()
            .next()
            .map_err(|error| CandidateRuntimeError::RequestContract(error.to_string()))?,
    );
    let hard_validation = decisions
        .record_validation(&RecordValidation {
            principal: authority.actor.principal.to_owned(),
            command_id: derived_command_id(&request.client_command_id, "hard-validation")?,
            candidate_id: candidate.candidate_id,
            expected_candidate_revision: ExpectedRevision::new(candidate.resulting_revision),
            expected_attempt_revision,
            scope: ValidationScope::HardCandidate,
            kernel_build_id: authority.actor.packet.kernel_build_id,
            performed_by_session_id: authority.actor.session_id,
            validation_profile: authority.validation.full_suite_identity.to_owned(),
            pristine_tree: candidate_tree.clone(),
            command_set: hard_evidence.command_set,
            result: hard_result,
            duration_millis: hard_evidence.duration_millis,
            log: hard_evidence.log,
        })
        .await?;
    if hard_result != ValidationResult::Passed {
        return Ok(CandidateSubmissionOutcome::Rejected {
            candidate: CandidateReceipt {
                candidate_id: candidate.candidate_id,
                state: hard_validation.candidate_state,
                resulting_revision: hard_validation.resulting_candidate_revision,
                audit_log_id: candidate.audit_log_id,
                was_idempotent_retry: candidate.was_idempotent_retry,
            },
            hard_validation,
        });
    }

    // `candidate.submit` is an actor terminal operation, but its transcript
    // does not exist yet.  Persist the exact validated candidate now; the
    // scheduler's `CandidateCommitAttachRequired` action constructs the ref
    // only after it can bind the commit trailer to that sealed transcript.
    Ok(CandidateSubmissionOutcome::Validated {
        candidate: CandidateReceipt {
            candidate_id: candidate.candidate_id,
            state: hard_validation.candidate_state,
            resulting_revision: hard_validation.resulting_candidate_revision,
            audit_log_id: candidate.audit_log_id,
            was_idempotent_retry: candidate.was_idempotent_retry,
        },
        hard_validation,
        candidate_tree,
    })
}

/// Replays a persisted Candidate hard-validation boundary after a daemon
/// interruption between `candidate.submit` and `validation.record`. The
/// caller must obtain [`ResumedHardValidationAuthority`] from durable joins;
/// no actor can invoke this or supply evidence/tree/command identities.
pub async fn resume_candidate_hard_validation(
    process: &ProcessStore,
    decisions: &DecisionStore,
    cas: &CasStore,
    runner: &CommandRunner,
    git: &GitCustody,
    authority: &ResumedHardValidationAuthority,
) -> Result<ResumedHardValidationOutcome, CandidateRuntimeError> {
    authority
        .application
        .validate()
        .map_err(|error| CandidateRuntimeError::ApplicationContract(error.to_string()))?;
    let configured_root = Path::new(
        authority
            .application
            .repository
            .canonical_local_path
            .as_str(),
    );
    if configured_root != authority.repository.root() {
        return Err(CandidateRuntimeError::RepositoryBindingMismatch);
    }
    assert_declared_reproducer(&authority.application, &authority.product_reproducer)?;
    assert_full_suite(
        &authority.application,
        &authority.full_suite_identity,
        &authority.full_suite,
    )?;
    validate_commit_message_policy(&authority.application, &authority.submission)?;
    let whitespace =
        git.check_candidate_whitespace(&authority.repository, &authority.candidate_tree)?;
    let mut commands = Vec::with_capacity(authority.full_suite.len() + 1);
    commands.push(authority.product_reproducer.clone());
    commands.extend_from_slice(&authority.full_suite);
    let execution = run_validation_in_fresh_worktree(
        git,
        &authority.repository,
        authority.candidate_tree.clone(),
        authority.validation_worktree_name.clone(),
        runner,
        &commands,
        ValidationInvocation::Candidate,
    );
    let (result, evidence) = match execution {
        Ok(receipt) => {
            let result = if whitespace.is_clean() {
                validation_result(receipt.status())
            } else {
                ValidationResult::Failed
            };
            let evidence = seal_validation_evidence(
                process,
                cas,
                &authority.principal,
                &derived_command_id(&authority.command_id, "hard-command-set")?,
                authority.kernel_build_id,
                VALIDATION_LOG_FORMAT,
                &authority.full_suite_identity,
                &commands,
                &receipt,
                Some(&whitespace),
            )
            .await?;
            (result, evidence)
        }
        Err(CandidateRuntimeError::ValidationCleanup(error)) => {
            return Err(CandidateRuntimeError::ValidationCleanup(error));
        }
        Err(error) => {
            let evidence = seal_interrupted_validation_evidence(
                process,
                cas,
                &authority.principal,
                &authority.command_id,
                authority.kernel_build_id,
                &authority.full_suite_identity,
                &commands,
                &error.to_string(),
                Some(&whitespace),
            )
            .await?;
            (ValidationResult::Interrupted, evidence)
        }
    };
    let validation = decisions
        .record_validation(&RecordValidation {
            principal: authority.principal.clone(),
            command_id: derived_command_id(&authority.command_id, "hard-validation")?,
            candidate_id: authority.candidate_id,
            expected_candidate_revision: authority.expected_candidate_revision,
            expected_attempt_revision: authority.ticket.expected_attempt_revision,
            scope: ValidationScope::HardCandidate,
            kernel_build_id: authority.kernel_build_id,
            performed_by_session_id: authority.engineering_session_id,
            validation_profile: authority.full_suite_identity.clone(),
            pristine_tree: repository_object(authority.candidate_tree.as_str())?,
            command_set: evidence.command_set,
            result,
            duration_millis: evidence.duration_millis,
            log: evidence.log,
        })
        .await?;
    if result != ValidationResult::Passed {
        return Ok(ResumedHardValidationOutcome { validation });
    }
    Ok(ResumedHardValidationOutcome { validation })
}

/// Completes a crash-stranded hard-validation pass by reconstructing the
/// bounded local candidate ref and attaching it to the already validated
/// candidate. This intentionally has no command runner or actor input.
pub async fn resume_candidate_commit_attach(
    decisions: &DecisionStore,
    git: &GitCustody,
    authority: &ResumeCandidateCommitAttachAuthority,
) -> Result<CandidateReceipt, CandidateRuntimeError> {
    authority
        .application
        .validate()
        .map_err(|error| CandidateRuntimeError::ApplicationContract(error.to_string()))?;
    let configured_root = Path::new(
        authority
            .application
            .repository
            .canonical_local_path
            .as_str(),
    );
    if configured_root != authority.repository.root() {
        return Err(CandidateRuntimeError::RepositoryBindingMismatch);
    }
    validate_commit_message_policy(&authority.application, &authority.submission)?;
    let message = CommitMessage::normalize(
        &authority.submission.commit_subject,
        &authority.submission.commit_body,
    )?;
    let candidate_commit = git.construct_or_recover_candidate_commit(
        &authority.repository,
        &ConstructCandidateCommit {
            candidate_tree: authority.candidate_tree.clone(),
            candidate_ref: CandidateRefName::new(
                authority.ticket.ticket_id,
                authority.candidate_id,
            ),
            message,
            author: authority.commit.author.clone(),
            committer: authority.commit.committer.clone(),
            timestamp_unix_seconds: authority.commit.timestamp_unix_seconds,
            provenance: CommitProvenance {
                campaign_id: authority.campaign_id,
                ticket_id: authority.ticket.ticket_id,
                ticket_revision_digest: authority.ticket.ticket_revision_digest,
                kernel_build_id: authority.kernel_build_id,
                application_revision_id: authority.application_revision_id,
                regression_tree: authority.regression_tree.clone(),
                patch_digest: authority.candidate_patch_digest,
                engineering_session_digest: authority.commit.engineering_session_digest,
                validation_id: authority.hard_validation_id,
            },
        },
    )?;
    Ok(decisions
        .attach_candidate_commit(&AttachCandidateCommit {
            principal: authority.principal.clone(),
            command_id: authority.command_id.clone(),
            candidate_id: authority.candidate_id,
            expected_candidate_revision: authority.expected_candidate_revision,
            candidate_commit: repository_object(candidate_commit.commit().as_str())?,
            candidate_ref: candidate_commit.candidate_ref().as_str().to_owned(),
        })
        .await?)
}

/// Runs Quality's complete profile in a fresh exact candidate worktree and
/// stores its independent receipt.  The actor can name only the profile
/// identity selected by the packet-bound authority.
pub async fn run_quality_full_suite(
    process: &ProcessStore,
    decisions: &DecisionStore,
    cas: &CasStore,
    runner: &CommandRunner,
    git: &GitCustody,
    authority: &QualityCandidateAuthority<'_>,
    request: &QualityRunFullSuiteRequest,
) -> Result<QualityFullSuiteOutcome, CandidateRuntimeError> {
    validate_quality_authority(authority)?;
    validate_request_revision(&authority.actor, request.expected_revision)?;
    let quality_request = request
        .full_suite_request()
        .map_err(|error| CandidateRuntimeError::RequestContract(error.to_string()))?;
    if quality_request.validation_profile != authority.validation.full_suite_identity {
        return Err(CandidateRuntimeError::ValidationProfileMismatch);
    }
    let tree = GitTreeId::parse(authority.candidate.candidate_tree.as_str())?;
    let execution = run_validation_in_fresh_worktree(
        git,
        authority.repository,
        tree,
        authority.validation.worktree_name.clone(),
        runner,
        authority.validation.full_suite,
        ValidationInvocation::Quality,
    );
    let (result, evidence) = match execution {
        Ok(receipt) => {
            let result = validation_result(receipt.status());
            let evidence = seal_validation_evidence(
                process,
                cas,
                authority.actor.principal,
                derived_command_id(&request.client_command_id, "quality-command-set")?.as_str(),
                authority.actor.packet.kernel_build_id,
                VALIDATION_LOG_FORMAT,
                authority.validation.full_suite_identity,
                authority.validation.full_suite,
                &receipt,
                None,
            )
            .await?;
            (result, evidence)
        }
        Err(CandidateRuntimeError::ValidationCleanup(error)) => {
            return Err(CandidateRuntimeError::ValidationCleanup(error));
        }
        Err(error) => {
            let evidence = seal_interrupted_validation_evidence(
                process,
                cas,
                authority.actor.principal,
                &request.client_command_id,
                authority.actor.packet.kernel_build_id,
                authority.validation.full_suite_identity,
                authority.validation.full_suite,
                &error.to_string(),
                None,
            )
            .await?;
            (ValidationResult::Interrupted, evidence)
        }
    };
    let validation = decisions
        .record_validation(&RecordValidation {
            principal: authority.actor.principal.to_owned(),
            command_id: derived_command_id(&request.client_command_id, "quality-validation")?,
            candidate_id: authority.candidate.candidate_id,
            expected_candidate_revision: ExpectedRevision::new(
                authority.candidate.candidate_revision,
            ),
            expected_attempt_revision: authority.expected_attempt_revision,
            scope: ValidationScope::QualityFullSuite,
            kernel_build_id: authority.actor.packet.kernel_build_id,
            performed_by_session_id: authority.actor.session_id,
            validation_profile: quality_request.validation_profile,
            pristine_tree: authority.candidate.candidate_tree.clone(),
            command_set: evidence.command_set,
            result,
            duration_millis: evidence.duration_millis,
            log: evidence.log.clone(),
        })
        .await?;
    let receipt = QualityValidationReceiptV2 {
        validation_id: validation.validation_id,
        candidate_id: authority.candidate.candidate_id,
        candidate_tree: authority.candidate.candidate_tree.clone(),
        log_artifact: evidence.log,
        revision: validation.resulting_candidate_revision,
    };
    receipt
        .validate()
        .map_err(|error| CandidateRuntimeError::RequestContract(error.to_string()))?;
    Ok(QualityFullSuiteOutcome {
        receipt,
        result,
        resulting_attempt_revision: validation.resulting_attempt_revision,
        audit_log_id: validation.audit_log_id,
    })
}

/// Verifies every Quality artifact belongs to this actor before admitting the
/// one terminal review bound to the exact passed full-suite receipt.
pub async fn submit_quality_review(
    process: &ProcessStore,
    decisions: &DecisionStore,
    cas: &CasStore,
    authority: &QualityReviewAuthority<'_>,
    request: &QualitySubmitReviewRequest,
) -> Result<ReviewReceipt, CandidateRuntimeError> {
    validate_review_authority(authority)?;
    validate_request_revision(&authority.actor, request.expected_revision)?;
    let submission = request
        .submission()
        .map_err(|error| CandidateRuntimeError::RequestContract(error.to_string()))?;
    if submission.full_suite_validation_id != authority.full_suite.validation_id {
        return Err(CandidateRuntimeError::QualityReceiptMismatch);
    }
    verify_actor_artifacts(
        process,
        cas,
        authority.actor.principal,
        review_artifacts(&submission),
    )
    .await?;
    Ok(decisions
        .submit_quality_review(&SubmitQualityReview {
            principal: authority.actor.principal.to_owned(),
            command_id: request.client_command_id.clone(),
            candidate_id: authority.candidate.candidate_id,
            expected_candidate_revision: ExpectedRevision::new(authority.full_suite.revision),
            expected_attempt_revision: authority.expected_attempt_revision,
            quality_session_id: authority.actor.session_id,
            submission,
        })
        .await?)
}

fn validate_engineering_authority(
    authority: &EngineeringCandidateAuthority<'_>,
    terminal: Option<TerminalOperationV2>,
) -> Result<(), CandidateRuntimeError> {
    validate_common_authority(
        authority.actor,
        authority.application,
        authority.repository,
        AssignmentRole::Engineering,
        terminal,
    )?;
    let packet_workspace = Path::new(authority.actor.packet.workspace_root.as_str());
    if packet_workspace != authority.actor_worktree.path() {
        return Err(CandidateRuntimeError::EngineeringWorkspaceMismatch);
    }
    assert_declared_reproducer(authority.application, authority.regression.command)?;
    assert_declared_reproducer(
        authority.application,
        authority.validation.product_reproducer,
    )?;
    assert_full_suite(
        authority.application,
        authority.validation.full_suite_identity,
        authority.validation.full_suite,
    )?;
    validate_regression_identity(authority.regression.expected_failure)?;
    Ok(())
}

fn validate_quality_authority(
    authority: &QualityCandidateAuthority<'_>,
) -> Result<(), CandidateRuntimeError> {
    validate_common_authority(
        authority.actor,
        authority.application,
        authority.repository,
        AssignmentRole::Quality,
        None,
    )?;
    authority
        .candidate
        .validate()
        .map_err(|error| CandidateRuntimeError::RequestContract(error.to_string()))?;
    assert_full_suite(
        authority.application,
        authority.validation.full_suite_identity,
        authority.validation.full_suite,
    )
}

fn validate_review_authority(
    authority: &QualityReviewAuthority<'_>,
) -> Result<(), CandidateRuntimeError> {
    validate_packet_office(
        authority.actor,
        AssignmentRole::Quality,
        Some(TerminalOperationV2::QualitySubmitReview),
    )?;
    authority
        .candidate
        .validate()
        .map_err(|error| CandidateRuntimeError::RequestContract(error.to_string()))?;
    authority
        .full_suite
        .validate()
        .map_err(|error| CandidateRuntimeError::RequestContract(error.to_string()))?;
    if authority.full_suite.candidate_id != authority.candidate.candidate_id
        || authority.full_suite.candidate_tree != authority.candidate.candidate_tree
    {
        return Err(CandidateRuntimeError::QualityReceiptMismatch);
    }
    Ok(())
}

fn validate_common_authority(
    actor: ActorRequestBinding<'_>,
    application: &ApplicationBundleV2,
    repository: &QualifiedRepository,
    assignment_role: AssignmentRole,
    terminal: Option<TerminalOperationV2>,
) -> Result<(), CandidateRuntimeError> {
    application
        .validate()
        .map_err(|error| CandidateRuntimeError::ApplicationContract(error.to_string()))?;
    validate_packet_office(actor, assignment_role, terminal)?;
    let configured_root = Path::new(application.repository.canonical_local_path.as_str());
    if configured_root != repository.root() {
        return Err(CandidateRuntimeError::RepositoryBindingMismatch);
    }
    if application.repository.default_branch != repository.default_branch().as_str() {
        return Err(CandidateRuntimeError::DefaultBranchMismatch);
    }
    Ok(())
}

fn validate_packet_office(
    actor: ActorRequestBinding<'_>,
    assignment_role: AssignmentRole,
    terminal: Option<TerminalOperationV2>,
) -> Result<(), CandidateRuntimeError> {
    actor
        .packet
        .validate()
        .map_err(|error| CandidateRuntimeError::PacketBinding(error.to_string()))?;
    if actor.packet.assignment_role != assignment_role {
        return Err(CandidateRuntimeError::PacketBinding(
            "assignment office is not authorized for this operation".to_owned(),
        ));
    }
    if let Some(operation) = terminal
        && !actor.packet.terminal_operations.contains(&operation)
    {
        return Err(CandidateRuntimeError::PacketBinding(
            "assignment terminal operation is not authorized".to_owned(),
        ));
    }
    Ok(())
}

fn validate_request_revision(
    actor: &ActorRequestBinding<'_>,
    request_revision: u64,
) -> Result<(), CandidateRuntimeError> {
    if request_revision != actor.session_revision.get().get() {
        return Err(CandidateRuntimeError::PacketBinding(
            "actor request carries a stale session revision".to_owned(),
        ));
    }
    Ok(())
}

fn validate_regression_request(
    authority: &EngineeringCandidateAuthority<'_>,
    request: &CandidateCheckpointRegressionRequest,
) -> Result<(), CandidateRuntimeError> {
    if request.regression_command != authority.regression.command.profile().name
        || request.expected_failure != authority.regression.expected_failure
    {
        return Err(CandidateRuntimeError::RegressionCheckpointMismatch);
    }
    validate_regression_identity(&request.expected_failure)
}

fn validate_regression_identity(value: &str) -> Result<(), CandidateRuntimeError> {
    if value.is_empty() || value.len() > 4096 || value.contains('\0') {
        return Err(CandidateRuntimeError::RegressionCheckpointMismatch);
    }
    Ok(())
}

fn validate_checkpoint(
    authority: &EngineeringCandidateAuthority<'_>,
    checkpoint: &RegressionCheckpoint,
) -> Result<(), CandidateRuntimeError> {
    if checkpoint.ticket_attempt_id != authority.ticket.ticket_attempt_id
        || checkpoint.engineering_session_id != authority.actor.session_id
        || checkpoint.base_tree != *authority.repository.snapshot().base_tree()
        || checkpoint.expected_failure != authority.regression.expected_failure
    {
        return Err(CandidateRuntimeError::CheckpointBindingMismatch);
    }
    Ok(())
}

fn assert_declared_reproducer(
    application: &ApplicationBundleV2,
    command: &DeterministicCommand,
) -> Result<(), CandidateRuntimeError> {
    if application
        .reproducer_profiles
        .iter()
        .any(|profile| profile == command.profile())
    {
        Ok(())
    } else {
        Err(CandidateRuntimeError::UndeclaredCommand)
    }
}

fn assert_full_suite(
    application: &ApplicationBundleV2,
    identity: &str,
    commands: &[DeterministicCommand],
) -> Result<(), CandidateRuntimeError> {
    if identity.is_empty() || identity.len() > 160 || identity.contains('\0') {
        return Err(CandidateRuntimeError::ValidationProfileMismatch);
    }
    if !exact_command_profiles(&application.validation_profiles.full, commands) {
        return Err(CandidateRuntimeError::UndeclaredCommand);
    }
    Ok(())
}

fn exact_command_profiles(
    declared: &[factory_protocol::CommandProfileV2],
    commands: &[DeterministicCommand],
) -> bool {
    declared.len() == commands.len()
        && declared
            .iter()
            .zip(commands)
            .all(|(profile, command)| profile == command.profile())
}

fn reject_forbidden_paths(
    application: &ApplicationBundleV2,
    capture: &TreeCapture,
) -> Result<(), CandidateRuntimeError> {
    if let Some(path) = forbidden_changed_path(
        &application.git_policy.forbidden_paths,
        capture.changed_paths(),
    ) {
        return Err(CandidateRuntimeError::ForbiddenChangedPath {
            path: path.to_owned(),
        });
    }
    Ok(())
}

fn forbidden_changed_path<'a>(
    forbidden: &[factory_protocol::RepositoryRelativePath],
    changed_paths: &'a [String],
) -> Option<&'a str> {
    changed_paths.iter().find_map(|path| {
        forbidden
            .iter()
            .any(|forbidden| forbidden.as_str() == path)
            .then_some(path.as_str())
    })
}

fn validate_commit_message_policy(
    application: &ApplicationBundleV2,
    submission: &CandidateSubmissionV2,
) -> Result<(), CandidateRuntimeError> {
    if submission.commit_subject.len()
        > usize::from(application.commit_message_policy.subject_byte_limit)
        || submission.commit_body.len()
            > usize::from(application.commit_message_policy.body_byte_limit)
    {
        return Err(CandidateRuntimeError::CommitMessagePolicyExceeded);
    }
    Ok(())
}

fn run_validation_in_fresh_worktree(
    git: &GitCustody,
    repository: &QualifiedRepository,
    tree: GitTreeId,
    name: WorktreeName,
    runner: &CommandRunner,
    commands: &[DeterministicCommand],
    invocation: ValidationInvocation,
) -> Result<CommandValidationReceipt, CandidateRuntimeError> {
    let worktree =
        git.rematerialize_tree(repository, tree.clone(), WorktreeKind::Validation, name)?;
    let result = (|| {
        let workspace = CommandWorkspace::open(worktree.path())?;
        let probe = GitTrackedTreeProbe::new(
            GitTreeIdentity::parse(tree.as_str())?,
            std::time::Duration::from_millis(TREE_PROBE_TIMEOUT_MILLIS),
            TREE_PROBE_STREAM_LIMIT,
            TREE_PROBE_STREAM_LIMIT,
            crate::command_supervision::ComparisonRevision::parse(TREE_COMPARISON_REVISION)?,
        )?;
        let pristine = PristineWorkspace::new(workspace, probe);
        match invocation {
            ValidationInvocation::Candidate => runner.run_candidate_validation(&pristine, commands),
            ValidationInvocation::Quality => runner.run_quality_validation(&pristine, commands),
        }
    })();
    let cleanup = git.cleanup_worktree(worktree);
    match (result, cleanup) {
        (Ok(receipt), Ok(())) => Ok(receipt),
        (_, Err(error)) => Err(CandidateRuntimeError::ValidationCleanup(error)),
        (Err(error), Ok(())) => Err(error.into()),
    }
}

fn validation_result(status: ValidationStatus) -> ValidationResult {
    match status {
        ValidationStatus::Passed => ValidationResult::Passed,
        ValidationStatus::CommandFailed | ValidationStatus::TreeChanged => ValidationResult::Failed,
    }
}

async fn verify_actor_artifacts(
    process: &ProcessStore,
    cas: &CasStore,
    principal: &str,
    artifacts: Vec<&SealedArtifactReferenceV2>,
) -> Result<(), CandidateRuntimeError> {
    for reference in artifacts {
        let sealed = process
            .registered_artifact_for_principal(cas, principal, reference.artifact_id)
            .await?;
        if sealed.digest() != reference.digest || sealed.byte_length() != reference.byte_length {
            return Err(CandidateRuntimeError::ArtifactIdentityMismatch);
        }
    }
    Ok(())
}

fn review_artifacts(
    submission: &factory_protocol::QualityReviewSubmissionV2,
) -> Vec<&SealedArtifactReferenceV2> {
    vec![
        &submission.rationale,
        &submission.risks,
        &submission.additional_probes,
    ]
}

struct EngineeringCompletionEvidence {
    report: SealedArtifactReferenceV2,
    risks: SealedArtifactReferenceV2,
}

/// Produces the two Engineering narrative slots from custody facts, not
/// workspace files supplied by an actor. These compact records leave Quality
/// with useful navigation evidence while a missing optional prose file cannot
/// discard a finished product change.
#[allow(clippy::too_many_arguments)]
async fn seal_engineering_completion_evidence(
    process: &ProcessStore,
    cas: &CasStore,
    principal: &str,
    command_id: &str,
    kernel_build_id: factory_protocol::KernelBuildId,
    checkpoint: &RegressionCheckpoint,
    capture: &TreeCapture,
    changed_paths: &SealedArtifactReferenceV2,
    candidate_patch: &SealedArtifactReferenceV2,
    hard_evidence: &ValidationEvidence,
    hard_result: ValidationResult,
) -> Result<EngineeringCompletionEvidence, CandidateRuntimeError> {
    let report_bytes = json::to_string(&KernelEngineeringReportEvidence {
        format: "factory-kernel-engineering-report-v1",
        regression_tree: checkpoint.regression.tree().as_str().to_owned(),
        candidate_tree: capture.tree().as_str().to_owned(),
        regression_identity: checkpoint.regression_identity.clone(),
        expected_failure: checkpoint.expected_failure.clone(),
        changed_paths: artifact_log(changed_paths.clone()),
        candidate_patch: artifact_log(candidate_patch.clone()),
        hard_validation_result: validation_result_name(hard_result),
        hard_validation_command_set: artifact_log(hard_evidence.command_set.clone()),
        hard_validation_log: artifact_log(hard_evidence.log.clone()),
    })
    .into_bytes();
    let report = seal_bytes(
        process,
        cas,
        principal,
        &derived_command_id(command_id, "kernel-engineering-report")?,
        kernel_build_id,
        &report_bytes,
        "kernel Engineering report",
        usize::try_from(factory_protocol::CANDIDATE_REPORT_BYTE_LIMIT).unwrap_or(usize::MAX),
    )
    .await?;
    let risks_bytes = json::to_string(&KernelEngineeringRisksEvidence {
        format: "factory-kernel-engineering-risks-v1",
        actor_risks_collected: false,
        note: "The kernel derived this candidate from the owned worktree and deterministic validation; Quality must assess residual product risk independently.".to_owned(),
    })
    .into_bytes();
    let risks = seal_bytes(
        process,
        cas,
        principal,
        &derived_command_id(command_id, "kernel-engineering-risks")?,
        kernel_build_id,
        &risks_bytes,
        "kernel Engineering risks",
        usize::try_from(factory_protocol::CANDIDATE_RISKS_BYTE_LIMIT).unwrap_or(usize::MAX),
    )
    .await?;
    Ok(EngineeringCompletionEvidence { report, risks })
}

async fn seal_changed_paths(
    process: &ProcessStore,
    cas: &CasStore,
    principal: &str,
    command_id: &str,
    kernel_build_id: factory_protocol::KernelBuildId,
    capture: &TreeCapture,
) -> Result<SealedArtifactReferenceV2, CandidateRuntimeError> {
    let bytes = json::to_string(&ChangedPathsEvidence {
        format: "factory-changed-paths-v1",
        paths: capture.changed_paths().to_vec(),
    })
    .into_bytes();
    seal_bytes(
        process,
        cas,
        principal,
        command_id,
        kernel_build_id,
        &bytes,
        "candidate changed paths",
        COMMAND_SET_LIMIT,
    )
    .await
}

struct ValidationEvidence {
    command_set: SealedArtifactReferenceV2,
    log: SealedArtifactReferenceV2,
    duration_millis: u64,
}

async fn seal_validation_evidence(
    process: &ProcessStore,
    cas: &CasStore,
    principal: &str,
    command_set_id: &str,
    kernel_build_id: factory_protocol::KernelBuildId,
    format: &'static str,
    profile: &str,
    commands: &[DeterministicCommand],
    receipt: &CommandValidationReceipt,
    whitespace: Option<&CandidateWhitespaceCheck>,
) -> Result<ValidationEvidence, CandidateRuntimeError> {
    let command_set = seal_command_set(
        process,
        cas,
        principal,
        command_set_id,
        kernel_build_id,
        profile,
        commands,
    )
    .await?;
    let mut logs = Vec::with_capacity(receipt.commands().len() + 2);
    logs.push(
        seal_receipt_log(
            process,
            cas,
            principal,
            &derived_command_id(command_set_id, "before")?,
            kernel_build_id,
            "tree-before",
            receipt.before().receipt(),
        )
        .await?,
    );
    for (index, command) in receipt.commands().iter().enumerate() {
        logs.push(
            seal_receipt_log(
                process,
                cas,
                principal,
                &derived_command_id(command_set_id, &format!("command-{index}"))?,
                kernel_build_id,
                "validation-command",
                command,
            )
            .await?,
        );
    }
    logs.push(
        seal_receipt_log(
            process,
            cas,
            principal,
            &derived_command_id(command_set_id, "after")?,
            kernel_build_id,
            "tree-after",
            receipt.after().receipt(),
        )
        .await?,
    );
    let duration_millis = logs.iter().try_fold(0_u64, |total, log| {
        total
            .checked_add(log.elapsed_millis)
            .ok_or(CandidateRuntimeError::EvidenceTooLarge {
                kind: "validation duration",
                maximum: usize::MAX,
                observed: usize::MAX,
            })
    })?;
    let status = whitespace.map_or_else(
        || validation_status_name(receipt.status()),
        |check| {
            if check.is_clean() {
                validation_status_name(receipt.status())
            } else {
                "git_whitespace_failed"
            }
        },
    );
    let whitespace = match whitespace {
        Some(check) => Some(
            seal_whitespace_check_log(
                process,
                cas,
                principal,
                &derived_command_id(command_set_id, "git-diff-check")?,
                kernel_build_id,
                check,
            )
            .await?,
        ),
        None => None,
    };
    let log_bytes = json::to_string(&ValidationLogEvidence {
        format,
        invocation: invocation_name(receipt.invocation()),
        status,
        exact_tree: receipt.exact_tree().as_str().to_owned(),
        commands: logs,
        whitespace,
    })
    .into_bytes();
    let log = seal_bytes(
        process,
        cas,
        principal,
        &derived_command_id(command_set_id, "log")?,
        kernel_build_id,
        &log_bytes,
        "validation log",
        VALIDATION_LOG_LIMIT,
    )
    .await?;
    Ok(ValidationEvidence {
        command_set,
        log,
        duration_millis,
    })
}

async fn seal_interrupted_validation_evidence(
    process: &ProcessStore,
    cas: &CasStore,
    principal: &str,
    base_command_id: &str,
    kernel_build_id: factory_protocol::KernelBuildId,
    profile: &str,
    commands: &[DeterministicCommand],
    reason: &str,
    whitespace: Option<&CandidateWhitespaceCheck>,
) -> Result<ValidationEvidence, CandidateRuntimeError> {
    let command_set_id = derived_command_id(base_command_id, "interrupted-command-set")?;
    let command_set = seal_command_set(
        process,
        cas,
        principal,
        &command_set_id,
        kernel_build_id,
        profile,
        commands,
    )
    .await?;
    let whitespace = match whitespace {
        Some(check) => Some(
            seal_whitespace_check_log(
                process,
                cas,
                principal,
                &derived_command_id(base_command_id, "git-diff-check")?,
                kernel_build_id,
                check,
            )
            .await?,
        ),
        None => None,
    };
    let bytes = json::to_string(&InterruptedValidationLogEvidence {
        format: VALIDATION_LOG_FORMAT,
        status: "interrupted",
        reason: reason.to_owned(),
        whitespace,
    })
    .into_bytes();
    let log = seal_bytes(
        process,
        cas,
        principal,
        &derived_command_id(base_command_id, "interrupted-log")?,
        kernel_build_id,
        &bytes,
        "validation log",
        VALIDATION_LOG_LIMIT,
    )
    .await?;
    Ok(ValidationEvidence {
        command_set,
        log,
        duration_millis: 0,
    })
}

async fn seal_command_set(
    process: &ProcessStore,
    cas: &CasStore,
    principal: &str,
    command_id: &str,
    kernel_build_id: factory_protocol::KernelBuildId,
    profile: &str,
    commands: &[DeterministicCommand],
) -> Result<SealedArtifactReferenceV2, CandidateRuntimeError> {
    let bytes = json::to_string(&CommandSetEvidence {
        format: COMMAND_SET_FORMAT,
        profile: profile.to_owned(),
        commands: commands.iter().map(command_set_entry).collect(),
    })
    .into_bytes();
    seal_bytes(
        process,
        cas,
        principal,
        command_id,
        kernel_build_id,
        &bytes,
        "validation command set",
        COMMAND_SET_LIMIT,
    )
    .await
}

async fn seal_receipt_log(
    process: &ProcessStore,
    cas: &CasStore,
    principal: &str,
    command_id: &str,
    kernel_build_id: factory_protocol::KernelBuildId,
    role: &'static str,
    receipt: &CommandReceipt,
) -> Result<CommandLogEvidence, CandidateRuntimeError> {
    let stdout = seal_bytes(
        process,
        cas,
        principal,
        &derived_command_id(command_id, "stdout")?,
        kernel_build_id,
        receipt.stdout(),
        "validation stdout",
        usize::try_from(cas.maximum_object_bytes()).unwrap_or(usize::MAX),
    )
    .await?;
    let stderr = seal_bytes(
        process,
        cas,
        principal,
        &derived_command_id(command_id, "stderr")?,
        kernel_build_id,
        receipt.stderr(),
        "validation stderr",
        usize::try_from(cas.maximum_object_bytes()).unwrap_or(usize::MAX),
    )
    .await?;
    let elapsed_millis = u64::try_from(receipt.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(CommandLogEvidence {
        role,
        executable: receipt.executable().to_string_lossy().into_owned(),
        argv: receipt.argv().to_vec(),
        working_directory: receipt.working_directory().as_str().to_owned(),
        comparison_revision: receipt.comparison_revision().as_str().to_owned(),
        terminal: terminal_name(receipt),
        exit_status: receipt.exit_status(),
        signal: receipt.signal(),
        elapsed_millis,
        matched_expectation: receipt.matches_expectation(),
        stdout: artifact_log(stdout),
        stderr: artifact_log(stderr),
    })
}

async fn seal_whitespace_check_log(
    process: &ProcessStore,
    cas: &CasStore,
    principal: &str,
    command_id: &str,
    kernel_build_id: factory_protocol::KernelBuildId,
    check: &CandidateWhitespaceCheck,
) -> Result<WhitespaceCheckEvidence, CandidateRuntimeError> {
    let stdout = seal_bytes(
        process,
        cas,
        principal,
        &derived_command_id(command_id, "stdout")?,
        kernel_build_id,
        check.stdout(),
        "Git whitespace-check stdout",
        usize::try_from(cas.maximum_object_bytes()).unwrap_or(usize::MAX),
    )
    .await?;
    let stderr = seal_bytes(
        process,
        cas,
        principal,
        &derived_command_id(command_id, "stderr")?,
        kernel_build_id,
        check.stderr(),
        "Git whitespace-check stderr",
        usize::try_from(cas.maximum_object_bytes()).unwrap_or(usize::MAX),
    )
    .await?;
    Ok(WhitespaceCheckEvidence {
        base_tree: check.base_tree().as_str().to_owned(),
        candidate_tree: check.candidate_tree().as_str().to_owned(),
        clean: check.is_clean(),
        stdout: artifact_log(stdout),
        stderr: artifact_log(stderr),
    })
}

async fn seal_bytes(
    process: &ProcessStore,
    cas: &CasStore,
    principal: &str,
    command_id: &str,
    kernel_build_id: factory_protocol::KernelBuildId,
    bytes: &[u8],
    kind: &'static str,
    maximum: usize,
) -> Result<SealedArtifactReferenceV2, CandidateRuntimeError> {
    if bytes.len() > maximum {
        return Err(CandidateRuntimeError::EvidenceTooLarge {
            kind,
            maximum,
            observed: bytes.len(),
        });
    }
    let (sealed, receipt) = process
        .adopt_and_register_kernel_bytes(cas, principal, command_id, kernel_build_id, bytes)
        .await?;
    Ok(reference(receipt.artifact_id, sealed))
}

fn reference(
    artifact_id: factory_protocol::ArtifactId,
    sealed: CasArtifact,
) -> SealedArtifactReferenceV2 {
    SealedArtifactReferenceV2 {
        artifact_id,
        digest: sealed.digest(),
        byte_length: sealed.byte_length(),
    }
}

fn repository_object(value: &str) -> Result<RepositoryObjectIdV2, CandidateRuntimeError> {
    RepositoryObjectIdV2::parse(value)
        .map_err(|error| CandidateRuntimeError::RequestContract(error.to_string()))
}

fn derived_command_id(base: &str, suffix: &str) -> Result<String, CandidateRuntimeError> {
    if base.is_empty()
        || base.len() > 160
        || !base
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'_' | b'-'))
    {
        return Err(CandidateRuntimeError::RequestContract(
            "client command ID is not a bounded audit identity".to_owned(),
        ));
    }
    if suffix.is_empty()
        || suffix.len() > 80
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'_' | b'-'))
    {
        return Err(CandidateRuntimeError::RequestContract(
            "kernel audit command suffix is invalid".to_owned(),
        ));
    }
    let direct = format!("{base}-{suffix}");
    if direct.len() <= 160 {
        return Ok(direct);
    }
    // Evidence often needs a nested suffix (for individual stream seals).
    // Preserve collision resistance while keeping the persisted audit command
    // within the common 160-byte boundary even for a long actor command ID.
    Ok(format!(
        "candidate-runtime-{}",
        ContentDigest::of_bytes(format!("{base}\0{suffix}").as_bytes()).to_hex()
    ))
}

fn invocation_name(invocation: ValidationInvocation) -> &'static str {
    match invocation {
        ValidationInvocation::Candidate => "candidate",
        ValidationInvocation::Quality => "quality",
    }
}

fn validation_status_name(status: ValidationStatus) -> &'static str {
    match status {
        ValidationStatus::Passed => "passed",
        ValidationStatus::CommandFailed => "command_failed",
        ValidationStatus::TreeChanged => "tree_changed",
    }
}

fn terminal_name(receipt: &CommandReceipt) -> &'static str {
    match receipt.terminal() {
        crate::command_supervision::CommandTerminal::Exited { .. } => "exited",
        crate::command_supervision::CommandTerminal::Signaled { .. } => "signaled",
        crate::command_supervision::CommandTerminal::TimedOut { .. } => "timed_out",
        crate::command_supervision::CommandTerminal::StdoutLimit { .. } => "stdout_limit",
        crate::command_supervision::CommandTerminal::StderrLimit { .. } => "stderr_limit",
    }
}

#[derive(Serialize)]
struct ChangedPathsEvidence {
    format: &'static str,
    paths: Vec<String>,
}

#[derive(Serialize)]
struct KernelEngineeringReportEvidence {
    format: &'static str,
    regression_tree: String,
    candidate_tree: String,
    regression_identity: String,
    expected_failure: String,
    changed_paths: ArtifactLogEvidence,
    candidate_patch: ArtifactLogEvidence,
    hard_validation_result: &'static str,
    hard_validation_command_set: ArtifactLogEvidence,
    hard_validation_log: ArtifactLogEvidence,
}

#[derive(Serialize)]
struct KernelEngineeringRisksEvidence {
    format: &'static str,
    actor_risks_collected: bool,
    note: String,
}

#[derive(Serialize)]
struct CommandSetEvidence {
    format: &'static str,
    profile: String,
    commands: Vec<CommandSetEntry>,
}

#[derive(Serialize)]
struct CommandSetEntry {
    name: String,
    executable: String,
    argv: Vec<String>,
    working_directory: String,
    environment: Vec<EnvironmentEvidence>,
    timeout_millis: u64,
    stdout_byte_limit: u32,
    stderr_byte_limit: u32,
    expected_exit_status: i32,
    comparison_revision: String,
}

#[derive(Serialize)]
struct EnvironmentEvidence {
    name: String,
    value: String,
}

fn command_set_entry(command: &DeterministicCommand) -> CommandSetEntry {
    let profile = command.profile();
    let executable = match &profile.executable {
        factory_protocol::ExecutableV2::ApprovedTool(tool) => match tool {
            factory_protocol::ApprovedToolV2::Cargo => "approved:cargo".to_owned(),
            factory_protocol::ApprovedToolV2::Git => "approved:git".to_owned(),
        },
        factory_protocol::ExecutableV2::RepositoryPath(path) => {
            format!("repository:{}", path.as_str())
        }
    };
    CommandSetEntry {
        name: profile.name.clone(),
        executable,
        argv: profile.argv.clone(),
        working_directory: profile.working_directory.as_str().to_owned(),
        environment: profile
            .environment
            .iter()
            .map(|addition| EnvironmentEvidence {
                name: addition.name.clone(),
                value: addition.value.clone(),
            })
            .collect(),
        timeout_millis: profile.timeout.get(),
        stdout_byte_limit: profile.stdout_byte_limit,
        stderr_byte_limit: profile.stderr_byte_limit,
        expected_exit_status: profile.expected_exit_status,
        comparison_revision: command
            .expectation()
            .comparison_revision()
            .as_str()
            .to_owned(),
    }
}

#[derive(Serialize)]
struct ValidationLogEvidence {
    format: &'static str,
    invocation: &'static str,
    status: &'static str,
    exact_tree: String,
    commands: Vec<CommandLogEvidence>,
    whitespace: Option<WhitespaceCheckEvidence>,
}

#[derive(Serialize)]
struct InterruptedValidationLogEvidence {
    format: &'static str,
    status: &'static str,
    reason: String,
    whitespace: Option<WhitespaceCheckEvidence>,
}

#[derive(Serialize)]
struct WhitespaceCheckEvidence {
    base_tree: String,
    candidate_tree: String,
    clean: bool,
    stdout: ArtifactLogEvidence,
    stderr: ArtifactLogEvidence,
}

#[derive(Serialize)]
struct CommandLogEvidence {
    role: &'static str,
    executable: String,
    argv: Vec<String>,
    working_directory: String,
    comparison_revision: String,
    terminal: &'static str,
    exit_status: Option<i32>,
    signal: Option<i32>,
    elapsed_millis: u64,
    matched_expectation: bool,
    stdout: ArtifactLogEvidence,
    stderr: ArtifactLogEvidence,
}

#[derive(Serialize)]
struct ArtifactLogEvidence {
    artifact_id: i64,
    digest: String,
    byte_length: u64,
}

fn artifact_log(reference: SealedArtifactReferenceV2) -> ArtifactLogEvidence {
    ArtifactLogEvidence {
        artifact_id: reference.artifact_id.get(),
        digest: reference.digest.to_hex(),
        byte_length: reference.byte_length,
    }
}

const fn validation_result_name(value: ValidationResult) -> &'static str {
    match value {
        ValidationResult::Passed => "passed",
        ValidationResult::Failed => "failed",
        ValidationResult::Interrupted => "interrupted",
    }
}

#[cfg(test)]
#[path = "candidate_runtime_tests.rs"]
mod tests;
