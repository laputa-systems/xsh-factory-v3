//! Quality, Architect, and local-delivery authority.
//!
//! This module owns the final five purpose-specific Factory relations. It is
//! intentionally a direct transactional domain surface: candidate identity is
//! immutable, deterministic validation is non-overridable, Quality prose is
//! durable but qualitative, and only a linked Architect decision can make a
//! validated candidate eligible for the guarded Git delivery performed by the
//! sibling Git-custody module.

use factory_protocol::{
    AggregateRevision, ArchitectDecisionKindV1, ArchitectDecisionReceiptV1,
    CandidateDecisionRequestV1, CandidateDecisionV1, CandidateId, CandidateState,
    CandidateSubmissionV1, ContentDigest, ExpectedRevision, KernelBuildId,
    QualityReviewSubmissionV1, ReleaseDecisionV1, RepositoryObjectIdV1, ReviewId, ReviewVerdict,
    SealedArtifactReferenceV1, SessionId, SponsorshipDecisionV1, TicketAttemptId, TicketRevisionId,
    TicketState, ValidationId, ValidationState,
};
use sqlx::{PgPool, Postgres, Row, Transaction};
use thiserror::Error;

use crate::{storage::KernelStore, ticket_store::CurrentHeadRequalification};

const TICKET_PROPOSED: i16 = 0;
const TICKET_SPONSORED: i16 = 1;
const TICKET_IN_FLIGHT: i16 = 2;
const TICKET_DELIVERED: i16 = 3;
const TICKET_BLOCKED: i16 = 4;
const TICKET_RESOLVED: i16 = 5;
const TICKET_REJECTED: i16 = 7;

const ATTEMPT_ENGINEERING: i16 = 0;
const ATTEMPT_HARD_VALIDATION: i16 = 1;
const ATTEMPT_QUALITY: i16 = 2;
const ATTEMPT_AWAITING_ARCHITECT: i16 = 3;
const ATTEMPT_REWORK_ENGINEERING: i16 = 4;
const ATTEMPT_REWORK_VALIDATION: i16 = 5;
const ATTEMPT_REWORK_QUALITY: i16 = 6;
const ATTEMPT_DELIVERED: i16 = 7;
const ATTEMPT_FAILED: i16 = 8;
const ATTEMPT_CANCELLED: i16 = 9;

const CANDIDATE_SUBMITTED: i16 = 0;
const CANDIDATE_VALIDATED: i16 = 1;
const CANDIDATE_REJECTED: i16 = 2;
const CANDIDATE_ACCEPTED: i16 = 3;
const CANDIDATE_DELIVERED: i16 = 4;

const VALIDATION_PASSED: i16 = 1;
const VALIDATION_FAILED: i16 = 2;
const VALIDATION_INTERRUPTED: i16 = 3;

const REVIEW_ACCEPT: i16 = 0;
const REVIEW_REJECT: i16 = 1;

const DECISION_SPONSOR: i16 = 0;
const DECISION_RELEASE: i16 = 1;
const DECISION_DELIVER: i16 = 2;
const DECISION_REWORK: i16 = 3;
const DECISION_REJECT: i16 = 4;

const SESSION_RUNNING: i16 = 1;
const SESSION_SUCCEEDED: i16 = 2;
const OFFICE_ENGINEERING: i16 = 1;
const OFFICE_QUALITY: i16 = 2;
const CAMPAIGN_RUNNING: i16 = 0;
const CAMPAIGN_COMPLETED: i16 = 1;

const REQUALIFICATION_REPRODUCED: i16 = 0;
const REQUALIFICATION_RESOLVED: i16 = 1;
const REQUALIFICATION_DIVERGED: i16 = 2;

const CANDIDATE_SUBMIT_OPERATION: &str = "candidate.submit";
const VALIDATION_RECORD_OPERATION: &str = "validation.record";
const CANDIDATE_COMMIT_ATTACH_OPERATION: &str = "candidate.commit.attach";
const REVIEW_SUBMIT_OPERATION: &str = "quality.review.submit";
const SPONSOR_OPERATION: &str = "architect.ticket.sponsor";
const RELEASE_OPERATION: &str = "architect.ticket.release";
const CANDIDATE_DECIDE_OPERATION: &str = "architect.candidate.decide";
const DELIVERY_RECORD_OPERATION: &str = "delivery.record";

const CANDIDATE_SUBJECT: i16 = 40;
const VALIDATION_SUBJECT: i16 = 41;
const REVIEW_SUBJECT: i16 = 42;
const DECISION_SUBJECT: i16 = 43;
const DELIVERY_SUBJECT: i16 = 44;

/// The two deterministic validation positions are fixed by the MVP product
/// circuit. This is not an application-defined workflow enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidationScope {
    HardCandidate,
    QualityFullSuite,
}

impl ValidationScope {
    const fn code(self) -> i16 {
        match self {
            Self::HardCandidate => 0,
            Self::QualityFullSuite => 1,
        }
    }

    const fn expected_office(self) -> i16 {
        match self {
            Self::HardCandidate => OFFICE_ENGINEERING,
            Self::QualityFullSuite => OFFICE_QUALITY,
        }
    }
}

/// Terminal result observed by the trusted command runner. `Running` is not
/// accepted because the process boundary has no durable resume semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidationResult {
    Passed,
    Failed,
    Interrupted,
}

impl ValidationResult {
    const fn code(self) -> i16 {
        match self {
            Self::Passed => VALIDATION_PASSED,
            Self::Failed => VALIDATION_FAILED,
            Self::Interrupted => VALIDATION_INTERRUPTED,
        }
    }

    const fn state(self) -> ValidationState {
        match self {
            Self::Passed => ValidationState::Passed,
            Self::Failed => ValidationState::Failed,
            Self::Interrupted => ValidationState::Interrupted,
        }
    }
}

/// Captured evidence plus the actor's bounded candidate submission. Git tree
/// identities come from the kernel-owned temporary-index capture, never from
/// the Engineering terminal payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmitCandidate {
    pub principal: String,
    pub command_id: String,
    pub ticket_attempt_id: TicketAttemptId,
    pub expected_attempt_revision: ExpectedRevision,
    pub expected_ticket_revision: ExpectedRevision,
    pub engineering_session_id: SessionId,
    pub base_commit: RepositoryObjectIdV1,
    pub base_tree: RepositoryObjectIdV1,
    pub regression_tree: RepositoryObjectIdV1,
    pub candidate_tree: RepositoryObjectIdV1,
    pub changed_paths: SealedArtifactReferenceV1,
    /// Kernel-captured regression-checkpoint evidence.  These identities are
    /// accepted only from the opaque checkpoint capability, never actor wire.
    pub regression_patch: SealedArtifactReferenceV1,
    pub regression_command_set: SealedArtifactReferenceV1,
    pub regression_log: SealedArtifactReferenceV1,
    pub candidate_patch: SealedArtifactReferenceV1,
    pub submission: CandidateSubmissionV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateReceipt {
    pub candidate_id: CandidateId,
    pub state: CandidateState,
    pub resulting_revision: AggregateRevision,
    pub audit_log_id: i64,
    pub was_idempotent_retry: bool,
}

/// Result from the command-supervision boundary. The command set and output
/// are sealed before this command is admitted, so PostgreSQL never receives
/// an unbounded stream or command transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordValidation {
    pub principal: String,
    pub command_id: String,
    pub candidate_id: CandidateId,
    pub expected_candidate_revision: ExpectedRevision,
    pub expected_attempt_revision: ExpectedRevision,
    pub scope: ValidationScope,
    pub kernel_build_id: KernelBuildId,
    pub performed_by_session_id: SessionId,
    pub validation_profile: String,
    pub pristine_tree: RepositoryObjectIdV1,
    pub command_set: SealedArtifactReferenceV1,
    pub result: ValidationResult,
    pub duration_millis: u64,
    pub log: SealedArtifactReferenceV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationReceipt {
    pub validation_id: ValidationId,
    pub candidate_id: CandidateId,
    pub state: ValidationState,
    pub candidate_state: CandidateState,
    pub resulting_candidate_revision: AggregateRevision,
    pub resulting_attempt_revision: AggregateRevision,
    pub audit_log_id: i64,
    pub was_idempotent_retry: bool,
}

