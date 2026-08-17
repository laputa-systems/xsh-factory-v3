//! Generic ticket-buffer authority.
//!
//! Ticket problem contracts and reproducer observations are sealed artifacts.
//! This module admits only their exact identities, applies the closed ticket
//! transitions, and derives scheduling pressure from PostgreSQL without
//! writing polling state. It deliberately does not execute a product command
//! or interpret a product-specific proposal.

use factory_protocol::{
    AggregateRevision, ApplicationRevisionId, ArchitectDecisionId, ArtifactId, CampaignId,
    CandidateId, ContentDigest, ExpectedRevision, ReviewId, TicketAttemptId, TicketAttemptStage,
    TicketBoundsV2, TicketId, TicketRevisionId, TicketState, ValidationId,
};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::storage::{KernelStore, StoreError};

const TICKET_PROPOSED: i16 = 0;
const TICKET_SPONSORED: i16 = 1;
const TICKET_IN_FLIGHT: i16 = 2;
const TICKET_DELIVERED: i16 = 3;
const TICKET_BLOCKED: i16 = 4;
const TICKET_RESOLVED: i16 = 5;
const TICKET_SUPERSEDED: i16 = 6;
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

const REQUALIFICATION_REPRODUCED: i16 = 0;
const REQUALIFICATION_RESOLVED: i16 = 1;
const REQUALIFICATION_DIVERGED: i16 = 2;

const CAMPAIGN_RUNNING: i16 = 0;
const CAMPAIGN_COMPLETED: i16 = 1;
const COST_KNOWN: i16 = 0;
const SESSION_RUNNING: i16 = 1;

const PROPOSAL_SUBJECT: i16 = 30;
const SPONSORSHIP_SUBJECT: i16 = 31;
const CLAIMED_ATTEMPT_SUBJECT: i16 = 32;
const CLAIM_RESOLVED_SUBJECT: i16 = 33;
const CLAIM_BLOCKED_SUBJECT: i16 = 34;
const FAILED_ATTEMPT_SUBJECT: i16 = 35;
const RELEASED_ATTEMPT_SUBJECT: i16 = 36;
const RELEASE_RESOLVED_SUBJECT: i16 = 37;
const RELEASE_BLOCKED_SUBJECT: i16 = 38;
const CAMPAIGN_COMPLETED_SUBJECT: i16 = 39;
// Subject kind 40 is the stable candidate subject family. Quality retry was
// added later and must retain its own ticket-attempt subject family.
const QUALITY_RETRY_SUBJECT: i16 = 45;
const ENGINEERING_RETRY_SUBJECT: i16 = 46;

const PROPOSE_OPERATION: &str = "ticket.propose";
const SPONSOR_OPERATION: &str = "ticket.sponsor";
const CLAIM_OPERATION: &str = "ticket.claim";
const FAIL_OPERATION: &str = "ticket_attempt.fail";
const RELEASE_OPERATION: &str = "ticket_attempt.release";
const QUALITY_RETRY_OPERATION: &str = "ticket_attempt.retry_quality";
const ENGINEERING_RETRY_OPERATION: &str = "ticket_attempt.retry_engineering";
const COMPLETE_CAMPAIGN_OPERATION: &str = "campaign.complete_delivery_target";

/// The narrow, named ticket authority over the kernel's fixed PostgreSQL
/// pool. It never exposes the pool to applications, actors, or callers.
#[derive(Clone, Debug)]
pub struct TicketStore {
    pool: PgPool,
}

impl KernelStore {
    #[must_use]
    pub fn ticket_store(&self) -> TicketStore {
        TicketStore {
            pool: self.pool_for_authority(),
        }
    }
}

/// A Product proposal already validated by the application and observed twice
/// by the deterministic reproducer boundary. The kernel independently checks
/// that both sealed actual observations agree and differ from expectation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmitTicketProposal {
    pub principal: String,
    pub command_id: String,
    pub expected_application_revision: ExpectedRevision,
    pub application_revision_id: ApplicationRevisionId,
    pub proposal_artifact_id: ArtifactId,
    pub reproducer_artifact_id: ArtifactId,
    pub expected_observation_artifact_id: ArtifactId,
    pub first_actual_observation_artifact_id: ArtifactId,
    pub second_actual_observation_artifact_id: ArtifactId,
    pub discovery_commit: String,
    pub discovery_tree: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TicketProposalReceipt {
    pub ticket_id: TicketId,
    pub ticket_revision_id: TicketRevisionId,
    pub resulting_revision: AggregateRevision,
    pub audit_log_id: i64,
    pub was_idempotent_retry: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SponsorTicketRevision {
    pub principal: String,
    pub command_id: String,
    pub ticket_revision_id: TicketRevisionId,
    pub expected_ticket_revision: ExpectedRevision,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TicketRevisionReceipt {
    pub ticket_id: TicketId,
    pub ticket_revision_id: TicketRevisionId,
    pub state: TicketState,
    pub resulting_revision: AggregateRevision,
    pub audit_log_id: i64,
    pub was_idempotent_retry: bool,
}

/// An exact re-run of a stored reproducer on a specific clean current head.
/// The artifact identities are compared byte-for-byte with the stored expected
/// and discovery observations; no actor decides that a changed failure is
/// close enough.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentHeadRequalification {
    pub current_head_commit: String,
    pub current_head_tree: String,
    pub first_actual_observation_artifact_id: ArtifactId,
    pub second_actual_observation_artifact_id: ArtifactId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimSponsoredTicket {
    pub principal: String,
    pub command_id: String,
    pub campaign_id: CampaignId,
    pub expected_campaign_revision: ExpectedRevision,
    pub ticket_revision_id: TicketRevisionId,
    pub expected_ticket_revision: ExpectedRevision,
    pub requalification: CurrentHeadRequalification,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaimOutcome {
    Claimed { ticket_attempt_id: TicketAttemptId },
    Resolved,
    Blocked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimTicketReceipt {
    pub ticket_id: TicketId,
    pub ticket_revision_id: TicketRevisionId,
    pub outcome: ClaimOutcome,
    pub resulting_ticket_revision: AggregateRevision,
    pub audit_log_id: i64,
    pub was_idempotent_retry: bool,
}

/// A paid actor/session/infrastructure failure ends the current attempt. It
/// cannot make the ticket claimable again; only [`ReleaseTicketAttempt`] can.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailTicketAttempt {
    pub principal: String,
    pub command_id: String,
    pub ticket_attempt_id: TicketAttemptId,
    pub expected_attempt_revision: ExpectedRevision,
    pub expected_ticket_revision: ExpectedRevision,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TicketAttemptReceipt {
    pub ticket_attempt_id: TicketAttemptId,
    pub resulting_attempt_revision: AggregateRevision,
    pub audit_log_id: i64,
    pub was_idempotent_retry: bool,
}

/// One kernel-owned Quality-only recovery for a terminal Quality-session
/// fault. It retains the exact validated candidate rather than requalifying
/// the Product ticket or launching Engineering again. A rework-Quality
/// failure is still terminal, so this transition cannot form an unbounded
/// paid retry loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetryQualityAttempt {
    pub principal: String,
    pub command_id: String,
    pub ticket_attempt_id: TicketAttemptId,
    pub candidate_id: CandidateId,
    pub expected_attempt_revision: ExpectedRevision,
    pub expected_ticket_revision: ExpectedRevision,
    pub reason: String,
}

/// One kernel-owned Engineering recovery for a terminal assignment/session
/// fault. It retains the claimed base and ticket identity, clears the failed
/// stage, and permits exactly one fresh Engineering packet/session. A second
/// fault remains terminal and requires the ordinary Architect release path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetryEngineeringAttempt {
    pub principal: String,
    pub command_id: String,
    pub ticket_attempt_id: TicketAttemptId,
    pub expected_attempt_revision: ExpectedRevision,
    pub expected_ticket_revision: ExpectedRevision,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineeringFailureContext {
    pub ticket_attempt_id: TicketAttemptId,
    pub attempt_revision: AggregateRevision,
    pub ticket_revision: AggregateRevision,
}

/// The current pair of durable optimistic-concurrency fences needed to close
/// an attempt after a daemon-owned assignment fault. This is read-only and is
/// not an alternate transition: [`FailTicketAttempt`] still locks and checks
/// both values atomically before it writes the terminal stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TicketAttemptFailureContext {
    pub attempt_revision: AggregateRevision,
    pub ticket_revision: AggregateRevision,
}

/// Explicitly releases a failed attempt after a successful current-head
/// requalification. A resolved or divergent fresh head remains terminal; it
/// never silently returns to the ready buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseTicketAttempt {
    pub principal: String,
    pub command_id: String,
    pub ticket_attempt_id: TicketAttemptId,
    pub expected_attempt_revision: ExpectedRevision,
    pub expected_ticket_revision: ExpectedRevision,
    pub reason: String,
    pub requalification: CurrentHeadRequalification,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReleaseOutcome {
    Released,
    Resolved,
    Blocked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseTicketReceipt {
    pub ticket_id: TicketId,
    pub ticket_revision_id: TicketRevisionId,
    pub ticket_attempt_id: TicketAttemptId,
    pub outcome: ReleaseOutcome,
    pub resulting_ticket_revision: AggregateRevision,
    pub resulting_attempt_revision: AggregateRevision,
    pub audit_log_id: i64,
    pub was_idempotent_retry: bool,
}

/// Delivery machinery in later tranches calls this only after it has recorded
/// an attempt's `Delivered` stage. Completion is derived from durable rows,
/// never supplied as a caller-owned count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompleteCampaignAtDeliveryTarget {
    pub principal: String,
    pub command_id: String,
    pub campaign_id: CampaignId,
    pub expected_campaign_revision: ExpectedRevision,
}

/// Read-only ticket-buffer pressure under the campaign's pinned application
/// revision. All counts are application-global except delivery target progress,
/// which belongs to the campaign that paid for its attempts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TicketBufferStatus {
    pub campaign_id: CampaignId,
    pub campaign_revision: AggregateRevision,
    pub campaign_is_running: bool,
    pub campaign_deadline_open: bool,
    pub campaign_cost_known: bool,
    pub delivery_target: u32,
    pub delivered_attempt_count: u32,
    pub ready_count: u32,
    pub proposed_count: u32,
    pub in_flight_count: u32,
    pub downstream_attempt_count: u32,
    /// The one oldest nonterminal attempt that must progress before the
    /// scheduler can replenish or claim unrelated work. It is derived by the
    /// same bounded read as `downstream_attempt_count`; a missing context with
    /// a nonzero count is intentionally left for the scheduler to fail closed.
    pub downstream_action: Option<DownstreamActionContext>,
    /// Bounded immutable evidence attached to the exact downstream candidate.
    /// It explains the action without making a status poll discover later
    /// candidates or issue an unbounded navigation query.
    pub downstream_evidence: Option<DownstreamEvidenceContext>,
    pub paid_session_active: bool,
    pub low_water: u32,
    pub target: u32,
    /// `None` is the admitted application's explicit unrestricted backlog
    /// policy. The scheduler therefore never blocks on ready-ticket count in
    /// that mode.
    pub maximum: Option<u32>,
    pub proposal_maximum: u32,
    /// The bounded FIFO head, including the exact optimistic-concurrency
    /// revision that a later claim must present. A scheduler read may race;
    /// this context makes a stale action fail closed at the claim boundary.
    pub oldest_sponsored_ticket: Option<SponsoredTicketClaimContext>,
}

/// Read-only identity of the oldest ready ticket that may be claimed. This is
/// intentionally not a capability: [`TicketStore::claim_sponsored_ticket`]
/// still rechecks lifecycle, campaign admission, global WIP, and the supplied
/// current-head requalification in one transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SponsoredTicketClaimContext {
    pub ticket_revision_id: TicketRevisionId,
    pub revision: AggregateRevision,
}

/// The exact stage at the FIFO head of already-started downstream work. The
/// scheduler exposes only these nine nonterminal stages, never a terminal or
/// fresh-Engineering attempt, so a daemon can choose its next bounded
/// preparation without rediscovering candidate state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownstreamActionStage {
    HardValidation,
    Quality,
    CandidateCommitAttachRequired,
    QualityReviewRequired,
    AwaitingArchitect,
    /// The Architect accepted this exact candidate for trusted delivery.
    /// Unlike the validated AwaitingArchitect form, this is an actionable
    /// delivery handoff, not a request for a Quality review.
    DeliverAccepted,
    ReworkEngineering,
    ReworkValidation,
    ReworkQuality,
}