/// The Git-custody module constructs the physical commit and ref before this
/// idempotent persistence step. A database rejection leaves only an
/// unreferenced local candidate ref, never a false delivery record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachCandidateCommit {
    pub principal: String,
    pub command_id: String,
    pub candidate_id: CandidateId,
    pub expected_candidate_revision: ExpectedRevision,
    pub candidate_commit: RepositoryObjectIdV1,
    pub candidate_ref: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmitQualityReview {
    pub principal: String,
    pub command_id: String,
    pub candidate_id: CandidateId,
    pub expected_candidate_revision: ExpectedRevision,
    pub expected_attempt_revision: ExpectedRevision,
    pub quality_session_id: SessionId,
    pub submission: QualityReviewSubmissionV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewReceipt {
    pub review_id: ReviewId,
    pub candidate_id: CandidateId,
    pub verdict: ReviewVerdict,
    pub resulting_candidate_revision: AggregateRevision,
    pub resulting_attempt_revision: AggregateRevision,
    pub audit_log_id: i64,
    pub was_idempotent_retry: bool,
}

/// A T8 sponsorship path replaces the earlier direct lifecycle mutation: the
/// ticket transition and immutable Architect decision are one transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SponsorTicket {
    pub command_id: String,
    pub expected_ticket_revision: ExpectedRevision,
    pub decision: SponsorshipDecisionV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SponsorshipReceipt {
    pub decision: ArchitectDecisionReceiptV1,
    pub ticket_revision_id: TicketRevisionId,
    pub resulting_ticket_revision: AggregateRevision,
    pub audit_log_id: i64,
    pub was_idempotent_retry: bool,
}

/// A release repeats current-head reproduction supplied by the trusted
/// command runner. It cannot convert a failed attempt into an automatic retry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseTicketAttempt {
    pub command_id: String,
    pub expected_attempt_revision: ExpectedRevision,
    pub expected_ticket_revision: ExpectedRevision,
    pub decision: ReleaseDecisionV1,
    pub requalification: CurrentHeadRequalification,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseOutcome {
    Released,
    Resolved,
    Blocked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseReceipt {
    pub decision: ArchitectDecisionReceiptV1,
    pub ticket_attempt_id: TicketAttemptId,
    pub outcome: ReleaseOutcome,
    pub resulting_attempt_revision: AggregateRevision,
    pub resulting_ticket_revision: AggregateRevision,
    pub audit_log_id: i64,
    pub was_idempotent_retry: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecideCandidate {
    pub command_id: String,
    pub expected_candidate_revision: ExpectedRevision,
    pub expected_attempt_revision: ExpectedRevision,
    pub expected_ticket_revision: ExpectedRevision,
    pub request: CandidateDecisionRequestV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateDecisionReceipt {
    pub decision: ArchitectDecisionReceiptV1,
    pub candidate_id: CandidateId,
    pub candidate_state: CandidateState,
    pub resulting_candidate_revision: AggregateRevision,
    pub resulting_attempt_revision: AggregateRevision,
    pub resulting_ticket_revision: AggregateRevision,
    pub audit_log_id: i64,
    pub was_idempotent_retry: bool,
}

/// Receipt from `GitCustody::guarded_local_fast_forward`. This store verifies
/// it remains the candidate's exact base/commit/tree before turning it into a
/// terminal ticket and, where needed, campaign completion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordDelivery {
    pub principal: String,
    pub command_id: String,
    pub candidate_id: CandidateId,
    pub expected_candidate_revision: ExpectedRevision,
    pub expected_attempt_revision: ExpectedRevision,
    pub expected_ticket_revision: ExpectedRevision,
    pub expected_campaign_revision: ExpectedRevision,
    pub expected_old_commit: RepositoryObjectIdV1,
    pub resulting_commit: RepositoryObjectIdV1,
    pub resulting_tree: RepositoryObjectIdV1,
    pub receipt: SealedArtifactReferenceV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryReceipt {
    pub delivery_id: factory_protocol::DeliveryId,
    pub candidate_id: CandidateId,
    pub resulting_candidate_revision: AggregateRevision,
    pub resulting_attempt_revision: AggregateRevision,
    pub resulting_ticket_revision: AggregateRevision,
    pub resulting_campaign_revision: AggregateRevision,
    pub campaign_completed: bool,
    pub audit_log_id: i64,
    pub was_idempotent_retry: bool,
}

/// The narrow PostgreSQL owner for the Quality/Architect tail of a ticket
/// attempt. It reuses the kernel's fixed pool and never exposes it outside
/// the crate.
#[derive(Clone, Debug)]
pub struct DecisionStore {
    pool: PgPool,
}

impl KernelStore {
    #[must_use]
    pub fn decision_store(&self) -> DecisionStore {
        DecisionStore {
            pool: self.pool_for_authority(),
        }
    }
}

impl DecisionStore {
    pub async fn submit_candidate(
        &self,
        command: &SubmitCandidate,
    ) -> Result<CandidateReceipt, DecisionStoreError> {
        validate_command(&command.principal, &command.command_id)?;
        command.submission.validate()?;
        command
            .changed_paths
            .validate("candidate changed paths", 256 * 1024, false)?;
        command
            .regression_patch
            .validate("regression binary patch", 16 * 1024 * 1024, false)?;
        command.regression_command_set.validate(
            "regression checkpoint command set",
            256 * 1024,
            false,
        )?;
        command
            .regression_log
            .validate("regression checkpoint log", 16 * 1024 * 1024, true)?;
        command
            .candidate_patch
            .validate("candidate binary patch", 16 * 1024 * 1024, false)?;
        let fingerprint = submit_candidate_fingerprint(command);
        let mut tx = self.pool.begin().await?;
        lock_attempt(&mut tx, command.ticket_attempt_id.get()).await?;
        if let Some(receipt) = find_audit(
            &mut tx,
            &command.principal,
            &command.command_id,
            CANDIDATE_SUBMIT_OPERATION,
            fingerprint,
        )
        .await?
        {
            require_subject(&receipt, CANDIDATE_SUBJECT)?;
            let candidate = load_candidate(&mut tx, CandidateId::new(receipt.subject_id)?).await?;
            tx.commit().await?;
            return Ok(CandidateReceipt {
                candidate_id: candidate.id,
                state: candidate_state(candidate.lifecycle)?,
                resulting_revision: receipt.resulting_revision,
                audit_log_id: receipt.audit_log_id,
                was_idempotent_retry: true,
            });
        }
        let attempt = lock_candidate_attempt(&mut tx, command.ticket_attempt_id).await?;
        require_revision(command.expected_attempt_revision, attempt.attempt_revision)?;
        require_revision(command.expected_ticket_revision, attempt.ticket_revision)?;
        require_ticket_state(TicketState::InFlight, attempt.ticket_state)?;
        if !matches!(
            attempt.stage,
            ATTEMPT_ENGINEERING | ATTEMPT_REWORK_ENGINEERING
        ) {
            return Err(DecisionStoreError::AttemptStageConflict {
                required: "Engineering or ReworkEngineering",
                observed: attempt.stage,
            });
        }
        if command.base_commit.as_str() != attempt.claimed_commit
            || command.base_tree.as_str() != attempt.claimed_tree
        {
            return Err(DecisionStoreError::CandidateBaseChanged);
        }
        let build_id = attempt.kernel_build_database_id;
        require_artifact(&mut tx, &command.changed_paths, build_id).await?;
        require_artifact(&mut tx, &command.regression_patch, build_id).await?;
        require_artifact(&mut tx, &command.regression_command_set, build_id).await?;
        require_artifact(&mut tx, &command.regression_log, build_id).await?;
        require_artifact(&mut tx, &command.candidate_patch, build_id).await?;
        require_artifact(&mut tx, &command.submission.engineering_report, build_id).await?;
        require_artifact(&mut tx, &command.submission.risks, build_id).await?;
        require_session(
            &mut tx,
            command.engineering_session_id,
            attempt.campaign_id,
            OFFICE_ENGINEERING,
        )
        .await?;
        let candidate_ordinal = attempt
            .candidate_ordinal
            .checked_add(1)
            .ok_or(DecisionStoreError::IntegerOutOfRange)?;
        let next_attempt = attempt.attempt_revision.next()?;
        let row = sqlx::query(
            "INSERT INTO factory.candidates (
                 ticket_attempt_id, base_commit, base_tree, regression_tree, candidate_tree,
                 changed_paths_artifact_id, regression_patch_artifact_id,
                 regression_command_set_artifact_id, regression_log_artifact_id,
                 patch_artifact_id, engineering_session_id,
                 engineering_report_artifact_id, commit_subject, commit_body,
                 regression_test_identity, risks_artifact_id, lifecycle, revision
             ) VALUES (
                 $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                 $13, $14, $15, $16, 0, 0
             )
             RETURNING id",
        )
        .bind(command.ticket_attempt_id.get())
        .bind(command.base_commit.as_str())
        .bind(command.base_tree.as_str())
        .bind(command.regression_tree.as_str())
        .bind(command.candidate_tree.as_str())
        .bind(command.changed_paths.artifact_id.get())
        .bind(command.regression_patch.artifact_id.get())
        .bind(command.regression_command_set.artifact_id.get())
        .bind(command.regression_log.artifact_id.get())
        .bind(command.candidate_patch.artifact_id.get())
        .bind(command.engineering_session_id.get())
        .bind(command.submission.engineering_report.artifact_id.get())
        .bind(&command.submission.commit_subject)
        .bind(&command.submission.commit_body)
        .bind(&command.submission.regression_test_identity)
        .bind(command.submission.risks.artifact_id.get())
        .bind(CANDIDATE_SUBMITTED)
        .fetch_one(&mut *tx)
        .await?;
        let candidate_id = CandidateId::new(row.try_get::<i64, _>("id")?)?;
        // Candidate submission is deliberately its own durable boundary.  A
        // daemon crash before hard validation must be visible to the
        // scheduler as a kernel-owned recovery action, not as Engineering
        // work that an actor could submit again.
        let validation_stage = if attempt.stage == ATTEMPT_ENGINEERING {
            ATTEMPT_HARD_VALIDATION
        } else {
            ATTEMPT_REWORK_VALIDATION
        };
        sqlx::query(
            "UPDATE factory.ticket_attempts
             SET candidate_ordinal = $1, stage = $2, revision = $3 WHERE id = $4",
        )
        .bind(candidate_ordinal)
        .bind(validation_stage)
        .bind(revision_sql(next_attempt)?)
        .bind(command.ticket_attempt_id.get())
        .execute(&mut *tx)
        .await?;
        let audit_log_id = insert_audit(
            &mut tx,
            &command.principal,
            &command.command_id,
            CANDIDATE_SUBMIT_OPERATION,
            fingerprint,
            CANDIDATE_SUBJECT,
            candidate_id.get(),
            AggregateRevision::initial(),
        )
        .await?;
        tx.commit().await?;
        Ok(CandidateReceipt {
            candidate_id,
            state: CandidateState::Submitted,
            resulting_revision: AggregateRevision::initial(),
            audit_log_id,
            was_idempotent_retry: false,
        })
    }

    pub async fn record_validation(
        &self,
        command: &RecordValidation,
    ) -> Result<ValidationReceipt, DecisionStoreError> {
        validate_command(&command.principal, &command.command_id)?;
        validate_validation_command(command)?;
        let fingerprint = validation_fingerprint(command);
        let mut tx = self.pool.begin().await?;
        lock_candidate(&mut tx, command.candidate_id.get()).await?;
        if let Some(receipt) = find_audit(
            &mut tx,
            &command.principal,
            &command.command_id,
            VALIDATION_RECORD_OPERATION,
            fingerprint,
        )
        .await?
        {
            require_subject(&receipt, VALIDATION_SUBJECT)?;
            let validation_id = ValidationId::new(receipt.subject_id)?;
            let validation = load_validation(&mut tx, validation_id).await?;
            let candidate = load_candidate(&mut tx, validation.candidate_id).await?;
            let attempt = lock_candidate_attempt(&mut tx, candidate.ticket_attempt_id).await?;
            tx.commit().await?;
            return Ok(ValidationReceipt {
                validation_id,
                candidate_id: candidate.id,
                state: validation_state(validation.lifecycle)?,
                candidate_state: candidate_state(candidate.lifecycle)?,
                resulting_candidate_revision: candidate.revision,
                resulting_attempt_revision: attempt.attempt_revision,
                audit_log_id: receipt.audit_log_id,
                was_idempotent_retry: true,
            });
        }
        let candidate = load_candidate_for_update(&mut tx, command.candidate_id).await?;
        require_revision(command.expected_candidate_revision, candidate.revision)?;
        let attempt = lock_candidate_attempt(&mut tx, candidate.ticket_attempt_id).await?;
        require_revision(command.expected_attempt_revision, attempt.attempt_revision)?;
        require_ticket_state(TicketState::InFlight, attempt.ticket_state)?;
        let required_candidate_state = match command.scope {
            ValidationScope::HardCandidate => CandidateState::Submitted,
            ValidationScope::QualityFullSuite => CandidateState::Validated,
        };
        require_candidate_state(
            required_candidate_state,
            candidate_state(candidate.lifecycle)?,
        )?;
        let required_stage = match command.scope {
            ValidationScope::HardCandidate => {
                if attempt.stage == ATTEMPT_HARD_VALIDATION {
                    ATTEMPT_HARD_VALIDATION
                } else {
                    ATTEMPT_REWORK_VALIDATION
                }
            }
            ValidationScope::QualityFullSuite => {
                if attempt.stage == ATTEMPT_QUALITY {
                    ATTEMPT_QUALITY
                } else {
                    ATTEMPT_REWORK_QUALITY
                }
            }
        };
        if attempt.stage != required_stage {
            return Err(DecisionStoreError::AttemptStageConflict {
                required: match command.scope {
                    ValidationScope::HardCandidate => "HardValidation or ReworkValidation",
                    ValidationScope::QualityFullSuite => "Quality or ReworkQuality",
                },
                observed: attempt.stage,
            });
        }
        if command.pristine_tree.as_str() != candidate.candidate_tree {
            return Err(DecisionStoreError::ValidationTreeChanged);
        }
        let build_database_id = require_kernel_build(
            &mut tx,
            command.kernel_build_id,
            attempt.kernel_build_database_id,
        )
        .await?;
        require_session(
            &mut tx,
            command.performed_by_session_id,
            attempt.campaign_id,
            command.scope.expected_office(),
        )
        .await?;
        require_artifact(&mut tx, &command.command_set, build_database_id).await?;
        require_artifact(&mut tx, &command.log, build_database_id).await?;
        if validation_exists(&mut tx, command.candidate_id, command.scope).await? {
            return Err(DecisionStoreError::ValidationAlreadyRecorded {
                candidate_id: command.candidate_id,
                scope: command.scope,
            });
        }
        let row = sqlx::query(
            "INSERT INTO factory.validations (
                candidate_id, kernel_build_id, performed_by_session_id, validation_scope,
                validation_profile, pristine_tree, command_set_artifact_id, lifecycle,
                duration_millis, log_artifact_id
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
        )
        .bind(command.candidate_id.get())
        .bind(build_database_id)
        .bind(command.performed_by_session_id.get())
        .bind(command.scope.code())
        .bind(&command.validation_profile)
        .bind(command.pristine_tree.as_str())
        .bind(command.command_set.artifact_id.get())
        .bind(command.result.code())
        .bind(
            i64::try_from(command.duration_millis)
                .map_err(|_| DecisionStoreError::IntegerOutOfRange)?,
        )
        .bind(command.log.artifact_id.get())
        .fetch_one(&mut *tx)
        .await?;
        let validation_id = ValidationId::new(row.try_get::<i64, _>("id")?)?;
        let (next_candidate_state, next_attempt_stage) =
            if command.result == ValidationResult::Passed {
                match command.scope {
                    ValidationScope::HardCandidate => (
                        CANDIDATE_VALIDATED,
                        if attempt.stage == ATTEMPT_HARD_VALIDATION {
                            ATTEMPT_QUALITY
                        } else {
                            ATTEMPT_REWORK_QUALITY
                        },
                    ),
                    ValidationScope::QualityFullSuite => {
                        (CANDIDATE_VALIDATED, ATTEMPT_AWAITING_ARCHITECT)
                    }
                }
            } else {
                (CANDIDATE_REJECTED, ATTEMPT_FAILED)
            };
        let next_candidate = candidate.revision.next()?;
        let next_attempt = attempt.attempt_revision.next()?;
        sqlx::query("UPDATE factory.candidates SET lifecycle = $1, revision = $2 WHERE id = $3")
            .bind(next_candidate_state)
            .bind(revision_sql(next_candidate)?)
            .bind(command.candidate_id.get())
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE factory.ticket_attempts SET stage = $1, revision = $2, failed_at = CASE WHEN $1 = $3 THEN CURRENT_TIMESTAMP ELSE failed_at END, failure_reason = CASE WHEN $1 = $3 THEN 'hard validation did not pass' ELSE failure_reason END WHERE id = $4")
            .bind(next_attempt_stage)
            .bind(revision_sql(next_attempt)?)
            .bind(ATTEMPT_FAILED)
            .bind(attempt.id.get())
            .execute(&mut *tx)
            .await?;
        let audit_log_id = insert_audit(
            &mut tx,
            &command.principal,
            &command.command_id,
            VALIDATION_RECORD_OPERATION,
            fingerprint,
            VALIDATION_SUBJECT,
            validation_id.get(),
            next_candidate,
        )
        .await?;
        tx.commit().await?;
        Ok(ValidationReceipt {
            validation_id,
            candidate_id: command.candidate_id,
            state: command.result.state(),
            candidate_state: candidate_state(next_candidate_state)?,
            resulting_candidate_revision: next_candidate,
            resulting_attempt_revision: next_attempt,
            audit_log_id,
            was_idempotent_retry: false,
        })
    }

    pub async fn attach_candidate_commit(
        &self,
        command: &AttachCandidateCommit,
    ) -> Result<CandidateReceipt, DecisionStoreError> {
        validate_command(&command.principal, &command.command_id)?;
        validate_candidate_ref(&command.candidate_ref)?;
        let fingerprint = commit_fingerprint(command);
        let mut tx = self.pool.begin().await?;
        lock_candidate(&mut tx, command.candidate_id.get()).await?;
        if let Some(receipt) = find_audit(
            &mut tx,
            &command.principal,
            &command.command_id,
            CANDIDATE_COMMIT_ATTACH_OPERATION,
            fingerprint,
        )
        .await?
        {
            require_subject(&receipt, CANDIDATE_SUBJECT)?;
            let candidate = load_candidate(&mut tx, CandidateId::new(receipt.subject_id)?).await?;
            tx.commit().await?;
            return Ok(CandidateReceipt {
                candidate_id: candidate.id,
                state: candidate_state(candidate.lifecycle)?,
                resulting_revision: candidate.revision,
                audit_log_id: receipt.audit_log_id,
                was_idempotent_retry: true,
            });
        }
        let candidate = load_candidate_for_update(&mut tx, command.candidate_id).await?;
        require_revision(command.expected_candidate_revision, candidate.revision)?;
        require_candidate_state(
            CandidateState::Validated,
            candidate_state(candidate.lifecycle)?,
        )?;
        if candidate.candidate_commit.is_some() {
            return Err(DecisionStoreError::CandidateCommitAlreadyAttached);
        }
        if !validation_passed(
            &mut tx,
            command.candidate_id,
            ValidationScope::HardCandidate,
        )
        .await?
        {
            return Err(DecisionStoreError::HardValidationMissing);
        }
        let attempt = lock_candidate_attempt(&mut tx, candidate.ticket_attempt_id).await?;
        // The Git custody ref is not merely shaped like a Factory ref: it
        // names this exact durable ticket/candidate pair, preventing a
        // qualified commit from being attached under another aggregate's ref.
        if command.candidate_ref
            != format!(
                "refs/heads/factory/{}/{}",
                attempt.ticket_id,
                command.candidate_id.get()
            )
        {
            return Err(DecisionStoreError::InvalidCandidateRef);
        }
        let next = candidate.revision.next()?;
        sqlx::query(
            "UPDATE factory.candidates SET candidate_commit = $1, candidate_ref = $2, revision = $3
             WHERE id = $4",
        )
        .bind(command.candidate_commit.as_str())
        .bind(&command.candidate_ref)
        .bind(revision_sql(next)?)
        .bind(command.candidate_id.get())
        .execute(&mut *tx)
        .await?;
        let audit_log_id = insert_audit(
            &mut tx,
            &command.principal,
            &command.command_id,
            CANDIDATE_COMMIT_ATTACH_OPERATION,
            fingerprint,
            CANDIDATE_SUBJECT,
            command.candidate_id.get(),
            next,
        )
        .await?;
        tx.commit().await?;
        Ok(CandidateReceipt {
            candidate_id: command.candidate_id,
            state: CandidateState::Validated,
            resulting_revision: next,
            audit_log_id,
            was_idempotent_retry: false,
        })
    }

    pub async fn submit_quality_review(
        &self,
        command: &SubmitQualityReview,
    ) -> Result<ReviewReceipt, DecisionStoreError> {
        validate_command(&command.principal, &command.command_id)?;
        command.submission.validate()?;
        let fingerprint = review_fingerprint(command);
        let mut tx = self.pool.begin().await?;
        lock_candidate(&mut tx, command.candidate_id.get()).await?;
        if let Some(receipt) = find_audit(
            &mut tx,
            &command.principal,
            &command.command_id,
            REVIEW_SUBMIT_OPERATION,
            fingerprint,
        )
        .await?
        {
            require_subject(&receipt, REVIEW_SUBJECT)?;
            let review = load_review(&mut tx, ReviewId::new(receipt.subject_id)?).await?;
            let candidate = load_candidate(&mut tx, review.candidate_id).await?;
            let attempt = lock_candidate_attempt(&mut tx, candidate.ticket_attempt_id).await?;
            tx.commit().await?;
            return Ok(ReviewReceipt {
                review_id: review.id,
                candidate_id: candidate.id,
                verdict: review_verdict(review.verdict)?,
                resulting_candidate_revision: candidate.revision,
                resulting_attempt_revision: attempt.attempt_revision,
                audit_log_id: receipt.audit_log_id,
                was_idempotent_retry: true,
            });
        }
        let candidate = load_candidate_for_update(&mut tx, command.candidate_id).await?;
        require_revision(command.expected_candidate_revision, candidate.revision)?;
        require_candidate_state(
            CandidateState::Validated,
            candidate_state(candidate.lifecycle)?,
        )?;
        if candidate.candidate_commit.is_none() {
            return Err(DecisionStoreError::CandidateCommitMissing);
        }
        let attempt = lock_candidate_attempt(&mut tx, candidate.ticket_attempt_id).await?;
        require_revision(command.expected_attempt_revision, attempt.attempt_revision)?;
        // The trusted full-suite command is nonterminal and advances the
        // attempt into the one Architect-awaiting state. Quality can submit
        // its sealed review only after that exact passed receipt exists; it
        // must not be admitted while the suite is still running.
        if attempt.stage != ATTEMPT_AWAITING_ARCHITECT {
            return Err(DecisionStoreError::AttemptStageConflict {
                required: "AwaitingArchitect after passed Quality full suite",
                observed: attempt.stage,
            });
        }
        let quality_validation = load_quality_validation(
            &mut tx,
            command.submission.full_suite_validation_id,
            command.candidate_id,
        )
        .await?;
        if quality_validation.lifecycle != VALIDATION_PASSED {
            return Err(DecisionStoreError::QualityValidationNotPassed);
        }
        if quality_validation.pristine_tree != candidate.candidate_tree {
            return Err(DecisionStoreError::ValidationTreeChanged);
        }
        if quality_validation.kernel_build_database_id != attempt.kernel_build_database_id {
            return Err(DecisionStoreError::ValidationBuildMismatch);
        }
        require_session(
            &mut tx,
            command.quality_session_id,
            attempt.campaign_id,
            OFFICE_QUALITY,
        )
        .await?;
        require_artifact(
            &mut tx,
            &command.submission.rationale,
            attempt.kernel_build_database_id,
        )
        .await?;
        require_artifact(
            &mut tx,
            &command.submission.risks,
            attempt.kernel_build_database_id,
        )
        .await?;
        require_artifact(
            &mut tx,
            &command.submission.additional_probes,
            attempt.kernel_build_database_id,
        )
        .await?;
        if review_exists(&mut tx, command.candidate_id).await? {
            return Err(DecisionStoreError::ReviewAlreadySubmitted {
                candidate_id: command.candidate_id,
            });
        }
        let verdict = review_verdict_code(command.submission.verdict);
        let row = sqlx::query(
            "INSERT INTO factory.reviews (
                 candidate_id, quality_session_id, full_suite_validation_id, verdict,
                 rationale_artifact_id, risks_artifact_id, additional_probes_artifact_id
             ) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
        )
        .bind(command.candidate_id.get())
        .bind(command.quality_session_id.get())
        .bind(command.submission.full_suite_validation_id.get())
        .bind(verdict)
        .bind(command.submission.rationale.artifact_id.get())
        .bind(command.submission.risks.artifact_id.get())
        .bind(command.submission.additional_probes.artifact_id.get())
        .fetch_one(&mut *tx)
        .await?;
        let review_id = ReviewId::new(row.try_get::<i64, _>("id")?)?;
        let next_candidate = candidate.revision.next()?;
        let next_attempt = attempt.attempt_revision.next()?;
        sqlx::query("UPDATE factory.candidates SET revision = $1 WHERE id = $2")
            .bind(revision_sql(next_candidate)?)
            .bind(command.candidate_id.get())
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE factory.ticket_attempts SET stage = $1, revision = $2 WHERE id = $3")
            .bind(ATTEMPT_AWAITING_ARCHITECT)
            .bind(revision_sql(next_attempt)?)
            .bind(attempt.id.get())
            .execute(&mut *tx)
            .await?;
        let audit_log_id = insert_audit(
            &mut tx,
            &command.principal,
            &command.command_id,
            REVIEW_SUBMIT_OPERATION,
            fingerprint,
            REVIEW_SUBJECT,
            review_id.get(),
            next_candidate,
        )
        .await?;
        tx.commit().await?;
        Ok(ReviewReceipt {
            review_id,
            candidate_id: command.candidate_id,
            verdict: command.submission.verdict,
            resulting_candidate_revision: next_candidate,
            resulting_attempt_revision: next_attempt,
            audit_log_id,
            was_idempotent_retry: false,
        })
    }

    pub async fn sponsor_ticket(
        &self,
        command: &SponsorTicket,
    ) -> Result<SponsorshipReceipt, DecisionStoreError> {
        command.decision.validate()?;
        validate_architect_command(command.decision.principal.as_str(), &command.command_id)?;
        let principal = command.decision.principal.as_str();
        let fingerprint = sponsorship_fingerprint(command);
        let mut tx = self.pool.begin().await?;
        lock_ticket_revision(&mut tx, command.decision.ticket_revision_id.get()).await?;
        if let Some(receipt) = find_audit(
            &mut tx,
            principal,
            &command.command_id,
            SPONSOR_OPERATION,
            fingerprint,
        )
        .await?
        {
            require_subject(&receipt, DECISION_SUBJECT)?;
            let decision = load_decision(&mut tx, receipt.subject_id).await?;
            let ticket_revision_id = TicketRevisionId::new(
                decision
                    .ticket_revision_id
                    .ok_or(DecisionStoreError::CorruptDecision)?,
            )?;
            let ticket = lock_ticket_revision_state(&mut tx, ticket_revision_id).await?;
            tx.commit().await?;
            return Ok(SponsorshipReceipt {
                decision: architect_receipt(receipt.subject_id, ArchitectDecisionKindV1::Sponsor)?,
                ticket_revision_id,
                resulting_ticket_revision: ticket.ticket_revision,
                audit_log_id: receipt.audit_log_id,
                was_idempotent_retry: true,
            });
        }
        let ticket =
            lock_ticket_revision_state(&mut tx, command.decision.ticket_revision_id).await?;
        require_revision(command.expected_ticket_revision, ticket.ticket_revision)?;
        require_ticket_state(TicketState::Proposed, ticket.ticket_state)?;
        require_artifact_unbound(&mut tx, &command.decision.rationale).await?;
        let decision_id = insert_decision(
            &mut tx,
            DECISION_SPONSOR,
            Some(command.decision.ticket_revision_id.get()),
            None,
            None,
            None,
            command.decision.rationale.artifact_id.get(),
            principal,
            false,
        )
        .await?;
        let next = ticket.ticket_revision.next()?;
        let reason = format!("architect decision {decision_id}");
        update_ticket_state_parts(
            &mut tx,
            ticket.ticket_id,
            ticket.ticket_revision_id,
            TICKET_SPONSORED,
            next,
            Some(&reason),
            None,
        )
        .await?;
        let audit_log_id = insert_audit(
            &mut tx,
            principal,
            &command.command_id,
            SPONSOR_OPERATION,
            fingerprint,
            DECISION_SUBJECT,
            decision_id,
            next,
        )
        .await?;
        tx.commit().await?;
        Ok(SponsorshipReceipt {
            decision: architect_receipt(decision_id, ArchitectDecisionKindV1::Sponsor)?,
            ticket_revision_id: command.decision.ticket_revision_id,
            resulting_ticket_revision: next,
            audit_log_id,
            was_idempotent_retry: false,
        })
    }

    pub async fn release_ticket_attempt(
        &self,
        command: &ReleaseTicketAttempt,
    ) -> Result<ReleaseReceipt, DecisionStoreError> {
        command.decision.validate()?;
        validate_architect_command(command.decision.principal.as_str(), &command.command_id)?;
        validate_requalification(&command.requalification)?;
        let principal = command.decision.principal.as_str();
        let fingerprint = release_fingerprint(command);
        let mut tx = self.pool.begin().await?;
        lock_attempt(&mut tx, command.decision.ticket_attempt_id.get()).await?;
        if let Some(receipt) = find_audit(
            &mut tx,
            principal,
            &command.command_id,
            RELEASE_OPERATION,
            fingerprint,
        )
        .await?
        {
            require_subject(&receipt, DECISION_SUBJECT)?;
            let decision = load_decision(&mut tx, receipt.subject_id).await?;
            let attempt_id = TicketAttemptId::new(
                decision
                    .ticket_attempt_id
                    .ok_or(DecisionStoreError::CorruptDecision)?,
            )?;
            let attempt = lock_candidate_attempt(&mut tx, attempt_id).await?;
            tx.commit().await?;
            return Ok(ReleaseReceipt {
                decision: architect_receipt(receipt.subject_id, ArchitectDecisionKindV1::Release)?,
                ticket_attempt_id: attempt_id,
                outcome: release_outcome(attempt.ticket_state)?,
                resulting_attempt_revision: attempt.attempt_revision,
                resulting_ticket_revision: attempt.ticket_revision,
                audit_log_id: receipt.audit_log_id,
                was_idempotent_retry: true,
            });
        }
        let attempt = lock_candidate_attempt(&mut tx, command.decision.ticket_attempt_id).await?;
        require_revision(command.expected_attempt_revision, attempt.attempt_revision)?;
        require_revision(command.expected_ticket_revision, attempt.ticket_revision)?;
        require_ticket_state(TicketState::InFlight, attempt.ticket_state)?;
        if attempt.released || !matches!(attempt.stage, ATTEMPT_FAILED | ATTEMPT_CANCELLED) {
            return Err(DecisionStoreError::AttemptNotReleasable);
        }
        require_artifact_unbound(&mut tx, &command.decision.rationale).await?;
        let classification =
            classify_requalification(&mut tx, &attempt, &command.requalification).await?;
        let (outcome, ticket_state, code, blocked_reason) = match classification {
            RequalificationClassification::Reproduced => (
                ReleaseOutcome::Released,
                TICKET_SPONSORED,
                REQUALIFICATION_REPRODUCED,
                None,
            ),
            RequalificationClassification::Resolved => (
                ReleaseOutcome::Resolved,
                TICKET_RESOLVED,
                REQUALIFICATION_RESOLVED,
                None,
            ),
            RequalificationClassification::Diverged => (
                ReleaseOutcome::Blocked,
                TICKET_BLOCKED,
                REQUALIFICATION_DIVERGED,
                Some("current-head reproducer differs from the sponsored failure"),
            ),
        };
        let decision_id = insert_decision(
            &mut tx,
            DECISION_RELEASE,
            None,
            Some(attempt.id.get()),
            None,
            None,
            command.decision.rationale.artifact_id.get(),
            principal,
            false,
        )
        .await?;
        let next_attempt = attempt.attempt_revision.next()?;
        let next_ticket = attempt.ticket_revision.next()?;
        sqlx::query("UPDATE factory.ticket_attempts SET released_at = CURRENT_TIMESTAMP, release_reason = $1, revision = $2 WHERE id = $3")
            .bind(format!("architect decision {decision_id}"))
            .bind(revision_sql(next_attempt)?)
            .bind(attempt.id.get())
            .execute(&mut *tx)
            .await?;
        update_ticket_requalification(
            &mut tx,
            &attempt,
            ticket_state,
            next_ticket,
            code,
            &command.requalification,
            blocked_reason,
        )
        .await?;
        let audit_log_id = insert_audit(
            &mut tx,
            principal,
            &command.command_id,
            RELEASE_OPERATION,
            fingerprint,
            DECISION_SUBJECT,
            decision_id,
            next_attempt,
        )
        .await?;
        tx.commit().await?;
        Ok(ReleaseReceipt {
            decision: architect_receipt(decision_id, ArchitectDecisionKindV1::Release)?,
            ticket_attempt_id: attempt.id,
            outcome,
            resulting_attempt_revision: next_attempt,
            resulting_ticket_revision: next_ticket,
            audit_log_id,
            was_idempotent_retry: false,
        })
    }

    pub async fn decide_candidate(
        &self,
        command: &DecideCandidate,
    ) -> Result<CandidateDecisionReceipt, DecisionStoreError> {
        command.request.validate()?;
        validate_architect_command(command.request.principal.as_str(), &command.command_id)?;
        let principal = command.request.principal.as_str();
        let fingerprint = candidate_decision_fingerprint(command);
        let mut tx = self.pool.begin().await?;
        lock_candidate(&mut tx, command.request.candidate_id.get()).await?;
        if let Some(receipt) = find_audit(
            &mut tx,
            principal,
            &command.command_id,
            CANDIDATE_DECIDE_OPERATION,
            fingerprint,
        )
        .await?
        {
            require_subject(&receipt, DECISION_SUBJECT)?;
            let decision = load_decision(&mut tx, receipt.subject_id).await?;
            let candidate_id = CandidateId::new(
                decision
                    .candidate_id
                    .ok_or(DecisionStoreError::CorruptDecision)?,
            )?;
            let candidate = load_candidate(&mut tx, candidate_id).await?;
            let attempt = lock_candidate_attempt(&mut tx, candidate.ticket_attempt_id).await?;
            tx.commit().await?;
            return Ok(CandidateDecisionReceipt {
                decision: architect_receipt(receipt.subject_id, command.request.decision.kind())?,
                candidate_id,
                candidate_state: candidate_state(candidate.lifecycle)?,
                resulting_candidate_revision: candidate.revision,
                resulting_attempt_revision: attempt.attempt_revision,
                resulting_ticket_revision: attempt.ticket_revision,
                audit_log_id: receipt.audit_log_id,
                was_idempotent_retry: true,
            });
        }
        let candidate = load_candidate_for_update(&mut tx, command.request.candidate_id).await?;
        require_revision(command.expected_candidate_revision, candidate.revision)?;
        require_candidate_state(
            CandidateState::Validated,
            candidate_state(candidate.lifecycle)?,
        )?;
        if candidate.candidate_commit.is_none() {
            return Err(DecisionStoreError::CandidateCommitMissing);
        }
        let attempt = lock_candidate_attempt(&mut tx, candidate.ticket_attempt_id).await?;
        require_revision(command.expected_attempt_revision, attempt.attempt_revision)?;
        require_revision(command.expected_ticket_revision, attempt.ticket_revision)?;
        require_ticket_state(TicketState::InFlight, attempt.ticket_state)?;
        if attempt.stage != ATTEMPT_AWAITING_ARCHITECT {
            return Err(DecisionStoreError::AttemptStageConflict {
                required: "AwaitingArchitect",
                observed: attempt.stage,
            });
        }
        require_hard_and_quality_validation(&mut tx, command.request.candidate_id).await?;
        let review = load_review(&mut tx, command.request.review_id).await?;
        if review.candidate_id != command.request.candidate_id {
            return Err(DecisionStoreError::ReviewCandidateMismatch);
        }
        require_artifact(
            &mut tx,
            &command.request.rationale,
            attempt.kernel_build_database_id,
        )
        .await?;
        let rejected_review = review.verdict == REVIEW_REJECT;
        let override_is_exact =
            command.request.quality_rejection_override == Some(command.request.review_id);
        if command.request.decision == CandidateDecisionV1::Deliver {
            if rejected_review != override_is_exact {
                return Err(DecisionStoreError::QualityRejectionOverrideRequired);
            }
        } else if command.request.quality_rejection_override.is_some() {
            return Err(DecisionStoreError::QualityRejectionOverrideForbidden);
        }
        if command.request.decision == CandidateDecisionV1::Rework && attempt.rework_ordinal != 0 {
            return Err(DecisionStoreError::ReworkLimitReached);
        }
        let decision_id = insert_decision(
            &mut tx,
            decision_code(command.request.decision),
            None,
            None,
            Some(command.request.candidate_id.get()),
            Some(command.request.review_id.get()),
            command.request.rationale.artifact_id.get(),
            principal,
            rejected_review,
        )
        .await?;
        let next_candidate = candidate.revision.next()?;
        let next_attempt = attempt.attempt_revision.next()?;
        let next_ticket = match command.request.decision {
            CandidateDecisionV1::Reject => attempt.ticket_revision.next()?,
            CandidateDecisionV1::Deliver | CandidateDecisionV1::Rework => attempt.ticket_revision,
        };
        let (candidate_lifecycle, attempt_stage) = match command.request.decision {
            CandidateDecisionV1::Deliver => (CANDIDATE_ACCEPTED, ATTEMPT_AWAITING_ARCHITECT),
            CandidateDecisionV1::Rework => (CANDIDATE_REJECTED, ATTEMPT_REWORK_ENGINEERING),
            CandidateDecisionV1::Reject => (CANDIDATE_REJECTED, ATTEMPT_FAILED),
        };
        sqlx::query("UPDATE factory.candidates SET lifecycle = $1, revision = $2 WHERE id = $3")
            .bind(candidate_lifecycle)
            .bind(revision_sql(next_candidate)?)
            .bind(command.request.candidate_id.get())
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE factory.ticket_attempts SET stage = $1, rework_ordinal = rework_ordinal + $2, failed_at = CASE WHEN $1 = $3 THEN CURRENT_TIMESTAMP ELSE failed_at END, failure_reason = CASE WHEN $1 = $3 THEN 'architect rejected reviewed candidate' ELSE failure_reason END, revision = $4 WHERE id = $5")
            .bind(attempt_stage)
            .bind(if command.request.decision == CandidateDecisionV1::Rework { 1 } else { 0 })
            .bind(ATTEMPT_FAILED)
            .bind(revision_sql(next_attempt)?)
            .bind(attempt.id.get())
            .execute(&mut *tx)
            .await?;
        if command.request.decision == CandidateDecisionV1::Reject {
            update_ticket_state(&mut tx, &attempt, TICKET_REJECTED, next_ticket, None, None)
                .await?;
        }
        let audit_log_id = insert_audit(
            &mut tx,
            principal,
            &command.command_id,
            CANDIDATE_DECIDE_OPERATION,
            fingerprint,
            DECISION_SUBJECT,
            decision_id,
            next_candidate,
        )
        .await?;
        tx.commit().await?;
        Ok(CandidateDecisionReceipt {
            decision: architect_receipt(decision_id, command.request.decision.kind())?,
            candidate_id: command.request.candidate_id,
            candidate_state: candidate_state(candidate_lifecycle)?,
            resulting_candidate_revision: next_candidate,
            resulting_attempt_revision: next_attempt,
            resulting_ticket_revision: next_ticket,
            audit_log_id,
            was_idempotent_retry: false,
        })
    }

    pub async fn record_delivery(
        &self,
        command: &RecordDelivery,
    ) -> Result<DeliveryReceipt, DecisionStoreError> {
        validate_command(&command.principal, &command.command_id)?;
        command
            .receipt
            .validate("delivery receipt", 16 * 1024 * 1024, false)?;
        let fingerprint = delivery_fingerprint(command);
        let mut tx = self.pool.begin().await?;
        lock_candidate(&mut tx, command.candidate_id.get()).await?;
        if let Some(receipt) = find_audit(
            &mut tx,
            &command.principal,
            &command.command_id,
            DELIVERY_RECORD_OPERATION,
            fingerprint,
        )
        .await?
        {
            require_subject(&receipt, DELIVERY_SUBJECT)?;
            let delivery = load_delivery(&mut tx, receipt.subject_id).await?;
            let candidate = load_candidate(&mut tx, delivery.candidate_id).await?;
            let attempt = lock_candidate_attempt(&mut tx, candidate.ticket_attempt_id).await?;
            let campaign = load_campaign(&mut tx, attempt.campaign_id).await?;
            tx.commit().await?;
            return Ok(DeliveryReceipt {
                delivery_id: factory_protocol::DeliveryId::new(receipt.subject_id)?,
                candidate_id: candidate.id,
                resulting_candidate_revision: candidate.revision,
                resulting_attempt_revision: attempt.attempt_revision,
                resulting_ticket_revision: attempt.ticket_revision,
                resulting_campaign_revision: campaign.revision,
                campaign_completed: campaign.lifecycle == CAMPAIGN_COMPLETED,
                audit_log_id: receipt.audit_log_id,
                was_idempotent_retry: true,
            });
        }
        let candidate = load_candidate_for_update(&mut tx, command.candidate_id).await?;
        require_revision(command.expected_candidate_revision, candidate.revision)?;
        require_candidate_state(
            CandidateState::Accepted,
            candidate_state(candidate.lifecycle)?,
        )?;
        let candidate_commit = candidate
            .candidate_commit
            .as_deref()
            .ok_or(DecisionStoreError::CandidateCommitMissing)?;
        if candidate_commit != command.resulting_commit.as_str()
            || candidate.base_commit != command.expected_old_commit.as_str()
            || candidate.candidate_tree != command.resulting_tree.as_str()
        {
            return Err(DecisionStoreError::DeliveryReceiptMismatch);
        }
        let attempt = lock_candidate_attempt(&mut tx, candidate.ticket_attempt_id).await?;
        require_revision(command.expected_attempt_revision, attempt.attempt_revision)?;
        require_revision(command.expected_ticket_revision, attempt.ticket_revision)?;
        require_ticket_state(TicketState::InFlight, attempt.ticket_state)?;
        if attempt.stage != ATTEMPT_AWAITING_ARCHITECT {
            return Err(DecisionStoreError::AttemptStageConflict {
                required: "AwaitingArchitect",
                observed: attempt.stage,
            });
        }
        if !deliver_decision_exists(&mut tx, command.candidate_id).await? {
            return Err(DecisionStoreError::ArchitectDeliveryDecisionMissing);
        }
        require_artifact(&mut tx, &command.receipt, attempt.kernel_build_database_id).await?;
        let campaign = lock_campaign(&mut tx, attempt.campaign_id).await?;
        require_revision(command.expected_campaign_revision, campaign.revision)?;
        if campaign.lifecycle != CAMPAIGN_RUNNING {
            return Err(DecisionStoreError::CampaignNotRunning);
        }
        if paid_session_active(&mut tx).await? {
            return Err(DecisionStoreError::PaidSessionStillRunning);
        }
        let before_delivered = delivered_attempt_count(&mut tx, attempt.campaign_id).await?;
        let completed = before_delivered
            .checked_add(1)
            .ok_or(DecisionStoreError::IntegerOutOfRange)?
            >= i64::from(campaign.delivery_target);
        let next_candidate = candidate.revision.next()?;
        let next_attempt = attempt.attempt_revision.next()?;
        let next_ticket = attempt.ticket_revision.next()?;
        let next_campaign = if completed {
            campaign.revision.next()?
        } else {
            campaign.revision
        };
        let row = sqlx::query(
            "INSERT INTO factory.deliveries (
                 candidate_id, candidate_commit, expected_old_commit, resulting_commit,
                 resulting_tree, method, lifecycle, recovery_status, receipt_artifact_id
             ) VALUES ($1, $2, $3, $4, $5, 0, 1, 0, $6) RETURNING id",
        )
        .bind(command.candidate_id.get())
        .bind(candidate_commit)
        .bind(command.expected_old_commit.as_str())
        .bind(command.resulting_commit.as_str())
        .bind(command.resulting_tree.as_str())
        .bind(command.receipt.artifact_id.get())
        .fetch_one(&mut *tx)
        .await?;
        let delivery_id = factory_protocol::DeliveryId::new(row.try_get::<i64, _>("id")?)?;
        sqlx::query("UPDATE factory.candidates SET lifecycle = $1, revision = $2 WHERE id = $3")
            .bind(CANDIDATE_DELIVERED)
            .bind(revision_sql(next_candidate)?)
            .bind(command.candidate_id.get())
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE factory.ticket_attempts SET stage = $1, revision = $2 WHERE id = $3")
            .bind(ATTEMPT_DELIVERED)
            .bind(revision_sql(next_attempt)?)
            .bind(attempt.id.get())
            .execute(&mut *tx)
            .await?;
        update_ticket_state(&mut tx, &attempt, TICKET_DELIVERED, next_ticket, None, None).await?;
        if completed {
            sqlx::query("UPDATE factory.campaigns SET lifecycle = $1, revision = $2 WHERE id = $3")
                .bind(CAMPAIGN_COMPLETED)
                .bind(revision_sql(next_campaign)?)
                .bind(attempt.campaign_id.get())
                .execute(&mut *tx)
                .await?;
        }
        let audit_log_id = insert_audit(
            &mut tx,
            &command.principal,
            &command.command_id,
            DELIVERY_RECORD_OPERATION,
            fingerprint,
            DELIVERY_SUBJECT,
            delivery_id.get(),
            next_candidate,
        )
        .await?;
        tx.commit().await?;
        Ok(DeliveryReceipt {
            delivery_id,
            candidate_id: command.candidate_id,
            resulting_candidate_revision: next_candidate,
            resulting_attempt_revision: next_attempt,
            resulting_ticket_revision: next_ticket,
            resulting_campaign_revision: next_campaign,
            campaign_completed: completed,
            audit_log_id,
            was_idempotent_retry: false,
        })
    }
}

#[derive(Debug, Error)]
pub enum DecisionStoreError {
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Contract(#[from] factory_protocol::ContractError),
    #[error("invalid command principal or command ID")]
    InvalidCommandIdentity,
    #[error("revision conflict: expected {expected:?}, current {current:?}")]
    RevisionConflict {
        expected: ExpectedRevision,
        current: AggregateRevision,
    },
    #[error("idempotency conflict for principal {principal:?} and command ID {command_id:?}")]
    IdempotencyConflict {
        principal: String,
        command_id: String,
    },
    #[error("unknown candidate {candidate_id}")]
    UnknownCandidate { candidate_id: CandidateId },
    #[error("unknown ticket attempt {ticket_attempt_id}")]
    UnknownTicketAttempt { ticket_attempt_id: TicketAttemptId },
    #[error("unknown validation {validation_id}")]
    UnknownValidation { validation_id: ValidationId },
    #[error("unknown review {review_id}")]
    UnknownReview { review_id: ReviewId },
    #[error("candidate is not in required {required:?} state (observed {observed:?})")]
    CandidateStateConflict {
        required: CandidateState,
        observed: CandidateState,
    },
    #[error("ticket is not in required {required:?} state (observed {observed:?})")]
    TicketStateConflict {
        required: TicketState,
        observed: TicketState,
    },
    #[error("ticket attempt stage requires {required}, observed code {observed}")]
    AttemptStageConflict {
        required: &'static str,
        observed: i16,
    },
    #[error("candidate base does not equal the claimed current-head snapshot")]
    CandidateBaseChanged,
    #[error("validation pristine tree does not equal the candidate tree")]
    ValidationTreeChanged,
    #[error("candidate already has this immutable validation scope")]
    ValidationAlreadyRecorded {
        candidate_id: CandidateId,
        scope: ValidationScope,
    },
    #[error("candidate already has an immutable Quality review")]
    ReviewAlreadySubmitted { candidate_id: CandidateId },
    #[error("candidate commit is missing")]
    CandidateCommitMissing,
    #[error("candidate commit is already attached")]
    CandidateCommitAlreadyAttached,
    #[error("passed hard validation is required")]
    HardValidationMissing,
    #[error("passed Quality full-suite validation is required")]
    QualityValidationNotPassed,
    #[error("Quality validation belongs to a different Quality session")]
    QualityValidationSessionMismatch,
    #[error("review does not belong to the exact candidate")]
    ReviewCandidateMismatch,
    #[error("a rejected Quality review requires an exact override link for delivery")]
    QualityRejectionOverrideRequired,
    #[error("a Quality rejection override is not legal for this decision")]
    QualityRejectionOverrideForbidden,
    #[error("the one allowed semantic rework has already been consumed")]
    ReworkLimitReached,
    #[error("attempt is not a failed or cancelled unreleased attempt")]
    AttemptNotReleasable,
    #[error("artifact custody does not match the sealed reference")]
    ArtifactReferenceMismatch,
    #[error("artifact was created by a build other than the attempt's pinned build")]
    ArtifactBuildMismatch,
    #[error("validation kernel build does not match the campaign's pinned build")]
    ValidationBuildMismatch,
    #[error("session does not belong to this campaign, office, or viable lifecycle")]
    SessionJurisdictionMismatch,
    #[error("candidate ref is not a bounded local factory ref")]
    InvalidCandidateRef,
    #[error("delivery receipt does not match the accepted candidate base/commit/tree")]
    DeliveryReceiptMismatch,
    #[error("an Architect delivery decision is required before local delivery")]
    ArchitectDeliveryDecisionMissing,
    #[error("campaign is no longer running")]
    CampaignNotRunning,
    #[error("a paid session is still running")]
    PaidSessionStillRunning,
    #[error("a stored closed-state value is corrupt")]
    CorruptState,
    #[error("a stored decision does not have its required target")]
    CorruptDecision,
    #[error("value cannot be represented by the durable integer type")]
    IntegerOutOfRange,
}

#[derive(Clone, Debug)]
struct CandidateRow {
    id: CandidateId,
    ticket_attempt_id: TicketAttemptId,
    base_commit: String,
    candidate_tree: String,
    lifecycle: i16,
    revision: AggregateRevision,
    candidate_commit: Option<String>,
}

#[derive(Clone, Debug)]
struct AttemptRow {
    id: TicketAttemptId,
    campaign_id: factory_protocol::CampaignId,
    kernel_build_database_id: i64,
    ticket_id: i64,
    ticket_revision_id: TicketRevisionId,
    ticket_state: TicketState,
    ticket_revision: AggregateRevision,
    attempt_revision: AggregateRevision,
    stage: i16,
    candidate_ordinal: i32,
    rework_ordinal: i32,
    released: bool,
    claimed_commit: String,
    claimed_tree: String,
    expected_observation_artifact_id: i64,
    discovery_observation_artifact_id: i64,
}

#[derive(Clone, Debug)]
struct TicketRevisionRow {
    ticket_id: i64,
    ticket_revision_id: TicketRevisionId,
    ticket_state: TicketState,
    ticket_revision: AggregateRevision,
}

#[derive(Clone, Debug)]
struct ValidationRow {
    candidate_id: CandidateId,
    performed_by_session_id: i64,
    lifecycle: i16,
    kernel_build_database_id: i64,
    pristine_tree: String,
}

#[derive(Clone, Debug)]
struct ReviewRow {
    id: ReviewId,
    candidate_id: CandidateId,
    verdict: i16,
}

#[derive(Clone, Debug)]
struct DecisionRow {
    ticket_revision_id: Option<i64>,
    ticket_attempt_id: Option<i64>,
    candidate_id: Option<i64>,
}

#[derive(Clone, Debug)]
struct DeliveryRow {
    candidate_id: CandidateId,
}

#[derive(Clone, Debug)]
struct CampaignRow {
    lifecycle: i16,
    revision: AggregateRevision,
    delivery_target: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequalificationClassification {
    Reproduced,
    Resolved,
    Diverged,
}

#[derive(Clone, Copy, Debug)]
struct AuditReceipt {
    audit_log_id: i64,
    subject_kind: i16,
    subject_id: i64,
    resulting_revision: AggregateRevision,
}

async fn lock_candidate(
    tx: &mut Transaction<'_, Postgres>,
    candidate_id: i64,
) -> Result<(), DecisionStoreError> {
    sqlx::query("SELECT id FROM factory.candidates WHERE id = $1 FOR UPDATE")
        .bind(candidate_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| DecisionStoreError::UnknownCandidate {
            candidate_id: CandidateId::new(candidate_id)
                .expect("database candidate IDs are positive by invariant"),
        })?;
    Ok(())
}

async fn lock_attempt(
    tx: &mut Transaction<'_, Postgres>,
    attempt_id: i64,
) -> Result<(), DecisionStoreError> {
    sqlx::query("SELECT id FROM factory.ticket_attempts WHERE id = $1 FOR UPDATE")
        .bind(attempt_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| DecisionStoreError::UnknownTicketAttempt {
            ticket_attempt_id: TicketAttemptId::new(attempt_id)
                .expect("database attempt IDs are positive by invariant"),
        })?;
    Ok(())
}

async fn lock_ticket_revision(
    tx: &mut Transaction<'_, Postgres>,
    ticket_revision_id: i64,
) -> Result<(), DecisionStoreError> {
    let row = sqlx::query("SELECT id FROM factory.ticket_revisions WHERE id = $1 FOR UPDATE")
        .bind(ticket_revision_id)
        .fetch_optional(&mut **tx)
        .await?;
    if row.is_none() {
        return Err(DecisionStoreError::CorruptState);
    }
    Ok(())
}

async fn load_candidate(
    tx: &mut Transaction<'_, Postgres>,
    candidate_id: CandidateId,
) -> Result<CandidateRow, DecisionStoreError> {
    candidate_from_row(
        sqlx::query(
            "SELECT id, ticket_attempt_id, base_commit, candidate_tree, lifecycle, revision,
                    candidate_commit
             FROM factory.candidates WHERE id = $1",
        )
        .bind(candidate_id.get())
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(DecisionStoreError::UnknownCandidate { candidate_id })?,
    )
}

async fn load_candidate_for_update(
    tx: &mut Transaction<'_, Postgres>,
    candidate_id: CandidateId,
) -> Result<CandidateRow, DecisionStoreError> {
    candidate_from_row(
        sqlx::query(
            "SELECT id, ticket_attempt_id, base_commit, candidate_tree, lifecycle, revision,
                    candidate_commit
             FROM factory.candidates WHERE id = $1 FOR UPDATE",
        )
        .bind(candidate_id.get())
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(DecisionStoreError::UnknownCandidate { candidate_id })?,
    )
}

fn candidate_from_row(row: sqlx::postgres::PgRow) -> Result<CandidateRow, DecisionStoreError> {
    Ok(CandidateRow {
        id: CandidateId::new(row.try_get("id")?)?,
        ticket_attempt_id: TicketAttemptId::new(row.try_get("ticket_attempt_id")?)?,
        base_commit: row.try_get("base_commit")?,
        candidate_tree: row.try_get("candidate_tree")?,
        lifecycle: row.try_get("lifecycle")?,
        revision: revision_from_sql(row.try_get("revision")?)?,
        candidate_commit: row.try_get("candidate_commit")?,
    })
}

async fn lock_candidate_attempt(
    tx: &mut Transaction<'_, Postgres>,
    attempt_id: TicketAttemptId,
) -> Result<AttemptRow, DecisionStoreError> {
    let row = sqlx::query(
        "SELECT ta.id, ta.campaign_id, c.kernel_build_id AS kernel_build_database_id,
                ta.ticket_revision_id, ta.stage, ta.candidate_ordinal, ta.rework_ordinal,
                ta.released_at IS NOT NULL AS released, ta.revision AS attempt_revision,
                ta.claimed_commit, ta.claimed_tree,
                tr.ticket_id, tr.lifecycle AS ticket_revision_lifecycle,
                tr.revision AS ticket_revision_aggregate_revision,
                tr.expected_observation_artifact_id, tr.discovery_observation_artifact_id,
                t.lifecycle AS ticket_lifecycle, t.revision AS ticket_aggregate_revision,
                t.current_ticket_revision_id
         FROM factory.ticket_attempts ta
         JOIN factory.campaigns c ON c.id = ta.campaign_id
         JOIN factory.ticket_revisions tr ON tr.id = ta.ticket_revision_id
         JOIN factory.tickets t ON t.id = tr.ticket_id
         WHERE ta.id = $1
         FOR UPDATE OF ta, tr, t, c",
    )
    .bind(attempt_id.get())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(DecisionStoreError::UnknownTicketAttempt {
        ticket_attempt_id: attempt_id,
    })?;
    let ticket_revision_id = TicketRevisionId::new(row.try_get("ticket_revision_id")?)?;
    let current_ticket_revision_id: i64 = row.try_get("current_ticket_revision_id")?;
    let observed_ticket_state = ticket_state(row.try_get("ticket_revision_lifecycle")?)?;
    if current_ticket_revision_id != ticket_revision_id.get()
        || observed_ticket_state != ticket_state(row.try_get("ticket_lifecycle")?)?
        || row.try_get::<i64, _>("ticket_revision_aggregate_revision")?
            != row.try_get::<i64, _>("ticket_aggregate_revision")?
    {
        return Err(DecisionStoreError::CorruptState);
    }
    Ok(AttemptRow {
        id: TicketAttemptId::new(row.try_get("id")?)?,
        campaign_id: factory_protocol::CampaignId::new(row.try_get("campaign_id")?)?,
        kernel_build_database_id: row.try_get("kernel_build_database_id")?,
        ticket_id: row.try_get("ticket_id")?,
        ticket_revision_id,
        ticket_state: observed_ticket_state,
        ticket_revision: revision_from_sql(row.try_get("ticket_revision_aggregate_revision")?)?,
        attempt_revision: revision_from_sql(row.try_get("attempt_revision")?)?,
        stage: row.try_get("stage")?,
        candidate_ordinal: row.try_get("candidate_ordinal")?,
        rework_ordinal: row.try_get("rework_ordinal")?,
        released: row.try_get("released")?,
        claimed_commit: row.try_get("claimed_commit")?,
        claimed_tree: row.try_get("claimed_tree")?,
        expected_observation_artifact_id: row.try_get("expected_observation_artifact_id")?,
        discovery_observation_artifact_id: row.try_get("discovery_observation_artifact_id")?,
    })
}

async fn lock_ticket_revision_state(
    tx: &mut Transaction<'_, Postgres>,
    ticket_revision_id: TicketRevisionId,
) -> Result<TicketRevisionRow, DecisionStoreError> {
    let row = sqlx::query(
        "SELECT tr.ticket_id, tr.lifecycle AS ticket_revision_lifecycle,
                tr.revision AS ticket_revision_aggregate_revision,
                t.lifecycle AS ticket_lifecycle, t.revision AS ticket_aggregate_revision,
                t.current_ticket_revision_id
         FROM factory.ticket_revisions tr
         JOIN factory.tickets t ON t.id = tr.ticket_id
         WHERE tr.id = $1 FOR UPDATE OF tr, t",
    )
    .bind(ticket_revision_id.get())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(DecisionStoreError::CorruptState)?;
    let state = ticket_state(row.try_get("ticket_revision_lifecycle")?)?;
    if row.try_get::<i64, _>("current_ticket_revision_id")? != ticket_revision_id.get()
        || state != ticket_state(row.try_get("ticket_lifecycle")?)?
        || row.try_get::<i64, _>("ticket_revision_aggregate_revision")?
            != row.try_get::<i64, _>("ticket_aggregate_revision")?
    {
        return Err(DecisionStoreError::CorruptState);
    }
    Ok(TicketRevisionRow {
        ticket_id: row.try_get("ticket_id")?,
        ticket_revision_id,
        ticket_state: state,
        ticket_revision: revision_from_sql(row.try_get("ticket_revision_aggregate_revision")?)?,
    })
}

async fn load_validation(
    tx: &mut Transaction<'_, Postgres>,
    validation_id: ValidationId,
) -> Result<ValidationRow, DecisionStoreError> {
    let row = sqlx::query(
        "SELECT candidate_id, performed_by_session_id, lifecycle, kernel_build_id, pristine_tree
         FROM factory.validations WHERE id = $1",
    )
    .bind(validation_id.get())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(DecisionStoreError::UnknownValidation { validation_id })?;
    Ok(ValidationRow {
        candidate_id: CandidateId::new(row.try_get("candidate_id")?)?,
        performed_by_session_id: row.try_get("performed_by_session_id")?,
        lifecycle: row.try_get("lifecycle")?,
        kernel_build_database_id: row.try_get("kernel_build_id")?,
        pristine_tree: row.try_get("pristine_tree")?,
    })
}

async fn load_quality_validation(
    tx: &mut Transaction<'_, Postgres>,
    validation_id: ValidationId,
    candidate_id: CandidateId,
) -> Result<ValidationRow, DecisionStoreError> {
    let row = sqlx::query(
        "SELECT candidate_id, performed_by_session_id, lifecycle, kernel_build_id, pristine_tree
         FROM factory.validations
         WHERE id = $1 AND candidate_id = $2 AND validation_scope = $3",
    )
    .bind(validation_id.get())
    .bind(candidate_id.get())
    .bind(ValidationScope::QualityFullSuite.code())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(DecisionStoreError::UnknownValidation { validation_id })?;
    Ok(ValidationRow {
        candidate_id: CandidateId::new(row.try_get("candidate_id")?)?,
        performed_by_session_id: row.try_get("performed_by_session_id")?,
        lifecycle: row.try_get("lifecycle")?,
        kernel_build_database_id: row.try_get("kernel_build_id")?,
        pristine_tree: row.try_get("pristine_tree")?,
    })
}

async fn load_review(
    tx: &mut Transaction<'_, Postgres>,
    review_id: ReviewId,
) -> Result<ReviewRow, DecisionStoreError> {
    let row = sqlx::query("SELECT id, candidate_id, verdict FROM factory.reviews WHERE id = $1")
        .bind(review_id.get())
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(DecisionStoreError::UnknownReview { review_id })?;
    Ok(ReviewRow {
        id: ReviewId::new(row.try_get("id")?)?,
        candidate_id: CandidateId::new(row.try_get("candidate_id")?)?,
        verdict: row.try_get("verdict")?,
    })
}

async fn load_decision(
    tx: &mut Transaction<'_, Postgres>,
    decision_id: i64,
) -> Result<DecisionRow, DecisionStoreError> {
    let row = sqlx::query(
        "SELECT ticket_revision_id, ticket_attempt_id, candidate_id
         FROM factory.architect_decisions WHERE id = $1",
    )
    .bind(decision_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(DecisionStoreError::CorruptDecision)?;
    Ok(DecisionRow {
        ticket_revision_id: row.try_get("ticket_revision_id")?,
        ticket_attempt_id: row.try_get("ticket_attempt_id")?,
        candidate_id: row.try_get("candidate_id")?,
    })
}

async fn load_delivery(
    tx: &mut Transaction<'_, Postgres>,
    delivery_id: i64,
) -> Result<DeliveryRow, DecisionStoreError> {
    let row = sqlx::query("SELECT candidate_id FROM factory.deliveries WHERE id = $1")
        .bind(delivery_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(DecisionStoreError::CorruptState)?;
    Ok(DeliveryRow {
        candidate_id: CandidateId::new(row.try_get("candidate_id")?)?,
    })
}

async fn lock_campaign(
    tx: &mut Transaction<'_, Postgres>,
    campaign_id: factory_protocol::CampaignId,
) -> Result<CampaignRow, DecisionStoreError> {
    campaign_from_row(
        sqlx::query("SELECT lifecycle, revision, delivery_target FROM factory.campaigns WHERE id = $1 FOR UPDATE")
            .bind(campaign_id.get())
            .fetch_optional(&mut **tx)
            .await?
            .ok_or(DecisionStoreError::CampaignNotRunning)?,
    )
}

async fn load_campaign(
    tx: &mut Transaction<'_, Postgres>,
    campaign_id: factory_protocol::CampaignId,
) -> Result<CampaignRow, DecisionStoreError> {
    campaign_from_row(
        sqlx::query(
            "SELECT lifecycle, revision, delivery_target FROM factory.campaigns WHERE id = $1",
        )
        .bind(campaign_id.get())
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(DecisionStoreError::CampaignNotRunning)?,
    )
}

fn campaign_from_row(row: sqlx::postgres::PgRow) -> Result<CampaignRow, DecisionStoreError> {
    Ok(CampaignRow {
        lifecycle: row.try_get("lifecycle")?,
        revision: revision_from_sql(row.try_get("revision")?)?,
        delivery_target: row.try_get("delivery_target")?,
    })
}

async fn validation_exists(
    tx: &mut Transaction<'_, Postgres>,
    candidate_id: CandidateId,
    scope: ValidationScope,
) -> Result<bool, DecisionStoreError> {
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM factory.validations WHERE candidate_id = $1 AND validation_scope = $2)",
    )
    .bind(candidate_id.get())
    .bind(scope.code())
    .fetch_one(&mut **tx)
    .await?)
}

async fn validation_passed(
    tx: &mut Transaction<'_, Postgres>,
    candidate_id: CandidateId,
    scope: ValidationScope,
) -> Result<bool, DecisionStoreError> {
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
             SELECT 1 FROM factory.validations
             WHERE candidate_id = $1 AND validation_scope = $2 AND lifecycle = $3
         )",
    )
    .bind(candidate_id.get())
    .bind(scope.code())
    .bind(VALIDATION_PASSED)
    .fetch_one(&mut **tx)
    .await?)
}

async fn review_exists(
    tx: &mut Transaction<'_, Postgres>,
    candidate_id: CandidateId,
) -> Result<bool, DecisionStoreError> {
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM factory.reviews WHERE candidate_id = $1)",
    )
    .bind(candidate_id.get())
    .fetch_one(&mut **tx)
    .await?)
}

async fn deliver_decision_exists(
    tx: &mut Transaction<'_, Postgres>,
    candidate_id: CandidateId,
) -> Result<bool, DecisionStoreError> {
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
             SELECT 1 FROM factory.architect_decisions
             WHERE candidate_id = $1 AND decision_kind = $2
         )",
    )
    .bind(candidate_id.get())
    .bind(DECISION_DELIVER)
    .fetch_one(&mut **tx)
    .await?)
}

async fn paid_session_active(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<bool, DecisionStoreError> {
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM factory.sessions WHERE lifecycle = $1)",
    )
    .bind(SESSION_RUNNING)
    .fetch_one(&mut **tx)
    .await?)
}

async fn delivered_attempt_count(
    tx: &mut Transaction<'_, Postgres>,
    campaign_id: factory_protocol::CampaignId,
) -> Result<i64, DecisionStoreError> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT count(*)::BIGINT FROM factory.ticket_attempts WHERE campaign_id = $1 AND stage = $2",
    )
    .bind(campaign_id.get())
    .bind(ATTEMPT_DELIVERED)
    .fetch_one(&mut **tx)
    .await?)
}