impl DownstreamActionStage {
    /// Closed, stable status spelling for the daemon and local operator
    /// surfaces. These are not database enum names.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::HardValidation => "hard_validation",
            Self::Quality => "quality",
            Self::CandidateCommitAttachRequired => "candidate_commit_attach_required",
            Self::QualityReviewRequired => "quality_review_required",
            Self::AwaitingArchitect => "awaiting_architect",
            Self::DeliverAccepted => "deliver_accepted",
            Self::ReworkEngineering => "rework_engineering",
            Self::ReworkValidation => "rework_validation",
            Self::ReworkQuality => "rework_quality",
        }
    }

    fn from_attempt_and_candidate(
        attempt_stage: i16,
        candidate_lifecycle: i16,
        candidate_commit_present: bool,
        quality_review_present: bool,
    ) -> Option<Self> {
        if attempt_stage == ATTEMPT_HARD_VALIDATION && candidate_lifecycle == CANDIDATE_SUBMITTED {
            Some(Self::HardValidation)
        } else if attempt_stage == ATTEMPT_QUALITY && candidate_lifecycle == CANDIDATE_VALIDATED {
            Some(if candidate_commit_present {
                Self::Quality
            } else {
                Self::CandidateCommitAttachRequired
            })
        } else if attempt_stage == ATTEMPT_AWAITING_ARCHITECT
            && candidate_lifecycle == CANDIDATE_VALIDATED
        {
            Some(if quality_review_present {
                Self::AwaitingArchitect
            } else {
                Self::QualityReviewRequired
            })
        } else if attempt_stage == ATTEMPT_AWAITING_ARCHITECT
            && candidate_lifecycle == CANDIDATE_ACCEPTED
        {
            Some(Self::DeliverAccepted)
        } else if attempt_stage == ATTEMPT_REWORK_ENGINEERING
            && candidate_lifecycle == CANDIDATE_REJECTED
        {
            Some(Self::ReworkEngineering)
        } else if attempt_stage == ATTEMPT_REWORK_VALIDATION
            && candidate_lifecycle == CANDIDATE_SUBMITTED
        {
            Some(Self::ReworkValidation)
        } else if attempt_stage == ATTEMPT_REWORK_QUALITY
            && candidate_lifecycle == CANDIDATE_VALIDATED
        {
            Some(if candidate_commit_present {
                Self::ReworkQuality
            } else {
                Self::CandidateCommitAttachRequired
            })
        } else {
            None
        }
    }
}

/// A read-only, exact downstream head. Each revision fences a later stage
/// transition; the scheduler itself neither runs a command nor starts a
/// session. The owning `ticket_attempt` is FIFO-ordered by its durable
/// creation timestamp and identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DownstreamActionContext {
    pub stage: DownstreamActionStage,
    pub ticket_attempt_id: TicketAttemptId,
    pub ticket_attempt_revision: AggregateRevision,
    /// The ticket's exact optimistic revision lets terminal-failure custody
    /// release only this downstream work item without rediscovering it.
    pub ticket_revision: AggregateRevision,
    pub candidate_id: CandidateId,
    pub candidate_revision: AggregateRevision,
}