async fn require_artifact(
    tx: &mut Transaction<'_, Postgres>,
    reference: &SealedArtifactReferenceV1,
    expected_build_database_id: i64,
) -> Result<(), DecisionStoreError> {
    let row = sqlx::query(
        "SELECT digest, byte_length, creating_kernel_build_id
         FROM factory.artifacts WHERE id = $1",
    )
    .bind(reference.artifact_id.get())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(DecisionStoreError::ArtifactReferenceMismatch)?;
    let digest: Vec<u8> = row.try_get("digest")?;
    let byte_length: i64 = row.try_get("byte_length")?;
    let build_id: i64 = row.try_get("creating_kernel_build_id")?;
    if digest.as_slice() != reference.digest.as_bytes()
        || u64::try_from(byte_length).map_err(|_| DecisionStoreError::CorruptState)?
            != reference.byte_length
    {
        return Err(DecisionStoreError::ArtifactReferenceMismatch);
    }
    if build_id != expected_build_database_id {
        return Err(DecisionStoreError::ArtifactBuildMismatch);
    }
    Ok(())
}

async fn require_artifact_unbound(
    tx: &mut Transaction<'_, Postgres>,
    reference: &SealedArtifactReferenceV1,
) -> Result<(), DecisionStoreError> {
    let row = sqlx::query("SELECT digest, byte_length FROM factory.artifacts WHERE id = $1")
        .bind(reference.artifact_id.get())
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(DecisionStoreError::ArtifactReferenceMismatch)?;
    let digest: Vec<u8> = row.try_get("digest")?;
    let byte_length: i64 = row.try_get("byte_length")?;
    if digest.as_slice() != reference.digest.as_bytes()
        || u64::try_from(byte_length).map_err(|_| DecisionStoreError::CorruptState)?
            != reference.byte_length
    {
        return Err(DecisionStoreError::ArtifactReferenceMismatch);
    }
    Ok(())
}