/// Closed immutable evidence currently attached to the downstream FIFO head.
/// The owning candidate remains the exact `DownstreamActionContext` candidate;
/// these rows cannot grant a transition and are presentation-only facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownstreamEvidenceContext {
    pub candidate_commit: Option<String>,
    pub latest_validation: Option<DownstreamValidationEvidence>,
    pub review: Option<DownstreamReviewEvidence>,
    pub architect_decision: Option<DownstreamArchitectDecisionEvidence>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DownstreamValidationEvidence {
    pub validation_id: ValidationId,
    pub state: DownstreamValidationState,
    pub log_artifact_id: ArtifactId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownstreamValidationState {
    Passed,
    Failed,
    Interrupted,
}

impl DownstreamValidationState {
    fn from_sql(value: i16) -> Result<Self, StoreError> {
        match value {
            1 => Ok(Self::Passed),
            2 => Ok(Self::Failed),
            3 => Ok(Self::Interrupted),
            _ => Err(StoreError::CorruptLifecycleColumn),
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DownstreamReviewEvidence {
    pub review_id: ReviewId,
    pub revision: AggregateRevision,
    pub verdict: DownstreamReviewVerdict,
    pub rationale_artifact_id: ArtifactId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownstreamReviewVerdict {
    Accept,
    Reject,
}

impl DownstreamReviewVerdict {
    fn from_sql(value: i16) -> Result<Self, StoreError> {
        match value {
            0 => Ok(Self::Accept),
            1 => Ok(Self::Reject),
            _ => Err(StoreError::CorruptLifecycleColumn),
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Reject => "reject",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DownstreamArchitectDecisionEvidence {
    pub architect_decision_id: ArchitectDecisionId,
    pub kind: DownstreamArchitectDecisionKind,
    pub rationale_artifact_id: ArtifactId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownstreamArchitectDecisionKind {
    Deliver,
    Rework,
    Reject,
}

impl DownstreamArchitectDecisionKind {
    fn from_sql(value: i16) -> Result<Self, StoreError> {
        match value {
            2 => Ok(Self::Deliver),
            3 => Ok(Self::Rework),
            4 => Ok(Self::Reject),
            _ => Err(StoreError::CorruptLifecycleColumn),
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Deliver => "deliver",
            Self::Rework => "rework",
            Self::Reject => "reject",
        }
    }
}

fn downstream_validation_evidence(
    validation_id: Option<i64>,
    lifecycle: Option<i16>,
    log_artifact_id: Option<i64>,
) -> Result<Option<DownstreamValidationEvidence>, StoreError> {
    match (validation_id, lifecycle, log_artifact_id) {
        (None, None, None) => Ok(None),
        (Some(validation_id), Some(lifecycle), Some(log_artifact_id)) => {
            Ok(Some(DownstreamValidationEvidence {
                validation_id: ValidationId::new(validation_id)?,
                state: DownstreamValidationState::from_sql(lifecycle)?,
                log_artifact_id: ArtifactId::new(log_artifact_id)?,
            }))
        }
        _ => Err(StoreError::CorruptLifecycleColumn),
    }
}

fn downstream_review_evidence(
    review_id: Option<i64>,
    revision: Option<i64>,
    verdict: Option<i16>,
    rationale_artifact_id: Option<i64>,
) -> Result<Option<DownstreamReviewEvidence>, StoreError> {
    match (review_id, revision, verdict, rationale_artifact_id) {
        (None, None, None, None) => Ok(None),
        (Some(review_id), Some(revision), Some(verdict), Some(rationale_artifact_id)) => {
            Ok(Some(DownstreamReviewEvidence {
                review_id: ReviewId::new(review_id)?,
                revision: revision_from_sql(revision)?,
                verdict: DownstreamReviewVerdict::from_sql(verdict)?,
                rationale_artifact_id: ArtifactId::new(rationale_artifact_id)?,
            }))
        }
        _ => Err(StoreError::CorruptLifecycleColumn),
    }
}

fn downstream_architect_decision_evidence(
    architect_decision_id: Option<i64>,
    kind: Option<i16>,
    rationale_artifact_id: Option<i64>,
) -> Result<Option<DownstreamArchitectDecisionEvidence>, StoreError> {
    match (architect_decision_id, kind, rationale_artifact_id) {
        (None, None, None) => Ok(None),
        (Some(architect_decision_id), Some(kind), Some(rationale_artifact_id)) => {
            Ok(Some(DownstreamArchitectDecisionEvidence {
                architect_decision_id: ArchitectDecisionId::new(architect_decision_id)?,
                kind: DownstreamArchitectDecisionKind::from_sql(kind)?,
                rationale_artifact_id: ArtifactId::new(rationale_artifact_id)?,
            }))
        }
        _ => Err(StoreError::CorruptLifecycleColumn),
    }
}

/// Read-only immutable application inputs needed for proposal admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposalAdmissionContext {
    pub application_revision_id: ApplicationRevisionId,
    pub aggregate_revision: AggregateRevision,
    pub bundle_artifact_id: ArtifactId,
    pub repository_id: factory_protocol::RepositoryId,
    pub proposal_maximum: u32,
    pub ticket_bounds: TicketBoundsV2,
}

/// A bounded live proposal artifact identity for application-side duplicate
/// interpretation. The kernel does not perform qualitative duplicate search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveTicketProposalArtifact {
    pub ticket_id: TicketId,
    pub ticket_revision_id: TicketRevisionId,
    pub state: TicketState,
    pub proposal_artifact_id: ArtifactId,
}

impl TicketStore {
    pub async fn proposal_admission_context(
        &self,
        application_revision_id: ApplicationRevisionId,
    ) -> Result<ProposalAdmissionContext, StoreError> {
        let row = sqlx::query!(
            "SELECT aggregate_revision, bundle_artifact_id, repository_id, proposal_maximum,
                    ticket_narrative_byte_limit, ticket_acceptance_criteria_limit,
                    ticket_contract_read_limit
             FROM factory.application_revisions WHERE id = $1",
            application_revision_id.get()
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::UnknownApplicationRevision {
            application_revision_id,
        })?;
        Ok(ProposalAdmissionContext {
            application_revision_id,
            aggregate_revision: revision_from_sql(row.aggregate_revision)?,
            bundle_artifact_id: ArtifactId::new(row.bundle_artifact_id)?,
            repository_id: factory_protocol::RepositoryId::new(row.repository_id)?,
            proposal_maximum: u32_from_sql(i64::from(row.proposal_maximum), "proposal maximum")?,
            ticket_bounds: TicketBoundsV2 {
                narrative_byte_limit: u32_from_sql(
                    i64::from(row.ticket_narrative_byte_limit),
                    "ticket narrative byte limit",
                )?,
                acceptance_criteria_limit: u16::try_from(row.ticket_acceptance_criteria_limit)
                    .map_err(|_| StoreError::InvalidTicketField {
                        field: "ticket acceptance criteria limit",
                    })?,
                contract_read_limit: u16::try_from(row.ticket_contract_read_limit).map_err(
                    |_| StoreError::InvalidTicketField {
                        field: "ticket contract read limit",
                    },
                )?,
            },
        })
    }

    pub async fn live_ticket_proposal_artifacts(
        &self,
        application_revision_id: ApplicationRevisionId,
    ) -> Result<Vec<LiveTicketProposalArtifact>, StoreError> {
        let rows = sqlx::query!(
            "SELECT tr.ticket_id, tr.id, tr.lifecycle, tr.proposal_artifact_id
             FROM factory.ticket_revisions AS tr
             JOIN factory.tickets AS t ON t.id = tr.ticket_id
             WHERE tr.application_revision_id = $1
               AND t.current_ticket_revision_id = tr.id
               AND tr.lifecycle IN ($2, $3, $4)
             ORDER BY tr.created_at ASC, tr.id ASC LIMIT 20",
            application_revision_id.get(),
            TICKET_PROPOSED,
            TICKET_SPONSORED,
            TICKET_IN_FLIGHT,
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(LiveTicketProposalArtifact {
                    ticket_id: TicketId::new(row.ticket_id)?,
                    ticket_revision_id: TicketRevisionId::new(row.id)?,
                    state: ticket_state_from_sql(row.lifecycle)?,
                    proposal_artifact_id: ArtifactId::new(row.proposal_artifact_id)?,
                })
            })
            .collect()
    }

    pub async fn submit_ticket_proposal(
        &self,
        command: &SubmitTicketProposal,
    ) -> Result<TicketProposalReceipt, StoreError> {
        validate_command(command.principal.as_str(), command.command_id.as_str())?;
        validate_snapshot(&command.discovery_commit, "discovery commit")?;
        validate_snapshot(&command.discovery_tree, "discovery tree")?;
        let fingerprint = proposal_fingerprint(command);
        let mut transaction = self.pool.begin().await?;
        let application = sqlx::query!(
            "SELECT application_key, aggregate_revision, proposal_maximum
             FROM factory.application_revisions WHERE id = $1 FOR SHARE",
            command.application_revision_id.get()
        )
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(StoreError::UnknownApplicationRevision {
            application_revision_id: command.application_revision_id,
        })?;
        application_advisory_lock(&mut transaction, &application.application_key).await?;
        if let Some(receipt) = find_ticket_audit(
            &mut transaction,
            &command.principal,
            &command.command_id,
            PROPOSE_OPERATION,
            fingerprint,
        )
        .await?
        {
            require_ticket_subject(&receipt, PROPOSAL_SUBJECT)?;
            let ticket_revision_id = TicketRevisionId::new(receipt.subject_id)?;
            let row = sqlx::query!(
                "SELECT ticket_id FROM factory.ticket_revisions WHERE id = $1",
                ticket_revision_id.get()
            )
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or(StoreError::UnknownTicketRevision { ticket_revision_id })?;
            transaction.commit().await?;
            return Ok(TicketProposalReceipt {
                ticket_id: TicketId::new(row.ticket_id)?,
                ticket_revision_id,
                resulting_revision: receipt.resulting_revision,
                audit_log_id: receipt.audit_log_id,
                was_idempotent_retry: true,
            });
        }
        let application_revision = revision_from_sql(application.aggregate_revision)?;
        if command.expected_application_revision.get() != application_revision {
            return Err(StoreError::RevisionConflict {
                expected: command.expected_application_revision,
                current: application_revision,
            });
        }
        let expected =
            artifact_digest(&mut transaction, command.expected_observation_artifact_id).await?;
        let first = artifact_digest(
            &mut transaction,
            command.first_actual_observation_artifact_id,
        )
        .await?;
        let second = artifact_digest(
            &mut transaction,
            command.second_actual_observation_artifact_id,
        )
        .await?;
        let _proposal = artifact_digest(&mut transaction, command.proposal_artifact_id).await?;
        let _reproducer = artifact_digest(&mut transaction, command.reproducer_artifact_id).await?;
        if first != second {
            return Err(StoreError::ProposalNotReproducible);
        }
        if first == expected {
            return Err(StoreError::ProposalDoesNotFail);
        }
        let proposal_count = application_ticket_count_in_transaction(
            &mut transaction,
            &application.application_key,
            TICKET_PROPOSED,
        )
        .await?;
        if proposal_count >= i64::from(application.proposal_maximum) {
            return Err(StoreError::ProposalBufferFull);
        }
        // A blocked ticket has no live work attached. It records why its
        // exact proposal could not proceed, but must not force a paid Product
        // rediscovery after a kernel-level reproduction correction changes the
        // canonical observation identity.
        let duplicate = sqlx::query!(
            "SELECT id FROM factory.ticket_revisions
             WHERE application_revision_id = $1 AND reproducer_artifact_id = $2
               AND lifecycle <> 4::SMALLINT",
            command.application_revision_id.get(),
            command.reproducer_artifact_id.get()
        )
        .fetch_optional(&mut *transaction)
        .await?;
        if duplicate.is_some() {
            return Err(StoreError::DuplicateTicketReproducer {
                reproducer_artifact_id: command.reproducer_artifact_id,
            });
        }
        // The ticket/current-revision relation is intentionally cyclic. Both
        // identities are allocated before either row exists, then the two
        // deferred foreign keys become valid together at transaction commit.
        let ticket_id = sqlx::query_scalar!(
            "SELECT nextval(pg_get_serial_sequence('factory.tickets', 'id')) AS \"id!\""
        )
        .fetch_one(&mut *transaction)
        .await?;
        let ticket_revision_id = sqlx::query_scalar!(
            "SELECT nextval(pg_get_serial_sequence('factory.ticket_revisions', 'id')) AS \"id!\""
        )
        .fetch_one(&mut *transaction)
        .await?;
        sqlx::query!(
            "INSERT INTO factory.tickets (
                 id, application_revision_id, lifecycle, current_ticket_revision_id
             ) OVERRIDING SYSTEM VALUE VALUES ($1, $2, $3, $4)",
            ticket_id,
            command.application_revision_id.get(),
            TICKET_PROPOSED,
            ticket_revision_id,
        )
        .execute(&mut *transaction)
        .await?;
        sqlx::query!(
            "INSERT INTO factory.ticket_revisions (
                 id, ticket_id, application_revision_id, revision_ordinal, lifecycle,
                 proposal_artifact_id, reproducer_artifact_id,
                 expected_observation_artifact_id, discovery_observation_artifact_id,
                 discovery_commit, discovery_tree
             ) OVERRIDING SYSTEM VALUE
             VALUES ($1, $2, $3, 1, $4, $5, $6, $7, $8, $9, $10)",
            ticket_revision_id,
            ticket_id,
            command.application_revision_id.get(),
            TICKET_PROPOSED,
            command.proposal_artifact_id.get(),
            command.reproducer_artifact_id.get(),
            command.expected_observation_artifact_id.get(),
            command.first_actual_observation_artifact_id.get(),
            &command.discovery_commit,
            &command.discovery_tree,
        )
        .execute(&mut *transaction)
        .await?;
        let audit_log_id = insert_ticket_audit(
            &mut transaction,
            &command.principal,
            &command.command_id,
            PROPOSE_OPERATION,
            fingerprint,
            PROPOSAL_SUBJECT,
            ticket_revision_id,
            AggregateRevision::initial(),
        )
        .await?;
        transaction.commit().await?;
        Ok(TicketProposalReceipt {
            ticket_id: TicketId::new(ticket_id)?,
            ticket_revision_id: TicketRevisionId::new(ticket_revision_id)?,
            resulting_revision: AggregateRevision::initial(),
            audit_log_id,
            was_idempotent_retry: false,
        })
    }

    pub async fn sponsor_ticket_revision(
        &self,
        command: &SponsorTicketRevision,
    ) -> Result<TicketRevisionReceipt, StoreError> {
        validate_command(command.principal.as_str(), command.command_id.as_str())?;
        validate_reason(&command.reason, "sponsorship reason")?;
        let fingerprint = sponsor_fingerprint(command);
        let mut transaction = self.pool.begin().await?;
        if let Some(receipt) = find_ticket_audit(
            &mut transaction,
            &command.principal,
            &command.command_id,
            SPONSOR_OPERATION,
            fingerprint,
        )
        .await?
        {
            require_ticket_subject(&receipt, SPONSORSHIP_SUBJECT)?;
            let status =
                ticket_revision_status(&mut transaction, command.ticket_revision_id).await?;
            transaction.commit().await?;
            return Ok(TicketRevisionReceipt {
                ticket_id: status.ticket_id,
                ticket_revision_id: command.ticket_revision_id,
                state: status.state,
                resulting_revision: receipt.resulting_revision,
                audit_log_id: receipt.audit_log_id,
                was_idempotent_retry: true,
            });
        }
        let status = lock_ticket_revision(&mut transaction, command.ticket_revision_id).await?;
        require_expected(command.expected_ticket_revision, status.revision)?;
        require_ticket_state(TicketState::Proposed, status.state)?;
        let application = sqlx::query!(
            "SELECT application_key, ticket_maximum AS \"ticket_maximum?\"
             FROM factory.application_revisions WHERE id = $1 FOR SHARE",
            status.application_revision_id.get()
        )
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(StoreError::UnknownApplicationRevision {
            application_revision_id: status.application_revision_id,
        })?;
        application_advisory_lock(&mut transaction, &application.application_key).await?;
        let sponsored_count = application_ticket_count_in_transaction(
            &mut transaction,
            &application.application_key,
            TICKET_SPONSORED,
        )
        .await?;
        if application
            .ticket_maximum
            .is_some_and(|maximum| sponsored_count >= i64::from(maximum))
        {
            return Err(StoreError::ReadyTicketBufferFull);
        }
        let next = status.revision.next()?;
        update_ticket_states(
            &mut transaction,
            &status,
            TICKET_SPONSORED,
            next,
            Some(&command.reason),
            None,
        )
        .await?;
        let audit_log_id = insert_ticket_audit(
            &mut transaction,
            &command.principal,
            &command.command_id,
            SPONSOR_OPERATION,
            fingerprint,
            SPONSORSHIP_SUBJECT,
            command.ticket_revision_id.get(),
            next,
        )
        .await?;
        transaction.commit().await?;
        Ok(TicketRevisionReceipt {
            ticket_id: status.ticket_id,
            ticket_revision_id: command.ticket_revision_id,
            state: TicketState::Sponsored,
            resulting_revision: next,
            audit_log_id,
            was_idempotent_retry: false,
        })
    }

    pub async fn claim_sponsored_ticket(
        &self,
        command: &ClaimSponsoredTicket,
    ) -> Result<ClaimTicketReceipt, StoreError> {
        validate_command(command.principal.as_str(), command.command_id.as_str())?;
        validate_requalification(&command.requalification)?;
        let fingerprint = claim_fingerprint(command);
        let mut transaction = self.pool.begin().await?;
        if let Some(receipt) = find_ticket_audit(
            &mut transaction,
            &command.principal,
            &command.command_id,
            CLAIM_OPERATION,
            fingerprint,
        )
        .await?
        {
            let outcome = claim_outcome_from_receipt(&receipt)?;
            let ticket_revision_id = match outcome {
                ClaimOutcome::Claimed { .. } => {
                    ticket_revision_for_attempt(
                        &mut transaction,
                        TicketAttemptId::new(receipt.subject_id)?,
                    )
                    .await?
                }
                ClaimOutcome::Resolved | ClaimOutcome::Blocked => command.ticket_revision_id,
            };
            let status = ticket_revision_status(&mut transaction, ticket_revision_id).await?;
            transaction.commit().await?;
            return Ok(ClaimTicketReceipt {
                ticket_id: status.ticket_id,
                ticket_revision_id,
                outcome,
                resulting_ticket_revision: receipt.resulting_revision,
                audit_log_id: receipt.audit_log_id,
                was_idempotent_retry: true,
            });
        }
        let status = lock_ticket_revision(&mut transaction, command.ticket_revision_id).await?;
        require_expected(command.expected_ticket_revision, status.revision)?;
        require_ticket_state(TicketState::Sponsored, status.state)?;
        validate_claim_campaign(&mut transaction, command, &status).await?;
        let outcome =
            classify_requalification(&mut transaction, &status, &command.requalification).await?;
        let next = status.revision.next()?;
        let (outcome, subject_kind, subject_id) = match outcome {
            RequalificationOutcome::Reproduced => {
                let attempt_id = sqlx::query_scalar!(
                    "INSERT INTO factory.ticket_attempts (
                         ticket_revision_id, campaign_id, claimed_commit, claimed_tree, stage
                     ) VALUES ($1, $2, $3, $4, $5) RETURNING id",
                    command.ticket_revision_id.get(),
                    command.campaign_id.get(),
                    &command.requalification.current_head_commit,
                    &command.requalification.current_head_tree,
                    ATTEMPT_ENGINEERING,
                )
                .fetch_one(&mut *transaction)
                .await?;
                update_requalification_and_ticket_state(
                    &mut transaction,
                    &status,
                    TICKET_IN_FLIGHT,
                    next,
                    REQUALIFICATION_REPRODUCED,
                    &command.requalification,
                    None,
                )
                .await?;
                (
                    ClaimOutcome::Claimed {
                        ticket_attempt_id: TicketAttemptId::new(attempt_id)?,
                    },
                    CLAIMED_ATTEMPT_SUBJECT,
                    attempt_id,
                )
            }
            RequalificationOutcome::Resolved => {
                update_requalification_and_ticket_state(
                    &mut transaction,
                    &status,
                    TICKET_RESOLVED,
                    next,
                    REQUALIFICATION_RESOLVED,
                    &command.requalification,
                    None,
                )
                .await?;
                (
                    ClaimOutcome::Resolved,
                    CLAIM_RESOLVED_SUBJECT,
                    command.ticket_revision_id.get(),
                )
            }
            RequalificationOutcome::Diverged => {
                update_requalification_and_ticket_state(
                    &mut transaction,
                    &status,
                    TICKET_BLOCKED,
                    next,
                    REQUALIFICATION_DIVERGED,
                    &command.requalification,
                    Some("current-head reproducer differs from the sponsored failure"),
                )
                .await?;
                (
                    ClaimOutcome::Blocked,
                    CLAIM_BLOCKED_SUBJECT,
                    command.ticket_revision_id.get(),
                )
            }
        };
        let audit_log_id = insert_ticket_audit(
            &mut transaction,
            &command.principal,
            &command.command_id,
            CLAIM_OPERATION,
            fingerprint,
            subject_kind,
            subject_id,
            next,
        )
        .await?;
        transaction.commit().await?;
        Ok(ClaimTicketReceipt {
            ticket_id: status.ticket_id,
            ticket_revision_id: command.ticket_revision_id,
            outcome,
            resulting_ticket_revision: next,
            audit_log_id,
            was_idempotent_retry: false,
        })
    }

    pub async fn fail_ticket_attempt(
        &self,
        command: &FailTicketAttempt,
    ) -> Result<TicketAttemptReceipt, StoreError> {
        validate_command(command.principal.as_str(), command.command_id.as_str())?;
        validate_reason(&command.reason, "failure reason")?;
        let fingerprint = failure_fingerprint(command);
        let mut transaction = self.pool.begin().await?;
        if let Some(receipt) = find_ticket_audit(
            &mut transaction,
            &command.principal,
            &command.command_id,
            FAIL_OPERATION,
            fingerprint,
        )
        .await?
        {
            require_ticket_subject(&receipt, FAILED_ATTEMPT_SUBJECT)?;
            transaction.commit().await?;
            return Ok(TicketAttemptReceipt {
                ticket_attempt_id: command.ticket_attempt_id,
                resulting_attempt_revision: receipt.resulting_revision,
                audit_log_id: receipt.audit_log_id,
                was_idempotent_retry: true,
            });
        }
        let attempt = lock_ticket_attempt(&mut transaction, command.ticket_attempt_id).await?;
        require_expected(command.expected_attempt_revision, attempt.revision)?;
        require_expected(command.expected_ticket_revision, attempt.ticket_revision)?;
        require_ticket_state(TicketState::InFlight, attempt.ticket_state)?;
        if !matches!(
            attempt.stage,
            ATTEMPT_ENGINEERING
                | ATTEMPT_HARD_VALIDATION
                | ATTEMPT_QUALITY
                | ATTEMPT_AWAITING_ARCHITECT
                | ATTEMPT_REWORK_ENGINEERING
                | ATTEMPT_REWORK_VALIDATION
                | ATTEMPT_REWORK_QUALITY
        ) {
            return Err(StoreError::TicketAttemptNotReleasable);
        }
        let next = attempt.revision.next()?;
        sqlx::query!(
            "UPDATE factory.ticket_attempts
             SET stage = $1, failed_at = CURRENT_TIMESTAMP, failure_reason = $2, revision = $3
             WHERE id = $4",
            ATTEMPT_FAILED,
            &command.reason,
            revision_to_sql(next)?,
            command.ticket_attempt_id.get(),
        )
        .execute(&mut *transaction)
        .await?;
        let audit_log_id = insert_ticket_audit(
            &mut transaction,
            &command.principal,
            &command.command_id,
            FAIL_OPERATION,
            fingerprint,
            FAILED_ATTEMPT_SUBJECT,
            command.ticket_attempt_id.get(),
            next,
        )
        .await?;
        transaction.commit().await?;
        Ok(TicketAttemptReceipt {
            ticket_attempt_id: command.ticket_attempt_id,
            resulting_attempt_revision: next,
            audit_log_id,
            was_idempotent_retry: false,
        })
    }

    /// Retries Quality exactly once after the session boundary itself fails.
    /// The candidate remains sealed and validated; only the attempt stage is
    /// advanced to the existing `ReworkQuality` scheduler head.
    pub async fn retry_quality_attempt(
        &self,
        command: &RetryQualityAttempt,
    ) -> Result<TicketAttemptReceipt, StoreError> {
        validate_command(command.principal.as_str(), command.command_id.as_str())?;
        validate_reason(&command.reason, "Quality retry reason")?;
        let fingerprint = quality_retry_fingerprint(command);
        let mut transaction = self.pool.begin().await?;
        if let Some(receipt) = find_ticket_audit(
            &mut transaction,
            &command.principal,
            &command.command_id,
            QUALITY_RETRY_OPERATION,
            fingerprint,
        )
        .await?
        {
            require_ticket_subject(&receipt, QUALITY_RETRY_SUBJECT)?;
            transaction.commit().await?;
            return Ok(TicketAttemptReceipt {
                ticket_attempt_id: command.ticket_attempt_id,
                resulting_attempt_revision: receipt.resulting_revision,
                audit_log_id: receipt.audit_log_id,
                was_idempotent_retry: true,
            });
        }
        let attempt = lock_ticket_attempt(&mut transaction, command.ticket_attempt_id).await?;
        require_expected(command.expected_attempt_revision, attempt.revision)?;
        require_expected(command.expected_ticket_revision, attempt.ticket_revision)?;
        require_ticket_state(TicketState::InFlight, attempt.ticket_state)?;
        let candidate = sqlx::query!(
            "SELECT lifecycle, candidate_commit
             FROM factory.candidates
             WHERE id = $1 AND ticket_attempt_id = $2
             FOR UPDATE",
            command.candidate_id.get(),
            command.ticket_attempt_id.get(),
        )
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(StoreError::TicketAttemptNotReleasable)?;
        let prior_quality_retry_exists = sqlx::query_scalar!(
            "SELECT EXISTS(
                 SELECT 1
                 FROM factory.audit_log
                 WHERE operation = $1
                   AND subject_kind = $2
                   AND subject_id = $3
             ) AS \"exists!\"",
            QUALITY_RETRY_OPERATION,
            QUALITY_RETRY_SUBJECT,
            command.ticket_attempt_id.get(),
        )
        .fetch_one(&mut *transaction)
        .await?;
        if !can_retry_quality_attempt(
            attempt.stage,
            candidate.lifecycle,
            candidate.candidate_commit.is_some(),
            prior_quality_retry_exists,
        ) {
            return Err(StoreError::TicketAttemptNotReleasable);
        }
        let next = attempt.revision.next()?;
        sqlx::query!(
            "UPDATE factory.ticket_attempts
             SET stage = $1, failed_at = CURRENT_TIMESTAMP, failure_reason = $2, revision = $3
             WHERE id = $4",
            ATTEMPT_REWORK_QUALITY,
            &command.reason,
            revision_to_sql(next)?,
            command.ticket_attempt_id.get(),
        )
        .execute(&mut *transaction)
        .await?;
        let audit_log_id = insert_ticket_audit(
            &mut transaction,
            &command.principal,
            &command.command_id,
            QUALITY_RETRY_OPERATION,
            fingerprint,
            QUALITY_RETRY_SUBJECT,
            command.ticket_attempt_id.get(),
            next,
        )
        .await?;
        transaction.commit().await?;
        Ok(TicketAttemptReceipt {
            ticket_attempt_id: command.ticket_attempt_id,
            resulting_attempt_revision: next,
            audit_log_id,
            was_idempotent_retry: false,
        })
    }

    /// Retries Engineering exactly once after a terminal assignment/session
    /// fault without changing the claimed ticket or base snapshot. The
    /// failed stage is cleared before the driver creates a fresh assignment;
    /// a second failure remains terminal for Architect release.
    pub async fn retry_engineering_attempt(
        &self,
        command: &RetryEngineeringAttempt,
    ) -> Result<TicketAttemptReceipt, StoreError> {
        validate_command(command.principal.as_str(), command.command_id.as_str())?;
        validate_reason(&command.reason, "Engineering retry reason")?;
        let fingerprint = engineering_retry_fingerprint(command);
        let mut transaction = self.pool.begin().await?;
        if let Some(receipt) = find_ticket_audit(
            &mut transaction,
            &command.principal,
            &command.command_id,
            ENGINEERING_RETRY_OPERATION,
            fingerprint,
        )
        .await?
        {
            require_ticket_subject(&receipt, ENGINEERING_RETRY_SUBJECT)?;
            transaction.commit().await?;
            return Ok(TicketAttemptReceipt {
                ticket_attempt_id: command.ticket_attempt_id,
                resulting_attempt_revision: receipt.resulting_revision,
                audit_log_id: receipt.audit_log_id,
                was_idempotent_retry: true,
            });
        }
        let attempt = lock_ticket_attempt(&mut transaction, command.ticket_attempt_id).await?;
        require_expected(command.expected_attempt_revision, attempt.revision)?;
        require_expected(command.expected_ticket_revision, attempt.ticket_revision)?;
        require_ticket_state(TicketState::InFlight, attempt.ticket_state)?;
        if attempt.released || attempt.stage != ATTEMPT_FAILED {
            return Err(StoreError::TicketAttemptNotReleasable);
        }
        let campaign = sqlx::query(
            "SELECT c.lifecycle, c.cost_state
             FROM factory.campaigns AS c
             JOIN factory.ticket_attempts AS ta ON ta.campaign_id = c.id
             WHERE ta.id = $1
             FOR SHARE",
        )
        .bind(command.ticket_attempt_id.get())
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(StoreError::TicketAttemptNotReleasable)?;
        if campaign.get::<i16, _>("lifecycle") != CAMPAIGN_RUNNING
            || campaign.get::<i16, _>("cost_state") != COST_KNOWN
        {
            return Err(StoreError::TicketAttemptNotReleasable);
        }
        let candidate_exists = sqlx::query(
            "SELECT EXISTS(
                 SELECT 1 FROM factory.candidates WHERE ticket_attempt_id = $1
             ) AS candidate_exists",
        )
        .bind(command.ticket_attempt_id.get())
        .fetch_one(&mut *transaction)
        .await?
        .get::<bool, _>("candidate_exists");
        let prior_retry_exists = sqlx::query(
            "SELECT EXISTS(
                 SELECT 1
                 FROM factory.audit_log
                 WHERE operation = $1
                   AND subject_kind = $2
                   AND subject_id = $3
             ) AS retry_exists",
        )
        .bind(ENGINEERING_RETRY_OPERATION)
        .bind(ENGINEERING_RETRY_SUBJECT)
        .bind(command.ticket_attempt_id.get())
        .fetch_one(&mut *transaction)
        .await?
        .get::<bool, _>("retry_exists");
        if !can_retry_engineering_attempt(attempt.stage, candidate_exists, prior_retry_exists) {
            return Err(StoreError::TicketAttemptNotReleasable);
        }
        let next = attempt.revision.next()?;
        sqlx::query(
            "UPDATE factory.ticket_attempts
             SET stage = $1, failed_at = NULL, failure_reason = NULL, revision = $2
             WHERE id = $3",
        )
        .bind(ATTEMPT_ENGINEERING)
        .bind(revision_to_sql(next)?)
        .bind(command.ticket_attempt_id.get())
        .execute(&mut *transaction)
        .await?;
        let audit_log_id = insert_ticket_audit(
            &mut transaction,
            &command.principal,
            &command.command_id,
            ENGINEERING_RETRY_OPERATION,
            fingerprint,
            ENGINEERING_RETRY_SUBJECT,
            command.ticket_attempt_id.get(),
            next,
        )
        .await?;
        transaction.commit().await?;
        Ok(TicketAttemptReceipt {
            ticket_attempt_id: command.ticket_attempt_id,
            resulting_attempt_revision: next,
            audit_log_id,
            was_idempotent_retry: false,
        })
    }

    /// Finds one failed, candidate-less Engineering attempt that has not yet
    /// consumed its single automatic retry. The campaign driver immediately
    /// consumes this read through [`Self::retry_engineering_attempt`].
    pub async fn recoverable_engineering_failure(
        &self,
        campaign_id: CampaignId,
    ) -> Result<Option<EngineeringFailureContext>, StoreError> {
        let row = sqlx::query(
            "SELECT ta.id AS ticket_attempt_id, ta.revision AS attempt_revision,
                    tr.revision AS ticket_revision
             FROM factory.ticket_attempts AS ta
             JOIN factory.ticket_revisions AS tr ON tr.id = ta.ticket_revision_id
             JOIN factory.tickets AS t ON t.id = tr.ticket_id
             JOIN factory.campaigns AS c ON c.id = ta.campaign_id
             WHERE ta.campaign_id = $1
               AND ta.stage = $2
               AND ta.released_at IS NULL
               AND t.current_ticket_revision_id = tr.id
               AND t.lifecycle = $3
               AND tr.lifecycle = $3
               AND c.lifecycle = $4
               AND c.cost_state = $5
               AND NOT EXISTS (
                   SELECT 1 FROM factory.candidates
                   WHERE ticket_attempt_id = ta.id
               )
               AND NOT EXISTS (
                   SELECT 1
                   FROM factory.audit_log AS retry
                   WHERE retry.operation = $6
                     AND retry.subject_kind = $7
                     AND retry.subject_id = ta.id
               )
             ORDER BY ta.failed_at ASC NULLS LAST, ta.id ASC
             LIMIT 1",
        )
        .bind(campaign_id.get())
        .bind(ATTEMPT_FAILED)
        .bind(TICKET_IN_FLIGHT)
        .bind(CAMPAIGN_RUNNING)
        .bind(COST_KNOWN)
        .bind(ENGINEERING_RETRY_OPERATION)
        .bind(ENGINEERING_RETRY_SUBJECT)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(EngineeringFailureContext {
                ticket_attempt_id: TicketAttemptId::new(row.get("ticket_attempt_id"))?,
                attempt_revision: revision_from_sql(row.get("attempt_revision"))?,
                ticket_revision: revision_from_sql(row.get("ticket_revision"))?,
            })
        })
        .transpose()
    }

    /// Finds one failed Quality attempt that can be recovered without changing
    /// its validated candidate. Recovery is limited to the newest candidate
    /// for each current ticket revision: a later Engineering attempt
    /// supersedes every earlier candidate, even when the earlier candidate has
    /// not received a Quality review. This is read-only; the driver must
    /// immediately consume the returned revisions through
    /// [`Self::retry_quality_attempt`].
    pub async fn recoverable_quality_failure(
        &self,
        campaign_id: CampaignId,
    ) -> Result<Option<DownstreamActionContext>, StoreError> {
        let row = sqlx::query!(
            "SELECT ta.id AS attempt_id, ta.revision AS attempt_revision,
                    tr.revision AS ticket_revision, c.id AS candidate_id,
                    c.revision AS candidate_revision
             FROM factory.ticket_attempts AS ta
             JOIN factory.ticket_revisions AS tr ON tr.id = ta.ticket_revision_id
             JOIN factory.tickets AS t ON t.id = tr.ticket_id
             JOIN factory.candidates AS c ON c.ticket_attempt_id = ta.id
             WHERE ta.campaign_id = $1
               AND ta.stage = $2
               AND t.current_ticket_revision_id = tr.id
               AND t.lifecycle = $3
               AND tr.lifecycle = $3
               AND c.lifecycle = $4
               AND c.candidate_commit IS NOT NULL
               AND NOT EXISTS (
                   SELECT 1 FROM factory.reviews AS r WHERE r.candidate_id = c.id
               )
               AND NOT EXISTS (
                   SELECT 1
                   FROM factory.audit_log AS retry
                   WHERE retry.operation = $5
                     AND retry.subject_kind = $6
                     AND retry.subject_id = ta.id
               )
               AND NOT EXISTS (
                   SELECT 1
                   FROM factory.candidates AS newer
                   JOIN factory.ticket_attempts AS newer_attempt
                     ON newer_attempt.id = newer.ticket_attempt_id
                   WHERE newer_attempt.ticket_revision_id = ta.ticket_revision_id
                     AND newer.id > c.id
               )
             ORDER BY ta.failed_at ASC NULLS LAST, ta.id ASC, c.id DESC
             LIMIT 1",
            campaign_id.get(),
            ATTEMPT_FAILED,
            TICKET_IN_FLIGHT,
            CANDIDATE_VALIDATED,
            QUALITY_RETRY_OPERATION,
            QUALITY_RETRY_SUBJECT,
        )
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(DownstreamActionContext {
                stage: DownstreamActionStage::ReworkQuality,
                ticket_attempt_id: TicketAttemptId::new(row.attempt_id)?,
                ticket_attempt_revision: revision_from_sql(row.attempt_revision)?,
                ticket_revision: revision_from_sql(row.ticket_revision)?,
                candidate_id: CandidateId::new(row.candidate_id)?,
                candidate_revision: revision_from_sql(row.candidate_revision)?,
            })
        })
        .transpose()
    }

    /// Reads the exact current failure fence without accepting any actor
    /// identity or state transition. The resident driver uses it only after a
    /// selected launch has failed, then immediately supplies it to the typed
    /// failure command above. A concurrent change turns that command into a
    /// revision conflict rather than a guessed retry.
    pub async fn failure_context(
        &self,
        ticket_attempt_id: TicketAttemptId,
    ) -> Result<TicketAttemptFailureContext, StoreError> {
        let mut transaction = self.pool.begin().await?;
        let attempt = ticket_attempt_status(&mut transaction, ticket_attempt_id).await?;
        transaction.commit().await?;
        Ok(TicketAttemptFailureContext {
            attempt_revision: attempt.revision,
            ticket_revision: attempt.ticket_revision,
        })
    }

    pub async fn release_ticket_attempt(
        &self,
        command: &ReleaseTicketAttempt,
    ) -> Result<ReleaseTicketReceipt, StoreError> {
        validate_command(command.principal.as_str(), command.command_id.as_str())?;
        validate_reason(&command.reason, "release reason")?;
        validate_requalification(&command.requalification)?;
        let fingerprint = release_fingerprint(command);
        let mut transaction = self.pool.begin().await?;
        if let Some(receipt) = find_ticket_audit(
            &mut transaction,
            &command.principal,
            &command.command_id,
            RELEASE_OPERATION,
            fingerprint,
        )
        .await?
        {
            let outcome = release_outcome_from_receipt(&receipt)?;
            let attempt =
                ticket_attempt_status(&mut transaction, command.ticket_attempt_id).await?;
            transaction.commit().await?;
            return Ok(ReleaseTicketReceipt {
                ticket_id: attempt.ticket_id,
                ticket_revision_id: attempt.ticket_revision_id,
                ticket_attempt_id: command.ticket_attempt_id,
                outcome,
                resulting_ticket_revision: attempt.ticket.revision,
                resulting_attempt_revision: receipt.resulting_revision,
                audit_log_id: receipt.audit_log_id,
                was_idempotent_retry: true,
            });
        }
        let attempt = lock_ticket_attempt(&mut transaction, command.ticket_attempt_id).await?;
        require_expected(command.expected_attempt_revision, attempt.revision)?;
        require_expected(command.expected_ticket_revision, attempt.ticket_revision)?;
        require_ticket_state(TicketState::InFlight, attempt.ticket_state)?;
        if attempt.released {
            return Err(StoreError::TicketAttemptAlreadyReleased);
        }
        if !matches!(attempt.stage, ATTEMPT_FAILED | ATTEMPT_CANCELLED) {
            return Err(StoreError::TicketAttemptNotReleasable);
        }
        let outcome =
            classify_requalification(&mut transaction, &attempt.ticket, &command.requalification)
                .await?;
        let next_ticket = attempt.ticket_revision.next()?;
        let next_attempt = attempt.revision.next()?;
        let (outcome, ticket_state, outcome_code, blocked_reason, subject_kind) = match outcome {
            RequalificationOutcome::Reproduced => (
                ReleaseOutcome::Released,
                TICKET_SPONSORED,
                REQUALIFICATION_REPRODUCED,
                None,
                RELEASED_ATTEMPT_SUBJECT,
            ),
            RequalificationOutcome::Resolved => (
                ReleaseOutcome::Resolved,
                TICKET_RESOLVED,
                REQUALIFICATION_RESOLVED,
                None,
                RELEASE_RESOLVED_SUBJECT,
            ),
            RequalificationOutcome::Diverged => (
                ReleaseOutcome::Blocked,
                TICKET_BLOCKED,
                REQUALIFICATION_DIVERGED,
                Some("current-head reproducer differs from the sponsored failure"),
                RELEASE_BLOCKED_SUBJECT,
            ),
        };
        if ticket_state == TICKET_SPONSORED {
            let application = sqlx::query!(
                "SELECT application_key, ticket_maximum AS \"ticket_maximum?\"
                 FROM factory.application_revisions WHERE id = $1 FOR SHARE",
                attempt.ticket.application_revision_id.get()
            )
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or(StoreError::UnknownApplicationRevision {
                application_revision_id: attempt.ticket.application_revision_id,
            })?;
            application_advisory_lock(&mut transaction, &application.application_key).await?;
            let ready = application_ticket_count_in_transaction(
                &mut transaction,
                &application.application_key,
                TICKET_SPONSORED,
            )
            .await?;
            if application
                .ticket_maximum
                .is_some_and(|maximum| ready >= i64::from(maximum))
            {
                return Err(StoreError::ReadyTicketBufferFull);
            }
        }
        sqlx::query!(
            "UPDATE factory.ticket_attempts
             SET released_at = CURRENT_TIMESTAMP, release_reason = $1, revision = $2
             WHERE id = $3",
            &command.reason,
            revision_to_sql(next_attempt)?,
            command.ticket_attempt_id.get(),
        )
        .execute(&mut *transaction)
        .await?;
        update_requalification_and_ticket_state(
            &mut transaction,
            &attempt.ticket,
            ticket_state,
            next_ticket,
            outcome_code,
            &command.requalification,
            blocked_reason,
        )
        .await?;
        let audit_log_id = insert_ticket_audit(
            &mut transaction,
            &command.principal,
            &command.command_id,
            RELEASE_OPERATION,
            fingerprint,
            subject_kind,
            command.ticket_attempt_id.get(),
            next_attempt,
        )
        .await?;
        transaction.commit().await?;
        Ok(ReleaseTicketReceipt {
            ticket_id: attempt.ticket_id,
            ticket_revision_id: attempt.ticket_revision_id,
            ticket_attempt_id: command.ticket_attempt_id,
            outcome,
            resulting_ticket_revision: next_ticket,
            resulting_attempt_revision: next_attempt,
            audit_log_id,
            was_idempotent_retry: false,
        })
    }

    /// Completes a running campaign only when durable delivered attempts meet
    /// its target. This is deliberately a separate exact-revision transition:
    /// ticket inventory survives and is not refilled after successful delivery.
    pub async fn complete_campaign_at_delivery_target(
        &self,
        command: &CompleteCampaignAtDeliveryTarget,
    ) -> Result<AggregateRevision, StoreError> {
        validate_command(command.principal.as_str(), command.command_id.as_str())?;
        let fingerprint = completion_fingerprint(command);
        let mut transaction = self.pool.begin().await?;
        if let Some(receipt) = find_ticket_audit(
            &mut transaction,
            &command.principal,
            &command.command_id,
            COMPLETE_CAMPAIGN_OPERATION,
            fingerprint,
        )
        .await?
        {
            require_ticket_subject(&receipt, CAMPAIGN_COMPLETED_SUBJECT)?;
            transaction.commit().await?;
            return Ok(receipt.resulting_revision);
        }
        let campaign = sqlx::query!(
            "SELECT lifecycle, revision, delivery_target FROM factory.campaigns
             WHERE id = $1 FOR UPDATE",
            command.campaign_id.get()
        )
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(StoreError::UnknownCampaign {
            campaign_id: command.campaign_id,
        })?;
        let revision = revision_from_sql(campaign.revision)?;
        require_expected(command.expected_campaign_revision, revision)?;
        if campaign.lifecycle != CAMPAIGN_RUNNING {
            return Err(StoreError::CampaignClosed {
                campaign_id: command.campaign_id,
            });
        }
        let delivered = sqlx::query_scalar!(
            "SELECT count(*)::BIGINT AS \"count!\" FROM factory.ticket_attempts
             WHERE campaign_id = $1 AND stage = $2",
            command.campaign_id.get(),
            ATTEMPT_DELIVERED,
        )
        .fetch_one(&mut *transaction)
        .await?;
        if delivered < i64::from(campaign.delivery_target) {
            return Err(StoreError::CampaignDeliveryTargetNotReached);
        }
        let next = revision.next()?;
        sqlx::query!(
            "UPDATE factory.campaigns SET lifecycle = $1, revision = $2 WHERE id = $3",
            CAMPAIGN_COMPLETED,
            revision_to_sql(next)?,
            command.campaign_id.get(),
        )
        .execute(&mut *transaction)
        .await?;
        insert_ticket_audit(
            &mut transaction,
            &command.principal,
            &command.command_id,
            COMPLETE_CAMPAIGN_OPERATION,
            fingerprint,
            CAMPAIGN_COMPLETED_SUBJECT,
            command.campaign_id.get(),
            next,
        )
        .await?;
        transaction.commit().await?;
        Ok(next)
    }

    /// Computes ticket pressure without inserting an audit row or any polling
    /// fact. This is the only input to the deterministic scheduler decision.
    pub async fn ticket_buffer_status(
        &self,
        campaign_id: CampaignId,
    ) -> Result<TicketBufferStatus, StoreError> {
        let campaign = sqlx::query!(
            "SELECT c.lifecycle, c.revision, c.delivery_target, c.cost_state,
                    c.deadline > CURRENT_TIMESTAMP AS \"deadline_open!\",
                    ar.id AS application_revision_id, ar.application_key,
                    ar.ticket_low_water, ar.ticket_target,
                    ar.ticket_maximum, ar.proposal_maximum
             FROM factory.campaigns AS c
             JOIN factory.application_revisions AS ar ON ar.id = c.application_revision_id
             WHERE c.id = $1",
            campaign_id.get()
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::UnknownCampaign { campaign_id })?;
        let application_revision_id = ApplicationRevisionId::new(campaign.application_revision_id)?;
        let ready = application_revision_ticket_count(
            &self.pool,
            application_revision_id,
            TICKET_SPONSORED,
        )
        .await?;
        let proposed =
            application_revision_ticket_count(&self.pool, application_revision_id, TICKET_PROPOSED)
                .await?;
        let in_flight = application_revision_ticket_count(
            &self.pool,
            application_revision_id,
            TICKET_IN_FLIGHT,
        )
        .await?;
        // Count and FIFO head come from one bounded statement so a status
        // projection cannot mix a count from one snapshot with a candidate
        // from another. A candidate-less or lifecycle-inconsistent head is
        // represented as `None`; `TicketScheduler::decide` then blocks rather
        // than skipping work and selecting a later row.
        let downstream = sqlx::query!(
            "WITH downstream AS (
                 SELECT ta.id, ta.revision, ta.stage, ta.created_at,
                        tr.revision AS ticket_revision
                 FROM factory.ticket_attempts AS ta
                 JOIN factory.ticket_revisions AS tr ON tr.id = ta.ticket_revision_id
                 WHERE tr.application_revision_id = $1
                   AND ta.stage BETWEEN $2 AND $3
             )
             SELECT (SELECT count(*)::BIGINT FROM downstream) AS \"count!\",
                    head.id AS \"ticket_attempt_id?\",
                    head.revision AS \"ticket_attempt_revision?\",
                    head.ticket_revision AS \"ticket_revision?\",
                    head.stage AS \"ticket_attempt_stage?\",
                    candidate.id AS \"candidate_id?\",
                    candidate.revision AS \"candidate_revision?\",
                    candidate.lifecycle AS \"candidate_lifecycle?\",
                    candidate.candidate_commit AS \"candidate_commit?\",
                    candidate.candidate_commit IS NOT NULL AS \"candidate_commit_present?\",
                    validation.id AS \"validation_id?\",
                    validation.lifecycle AS \"validation_lifecycle?\",
                    validation.log_artifact_id AS \"validation_log_artifact_id?\",
                    review.id AS \"quality_review_id?\",
                    review.revision AS \"review_revision?\",
                    review.verdict AS \"review_verdict?\",
                    review.rationale_artifact_id AS \"review_rationale_artifact_id?\",
                    decision.id AS \"architect_decision_id?\",
                    decision.decision_kind AS \"architect_decision_kind?\",
                    decision.rationale_artifact_id AS \"architect_decision_rationale_artifact_id?\"
             FROM (SELECT 1) AS singleton
             LEFT JOIN LATERAL (
                 SELECT id, revision, stage, ticket_revision
                 FROM downstream
                 ORDER BY created_at ASC, id ASC
                 LIMIT 1
             ) AS head ON TRUE
             LEFT JOIN LATERAL (
                 SELECT id, revision, lifecycle, candidate_commit
                 FROM factory.candidates
                 WHERE ticket_attempt_id = head.id
                 ORDER BY created_at DESC, id DESC
                 LIMIT 1
             ) AS candidate ON TRUE
             LEFT JOIN LATERAL (
                 SELECT id, lifecycle, log_artifact_id
                 FROM factory.validations
                 WHERE candidate_id = candidate.id
                 ORDER BY created_at DESC, id DESC
                 LIMIT 1
             ) AS validation ON TRUE
             LEFT JOIN factory.reviews AS review ON review.candidate_id = candidate.id
             LEFT JOIN LATERAL (
                 SELECT id, decision_kind, rationale_artifact_id
                 FROM factory.architect_decisions
                 WHERE candidate_id = candidate.id
                 ORDER BY created_at DESC, id DESC
                 LIMIT 1
             ) AS decision ON TRUE",
            application_revision_id.get(),
            ATTEMPT_HARD_VALIDATION,
            ATTEMPT_REWORK_QUALITY,
        )
        .fetch_one(&self.pool)
        .await?;
        let paid_session_active = sqlx::query_scalar!(
            "SELECT EXISTS (SELECT 1 FROM factory.sessions WHERE lifecycle = $1) AS \"active!\"",
            SESSION_RUNNING
        )
        .fetch_one(&self.pool)
        .await?;
        let delivered = sqlx::query_scalar!(
            "SELECT count(*)::BIGINT AS \"count!\" FROM factory.ticket_attempts
             WHERE campaign_id = $1 AND stage = $2",
            campaign_id.get(),
            ATTEMPT_DELIVERED,
        )
        .fetch_one(&self.pool)
        .await?;
        let oldest = sqlx::query!(
            "SELECT tr.id, tr.revision
             FROM factory.ticket_revisions AS tr
             JOIN factory.tickets AS t ON t.id = tr.ticket_id
             WHERE tr.application_revision_id = $1
               AND t.current_ticket_revision_id = tr.id
               AND tr.lifecycle = $2
             ORDER BY tr.sponsored_at ASC, tr.id ASC LIMIT 1",
            application_revision_id.get(),
            TICKET_SPONSORED
        )
        .fetch_optional(&self.pool)
        .await?;
        let downstream_evidence = match downstream.candidate_id {
            Some(_) => Some(DownstreamEvidenceContext {
                candidate_commit: downstream.candidate_commit,
                latest_validation: downstream_validation_evidence(
                    downstream.validation_id,
                    downstream.validation_lifecycle,
                    downstream.validation_log_artifact_id,
                )?,
                review: downstream_review_evidence(
                    downstream.quality_review_id,
                    downstream.review_revision,
                    downstream.review_verdict,
                    downstream.review_rationale_artifact_id,
                )?,
                architect_decision: downstream_architect_decision_evidence(
                    downstream.architect_decision_id,
                    downstream.architect_decision_kind,
                    downstream.architect_decision_rationale_artifact_id,
                )?,
            }),
            None => {
                if downstream.validation_id.is_some()
                    || downstream.quality_review_id.is_some()
                    || downstream.architect_decision_id.is_some()
                {
                    return Err(StoreError::CorruptLifecycleColumn);
                }
                None
            }
        };
        Ok(TicketBufferStatus {
            campaign_id,
            campaign_revision: revision_from_sql(campaign.revision)?,
            campaign_is_running: campaign.lifecycle == CAMPAIGN_RUNNING,
            campaign_deadline_open: campaign.deadline_open,
            campaign_cost_known: campaign.cost_state == COST_KNOWN,
            delivery_target: u32_from_sql(i64::from(campaign.delivery_target), "delivery target")?,
            delivered_attempt_count: u32_from_sql(delivered, "delivered attempt count")?,
            ready_count: u32_from_sql(ready, "ready ticket count")?,
            proposed_count: u32_from_sql(proposed, "proposed ticket count")?,
            in_flight_count: u32_from_sql(in_flight, "in-flight ticket count")?,
            downstream_attempt_count: u32_from_sql(downstream.count, "downstream attempt count")?,
            downstream_action: match (
                downstream.ticket_attempt_id,
                downstream.ticket_attempt_revision,
                downstream.ticket_revision,
                downstream.ticket_attempt_stage,
                downstream.candidate_id,
                downstream.candidate_revision,
                downstream.candidate_lifecycle,
                downstream.candidate_commit_present,
            ) {
                (
                    Some(ticket_attempt_id),
                    Some(ticket_attempt_revision),
                    Some(ticket_revision),
                    Some(ticket_attempt_stage),
                    Some(candidate_id),
                    Some(candidate_revision),
                    Some(candidate_lifecycle),
                    Some(candidate_commit_present),
                ) => DownstreamActionStage::from_attempt_and_candidate(
                    ticket_attempt_stage,
                    candidate_lifecycle,
                    candidate_commit_present,
                    downstream.quality_review_id.is_some(),
                )
                .map(|stage| -> Result<DownstreamActionContext, StoreError> {
                    Ok(DownstreamActionContext {
                        stage,
                        ticket_attempt_id: TicketAttemptId::new(ticket_attempt_id)?,
                        ticket_attempt_revision: revision_from_sql(ticket_attempt_revision)?,
                        ticket_revision: revision_from_sql(ticket_revision)?,
                        candidate_id: CandidateId::new(candidate_id)?,
                        candidate_revision: revision_from_sql(candidate_revision)?,
                    })
                })
                .transpose()?,
                _ => None,
            },
            downstream_evidence,
            paid_session_active,
            low_water: u32_from_sql(i64::from(campaign.ticket_low_water), "low water")?,
            target: u32_from_sql(i64::from(campaign.ticket_target), "ticket target")?,
            maximum: campaign
                .ticket_maximum
                .map(|maximum| u32_from_sql(i64::from(maximum), "ticket maximum"))
                .transpose()?,
            proposal_maximum: u32_from_sql(
                i64::from(campaign.proposal_maximum),
                "proposal maximum",
            )?,
            oldest_sponsored_ticket: oldest
                .map(|row| -> Result<SponsoredTicketClaimContext, StoreError> {
                    Ok(SponsoredTicketClaimContext {
                        ticket_revision_id: TicketRevisionId::new(row.id)?,
                        revision: revision_from_sql(row.revision)?,
                    })
                })
                .transpose()?,
        })
    }
}

#[derive(Clone, Debug)]
struct TicketRevisionLocked {
    ticket_id: TicketId,
    ticket_revision_id: TicketRevisionId,
    application_revision_id: ApplicationRevisionId,
    state: TicketState,
    revision: AggregateRevision,
    expected_observation_artifact_id: ArtifactId,
    discovery_observation_artifact_id: ArtifactId,
}

#[derive(Clone, Debug)]
struct TicketAttemptLocked {
    ticket_id: TicketId,
    ticket_revision_id: TicketRevisionId,
    stage: i16,
    released: bool,
    revision: AggregateRevision,
    ticket_revision: AggregateRevision,
    ticket_state: TicketState,
    ticket: TicketRevisionLocked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequalificationOutcome {
    Reproduced,
    Resolved,
    Diverged,
}

/// A campaign is pinned to one immutable application revision. Its queue must
/// ignore tickets from earlier revisions of the same application key: those
/// tickets cannot be requalified or materialized under the new bundle.
async fn application_revision_ticket_count(
    pool: &PgPool,
    application_revision_id: ApplicationRevisionId,
    state: i16,
) -> Result<i64, StoreError> {
    Ok(sqlx::query_scalar!(
        "SELECT count(*)::BIGINT AS \"count!\"
         FROM factory.ticket_revisions AS tr
         JOIN factory.tickets AS t ON t.id = tr.ticket_id
         WHERE tr.application_revision_id = $1
           AND t.current_ticket_revision_id = tr.id
           AND tr.lifecycle = $2",
        application_revision_id.get(),
        state,
    )
    .fetch_one(pool)
    .await?)
}

async fn application_ticket_count_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    application_key: &str,
    state: i16,
) -> Result<i64, StoreError> {
    Ok(sqlx::query_scalar!(
        "SELECT count(*)::BIGINT AS \"count!\"
         FROM factory.ticket_revisions AS tr
         JOIN factory.tickets AS t ON t.id = tr.ticket_id
         JOIN factory.application_revisions AS ticket_application
           ON ticket_application.id = tr.application_revision_id
         WHERE ticket_application.application_key = $1
           AND t.current_ticket_revision_id = tr.id
           AND tr.lifecycle = $2",
        application_key,
        state,
    )
    .fetch_one(&mut **transaction)
    .await?)
}