async fn artifact_digest_for_build(
    tx: &mut Transaction<'_, Postgres>,
    artifact_id: i64,
    expected_build_database_id: i64,
) -> Result<ContentDigest, DecisionStoreError> {
    let row =
        sqlx::query("SELECT digest, creating_kernel_build_id FROM factory.artifacts WHERE id = $1")
            .bind(artifact_id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or(DecisionStoreError::ArtifactReferenceMismatch)?;
    if row.try_get::<i64, _>("creating_kernel_build_id")? != expected_build_database_id {
        return Err(DecisionStoreError::ArtifactBuildMismatch);
    }
    let bytes: Vec<u8> = row.try_get("digest")?;
    let bytes: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| DecisionStoreError::CorruptState)?;
    Ok(ContentDigest::from_bytes(bytes))
}

async fn require_kernel_build(
    tx: &mut Transaction<'_, Postgres>,
    kernel_build_id: KernelBuildId,
    expected_build_database_id: i64,
) -> Result<i64, DecisionStoreError> {
    let row = sqlx::query("SELECT id FROM factory.kernel_builds WHERE build_digest = $1")
        .bind(kernel_build_id.digest().as_bytes().as_slice())
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(DecisionStoreError::ValidationBuildMismatch)?;
    let id: i64 = row.try_get("id")?;
    if id != expected_build_database_id {
        return Err(DecisionStoreError::ValidationBuildMismatch);
    }
    Ok(id)
}

async fn require_session(
    tx: &mut Transaction<'_, Postgres>,
    session_id: SessionId,
    expected_campaign_id: factory_protocol::CampaignId,
    expected_office: i16,
) -> Result<(), DecisionStoreError> {
    let row =
        sqlx::query("SELECT campaign_id, office, lifecycle FROM factory.sessions WHERE id = $1")
            .bind(session_id.get())
            .fetch_optional(&mut **tx)
            .await?
            .ok_or(DecisionStoreError::SessionJurisdictionMismatch)?;
    let lifecycle: i16 = row.try_get("lifecycle")?;
    if row.try_get::<i64, _>("campaign_id")? != expected_campaign_id.get()
        || row.try_get::<i16, _>("office")? != expected_office
        || !matches!(lifecycle, SESSION_RUNNING | SESSION_SUCCEEDED)
    {
        return Err(DecisionStoreError::SessionJurisdictionMismatch);
    }
    Ok(())
}

async fn require_hard_and_quality_validation(
    tx: &mut Transaction<'_, Postgres>,
    candidate_id: CandidateId,
) -> Result<(), DecisionStoreError> {
    if !validation_passed(tx, candidate_id, ValidationScope::HardCandidate).await? {
        return Err(DecisionStoreError::HardValidationMissing);
    }
    if !validation_passed(tx, candidate_id, ValidationScope::QualityFullSuite).await? {
        return Err(DecisionStoreError::QualityValidationNotPassed);
    }
    Ok(())
}