async fn application_revision_ticket_count_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    application_revision_id: ApplicationRevisionId,
    state: i16,
) -> Result<i64, StoreError> {
    Ok(sqlx::query_scalar!(
        "SELECT count(*)::BIGINT AS \"count!\"
         FROM factory.ticket_revisions AS tr
         JOIN factory.tickets AS t ON t.id = tr.ticket_id
         WHERE tr.application_revision_id = $1
           AND t.current_ticket_revision_id = tr.id
           AND tr.lifecycle = $2",
        application_revision_id.get(),
        state,
    )
    .fetch_one(&mut **transaction)
    .await?)
}

async fn validate_claim_campaign(
    transaction: &mut Transaction<'_, Postgres>,
    command: &ClaimSponsoredTicket,
    ticket: &TicketRevisionLocked,
) -> Result<(), StoreError> {
    let campaign = sqlx::query!(
        "SELECT c.lifecycle, c.revision, c.cost_state,
                c.deadline > CURRENT_TIMESTAMP AS \"deadline_open!\",
                c.application_revision_id AS campaign_application_revision_id,
                campaign_application.application_key AS campaign_application_key,
                ticket_application.id AS ticket_application_revision_id
         FROM factory.campaigns AS c
         JOIN factory.application_revisions AS campaign_application
           ON campaign_application.id = c.application_revision_id
         JOIN factory.application_revisions AS ticket_application
           ON ticket_application.id = $2
         WHERE c.id = $1 FOR SHARE",
        command.campaign_id.get(),
        ticket.application_revision_id.get(),
    )
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(StoreError::UnknownCampaign {
        campaign_id: command.campaign_id,
    })?;
    require_expected(
        command.expected_campaign_revision,
        revision_from_sql(campaign.revision)?,
    )?;
    if campaign.lifecycle != CAMPAIGN_RUNNING {
        return Err(StoreError::CampaignClosed {
            campaign_id: command.campaign_id,
        });
    }
    if !campaign.deadline_open {
        return Err(StoreError::CampaignDeadlineElapsed);
    }
    if campaign.cost_state != COST_KNOWN {
        return Err(StoreError::CampaignCostFrozen {
            campaign_id: command.campaign_id,
        });
    }
    if campaign.campaign_application_revision_id != campaign.ticket_application_revision_id {
        return Err(StoreError::CampaignApplicationMismatch);
    }
    // A claim becomes durable before assignment/session creation. Serialize
    // the stable application key here, rather than inferring Engineering WIP
    // from a later running session, so two schedulers cannot claim distinct
    // ready tickets in the gap between those transitions.
    application_advisory_lock(transaction, &campaign.campaign_application_key).await?;
    let in_flight = application_revision_ticket_count_in_transaction(
        transaction,
        ticket.application_revision_id,
        TICKET_IN_FLIGHT,
    )
    .await?;
    if in_flight > 0 {
        return Err(StoreError::EngineeringTicketAlreadyInFlight);
    }
    let paid_session_active = sqlx::query_scalar!(
        "SELECT EXISTS (SELECT 1 FROM factory.sessions WHERE lifecycle = $1) AS \"active!\"",
        SESSION_RUNNING
    )
    .fetch_one(&mut **transaction)
    .await?;
    if paid_session_active {
        return Err(StoreError::PaidSessionAlreadyRunning);
    }
    Ok(())
}

async fn lock_ticket_revision(
    transaction: &mut Transaction<'_, Postgres>,
    ticket_revision_id: TicketRevisionId,
) -> Result<TicketRevisionLocked, StoreError> {
    let row = sqlx::query!(
        "SELECT tr.ticket_id, tr.application_revision_id, tr.lifecycle AS ticket_revision_lifecycle,
                tr.revision AS ticket_revision_aggregate_revision,
                tr.expected_observation_artifact_id, tr.discovery_observation_artifact_id,
                t.lifecycle AS ticket_lifecycle, t.revision AS ticket_aggregate_revision,
                t.current_ticket_revision_id
         FROM factory.ticket_revisions AS tr
         JOIN factory.tickets AS t ON t.id = tr.ticket_id
         WHERE tr.id = $1 FOR UPDATE OF tr, t",
        ticket_revision_id.get()
    )
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(StoreError::UnknownTicketRevision { ticket_revision_id })?;
    if row.current_ticket_revision_id != ticket_revision_id.get() {
        return Err(StoreError::CorruptTicketState);
    }
    let state = ticket_state_from_sql(row.ticket_revision_lifecycle)?;
    if state != ticket_state_from_sql(row.ticket_lifecycle)? {
        return Err(StoreError::CorruptTicketState);
    }
    if row.ticket_revision_aggregate_revision != row.ticket_aggregate_revision {
        return Err(StoreError::CorruptTicketState);
    }
    Ok(TicketRevisionLocked {
        ticket_id: TicketId::new(row.ticket_id)?,
        ticket_revision_id,
        application_revision_id: ApplicationRevisionId::new(row.application_revision_id)?,
        state,
        revision: revision_from_sql(row.ticket_revision_aggregate_revision)?,
        expected_observation_artifact_id: ArtifactId::new(row.expected_observation_artifact_id)?,
        discovery_observation_artifact_id: ArtifactId::new(row.discovery_observation_artifact_id)?,
    })
}