async fn insert_decision(
    tx: &mut Transaction<'_, Postgres>,
    decision_kind: i16,
    ticket_revision_id: Option<i64>,
    ticket_attempt_id: Option<i64>,
    candidate_id: Option<i64>,
    review_id: Option<i64>,
    rationale_artifact_id: i64,
    principal: &str,
    overrides_quality_rejection: bool,
) -> Result<i64, DecisionStoreError> {
    let row = sqlx::query(
        "INSERT INTO factory.architect_decisions (
             decision_kind, ticket_revision_id, ticket_attempt_id, candidate_id, review_id,
             rationale_artifact_id, principal, overrides_quality_rejection
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(decision_kind)
    .bind(ticket_revision_id)
    .bind(ticket_attempt_id)
    .bind(candidate_id)
    .bind(review_id)
    .bind(rationale_artifact_id)
    .bind(principal)
    .bind(overrides_quality_rejection)
    .fetch_one(&mut **tx)
    .await?;
    Ok(row.try_get("id")?)
}

async fn update_ticket_state(
    tx: &mut Transaction<'_, Postgres>,
    attempt: &AttemptRow,
    state: i16,
    next: AggregateRevision,
    sponsorship_reason: Option<&str>,
    blocked_reason: Option<&str>,
) -> Result<(), DecisionStoreError> {
    update_ticket_state_parts(
        tx,
        attempt.ticket_id,
        attempt.ticket_revision_id,
        state,
        next,
        sponsorship_reason,
        blocked_reason,
    )
    .await
}

async fn update_ticket_state_parts(
    tx: &mut Transaction<'_, Postgres>,
    ticket_id: i64,
    ticket_revision_id: TicketRevisionId,
    state: i16,
    next: AggregateRevision,
    sponsorship_reason: Option<&str>,
    blocked_reason: Option<&str>,
) -> Result<(), DecisionStoreError> {
    sqlx::query(
        "UPDATE factory.ticket_revisions
         SET lifecycle = $1, revision = $2,
             sponsored_at = CASE WHEN $3::TEXT IS NULL THEN sponsored_at ELSE CURRENT_TIMESTAMP END,
             sponsorship_reason = COALESCE($3, sponsorship_reason),
             blocked_reason = COALESCE($4, blocked_reason)
         WHERE id = $5",
    )
    .bind(state)
    .bind(revision_sql(next)?)
    .bind(sponsorship_reason)
    .bind(blocked_reason)
    .bind(ticket_revision_id.get())
    .execute(&mut **tx)
    .await?;
    sqlx::query("UPDATE factory.tickets SET lifecycle = $1, revision = $2 WHERE id = $3")
        .bind(state)
        .bind(revision_sql(next)?)
        .bind(ticket_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn update_ticket_requalification(
    tx: &mut Transaction<'_, Postgres>,
    attempt: &AttemptRow,
    state: i16,
    next: AggregateRevision,
    outcome: i16,
    requalification: &CurrentHeadRequalification,
    blocked_reason: Option<&str>,
) -> Result<(), DecisionStoreError> {
    sqlx::query(
        "UPDATE factory.ticket_revisions
         SET lifecycle = $1, revision = $2,
             last_requalification_outcome = $3,
             last_requalification_commit = $4,
             last_requalification_tree = $5,
             last_requalification_first_observation_artifact_id = $6,
             last_requalification_second_observation_artifact_id = $7,
             last_requalified_at = CURRENT_TIMESTAMP,
             blocked_reason = $8
         WHERE id = $9",
    )
    .bind(state)
    .bind(revision_sql(next)?)
    .bind(outcome)
    .bind(&requalification.current_head_commit)
    .bind(&requalification.current_head_tree)
    .bind(requalification.first_actual_observation_artifact_id.get())
    .bind(requalification.second_actual_observation_artifact_id.get())
    .bind(blocked_reason)
    .bind(attempt.ticket_revision_id.get())
    .execute(&mut **tx)
    .await?;
    sqlx::query("UPDATE factory.tickets SET lifecycle = $1, revision = $2 WHERE id = $3")
        .bind(state)
        .bind(revision_sql(next)?)
        .bind(attempt.ticket_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn classify_requalification(
    tx: &mut Transaction<'_, Postgres>,
    attempt: &AttemptRow,
    value: &CurrentHeadRequalification,
) -> Result<RequalificationClassification, DecisionStoreError> {
    let expected = artifact_digest_for_build(
        tx,
        attempt.expected_observation_artifact_id,
        attempt.kernel_build_database_id,
    )
    .await?;
    let discovery = artifact_digest_for_build(
        tx,
        attempt.discovery_observation_artifact_id,
        attempt.kernel_build_database_id,
    )
    .await?;
    let first = artifact_digest_for_build(
        tx,
        value.first_actual_observation_artifact_id.get(),
        attempt.kernel_build_database_id,
    )
    .await?;
    let second = artifact_digest_for_build(
        tx,
        value.second_actual_observation_artifact_id.get(),
        attempt.kernel_build_database_id,
    )
    .await?;
    if first != second {
        return Ok(RequalificationClassification::Diverged);
    }
    if first == expected {
        return Ok(RequalificationClassification::Resolved);
    }
    if first == discovery {
        return Ok(RequalificationClassification::Reproduced);
    }
    Ok(RequalificationClassification::Diverged)
}

async fn find_audit(
    tx: &mut Transaction<'_, Postgres>,
    principal: &str,
    command_id: &str,
    operation: &'static str,
    fingerprint: ContentDigest,
) -> Result<Option<AuditReceipt>, DecisionStoreError> {
    let row = sqlx::query(
        "SELECT id, operation, command_fingerprint, subject_kind, subject_id, resulting_revision
         FROM factory.audit_log WHERE principal = $1 AND command_id = $2",
    )
    .bind(principal)
    .bind(command_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let stored: Vec<u8> = row.try_get("command_fingerprint")?;
    if row.try_get::<String, _>("operation")? != operation
        || stored.as_slice() != fingerprint.as_bytes()
    {
        return Err(DecisionStoreError::IdempotencyConflict {
            principal: principal.to_owned(),
            command_id: command_id.to_owned(),
        });
    }
    Ok(Some(AuditReceipt {
        audit_log_id: row.try_get("id")?,
        subject_kind: row.try_get("subject_kind")?,
        subject_id: row.try_get("subject_id")?,
        resulting_revision: revision_from_sql(row.try_get("resulting_revision")?)?,
    }))
}

async fn insert_audit(
    tx: &mut Transaction<'_, Postgres>,
    principal: &str,
    command_id: &str,
    operation: &'static str,
    fingerprint: ContentDigest,
    subject_kind: i16,
    subject_id: i64,
    resulting_revision: AggregateRevision,
) -> Result<i64, DecisionStoreError> {
    let row = sqlx::query(
        "INSERT INTO factory.audit_log (
             principal, command_id, operation, command_fingerprint,
             subject_kind, subject_id, resulting_revision
         ) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(principal)
    .bind(command_id)
    .bind(operation)
    .bind(fingerprint.as_bytes().as_slice())
    .bind(subject_kind)
    .bind(subject_id)
    .bind(revision_sql(resulting_revision)?)
    .fetch_one(&mut **tx)
    .await?;
    Ok(row.try_get("id")?)
}

fn validate_command(principal: &str, command_id: &str) -> Result<(), DecisionStoreError> {
    for value in [principal, command_id] {
        if value.is_empty()
            || value.len() > 160
            || !value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'_' | b'-')
            })
        {
            return Err(DecisionStoreError::InvalidCommandIdentity);
        }
    }
    Ok(())
}

fn validate_architect_command(principal: &str, command_id: &str) -> Result<(), DecisionStoreError> {
    // The external protocol permits a broader display identity, while the
    // single audit receipt deliberately bounds durable principals to 160
    // bytes. Refuse rather than truncating an attribution at that boundary.
    if principal.is_empty() || principal.len() > 160 || principal.contains('\0') {
        return Err(DecisionStoreError::InvalidCommandIdentity);
    }
    validate_command("architect", command_id)
}

fn validate_validation_command(command: &RecordValidation) -> Result<(), DecisionStoreError> {
    if command.validation_profile.is_empty()
        || command.validation_profile.len() > 160
        || command.validation_profile.contains('\0')
        || command.duration_millis > i64::MAX as u64
    {
        return Err(DecisionStoreError::CorruptState);
    }
    command
        .command_set
        .validate("validation command set", 256 * 1024, false)?;
    command
        .log
        .validate("validation log", 16 * 1024 * 1024, true)?;
    Ok(())
}

fn validate_candidate_ref(value: &str) -> Result<(), DecisionStoreError> {
    let Some(suffix) = value.strip_prefix("refs/heads/factory/") else {
        return Err(DecisionStoreError::InvalidCandidateRef);
    };
    let mut fields = suffix.split('/');
    let Some(ticket_id) = fields.next() else {
        return Err(DecisionStoreError::InvalidCandidateRef);
    };
    let Some(candidate_id) = fields.next() else {
        return Err(DecisionStoreError::InvalidCandidateRef);
    };
    if fields.next().is_some()
        || value.len() > 512
        || [ticket_id, candidate_id].iter().any(|field| {
            field.is_empty()
                || field.starts_with('0')
                || !field.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        return Err(DecisionStoreError::InvalidCandidateRef);
    }
    Ok(())
}

fn validate_requalification(value: &CurrentHeadRequalification) -> Result<(), DecisionStoreError> {
    for field in [&value.current_head_commit, &value.current_head_tree] {
        if !matches!(field.len(), 40 | 64)
            || !field
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(DecisionStoreError::CorruptState);
        }
    }
    Ok(())
}

fn require_revision(
    expected: ExpectedRevision,
    current: AggregateRevision,
) -> Result<(), DecisionStoreError> {
    if expected.get() == current {
        Ok(())
    } else {
        Err(DecisionStoreError::RevisionConflict { expected, current })
    }
}

fn require_candidate_state(
    required: CandidateState,
    observed: CandidateState,
) -> Result<(), DecisionStoreError> {
    if required == observed {
        Ok(())
    } else {
        Err(DecisionStoreError::CandidateStateConflict { required, observed })
    }
}

fn require_ticket_state(
    required: TicketState,
    observed: TicketState,
) -> Result<(), DecisionStoreError> {
    if required == observed {
        Ok(())
    } else {
        Err(DecisionStoreError::TicketStateConflict { required, observed })
    }
}

fn require_subject(receipt: &AuditReceipt, subject_kind: i16) -> Result<(), DecisionStoreError> {
    if receipt.subject_kind == subject_kind {
        Ok(())
    } else {
        Err(DecisionStoreError::CorruptState)
    }
}

fn revision_from_sql(value: i64) -> Result<AggregateRevision, DecisionStoreError> {
    u64::try_from(value)
        .map(AggregateRevision::from_persisted)
        .map_err(|_| DecisionStoreError::IntegerOutOfRange)
}

fn revision_sql(value: AggregateRevision) -> Result<i64, DecisionStoreError> {
    i64::try_from(value.get()).map_err(|_| DecisionStoreError::IntegerOutOfRange)
}

fn candidate_state(value: i16) -> Result<CandidateState, DecisionStoreError> {
    match value {
        CANDIDATE_SUBMITTED => Ok(CandidateState::Submitted),
        CANDIDATE_VALIDATED => Ok(CandidateState::Validated),
        CANDIDATE_REJECTED => Ok(CandidateState::Rejected),
        CANDIDATE_ACCEPTED => Ok(CandidateState::Accepted),
        CANDIDATE_DELIVERED => Ok(CandidateState::Delivered),
        _ => Err(DecisionStoreError::CorruptState),
    }
}

fn validation_state(value: i16) -> Result<ValidationState, DecisionStoreError> {
    match value {
        VALIDATION_PASSED => Ok(ValidationState::Passed),
        VALIDATION_FAILED => Ok(ValidationState::Failed),
        VALIDATION_INTERRUPTED => Ok(ValidationState::Interrupted),
        _ => Err(DecisionStoreError::CorruptState),
    }
}

fn review_verdict(value: i16) -> Result<ReviewVerdict, DecisionStoreError> {
    match value {
        REVIEW_ACCEPT => Ok(ReviewVerdict::Accept),
        REVIEW_REJECT => Ok(ReviewVerdict::Reject),
        _ => Err(DecisionStoreError::CorruptState),
    }
}

const fn review_verdict_code(value: ReviewVerdict) -> i16 {
    match value {
        ReviewVerdict::Accept => REVIEW_ACCEPT,
        ReviewVerdict::Reject => REVIEW_REJECT,
    }
}

fn ticket_state(value: i16) -> Result<TicketState, DecisionStoreError> {
    match value {
        TICKET_PROPOSED => Ok(TicketState::Proposed),
        TICKET_SPONSORED => Ok(TicketState::Sponsored),
        TICKET_IN_FLIGHT => Ok(TicketState::InFlight),
        TICKET_DELIVERED => Ok(TicketState::Delivered),
        TICKET_BLOCKED => Ok(TicketState::Blocked),
        TICKET_RESOLVED => Ok(TicketState::Resolved),
        TICKET_REJECTED => Ok(TicketState::Rejected),
        _ => Err(DecisionStoreError::CorruptState),
    }
}

fn release_outcome(state: TicketState) -> Result<ReleaseOutcome, DecisionStoreError> {
    match state {
        TicketState::Sponsored => Ok(ReleaseOutcome::Released),
        TicketState::Resolved => Ok(ReleaseOutcome::Resolved),
        TicketState::Blocked => Ok(ReleaseOutcome::Blocked),
        _ => Err(DecisionStoreError::CorruptState),
    }
}

const fn decision_code(value: CandidateDecisionV1) -> i16 {
    match value {
        CandidateDecisionV1::Deliver => DECISION_DELIVER,
        CandidateDecisionV1::Rework => DECISION_REWORK,
        CandidateDecisionV1::Reject => DECISION_REJECT,
    }
}

fn architect_receipt(
    decision_id: i64,
    kind: ArchitectDecisionKindV1,
) -> Result<ArchitectDecisionReceiptV1, DecisionStoreError> {
    Ok(ArchitectDecisionReceiptV1 {
        architect_decision_id: factory_protocol::ArchitectDecisionId::new(decision_id)?,
        kind,
    })
}

fn hash_string(hasher: &mut blake3::Hasher, value: &str) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

fn hash_i64(hasher: &mut blake3::Hasher, value: i64) {
    hasher.update(&value.to_be_bytes());
}

fn hash_u64(hasher: &mut blake3::Hasher, value: u64) {
    hasher.update(&value.to_be_bytes());
}

fn hash_expected(hasher: &mut blake3::Hasher, value: ExpectedRevision) {
    hash_u64(hasher, value.get().get());
}

fn hash_artifact(hasher: &mut blake3::Hasher, value: &SealedArtifactReferenceV1) {
    hash_i64(hasher, value.artifact_id.get());
    hasher.update(&value.digest.as_bytes());
    hash_u64(hasher, value.byte_length);
}

fn hash_object(hasher: &mut blake3::Hasher, value: &RepositoryObjectIdV1) {
    hash_string(hasher, value.as_str());
}

fn fingerprint_prefix(operation: &str, principal: &str, command_id: &str) -> blake3::Hasher {
    let mut hasher = blake3::Hasher::new();
    hash_string(&mut hasher, operation);
    hash_string(&mut hasher, principal);
    hash_string(&mut hasher, command_id);
    hasher
}

fn finish_hash(hasher: blake3::Hasher) -> ContentDigest {
    ContentDigest::from_bytes(*hasher.finalize().as_bytes())
}

fn submit_candidate_fingerprint(command: &SubmitCandidate) -> ContentDigest {
    let mut hasher = fingerprint_prefix(
        CANDIDATE_SUBMIT_OPERATION,
        &command.principal,
        &command.command_id,
    );
    hash_i64(&mut hasher, command.ticket_attempt_id.get());
    hash_expected(&mut hasher, command.expected_attempt_revision);
    hash_expected(&mut hasher, command.expected_ticket_revision);
    hash_i64(&mut hasher, command.engineering_session_id.get());
    for object in [
        &command.base_commit,
        &command.base_tree,
        &command.regression_tree,
        &command.candidate_tree,
    ] {
        hash_object(&mut hasher, object);
    }
    hash_artifact(&mut hasher, &command.changed_paths);
    hash_artifact(&mut hasher, &command.regression_patch);
    hash_artifact(&mut hasher, &command.regression_command_set);
    hash_artifact(&mut hasher, &command.regression_log);
    hash_artifact(&mut hasher, &command.candidate_patch);
    hash_artifact(&mut hasher, &command.submission.engineering_report);
    hash_string(&mut hasher, &command.submission.commit_subject);
    hash_string(&mut hasher, &command.submission.commit_body);
    hash_string(&mut hasher, &command.submission.regression_test_identity);
    hash_artifact(&mut hasher, &command.submission.risks);
    finish_hash(hasher)
}

fn validation_fingerprint(command: &RecordValidation) -> ContentDigest {
    let mut hasher = fingerprint_prefix(
        VALIDATION_RECORD_OPERATION,
        &command.principal,
        &command.command_id,
    );
    hash_i64(&mut hasher, command.candidate_id.get());
    hash_expected(&mut hasher, command.expected_candidate_revision);
    hash_expected(&mut hasher, command.expected_attempt_revision);
    hasher.update(&[command.scope.code() as u8, command.result.code() as u8]);
    hasher.update(&command.kernel_build_id.digest().as_bytes());
    hash_i64(&mut hasher, command.performed_by_session_id.get());
    hash_string(&mut hasher, &command.validation_profile);
    hash_object(&mut hasher, &command.pristine_tree);
    hash_artifact(&mut hasher, &command.command_set);
    hash_u64(&mut hasher, command.duration_millis);
    hash_artifact(&mut hasher, &command.log);
    finish_hash(hasher)
}

fn commit_fingerprint(command: &AttachCandidateCommit) -> ContentDigest {
    let mut hasher = fingerprint_prefix(
        CANDIDATE_COMMIT_ATTACH_OPERATION,
        &command.principal,
        &command.command_id,
    );
    hash_i64(&mut hasher, command.candidate_id.get());
    hash_expected(&mut hasher, command.expected_candidate_revision);
    hash_object(&mut hasher, &command.candidate_commit);
    hash_string(&mut hasher, &command.candidate_ref);
    finish_hash(hasher)
}

fn review_fingerprint(command: &SubmitQualityReview) -> ContentDigest {
    let mut hasher = fingerprint_prefix(
        REVIEW_SUBMIT_OPERATION,
        &command.principal,
        &command.command_id,
    );
    hash_i64(&mut hasher, command.candidate_id.get());
    hash_expected(&mut hasher, command.expected_candidate_revision);
    hash_expected(&mut hasher, command.expected_attempt_revision);
    hash_i64(&mut hasher, command.quality_session_id.get());
    hash_i64(
        &mut hasher,
        command.submission.full_suite_validation_id.get(),
    );
    hasher.update(&[review_verdict_code(command.submission.verdict) as u8]);
    hash_artifact(&mut hasher, &command.submission.rationale);
    hash_artifact(&mut hasher, &command.submission.risks);
    hash_artifact(&mut hasher, &command.submission.additional_probes);
    finish_hash(hasher)
}

fn sponsorship_fingerprint(command: &SponsorTicket) -> ContentDigest {
    let mut hasher = fingerprint_prefix(
        SPONSOR_OPERATION,
        command.decision.principal.as_str(),
        &command.command_id,
    );
    hash_i64(&mut hasher, command.decision.ticket_revision_id.get());
    hash_expected(&mut hasher, command.expected_ticket_revision);
    hash_artifact(&mut hasher, &command.decision.rationale);
    finish_hash(hasher)
}

fn release_fingerprint(command: &ReleaseTicketAttempt) -> ContentDigest {
    let mut hasher = fingerprint_prefix(
        RELEASE_OPERATION,
        command.decision.principal.as_str(),
        &command.command_id,
    );
    hash_i64(&mut hasher, command.decision.ticket_attempt_id.get());
    hash_expected(&mut hasher, command.expected_attempt_revision);
    hash_expected(&mut hasher, command.expected_ticket_revision);
    hash_artifact(&mut hasher, &command.decision.rationale);
    hash_string(&mut hasher, &command.requalification.current_head_commit);
    hash_string(&mut hasher, &command.requalification.current_head_tree);
    hash_i64(
        &mut hasher,
        command
            .requalification
            .first_actual_observation_artifact_id
            .get(),
    );
    hash_i64(
        &mut hasher,
        command
            .requalification
            .second_actual_observation_artifact_id
            .get(),
    );
    finish_hash(hasher)
}

fn candidate_decision_fingerprint(command: &DecideCandidate) -> ContentDigest {
    let mut hasher = fingerprint_prefix(
        CANDIDATE_DECIDE_OPERATION,
        command.request.principal.as_str(),
        &command.command_id,
    );
    hash_expected(&mut hasher, command.expected_candidate_revision);
    hash_expected(&mut hasher, command.expected_attempt_revision);
    hash_expected(&mut hasher, command.expected_ticket_revision);
    hash_i64(&mut hasher, command.request.candidate_id.get());
    hash_i64(&mut hasher, command.request.review_id.get());
    hasher.update(&[decision_code(command.request.decision) as u8]);
    hash_artifact(&mut hasher, &command.request.rationale);
    match command.request.quality_rejection_override {
        Some(value) => {
            hasher.update(&[1]);
            hash_i64(&mut hasher, value.get());
        }
        None => {
            hasher.update(&[0]);
        }
    }
    finish_hash(hasher)
}

fn delivery_fingerprint(command: &RecordDelivery) -> ContentDigest {
    let mut hasher = fingerprint_prefix(
        DELIVERY_RECORD_OPERATION,
        &command.principal,
        &command.command_id,
    );
    hash_i64(&mut hasher, command.candidate_id.get());
    hash_expected(&mut hasher, command.expected_candidate_revision);
    hash_expected(&mut hasher, command.expected_attempt_revision);
    hash_expected(&mut hasher, command.expected_ticket_revision);
    hash_expected(&mut hasher, command.expected_campaign_revision);
    hash_object(&mut hasher, &command.expected_old_commit);
    hash_object(&mut hasher, &command.resulting_commit);
    hash_object(&mut hasher, &command.resulting_tree);
    hash_artifact(&mut hasher, &command.receipt);
    finish_hash(hasher)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_rejection_is_overridable_only_for_the_exact_linked_review() {
        let review = ReviewId::new(3).unwrap();
        assert!(matches!(
            (true, Some(review)),
            (true, Some(value)) if value == review
        ));
        assert_ne!(Some(review), Some(ReviewId::new(4).unwrap()));
    }

    #[test]
    fn candidate_ref_never_accepts_a_remote_or_revision_expression() {
        assert!(validate_candidate_ref("refs/heads/factory/12/34").is_ok());
        for invalid in [
            "refs/remotes/origin/main",
            "refs/heads/factory/12/../34",
            "refs/heads/factory/12/34^{tree}",
            "refs/heads/factory/12/34\nnext",
        ] {
            assert!(validate_candidate_ref(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    #[ignore = "requires FACTORY_TEST_DATABASE_URL for a disposable PostgreSQL 18 database"]
    fn postgres_final_authority_schema_has_exactly_twenty_named_tables() {
        smol::block_on(async {
            let database_url = std::env::var("FACTORY_TEST_DATABASE_URL")
                .expect("FACTORY_TEST_DATABASE_URL must name a disposable PostgreSQL 18 database");
            let database_name = database_url
                .rsplit('/')
                .next()
                .and_then(|value| value.split('?').next())
                .expect("database URL has a final path component");
            assert!(
                database_name
                    .strip_prefix("factory_test_v3_")
                    .is_some_and(|suffix| {
                        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
                    }),
                "FACTORY_TEST_DATABASE_URL must name factory_test_v3_<digits>"
            );
            let store = KernelStore::connect(&database_url)
                .await
                .expect("connect PostgreSQL 18");
            store
                .migrate_and_verify()
                .await
                .expect("migrate through T8");
            let pool = store.pool_for_authority();
            let table_count = sqlx::query_scalar::<_, i64>(
                "SELECT count(*)::BIGINT
                 FROM information_schema.tables
                 WHERE table_schema = 'factory' AND table_type = 'BASE TABLE'",
            )
            .fetch_one(&pool)
            .await
            .expect("count Factory tables");
            assert_eq!(
                table_count, 20,
                "T8 consumes the fixed MVP table budget exactly"
            );
            for relation in [
                "candidates",
                "validations",
                "reviews",
                "architect_decisions",
                "deliveries",
            ] {
                let exists = sqlx::query_scalar::<_, bool>(
                    "SELECT to_regclass(format('factory.%s', $1)) IS NOT NULL",
                )
                .bind(relation)
                .fetch_one(&pool)
                .await
                .expect("named final authority relation");
                assert!(exists, "missing final authority table {relation}");
            }
            store.close().await;
        });
    }
}