async fn ticket_revision_status(
    transaction: &mut Transaction<'_, Postgres>,
    ticket_revision_id: TicketRevisionId,
) -> Result<TicketRevisionLocked, StoreError> {
    lock_ticket_revision(transaction, ticket_revision_id).await
}

async fn lock_ticket_attempt(
    transaction: &mut Transaction<'_, Postgres>,
    ticket_attempt_id: TicketAttemptId,
) -> Result<TicketAttemptLocked, StoreError> {
    let row = sqlx::query!(
        "SELECT ta.ticket_revision_id, ta.stage, ta.released_at IS NOT NULL AS \"released!\",
                ta.revision AS attempt_revision,
                tr.ticket_id, tr.application_revision_id,
                tr.lifecycle AS ticket_revision_lifecycle,
                tr.revision AS ticket_revision_aggregate_revision,
                tr.expected_observation_artifact_id, tr.discovery_observation_artifact_id,
                t.lifecycle AS ticket_lifecycle, t.revision AS ticket_aggregate_revision,
                t.current_ticket_revision_id
         FROM factory.ticket_attempts AS ta
         JOIN factory.ticket_revisions AS tr ON tr.id = ta.ticket_revision_id
         JOIN factory.tickets AS t ON t.id = tr.ticket_id
         WHERE ta.id = $1 FOR UPDATE OF ta, tr, t",
        ticket_attempt_id.get()
    )
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(StoreError::UnknownTicketAttempt { ticket_attempt_id })?;
    let ticket_revision_id = TicketRevisionId::new(row.ticket_revision_id)?;
    if row.current_ticket_revision_id != ticket_revision_id.get() {
        return Err(StoreError::CorruptTicketState);
    }
    let ticket_state = ticket_state_from_sql(row.ticket_revision_lifecycle)?;
    if ticket_state != ticket_state_from_sql(row.ticket_lifecycle)? {
        return Err(StoreError::CorruptTicketState);
    }
    if row.ticket_revision_aggregate_revision != row.ticket_aggregate_revision {
        return Err(StoreError::CorruptTicketState);
    }
    let ticket = TicketRevisionLocked {
        ticket_id: TicketId::new(row.ticket_id)?,
        ticket_revision_id,
        application_revision_id: ApplicationRevisionId::new(row.application_revision_id)?,
        state: ticket_state,
        revision: revision_from_sql(row.ticket_revision_aggregate_revision)?,
        expected_observation_artifact_id: ArtifactId::new(row.expected_observation_artifact_id)?,
        discovery_observation_artifact_id: ArtifactId::new(row.discovery_observation_artifact_id)?,
    };
    Ok(TicketAttemptLocked {
        ticket_id: ticket.ticket_id,
        ticket_revision_id,
        stage: row.stage,
        released: row.released,
        revision: revision_from_sql(row.attempt_revision)?,
        ticket_revision: ticket.revision,
        ticket_state,
        ticket,
    })
}

async fn ticket_attempt_status(
    transaction: &mut Transaction<'_, Postgres>,
    ticket_attempt_id: TicketAttemptId,
) -> Result<TicketAttemptLocked, StoreError> {
    lock_ticket_attempt(transaction, ticket_attempt_id).await
}

async fn ticket_revision_for_attempt(
    transaction: &mut Transaction<'_, Postgres>,
    ticket_attempt_id: TicketAttemptId,
) -> Result<TicketRevisionId, StoreError> {
    let row = sqlx::query!(
        "SELECT ticket_revision_id FROM factory.ticket_attempts WHERE id = $1",
        ticket_attempt_id.get()
    )
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(StoreError::UnknownTicketAttempt { ticket_attempt_id })?;
    Ok(TicketRevisionId::new(row.ticket_revision_id)?)
}

async fn update_ticket_states(
    transaction: &mut Transaction<'_, Postgres>,
    ticket: &TicketRevisionLocked,
    state: i16,
    next: AggregateRevision,
    sponsorship_reason: Option<&str>,
    blocked_reason: Option<&str>,
) -> Result<(), StoreError> {
    sqlx::query!(
        "UPDATE factory.ticket_revisions
         SET lifecycle = $1, revision = $2,
             sponsored_at = CASE WHEN $3::TEXT IS NULL THEN sponsored_at ELSE CURRENT_TIMESTAMP END,
             sponsorship_reason = COALESCE($3, sponsorship_reason),
             blocked_reason = COALESCE($4, blocked_reason)
         WHERE id = $5",
        state,
        revision_to_sql(next)?,
        sponsorship_reason,
        blocked_reason,
        ticket.ticket_revision_id.get(),
    )
    .execute(&mut **transaction)
    .await?;
    sqlx::query!(
        "UPDATE factory.tickets SET lifecycle = $1, revision = $2 WHERE id = $3",
        state,
        revision_to_sql(next)?,
        ticket.ticket_id.get(),
    )
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn update_requalification_and_ticket_state(
    transaction: &mut Transaction<'_, Postgres>,
    ticket: &TicketRevisionLocked,
    state: i16,
    next: AggregateRevision,
    requalification_outcome: i16,
    requalification: &CurrentHeadRequalification,
    blocked_reason: Option<&str>,
) -> Result<(), StoreError> {
    sqlx::query!(
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
        state,
        revision_to_sql(next)?,
        requalification_outcome,
        &requalification.current_head_commit,
        &requalification.current_head_tree,
        requalification.first_actual_observation_artifact_id.get(),
        requalification.second_actual_observation_artifact_id.get(),
        blocked_reason,
        ticket.ticket_revision_id.get(),
    )
    .execute(&mut **transaction)
    .await?;
    sqlx::query!(
        "UPDATE factory.tickets SET lifecycle = $1, revision = $2 WHERE id = $3",
        state,
        revision_to_sql(next)?,
        ticket.ticket_id.get(),
    )
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn classify_requalification(
    transaction: &mut Transaction<'_, Postgres>,
    ticket: &TicketRevisionLocked,
    requalification: &CurrentHeadRequalification,
) -> Result<RequalificationOutcome, StoreError> {
    let expected = artifact_digest(transaction, ticket.expected_observation_artifact_id).await?;
    let discovery = artifact_digest(transaction, ticket.discovery_observation_artifact_id).await?;
    let first = artifact_digest(
        transaction,
        requalification.first_actual_observation_artifact_id,
    )
    .await?;
    let second = artifact_digest(
        transaction,
        requalification.second_actual_observation_artifact_id,
    )
    .await?;
    if first != second {
        return Ok(RequalificationOutcome::Diverged);
    }
    if first == expected {
        return Ok(RequalificationOutcome::Resolved);
    }
    if first == discovery {
        return Ok(RequalificationOutcome::Reproduced);
    }
    Ok(RequalificationOutcome::Diverged)
}

async fn artifact_digest(
    transaction: &mut Transaction<'_, Postgres>,
    artifact_id: ArtifactId,
) -> Result<ContentDigest, StoreError> {
    let row = sqlx::query!(
        "SELECT digest FROM factory.artifacts WHERE id = $1",
        artifact_id.get()
    )
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(StoreError::UnknownArtifact { artifact_id })?;
    let bytes: [u8; 32] = row
        .digest
        .as_slice()
        .try_into()
        .map_err(|_| StoreError::CorruptDigestColumn)?;
    Ok(ContentDigest::from_bytes(bytes))
}

async fn application_advisory_lock(
    transaction: &mut Transaction<'_, Postgres>,
    application_key: &str,
) -> Result<(), StoreError> {
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        application_key
    )
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct TicketAuditReceipt {
    audit_log_id: i64,
    subject_kind: i16,
    subject_id: i64,
    resulting_revision: AggregateRevision,
}

async fn find_ticket_audit(
    transaction: &mut Transaction<'_, Postgres>,
    principal: &str,
    command_id: &str,
    expected_operation: &'static str,
    fingerprint: ContentDigest,
) -> Result<Option<TicketAuditReceipt>, StoreError> {
    let row = sqlx::query!(
        "SELECT id, operation, command_fingerprint, subject_kind, subject_id, resulting_revision
         FROM factory.audit_log WHERE principal = $1 AND command_id = $2",
        principal,
        command_id,
    )
    .fetch_optional(&mut **transaction)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    if row.operation != expected_operation
        || row.command_fingerprint.as_slice() != fingerprint.as_bytes()
    {
        return Err(StoreError::IdempotencyConflict {
            principal: principal.to_owned(),
            command_id: command_id.to_owned(),
        });
    }
    Ok(Some(TicketAuditReceipt {
        audit_log_id: row.id,
        subject_kind: row.subject_kind,
        subject_id: row.subject_id,
        resulting_revision: revision_from_sql(row.resulting_revision)?,
    }))
}

async fn insert_ticket_audit(
    transaction: &mut Transaction<'_, Postgres>,
    principal: &str,
    command_id: &str,
    operation: &'static str,
    fingerprint: ContentDigest,
    subject_kind: i16,
    subject_id: i64,
    resulting_revision: AggregateRevision,
) -> Result<i64, StoreError> {
    let fingerprint = fingerprint.as_bytes();
    Ok(sqlx::query_scalar!(
        "INSERT INTO factory.audit_log (
             principal, command_id, operation, command_fingerprint,
             subject_kind, subject_id, resulting_revision
         ) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
        principal,
        command_id,
        operation,
        &fingerprint[..],
        subject_kind,
        subject_id,
        revision_to_sql(resulting_revision)?,
    )
    .fetch_one(&mut **transaction)
    .await?)
}

fn require_ticket_subject(receipt: &TicketAuditReceipt, subject: i16) -> Result<(), StoreError> {
    if receipt.subject_kind == subject {
        Ok(())
    } else {
        Err(StoreError::AuditSubjectKindMismatch)
    }
}

fn claim_outcome_from_receipt(receipt: &TicketAuditReceipt) -> Result<ClaimOutcome, StoreError> {
    match receipt.subject_kind {
        CLAIMED_ATTEMPT_SUBJECT => Ok(ClaimOutcome::Claimed {
            ticket_attempt_id: TicketAttemptId::new(receipt.subject_id)?,
        }),
        CLAIM_RESOLVED_SUBJECT => Ok(ClaimOutcome::Resolved),
        CLAIM_BLOCKED_SUBJECT => Ok(ClaimOutcome::Blocked),
        _ => Err(StoreError::AuditSubjectKindMismatch),
    }
}

fn release_outcome_from_receipt(
    receipt: &TicketAuditReceipt,
) -> Result<ReleaseOutcome, StoreError> {
    match receipt.subject_kind {
        RELEASED_ATTEMPT_SUBJECT => Ok(ReleaseOutcome::Released),
        RELEASE_RESOLVED_SUBJECT => Ok(ReleaseOutcome::Resolved),
        RELEASE_BLOCKED_SUBJECT => Ok(ReleaseOutcome::Blocked),
        _ => Err(StoreError::AuditSubjectKindMismatch),
    }
}

fn ticket_state_from_sql(value: i16) -> Result<TicketState, StoreError> {
    match value {
        TICKET_PROPOSED => Ok(TicketState::Proposed),
        TICKET_SPONSORED => Ok(TicketState::Sponsored),
        TICKET_IN_FLIGHT => Ok(TicketState::InFlight),
        TICKET_DELIVERED => Ok(TicketState::Delivered),
        TICKET_BLOCKED => Ok(TicketState::Blocked),
        TICKET_RESOLVED => Ok(TicketState::Resolved),
        TICKET_SUPERSEDED => Ok(TicketState::Superseded),
        TICKET_REJECTED => Ok(TicketState::Rejected),
        _ => Err(StoreError::CorruptTicketState),
    }
}

#[allow(dead_code)]
fn ticket_attempt_stage_from_sql(value: i16) -> Result<TicketAttemptStage, StoreError> {
    match value {
        ATTEMPT_ENGINEERING => Ok(TicketAttemptStage::Engineering),
        ATTEMPT_HARD_VALIDATION => Ok(TicketAttemptStage::HardValidation),
        ATTEMPT_QUALITY => Ok(TicketAttemptStage::Quality),
        ATTEMPT_AWAITING_ARCHITECT => Ok(TicketAttemptStage::AwaitingArchitect),
        ATTEMPT_REWORK_ENGINEERING => Ok(TicketAttemptStage::ReworkEngineering),
        ATTEMPT_REWORK_VALIDATION => Ok(TicketAttemptStage::ReworkValidation),
        ATTEMPT_REWORK_QUALITY => Ok(TicketAttemptStage::ReworkQuality),
        ATTEMPT_DELIVERED => Ok(TicketAttemptStage::Delivered),
        ATTEMPT_FAILED => Ok(TicketAttemptStage::Failed),
        ATTEMPT_CANCELLED => Ok(TicketAttemptStage::Cancelled),
        _ => Err(StoreError::CorruptTicketState),
    }
}

fn require_ticket_state(required: TicketState, observed: TicketState) -> Result<(), StoreError> {
    if required == observed {
        Ok(())
    } else {
        Err(StoreError::TicketStateConflict { required, observed })
    }
}

fn require_expected(
    expected: ExpectedRevision,
    current: AggregateRevision,
) -> Result<(), StoreError> {
    if expected.get() == current {
        Ok(())
    } else {
        Err(StoreError::RevisionConflict { expected, current })
    }
}

fn revision_from_sql(value: i64) -> Result<AggregateRevision, StoreError> {
    u64::try_from(value)
        .map(AggregateRevision::from_persisted)
        .map_err(|_| StoreError::RevisionOutOfRange)
}

fn revision_to_sql(value: AggregateRevision) -> Result<i64, StoreError> {
    i64::try_from(value.get()).map_err(|_| StoreError::RevisionOutOfRange)
}

fn u32_from_sql(value: i64, field: &'static str) -> Result<u32, StoreError> {
    u32::try_from(value).map_err(|_| StoreError::InvalidTicketField { field })
}

fn validate_command(principal: &str, command_id: &str) -> Result<(), StoreError> {
    for (field, value) in [("principal", principal), ("command ID", command_id)] {
        if value.is_empty()
            || value.len() > 160
            || !value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'_' | b'-')
            })
        {
            return Err(StoreError::InvalidCommandComponent { field });
        }
    }
    Ok(())
}

fn validate_snapshot(value: &str, field: &'static str) -> Result<(), StoreError> {
    if value.is_empty() || value.len() > 240 || value.contains('\0') {
        return Err(StoreError::InvalidTicketField { field });
    }
    Ok(())
}

fn validate_reason(value: &str, field: &'static str) -> Result<(), StoreError> {
    if value.is_empty() || value.len() > 4096 || value.contains('\0') {
        return Err(StoreError::InvalidTicketField { field });
    }
    Ok(())
}

fn validate_requalification(value: &CurrentHeadRequalification) -> Result<(), StoreError> {
    validate_snapshot(&value.current_head_commit, "current-head commit")?;
    validate_snapshot(&value.current_head_tree, "current-head tree")?;
    Ok(())
}

fn hash_string(hasher: &mut blake3::Hasher, value: &str) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

fn hash_revision(hasher: &mut blake3::Hasher, value: ExpectedRevision) {
    hasher.update(&value.get().get().to_be_bytes());
}

fn hash_artifact(hasher: &mut blake3::Hasher, value: ArtifactId) {
    hasher.update(&value.get().to_be_bytes());
}

fn ticket_fingerprint_prefix(operation: &str, principal: &str, command_id: &str) -> blake3::Hasher {
    let mut hasher = blake3::Hasher::new();
    hasher.update(operation.as_bytes());
    hash_string(&mut hasher, principal);
    hash_string(&mut hasher, command_id);
    hasher
}

fn finish_hash(hasher: blake3::Hasher) -> ContentDigest {
    ContentDigest::from_bytes(*hasher.finalize().as_bytes())
}

fn proposal_fingerprint(command: &SubmitTicketProposal) -> ContentDigest {
    let mut hasher =
        ticket_fingerprint_prefix(PROPOSE_OPERATION, &command.principal, &command.command_id);
    hash_revision(&mut hasher, command.expected_application_revision);
    hasher.update(&command.application_revision_id.get().to_be_bytes());
    for artifact in [
        command.proposal_artifact_id,
        command.reproducer_artifact_id,
        command.expected_observation_artifact_id,
        command.first_actual_observation_artifact_id,
        command.second_actual_observation_artifact_id,
    ] {
        hash_artifact(&mut hasher, artifact);
    }
    hash_string(&mut hasher, &command.discovery_commit);
    hash_string(&mut hasher, &command.discovery_tree);
    finish_hash(hasher)
}

fn sponsor_fingerprint(command: &SponsorTicketRevision) -> ContentDigest {
    let mut hasher =
        ticket_fingerprint_prefix(SPONSOR_OPERATION, &command.principal, &command.command_id);
    hasher.update(&command.ticket_revision_id.get().to_be_bytes());
    hash_revision(&mut hasher, command.expected_ticket_revision);
    hash_string(&mut hasher, &command.reason);
    finish_hash(hasher)
}

fn hash_requalification(hasher: &mut blake3::Hasher, value: &CurrentHeadRequalification) {
    hash_string(hasher, &value.current_head_commit);
    hash_string(hasher, &value.current_head_tree);
    hash_artifact(hasher, value.first_actual_observation_artifact_id);
    hash_artifact(hasher, value.second_actual_observation_artifact_id);
}

fn claim_fingerprint(command: &ClaimSponsoredTicket) -> ContentDigest {
    let mut hasher =
        ticket_fingerprint_prefix(CLAIM_OPERATION, &command.principal, &command.command_id);
    hasher.update(&command.campaign_id.get().to_be_bytes());
    hash_revision(&mut hasher, command.expected_campaign_revision);
    hasher.update(&command.ticket_revision_id.get().to_be_bytes());
    hash_revision(&mut hasher, command.expected_ticket_revision);
    hash_requalification(&mut hasher, &command.requalification);
    finish_hash(hasher)
}

fn failure_fingerprint(command: &FailTicketAttempt) -> ContentDigest {
    let mut hasher =
        ticket_fingerprint_prefix(FAIL_OPERATION, &command.principal, &command.command_id);
    hasher.update(&command.ticket_attempt_id.get().to_be_bytes());
    hash_revision(&mut hasher, command.expected_attempt_revision);
    hash_revision(&mut hasher, command.expected_ticket_revision);
    hash_string(&mut hasher, &command.reason);
    finish_hash(hasher)
}

fn quality_retry_fingerprint(command: &RetryQualityAttempt) -> ContentDigest {
    let mut hasher = ticket_fingerprint_prefix(
        QUALITY_RETRY_OPERATION,
        &command.principal,
        &command.command_id,
    );
    hasher.update(&command.ticket_attempt_id.get().to_be_bytes());
    hasher.update(&command.candidate_id.get().to_be_bytes());
    hash_revision(&mut hasher, command.expected_attempt_revision);
    hash_revision(&mut hasher, command.expected_ticket_revision);
    hash_string(&mut hasher, &command.reason);
    finish_hash(hasher)
}

fn engineering_retry_fingerprint(command: &RetryEngineeringAttempt) -> ContentDigest {
    let mut hasher = ticket_fingerprint_prefix(
        ENGINEERING_RETRY_OPERATION,
        &command.principal,
        &command.command_id,
    );
    hasher.update(&command.ticket_attempt_id.get().to_be_bytes());
    hash_revision(&mut hasher, command.expected_attempt_revision);
    hash_revision(&mut hasher, command.expected_ticket_revision);
    hash_string(&mut hasher, &command.reason);
    finish_hash(hasher)
}

fn can_retry_quality_attempt(
    attempt_stage: i16,
    candidate_lifecycle: i16,
    candidate_commit_present: bool,
    prior_quality_retry_exists: bool,
) -> bool {
    matches!(attempt_stage, ATTEMPT_QUALITY | ATTEMPT_FAILED)
        && candidate_lifecycle == CANDIDATE_VALIDATED
        && candidate_commit_present
        && !prior_quality_retry_exists
}

fn can_retry_engineering_attempt(
    attempt_stage: i16,
    candidate_exists: bool,
    prior_engineering_retry_exists: bool,
) -> bool {
    attempt_stage == ATTEMPT_FAILED && !candidate_exists && !prior_engineering_retry_exists
}

fn release_fingerprint(command: &ReleaseTicketAttempt) -> ContentDigest {
    let mut hasher =
        ticket_fingerprint_prefix(RELEASE_OPERATION, &command.principal, &command.command_id);
    hasher.update(&command.ticket_attempt_id.get().to_be_bytes());
    hash_revision(&mut hasher, command.expected_attempt_revision);
    hash_revision(&mut hasher, command.expected_ticket_revision);
    hash_string(&mut hasher, &command.reason);
    hash_requalification(&mut hasher, &command.requalification);
    finish_hash(hasher)
}

fn completion_fingerprint(command: &CompleteCampaignAtDeliveryTarget) -> ContentDigest {
    let mut hasher = ticket_fingerprint_prefix(
        COMPLETE_CAMPAIGN_OPERATION,
        &command.principal,
        &command.command_id,
    );
    hasher.update(&command.campaign_id.get().to_be_bytes());
    hash_revision(&mut hasher, command.expected_campaign_revision);
    finish_hash(hasher)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downstream_stage_mapping_is_closed_and_preserves_delivery_handoff() {
        assert_eq!(
            DownstreamActionStage::from_attempt_and_candidate(
                ATTEMPT_QUALITY,
                CANDIDATE_VALIDATED,
                true,
                false,
            ),
            Some(DownstreamActionStage::Quality)
        );
        assert_eq!(
            DownstreamActionStage::from_attempt_and_candidate(
                ATTEMPT_QUALITY,
                CANDIDATE_VALIDATED,
                false,
                false,
            ),
            Some(DownstreamActionStage::CandidateCommitAttachRequired)
        );
        assert_eq!(
            DownstreamActionStage::from_attempt_and_candidate(
                ATTEMPT_REWORK_ENGINEERING,
                CANDIDATE_REJECTED,
                false,
                false,
            ),
            Some(DownstreamActionStage::ReworkEngineering)
        );
        assert_eq!(
            DownstreamActionStage::from_attempt_and_candidate(
                ATTEMPT_REWORK_QUALITY,
                CANDIDATE_VALIDATED,
                true,
                false,
            ),
            Some(DownstreamActionStage::ReworkQuality)
        );
        assert_eq!(
            DownstreamActionStage::from_attempt_and_candidate(
                ATTEMPT_AWAITING_ARCHITECT,
                CANDIDATE_ACCEPTED,
                true,
                true,
            ),
            Some(DownstreamActionStage::DeliverAccepted)
        );
        assert_eq!(
            DownstreamActionStage::from_attempt_and_candidate(
                ATTEMPT_AWAITING_ARCHITECT,
                CANDIDATE_REJECTED,
                false,
                false,
            ),
            None,
            "a rejected candidate must be terminally released, not rescheduled"
        );
        assert_eq!(
            DownstreamActionStage::from_attempt_and_candidate(
                ATTEMPT_AWAITING_ARCHITECT,
                CANDIDATE_VALIDATED,
                true,
                false,
            ),
            Some(DownstreamActionStage::QualityReviewRequired)
        );
        assert_eq!(
            DownstreamActionStage::from_attempt_and_candidate(
                ATTEMPT_AWAITING_ARCHITECT,
                CANDIDATE_VALIDATED,
                true,
                true,
            ),
            Some(DownstreamActionStage::AwaitingArchitect)
        );
    }

    #[test]
    fn quality_retry_is_bounded_to_one_validated_committed_candidate() {
        assert!(can_retry_quality_attempt(
            ATTEMPT_QUALITY,
            CANDIDATE_VALIDATED,
            true,
            false,
        ));
        assert!(can_retry_quality_attempt(
            ATTEMPT_FAILED,
            CANDIDATE_VALIDATED,
            true,
            false,
        ));
        assert!(!can_retry_quality_attempt(
            ATTEMPT_FAILED,
            CANDIDATE_VALIDATED,
            true,
            true,
        ));
        assert!(!can_retry_quality_attempt(
            ATTEMPT_REWORK_QUALITY,
            CANDIDATE_VALIDATED,
            true,
            false,
        ));
        assert!(!can_retry_quality_attempt(
            ATTEMPT_QUALITY,
            CANDIDATE_SUBMITTED,
            true,
            false,
        ));
        assert!(!can_retry_quality_attempt(
            ATTEMPT_QUALITY,
            CANDIDATE_VALIDATED,
            false,
            false,
        ));
    }

    #[test]
    fn engineering_retry_is_bounded_to_one_candidate_less_failure() {
        assert!(can_retry_engineering_attempt(ATTEMPT_FAILED, false, false));
        assert!(!can_retry_engineering_attempt(ATTEMPT_FAILED, false, true));
        assert!(!can_retry_engineering_attempt(
            ATTEMPT_ENGINEERING,
            false,
            false
        ));
        assert!(!can_retry_engineering_attempt(ATTEMPT_FAILED, true, false));
    }
}
