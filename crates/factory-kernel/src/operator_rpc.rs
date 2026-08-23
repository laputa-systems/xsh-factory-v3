//! External Grand Architect routing on the authenticated operator socket.
//!
//! This is deliberately separate from actor routing.  The `0600` local
//! operator socket is the only capability which reaches this adapter; neither
//! an actor binding nor a JSON field can select an Architect operation.  The
//! router owns wire parsing and response projection, while `DecisionStore`
//! remains the durable authority for every accepted transition.
//!
//! Sponsorship needs one ticket revision and is usable immediately.  Release
//! and final-candidate decisions have richer store preconditions than their
//! intentionally compact public wire requests.  Their caller-facing
//! `expected_revision` names the attempt and candidate respectively.  A
//! daemon-composed [`ArchitectTransitionResolver`] must obtain the additional
//! exact revisions and, for release, trusted current-head requalification
//! evidence.  The store proves all hard-validation facts transactionally
//! again; no resolver result can waive them.  Until that trusted composition
//! exists, those operations reject before any durable authority call.

use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

use factory_protocol::{
    AggregateRevision, ArchitectDecideCandidateRequest, ArchitectDecisionKindV2,
    ArchitectDecisionReceiptResponse, ArchitectReleaseTicketAttemptRequest,
    ArchitectSponsorTicketRevisionRequest, CampaignId, CampaignReceiptResponse,
    CampaignSessionCostAggregateResponse, CampaignSessionCostResponse, CampaignStatusResponse,
    CandidateDecisionRequestV2, CandidateId, ConflictResponse, ContractError,
    DownstreamArchitectDecisionEvidenceResponse, DownstreamEvidenceResponse,
    DownstreamReviewEvidenceResponse, DownstreamValidationEvidenceResponse, ErrorResponse,
    ExpectedRevision, FrameError, OP_ARCHITECT_DECIDE_CANDIDATE,
    OP_ARCHITECT_RELEASE_TICKET_ATTEMPT, OP_ARCHITECT_SPONSOR_TICKET_REVISION,
    OP_OPERATOR_CAMPAIGN_STATUS, OP_OPERATOR_CANCEL_CAMPAIGN, OP_OPERATOR_START_CAMPAIGN,
    OperatorCampaignStatusRequest, OperatorCancelCampaignRequest, OperatorStartCampaignRequest,
    PROTOCOL_VERSION_V2, ReviewId, TerminalCostV2, TicketAttemptId, decode_operation_request,
    decode_routing_envelope,
};
use miniserde::json;
use thiserror::Error;

use crate::{
    decision_store::{
        CandidateDecisionReceipt, DecideCandidate, DecisionStore, DecisionStoreError,
        ReleaseReceipt, ReleaseTicketAttempt, SponsorTicket, SponsorshipReceipt,
    },
    process::{
        CampaignCancellationAdmission, CampaignReceipt, CancelCampaign, ProcessStore, StartCampaign,
    },
    scheduler::{SchedulerConstraint, SchedulerNextAction, TicketScheduler},
    session_runtime::{ActiveSessionCancellationRegistry, SessionRuntimeError},
    storage::StoreError,
    ticket_store::CurrentHeadRequalification,
    ticket_store::TicketStore,
};
use factory_settings::SESSION_PARTIAL_TRANSCRIPT_RELATIVE_PATH;

type DecisionFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DecisionStoreError>> + Send + 'a>>;

/// Future shape used by the daemon-composed trusted transition resolver.
/// It remains dependency-free and object-safe so the resident daemon can hold
/// exactly one resolver without creating a generic workflow runtime.
pub type ArchitectTransitionFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ArchitectTransitionResolutionError>> + Send + 'a>>;

/// Trusted current state required to expand a compact release request into
/// the `DecisionStore` command.  The requalification belongs to a kernel-owned
/// command runner; it is never supplied by the operator wire request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedReleaseTransition {
    pub expected_attempt_revision: ExpectedRevision,
    pub expected_ticket_revision: ExpectedRevision,
    pub requalification: CurrentHeadRequalification,
}

/// Trusted current revisions required for a final candidate decision.  Passed
/// validation is deliberately not represented as an overrideable flag: the
/// decision store reads and proves both validation rows in its own transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedCandidateDecisionTransition {
    pub expected_candidate_revision: ExpectedRevision,
    pub expected_attempt_revision: ExpectedRevision,
    pub expected_ticket_revision: ExpectedRevision,
}

/// Daemon-composition seam for the two Architect operations that need more
/// than a public caller-facing revision.  Implementors must source every
/// field from kernel-owned authoritative reads and command runners.  They
/// must not accept actor- or operator-supplied requalification or validation
/// facts.
pub trait ArchitectTransitionResolver: Send + Sync {
    fn resolve_release<'a>(
        &'a self,
        ticket_attempt_id: TicketAttemptId,
        caller_expected_attempt_revision: ExpectedRevision,
    ) -> ArchitectTransitionFuture<'a, ResolvedReleaseTransition>;

    fn resolve_candidate_decision<'a>(
        &'a self,
        candidate_id: CandidateId,
        review_id: ReviewId,
        caller_expected_candidate_revision: ExpectedRevision,
    ) -> ArchitectTransitionFuture<'a, ResolvedCandidateDecisionTransition>;
}

/// Resolver outcomes which are safe to return to the operator.  A stale
/// caller-facing revision is represented explicitly so a CLI/SDK retry cannot
/// accidentally reuse a transition context for a moved aggregate.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ArchitectTransitionResolutionError {
    #[error("the trusted transition resolver is not configured")]
    Unavailable,

    #[error("the requested transition is not currently resolvable: {message}")]
    Precondition { message: String },

    #[error("caller revision is stale: expected {expected}, current {current}")]
    RevisionConflict { expected: u64, current: u64 },
}

/// Capability minted only by the local operator transport after it has bound
/// the mode-`0600` socket.  It has no public constructor, so an actor route
/// cannot manufacture Architect authority by adding a payload field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OperatorArchitectCapability {
    _private: (),
}

impl OperatorArchitectCapability {
    pub(crate) const fn from_operator_transport() -> Self {
        Self { _private: () }
    }
}

trait ArchitectDecisionAuthority: Send + Sync {
    fn sponsor_ticket<'a>(
        &'a self,
        command: SponsorTicket,
    ) -> DecisionFuture<'a, SponsorshipReceipt>;

    fn release_ticket_attempt<'a>(
        &'a self,
        command: ReleaseTicketAttempt,
    ) -> DecisionFuture<'a, ReleaseReceipt>;

    fn decide_candidate<'a>(
        &'a self,
        command: DecideCandidate,
    ) -> DecisionFuture<'a, CandidateDecisionReceipt>;
}

impl ArchitectDecisionAuthority for DecisionStore {
    fn sponsor_ticket<'a>(
        &'a self,
        command: SponsorTicket,
    ) -> DecisionFuture<'a, SponsorshipReceipt> {
        Box::pin(async move { self.sponsor_ticket(&command).await })
    }

    fn release_ticket_attempt<'a>(
        &'a self,
        command: ReleaseTicketAttempt,
    ) -> DecisionFuture<'a, ReleaseReceipt> {
        Box::pin(async move { self.release_ticket_attempt(&command).await })
    }

    fn decide_candidate<'a>(
        &'a self,
        command: DecideCandidate,
    ) -> DecisionFuture<'a, CandidateDecisionReceipt> {
        Box::pin(async move { self.decide_candidate(&command).await })
    }
}

/// Socket-only adapter over the narrow durable decision authority.  It owns
/// no pool and exposes no database type; `LocalDaemon` constructs it only
/// while wiring the authenticated operator listener.
#[derive(Clone)]
pub(crate) struct OperatorRpc {
    authority: Arc<dyn ArchitectDecisionAuthority>,
    resolver: Option<Arc<dyn ArchitectTransitionResolver>>,
}

impl core::fmt::Debug for OperatorRpc {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("OperatorRpc")
            .field("has_transition_resolver", &self.resolver.is_some())
            .finish_non_exhaustive()
    }
}

impl OperatorRpc {
    pub(crate) fn from_operator_transport(
        _capability: OperatorArchitectCapability,
        store: DecisionStore,
    ) -> Self {
        Self {
            authority: Arc::new(store),
            resolver: None,
        }
    }

    pub(crate) fn with_transition_resolver(
        mut self,
        resolver: Arc<dyn ArchitectTransitionResolver>,
    ) -> Self {
        self.resolver = Some(resolver);
        self
    }

    /// Dispatches exactly one authenticated Architect request.  Invalid wire
    /// frames remain transport errors; valid request shapes with rejected
    /// domain state receive a typed protocol error response.
    pub(crate) async fn dispatch(&self, frame: &[u8]) -> Result<Vec<u8>, OperatorRpcError> {
        let envelope = decode_routing_envelope(frame, factory_protocol::REQUEST_FRAME_MAX_BYTES)?;
        let request_id = envelope.request_id.clone();
        let operation = envelope.operation.clone();
        let outcome = match operation.as_str() {
            OP_ARCHITECT_SPONSOR_TICKET_REVISION => self.dispatch_sponsorship(frame).await,
            OP_ARCHITECT_RELEASE_TICKET_ATTEMPT => self.dispatch_release(frame).await,
            OP_ARCHITECT_DECIDE_CANDIDATE => self.dispatch_candidate_decision(frame).await,
            _ => return Err(OperatorRpcError::OperationNotArchitect { operation }),
        };
        Ok(match outcome {
            Ok(response) => response,
            Err(rejection) => rejection.response(request_id, envelope.operation),
        })
    }

    async fn dispatch_sponsorship(
        &self,
        frame: &[u8],
    ) -> Result<Vec<u8>, ArchitectOperationRejection> {
        let request: ArchitectSponsorTicketRevisionRequest = decode_operation_request(
            frame,
            factory_protocol::REQUEST_FRAME_MAX_BYTES,
            OP_ARCHITECT_SPONSOR_TICKET_REVISION,
        )
        .map_err(ArchitectOperationRejection::Frame)?;
        let decision = request
            .decision()
            .map_err(ArchitectOperationRejection::Contract)?;
        let receipt = self
            .authority
            .sponsor_ticket(SponsorTicket {
                command_id: request.client_command_id,
                expected_ticket_revision: expected_revision(request.expected_revision),
                decision,
            })
            .await
            .map_err(ArchitectOperationRejection::Store)?;
        Ok(receipt_response(
            request.request_id,
            OP_ARCHITECT_SPONSOR_TICKET_REVISION,
            receipt.audit_log_id,
            receipt.resulting_ticket_revision,
            receipt.decision.kind,
            receipt.decision.architect_decision_id.get(),
        ))
    }

    async fn dispatch_release(&self, frame: &[u8]) -> Result<Vec<u8>, ArchitectOperationRejection> {
        let request: ArchitectReleaseTicketAttemptRequest = decode_operation_request(
            frame,
            factory_protocol::REQUEST_FRAME_MAX_BYTES,
            OP_ARCHITECT_RELEASE_TICKET_ATTEMPT,
        )
        .map_err(ArchitectOperationRejection::Frame)?;
        let decision = request
            .decision()
            .map_err(ArchitectOperationRejection::Contract)?;
        let context = self
            .resolve_release(
                decision.ticket_attempt_id,
                expected_revision(request.expected_revision),
            )
            .await
            .map_err(ArchitectOperationRejection::Resolver)?;
        require_caller_revision(request.expected_revision, context.expected_attempt_revision)
            .map_err(ArchitectOperationRejection::Resolver)?;
        let receipt = self
            .authority
            .release_ticket_attempt(ReleaseTicketAttempt {
                command_id: request.client_command_id,
                expected_attempt_revision: context.expected_attempt_revision,
                expected_ticket_revision: context.expected_ticket_revision,
                decision,
                requalification: context.requalification,
            })
            .await
            .map_err(ArchitectOperationRejection::Store)?;
        Ok(receipt_response(
            request.request_id,
            OP_ARCHITECT_RELEASE_TICKET_ATTEMPT,
            receipt.audit_log_id,
            receipt.resulting_attempt_revision,
            receipt.decision.kind,
            receipt.decision.architect_decision_id.get(),
        ))
    }

    async fn dispatch_candidate_decision(
        &self,
        frame: &[u8],
    ) -> Result<Vec<u8>, ArchitectOperationRejection> {
        let request: ArchitectDecideCandidateRequest = decode_operation_request(
            frame,
            factory_protocol::REQUEST_FRAME_MAX_BYTES,
            OP_ARCHITECT_DECIDE_CANDIDATE,
        )
        .map_err(ArchitectOperationRejection::Frame)?;
        let decision = request
            .decision()
            .map_err(ArchitectOperationRejection::Contract)?;
        let context = self
            .resolve_candidate_decision(&decision, expected_revision(request.expected_revision))
            .await
            .map_err(ArchitectOperationRejection::Resolver)?;
        require_caller_revision(
            request.expected_revision,
            context.expected_candidate_revision,
        )
        .map_err(ArchitectOperationRejection::Resolver)?;
        let receipt = self
            .authority
            .decide_candidate(DecideCandidate {
                command_id: request.client_command_id,
                expected_candidate_revision: context.expected_candidate_revision,
                expected_attempt_revision: context.expected_attempt_revision,
                expected_ticket_revision: context.expected_ticket_revision,
                request: decision,
            })
            .await
            .map_err(ArchitectOperationRejection::Store)?;
        Ok(receipt_response(
            request.request_id,
            OP_ARCHITECT_DECIDE_CANDIDATE,
            receipt.audit_log_id,
            receipt.resulting_candidate_revision,
            receipt.decision.kind,
            receipt.decision.architect_decision_id.get(),
        ))
    }

    async fn resolve_release(
        &self,
        ticket_attempt_id: TicketAttemptId,
        caller_expected_attempt_revision: ExpectedRevision,
    ) -> Result<ResolvedReleaseTransition, ArchitectTransitionResolutionError> {
        let Some(resolver) = &self.resolver else {
            return Err(ArchitectTransitionResolutionError::Unavailable);
        };
        resolver
            .resolve_release(ticket_attempt_id, caller_expected_attempt_revision)
            .await
    }

    async fn resolve_candidate_decision(
        &self,
        request: &CandidateDecisionRequestV2,
        caller_expected_candidate_revision: ExpectedRevision,
    ) -> Result<ResolvedCandidateDecisionTransition, ArchitectTransitionResolutionError> {
        let Some(resolver) = &self.resolver else {
            return Err(ArchitectTransitionResolutionError::Unavailable);
        };
        resolver
            .resolve_candidate_decision(
                request.candidate_id,
                request.review_id,
                caller_expected_candidate_revision,
            )
            .await
    }
}

#[derive(Debug, Error)]
pub(crate) enum OperatorRpcError {
    #[error(transparent)]
    Frame(#[from] FrameError),

    #[error("operation {operation:?} is not an Architect operation")]
    OperationNotArchitect { operation: String },
}

#[derive(Debug)]
enum ArchitectOperationRejection {
    Frame(FrameError),
    Contract(ContractError),
    Resolver(ArchitectTransitionResolutionError),
    Store(DecisionStoreError),
}

impl ArchitectOperationRejection {
    fn response(self, request_id: String, operation: String) -> Vec<u8> {
        match self {
            Self::Store(DecisionStoreError::RevisionConflict { current, .. }) => {
                json::to_string(&ConflictResponse {
                    protocol_version: PROTOCOL_VERSION_V2,
                    request_id,
                    operation,
                    error_code: "revision_conflict".to_owned(),
                    current_revision: current.get(),
                    message: "the observed aggregate revision is stale".to_owned(),
                })
                .into_bytes()
            }
            Self::Resolver(ArchitectTransitionResolutionError::RevisionConflict {
                current,
                ..
            }) => json::to_string(&ConflictResponse {
                protocol_version: PROTOCOL_VERSION_V2,
                request_id,
                operation,
                error_code: "revision_conflict".to_owned(),
                current_revision: current,
                message: "the observed aggregate revision is stale".to_owned(),
            })
            .into_bytes(),
            Self::Resolver(ArchitectTransitionResolutionError::Unavailable) => error_response(
                request_id,
                operation,
                "architect_transition_unavailable",
                "the daemon has no trusted resolver for this transition",
            ),
            Self::Resolver(error) => error_response(
                request_id,
                operation,
                "architect_transition_precondition",
                &error.to_string(),
            ),
            Self::Contract(error) => error_response(
                request_id,
                operation,
                "invalid_architect_request",
                &error.to_string(),
            ),
            Self::Store(error) => error_response(
                request_id,
                operation,
                decision_store_error_code(&error),
                &error.to_string(),
            ),
            Self::Frame(error) => error_response(
                request_id,
                operation,
                "invalid_architect_request",
                &error.to_string(),
            ),
        }
    }
}

fn expected_revision(value: u64) -> ExpectedRevision {
    ExpectedRevision::new(AggregateRevision::from_persisted(value))
}

fn require_caller_revision(
    requested: u64,
    current: ExpectedRevision,
) -> Result<(), ArchitectTransitionResolutionError> {
    if requested == current.get().get() {
        Ok(())
    } else {
        Err(ArchitectTransitionResolutionError::RevisionConflict {
            expected: requested,
            current: current.get().get(),
        })
    }
}

fn receipt_response(
    request_id: String,
    operation: &'static str,
    audit_id: i64,
    aggregate_revision: AggregateRevision,
    decision_kind: ArchitectDecisionKindV2,
    architect_decision_id: i64,
) -> Vec<u8> {
    json::to_string(&ArchitectDecisionReceiptResponse {
        protocol_version: PROTOCOL_VERSION_V2,
        request_id,
        operation: operation.to_owned(),
        audit_id,
        aggregate_revision: aggregate_revision.get(),
        architect_decision_id,
        decision_kind: decision_kind_name(decision_kind).to_owned(),
    })
    .into_bytes()
}

fn error_response(
    request_id: String,
    operation: String,
    error_code: &str,
    message: &str,
) -> Vec<u8> {
    json::to_string(&ErrorResponse {
        protocol_version: PROTOCOL_VERSION_V2,
        request_id,
        operation,
        error_code: error_code.to_owned(),
        message: message.to_owned(),
    })
    .into_bytes()
}

fn decision_kind_name(value: ArchitectDecisionKindV2) -> &'static str {
    match value {
        ArchitectDecisionKindV2::Sponsor => "sponsor",
        ArchitectDecisionKindV2::Release => "release",
        ArchitectDecisionKindV2::Deliver => "deliver",
        ArchitectDecisionKindV2::Rework => "rework",
        ArchitectDecisionKindV2::Reject => "reject",
    }
}

fn decision_store_error_code(error: &DecisionStoreError) -> &'static str {
    match error {
        DecisionStoreError::IdempotencyConflict { .. } => "idempotency_conflict",
        DecisionStoreError::HardValidationMissing => "hard_validation_missing",
        DecisionStoreError::QualityValidationNotPassed => "quality_validation_not_passed",
        DecisionStoreError::QualityRejectionOverrideRequired => {
            "quality_rejection_override_required"
        }
        DecisionStoreError::QualityRejectionOverrideForbidden => {
            "quality_rejection_override_forbidden"
        }
        DecisionStoreError::ReworkLimitReached => "rework_limit_reached",
        DecisionStoreError::ReviewCandidateMismatch => "review_candidate_mismatch",
        _ => "architect_decision_rejected",
    }
}

/// Capability minted solely while wiring the authenticated local operator
/// listener. It is intentionally distinct from the Architect capability so
/// an actor route cannot gain campaign authority by adding a JSON field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OperatorCampaignCapability {
    _private: (),
}

impl OperatorCampaignCapability {
    pub(crate) const fn from_operator_transport() -> Self {
        Self { _private: () }
    }
}

/// Socket-only campaign controller. It composes the two narrow stores rather
/// than exposing a pool: campaign transitions remain in `ProcessStore`, while
/// the status-only buffer view remains in `TicketStore`.
#[derive(Clone, Debug)]
pub(crate) struct CampaignOperatorRpc {
    process: ProcessStore,
    tickets: TicketStore,
    active_sessions: ActiveSessionCancellationRegistry,
    runtime_root: PathBuf,
}

impl CampaignOperatorRpc {
    pub(crate) fn from_operator_transport(
        _capability: OperatorCampaignCapability,
        process: ProcessStore,
        tickets: TicketStore,
        active_sessions: ActiveSessionCancellationRegistry,
        runtime_root: PathBuf,
    ) -> Self {
        Self {
            process,
            tickets,
            active_sessions,
            runtime_root,
        }
    }

    /// Dispatches one typed campaign operation. Campaign status is a pair of
    /// bounded reads and never inserts an audit row or scheduler receipt.
    pub(crate) async fn dispatch(&self, frame: &[u8]) -> Result<Vec<u8>, CampaignOperatorRpcError> {
        let envelope = decode_routing_envelope(frame, factory_protocol::REQUEST_FRAME_MAX_BYTES)?;
        let request_id = envelope.request_id.clone();
        let operation = envelope.operation.clone();
        let outcome = match operation.as_str() {
            OP_OPERATOR_START_CAMPAIGN => self.dispatch_start(frame).await,
            OP_OPERATOR_CAMPAIGN_STATUS => self.dispatch_status(frame).await,
            OP_OPERATOR_CANCEL_CAMPAIGN => self.dispatch_cancel(frame).await,
            _ => return Err(CampaignOperatorRpcError::OperationNotCampaign { operation }),
        };
        Ok(match outcome {
            Ok(response) => response,
            Err(rejection) => rejection.response(request_id, envelope.operation),
        })
    }

    async fn dispatch_start(&self, frame: &[u8]) -> Result<Vec<u8>, CampaignOperationRejection> {
        let request: OperatorStartCampaignRequest = decode_operation_request(
            frame,
            factory_protocol::REQUEST_FRAME_MAX_BYTES,
            OP_OPERATOR_START_CAMPAIGN,
        )
        .map_err(CampaignOperationRejection::Frame)?;
        let receipt = self
            .process
            .start_campaign(&StartCampaign {
                principal: request.principal,
                command_id: request.client_command_id,
                expected_application_revision: expected_revision(
                    request.expected_application_revision,
                ),
                application_revision_id: factory_protocol::ApplicationRevisionId::new(
                    request.application_revision_id,
                )
                .map_err(CampaignOperationRejection::Contract)?,
                aggregate_budget: factory_protocol::MicroUsd::new(
                    request.aggregate_budget_micro_usd,
                ),
                deadline_unix_millis: request.deadline_unix_millis,
                delivery_target: request.delivery_target,
            })
            .await
            .map_err(CampaignOperationRejection::Store)?;
        Ok(campaign_receipt_response(
            request.request_id,
            OP_OPERATOR_START_CAMPAIGN,
            receipt,
        ))
    }

    async fn dispatch_status(&self, frame: &[u8]) -> Result<Vec<u8>, CampaignOperationRejection> {
        let request: OperatorCampaignStatusRequest = decode_operation_request(
            frame,
            factory_protocol::REQUEST_FRAME_MAX_BYTES,
            OP_OPERATOR_CAMPAIGN_STATUS,
        )
        .map_err(CampaignOperationRejection::Frame)?;
        let campaign_id =
            CampaignId::new(request.campaign_id).map_err(CampaignOperationRejection::Contract)?;
        let campaign = self
            .process
            .campaign_status(campaign_id)
            .await
            .map_err(CampaignOperationRejection::Store)?;
        let buffer = self
            .tickets
            .ticket_buffer_status(campaign_id)
            .await
            .map_err(CampaignOperationRejection::Store)?;
        let session_costs = self
            .process
            .campaign_session_costs(campaign_id, None, 20)
            .await
            .map_err(CampaignOperationRejection::Store)?;
        let product_identity = self
            .process
            .campaign_product_identity(campaign_id)
            .await
            .map_err(CampaignOperationRejection::Store)?;
        let session_cost_aggregates = self
            .process
            .campaign_session_cost_aggregates(campaign_id)
            .await
            .map_err(CampaignOperationRejection::Store)?;
        Ok(campaign_status_response(
            request.request_id,
            campaign,
            &buffer,
            product_identity,
            &session_costs,
            &session_cost_aggregates,
            &self.runtime_root,
        ))
    }

    async fn dispatch_cancel(&self, frame: &[u8]) -> Result<Vec<u8>, CampaignOperationRejection> {
        let request: OperatorCancelCampaignRequest = decode_operation_request(
            frame,
            factory_protocol::REQUEST_FRAME_MAX_BYTES,
            OP_OPERATOR_CANCEL_CAMPAIGN,
        )
        .map_err(CampaignOperationRejection::Frame)?;
        let command = CancelCampaign {
            principal: request.principal,
            command_id: request.client_command_id,
            expected_revision: expected_revision(request.expected_revision),
            campaign_id: CampaignId::new(request.campaign_id)
                .map_err(CampaignOperationRejection::Contract)?,
        };
        let receipt = match self
            .process
            .admit_campaign_cancellation(&command)
            .await
            .map_err(CampaignOperationRejection::Store)?
        {
            CampaignCancellationAdmission::Completed(receipt) => receipt,
            CampaignCancellationAdmission::ActiveSession { session_id } => {
                let reconciled = self
                    .active_sessions
                    .cancel_and_wait(session_id)
                    .await
                    .map_err(CampaignOperationRejection::Session)?;
                self.process
                    .finish_campaign_cancellation(&command, reconciled)
                    .await
                    .map_err(CampaignOperationRejection::Store)?
            }
        };
        Ok(campaign_receipt_response(
            request.request_id,
            OP_OPERATOR_CANCEL_CAMPAIGN,
            receipt,
        ))
    }
}

#[derive(Debug, Error)]
pub(crate) enum CampaignOperatorRpcError {
    #[error(transparent)]
    Frame(#[from] FrameError),

    #[error("operation {operation:?} is not a campaign operation")]
    OperationNotCampaign { operation: String },
}

#[derive(Debug)]
enum CampaignOperationRejection {
    Frame(FrameError),
    Contract(ContractError),
    Store(StoreError),
    Session(SessionRuntimeError),
}

impl CampaignOperationRejection {
    fn response(self, request_id: String, operation: String) -> Vec<u8> {
        match self {
            Self::Store(StoreError::RevisionConflict { current, .. }) => {
                json::to_string(&ConflictResponse {
                    protocol_version: PROTOCOL_VERSION_V2,
                    request_id,
                    operation,
                    error_code: "revision_conflict".to_owned(),
                    current_revision: current.get(),
                    message: "the observed aggregate revision is stale".to_owned(),
                })
                .into_bytes()
            }
            Self::Store(error) => error_response(
                request_id,
                operation,
                campaign_store_error_code(&error),
                &error.to_string(),
            ),
            Self::Session(error) => error_response(
                request_id,
                operation,
                "campaign_cancellation_failed",
                &error.to_string(),
            ),
            Self::Contract(error) => error_response(
                request_id,
                operation,
                "invalid_campaign_request",
                &error.to_string(),
            ),
            Self::Frame(error) => error_response(
                request_id,
                operation,
                "invalid_campaign_request",
                &error.to_string(),
            ),
        }
    }
}

fn campaign_receipt_response(
    request_id: String,
    operation: &'static str,
    receipt: CampaignReceipt,
) -> Vec<u8> {
    json::to_string(&CampaignReceiptResponse {
        protocol_version: PROTOCOL_VERSION_V2,
        request_id,
        operation: operation.to_owned(),
        audit_id: receipt.audit_log_id,
        aggregate_revision: receipt.resulting_revision.get(),
        campaign_id: receipt.campaign_id.get(),
        kernel_build_id: receipt.kernel_build_id.digest().to_hex(),
        application_revision_id: receipt.application_revision_id.get(),
        repository_id: receipt.repository_id.get(),
        was_idempotent_retry: receipt.was_idempotent_retry,
    })
    .into_bytes()
}

fn campaign_status_response(
    request_id: String,
    campaign: crate::process::CampaignStatus,
    buffer: &crate::ticket_store::TicketBufferStatus,
    product_identity: crate::process::CampaignProductIdentity,
    session_costs: &[crate::process::SessionCostBreakdown],
    session_cost_aggregates: &[crate::process::SessionCostAggregate],
    runtime_root: &Path,
) -> Vec<u8> {
    let (measured_cost_state, measured_cost_micro_usd, remaining_budget_micro_usd) =
        campaign_cost_projection(campaign.measured_cost, campaign.aggregate_budget);
    let action = TicketScheduler::decide(buffer);
    let (scheduler_next_action, scheduler_constraint) = scheduler_projection(&action);
    let (oldest_sponsored_ticket_revision_id, oldest_sponsored_ticket_revision) = buffer
        .oldest_sponsored_ticket
        .map(|head| {
            (
                Some(head.ticket_revision_id.get()),
                Some(head.revision.get()),
            )
        })
        .unwrap_or((None, None));
    let (
        downstream_action_stage,
        downstream_ticket_attempt_id,
        downstream_ticket_attempt_revision,
        downstream_candidate_id,
        downstream_candidate_revision,
    ) = buffer
        .downstream_action
        .map(|action| {
            (
                Some(action.stage.name().to_owned()),
                Some(action.ticket_attempt_id.get()),
                Some(action.ticket_attempt_revision.get()),
                Some(action.candidate_id.get()),
                Some(action.candidate_revision.get()),
            )
        })
        .unwrap_or((None, None, None, None, None));
    let downstream_evidence =
        buffer
            .downstream_evidence
            .as_ref()
            .map(|evidence| DownstreamEvidenceResponse {
                candidate_commit: evidence.candidate_commit.clone(),
                latest_validation: evidence.latest_validation.map(|validation| {
                    DownstreamValidationEvidenceResponse {
                        validation_id: validation.validation_id.get(),
                        state: validation.state.name().to_owned(),
                        log_artifact_id: validation.log_artifact_id.get(),
                    }
                }),
                review: evidence
                    .review
                    .map(|review| DownstreamReviewEvidenceResponse {
                        review_id: review.review_id.get(),
                        review_revision: review.revision.get(),
                        verdict: review.verdict.name().to_owned(),
                        rationale_artifact_id: review.rationale_artifact_id.get(),
                    }),
                architect_decision: evidence.architect_decision.map(|decision| {
                    DownstreamArchitectDecisionEvidenceResponse {
                        architect_decision_id: decision.architect_decision_id.get(),
                        decision_kind: decision.kind.name().to_owned(),
                        rationale_artifact_id: decision.rationale_artifact_id.get(),
                    }
                }),
            });
    json::to_string(&CampaignStatusResponse {
        protocol_version: PROTOCOL_VERSION_V2,
        request_id,
        operation: OP_OPERATOR_CAMPAIGN_STATUS.to_owned(),
        campaign_id: campaign.campaign_id.get(),
        state: campaign_state_name(campaign.state).to_owned(),
        aggregate_revision: campaign.revision.get(),
        kernel_build_id: campaign.kernel_build_id.digest().to_hex(),
        application_revision_id: campaign.application_revision_id.get(),
        repository_id: campaign.repository_id.get(),
        aggregate_budget_micro_usd: campaign.aggregate_budget.get(),
        measured_cost_state: measured_cost_state.to_owned(),
        measured_cost_micro_usd,
        remaining_budget_micro_usd,
        deadline_unix_millis: campaign.deadline_unix_millis,
        delivery_target: campaign.delivery_target,
        failure_reason: campaign.failure_reason,
        base_commit: product_identity.base_commit,
        candidate_tree: product_identity.candidate_tree,
        candidate_commit: product_identity.candidate_commit,
        delivered_commit: product_identity.delivered_commit,
        delivered_factory_cost_micro_usd: product_identity.delivered_factory_cost_micro_usd,
        delivered_attempt_count: buffer.delivered_attempt_count,
        ready_ticket_count: buffer.ready_count,
        proposed_ticket_count: buffer.proposed_count,
        in_flight_ticket_count: buffer.in_flight_count,
        downstream_ticket_attempt_count: buffer.downstream_attempt_count,
        downstream_action_stage,
        downstream_ticket_attempt_id,
        downstream_ticket_attempt_revision,
        downstream_candidate_id,
        downstream_candidate_revision,
        downstream_evidence,
        ready_low_water: buffer.low_water,
        ready_target: buffer.target,
        ready_maximum: buffer.maximum,
        proposal_maximum: buffer.proposal_maximum,
        oldest_sponsored_ticket_revision_id,
        oldest_sponsored_ticket_revision,
        scheduler_next_action,
        scheduler_constraint,
        session_costs: session_costs
            .iter()
            .map(|session| {
                let (cost_state, cost_micro_usd) = session_cost_projection(session.cost);
                CampaignSessionCostResponse {
                    session_id: session.session_id.get(),
                    assignment_id: session.assignment_id.get(),
                    assignment_role: office_name(session.assignment_role).to_owned(),
                    model_provider: session.model_provider.clone(),
                    model_id: session.model_id.clone(),
                    outcome: session_state_name(session.outcome).to_owned(),
                    cost_state: cost_state.to_owned(),
                    cost_micro_usd,
                    elapsed_millis: session.elapsed_millis,
                    transcript_path: live_transcript_path(
                        runtime_root,
                        session.assignment_id,
                        session.outcome,
                    ),
                }
            })
            .collect(),
        session_cost_aggregates: session_cost_aggregates
            .iter()
            .map(|aggregate| CampaignSessionCostAggregateResponse {
                assignment_role: office_name(aggregate.assignment_role).to_owned(),
                model_provider: aggregate.model_provider.clone(),
                model_id: aggregate.model_id.clone(),
                outcome: session_state_name(aggregate.outcome).to_owned(),
                session_count: aggregate.session_count,
                accounted_cost_micro_usd: aggregate.accounted_cost_micro_usd,
                pending_cost_session_count: aggregate.pending_cost_session_count,
                unknown_cost_session_count: aggregate.unknown_cost_session_count,
                exceeded_cost_session_count: aggregate.exceeded_cost_session_count,
            })
            .collect(),
    })
    .into_bytes()
}

fn session_cost_projection(cost: Option<TerminalCostV2>) -> (&'static str, Option<u64>) {
    match cost {
        None => ("pending", None),
        Some(TerminalCostV2::Known(cost)) => ("known", Some(cost.get())),
        Some(TerminalCostV2::Unknown) => ("unknown", None),
        Some(TerminalCostV2::Exceeded(cost)) => ("exceeded", Some(cost.get())),
    }
}

fn office_name(assignment_role: factory_protocol::AssignmentRole) -> &'static str {
    match assignment_role {
        factory_protocol::AssignmentRole::ProductResearch => "product_research",
        factory_protocol::AssignmentRole::Engineering => "engineering",
        factory_protocol::AssignmentRole::Quality => "quality",
    }
}

fn live_transcript_path(
    runtime_root: &Path,
    assignment_id: factory_protocol::AssignmentId,
    state: factory_protocol::SessionState,
) -> Option<String> {
    if !matches!(
        state,
        factory_protocol::SessionState::Prepared | factory_protocol::SessionState::Running
    ) {
        return None;
    }
    Some(
        runtime_root
            .join("staging")
            .join(format!("assignment-{}", assignment_id.get()))
            .join(SESSION_PARTIAL_TRANSCRIPT_RELATIVE_PATH)
            .display()
            .to_string(),
    )
}

fn session_state_name(state: factory_protocol::SessionState) -> &'static str {
    match state {
        factory_protocol::SessionState::Prepared => "prepared",
        factory_protocol::SessionState::Running => "running",
        factory_protocol::SessionState::Succeeded => "succeeded",
        factory_protocol::SessionState::Failed => "failed",
        factory_protocol::SessionState::Cancelled => "cancelled",
        factory_protocol::SessionState::Interrupted => "interrupted",
    }
}

fn campaign_cost_projection(
    cost: TerminalCostV2,
    budget: factory_protocol::MicroUsd,
) -> (&'static str, Option<u64>, Option<u64>) {
    match cost {
        TerminalCostV2::Known(measured) => (
            "known",
            Some(measured.get()),
            Some(budget.get().saturating_sub(measured.get())),
        ),
        TerminalCostV2::Unknown => ("unknown", None, None),
        TerminalCostV2::Exceeded(measured) => ("exceeded", Some(measured.get()), Some(0)),
    }
}

fn campaign_state_name(state: factory_protocol::CampaignState) -> &'static str {
    match state {
        factory_protocol::CampaignState::Running => "running",
        factory_protocol::CampaignState::Completed => "completed",
        factory_protocol::CampaignState::Failed => "failed",
        factory_protocol::CampaignState::Cancelled => "cancelled",
    }
}

fn scheduler_projection(action: &SchedulerNextAction) -> (String, Option<String>) {
    match action {
        SchedulerNextAction::CompleteCampaign(_) => ("complete_campaign".to_owned(), None),
        SchedulerNextAction::ReplenishProduct { .. } => ("replenish_product".to_owned(), None),
        SchedulerNextAction::ClaimReadyTicket(_) => ("claim_ready_ticket".to_owned(), None),
        SchedulerNextAction::ContinueDownstream(_) => ("continue_downstream".to_owned(), None),
        SchedulerNextAction::AwaitArchitectDecision { .. } => {
            ("await_architect_decision".to_owned(), None)
        }
        SchedulerNextAction::Idle { .. } => ("idle".to_owned(), None),
        SchedulerNextAction::Blocked(constraint) => (
            "blocked".to_owned(),
            Some(scheduler_constraint_name(constraint).to_owned()),
        ),
    }
}

fn scheduler_constraint_name(constraint: &SchedulerConstraint) -> &'static str {
    match constraint {
        SchedulerConstraint::CampaignTerminal => "campaign_terminal",
        SchedulerConstraint::AggregateCostFrozen => "aggregate_cost_frozen",
        SchedulerConstraint::CampaignDeadlineElapsed => "campaign_deadline_elapsed",
        SchedulerConstraint::PaidSessionActive => "paid_session_active",
        SchedulerConstraint::InFlightTicketLimitReached { .. } => "in_flight_ticket_limit_reached",
        SchedulerConstraint::ReadyBufferMaximumExceeded { .. } => "ready_buffer_maximum_exceeded",
        SchedulerConstraint::ProposalBufferMaximumExceeded { .. } => {
            "proposal_buffer_maximum_exceeded"
        }
        SchedulerConstraint::DownstreamActionHeadMissing => "downstream_action_head_missing",
        SchedulerConstraint::DownstreamActionHeadUnexpected => "downstream_action_head_unexpected",
        SchedulerConstraint::ReadyBufferHeadMissing => "ready_buffer_head_missing",
        SchedulerConstraint::ReadyBufferHeadUnexpected => "ready_buffer_head_unexpected",
    }
}

fn campaign_store_error_code(error: &StoreError) -> &'static str {
    match error {
        StoreError::IdempotencyConflict { .. } => "idempotency_conflict",
        StoreError::CampaignAlreadyRunning => "campaign_already_running",
        StoreError::CampaignDeadlineElapsed => "campaign_deadline_elapsed",
        StoreError::CampaignHasRunningSession { .. } => "campaign_has_running_session",
        StoreError::CampaignClosed { .. } => "campaign_closed",
        StoreError::NoCurrentKernelBuild => "no_current_kernel_build",
        StoreError::UnknownApplicationRevision { .. } => "unknown_application_revision",
        StoreError::UnknownCampaign { .. } => "unknown_campaign",
        _ => "campaign_rejected",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use factory_protocol::{SealedArtifactReferenceWireV2, decode_json_frame, encode_json_frame};

    use super::*;

    #[derive(Clone)]
    struct CountingAuthority {
        calls: Arc<AtomicUsize>,
    }

    #[derive(Clone)]
    struct SponsoringAuthority {
        calls: Arc<AtomicUsize>,
    }

    impl ArchitectDecisionAuthority for CountingAuthority {
        fn sponsor_ticket<'a>(
            &'a self,
            _command: SponsorTicket,
        ) -> DecisionFuture<'a, SponsorshipReceipt> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err(DecisionStoreError::CorruptState) })
        }

        fn release_ticket_attempt<'a>(
            &'a self,
            _command: ReleaseTicketAttempt,
        ) -> DecisionFuture<'a, ReleaseReceipt> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err(DecisionStoreError::CorruptState) })
        }

        fn decide_candidate<'a>(
            &'a self,
            _command: DecideCandidate,
        ) -> DecisionFuture<'a, CandidateDecisionReceipt> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err(DecisionStoreError::CorruptState) })
        }
    }

    impl ArchitectDecisionAuthority for SponsoringAuthority {
        fn sponsor_ticket<'a>(
            &'a self,
            command: SponsorTicket,
        ) -> DecisionFuture<'a, SponsorshipReceipt> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(command.expected_ticket_revision.get().get(), 4);
            assert_eq!(command.decision.ticket_revision_id.get(), 7);
            Box::pin(async {
                Ok(SponsorshipReceipt {
                    decision: factory_protocol::ArchitectDecisionReceiptV2 {
                        architect_decision_id: factory_protocol::ArchitectDecisionId::new(22)
                            .expect("decision ID"),
                        kind: ArchitectDecisionKindV2::Sponsor,
                    },
                    ticket_revision_id: factory_protocol::TicketRevisionId::new(7)
                        .expect("ticket revision ID"),
                    resulting_ticket_revision: AggregateRevision::from_persisted(5),
                    audit_log_id: 21,
                    was_idempotent_retry: false,
                })
            })
        }

        fn release_ticket_attempt<'a>(
            &'a self,
            _command: ReleaseTicketAttempt,
        ) -> DecisionFuture<'a, ReleaseReceipt> {
            Box::pin(async { Err(DecisionStoreError::CorruptState) })
        }

        fn decide_candidate<'a>(
            &'a self,
            _command: DecideCandidate,
        ) -> DecisionFuture<'a, CandidateDecisionReceipt> {
            Box::pin(async { Err(DecisionStoreError::CorruptState) })
        }
    }

    fn router(calls: Arc<AtomicUsize>) -> OperatorRpc {
        OperatorRpc {
            authority: Arc::new(CountingAuthority { calls }),
            resolver: None,
        }
    }

    fn sponsorship_router(calls: Arc<AtomicUsize>) -> OperatorRpc {
        OperatorRpc {
            authority: Arc::new(SponsoringAuthority { calls }),
            resolver: None,
        }
    }

    fn rationale() -> SealedArtifactReferenceWireV2 {
        SealedArtifactReferenceWireV2 {
            artifact_id: 1,
            digest: "a".repeat(64),
            byte_length: 12,
        }
    }

    #[test]
    fn unresolved_release_never_reaches_durable_authority() {
        smol::block_on(async {
            let calls = Arc::new(AtomicUsize::new(0));
            let frame = encode_json_frame(
                &ArchitectReleaseTicketAttemptRequest {
                    protocol_version: PROTOCOL_VERSION_V2,
                    request_id: "release-1".to_owned(),
                    operation: OP_ARCHITECT_RELEASE_TICKET_ATTEMPT.to_owned(),
                    client_command_id: "release-command".to_owned(),
                    expected_revision: 4,
                    ticket_attempt_id: 7,
                    rationale: rationale(),
                    principal: "grand-architect".to_owned(),
                },
                factory_protocol::REQUEST_FRAME_MAX_BYTES,
            )
            .unwrap();
            let payload = router(calls.clone()).dispatch(&frame).await.unwrap();
            let response: ErrorResponse = decode_json_frame(
                &factory_protocol::encode_frame(
                    &payload,
                    factory_protocol::RESPONSE_FRAME_MAX_BYTES,
                )
                .unwrap(),
                factory_protocol::RESPONSE_FRAME_MAX_BYTES,
                OP_ARCHITECT_RELEASE_TICKET_ATTEMPT,
            )
            .unwrap();
            assert_eq!(response.error_code, "architect_transition_unavailable");
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        });
    }

    #[test]
    fn sponsorship_routes_only_the_explicit_operator_operation() {
        smol::block_on(async {
            let calls = Arc::new(AtomicUsize::new(0));
            let frame = encode_json_frame(
                &ArchitectSponsorTicketRevisionRequest {
                    protocol_version: PROTOCOL_VERSION_V2,
                    request_id: "sponsor-1".to_owned(),
                    operation: OP_ARCHITECT_SPONSOR_TICKET_REVISION.to_owned(),
                    client_command_id: "sponsor-command".to_owned(),
                    expected_revision: 4,
                    ticket_revision_id: 7,
                    rationale: rationale(),
                    principal: "grand-architect".to_owned(),
                },
                factory_protocol::REQUEST_FRAME_MAX_BYTES,
            )
            .unwrap();
            let payload = sponsorship_router(calls.clone())
                .dispatch(&frame)
                .await
                .unwrap();
            let response: ArchitectDecisionReceiptResponse = decode_json_frame(
                &factory_protocol::encode_frame(
                    &payload,
                    factory_protocol::RESPONSE_FRAME_MAX_BYTES,
                )
                .unwrap(),
                factory_protocol::RESPONSE_FRAME_MAX_BYTES,
                OP_ARCHITECT_SPONSOR_TICKET_REVISION,
            )
            .unwrap();
            assert_eq!(response.decision_kind, "sponsor");
            assert_eq!(response.architect_decision_id, 22);
            assert_eq!(response.aggregate_revision, 5);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn unresolved_candidate_decision_never_reaches_durable_authority() {
        smol::block_on(async {
            let calls = Arc::new(AtomicUsize::new(0));
            let frame = encode_json_frame(
                &ArchitectDecideCandidateRequest {
                    protocol_version: PROTOCOL_VERSION_V2,
                    request_id: "candidate-1".to_owned(),
                    operation: OP_ARCHITECT_DECIDE_CANDIDATE.to_owned(),
                    client_command_id: "candidate-command".to_owned(),
                    expected_revision: 4,
                    candidate_id: 7,
                    review_id: 8,
                    decision: "deliver".to_owned(),
                    rationale: rationale(),
                    quality_rejection_override_review_id: None,
                    principal: "grand-architect".to_owned(),
                },
                factory_protocol::REQUEST_FRAME_MAX_BYTES,
            )
            .unwrap();
            let payload = router(calls.clone()).dispatch(&frame).await.unwrap();
            let response: ErrorResponse = decode_json_frame(
                &factory_protocol::encode_frame(
                    &payload,
                    factory_protocol::RESPONSE_FRAME_MAX_BYTES,
                )
                .unwrap(),
                factory_protocol::RESPONSE_FRAME_MAX_BYTES,
                OP_ARCHITECT_DECIDE_CANDIDATE,
            )
            .unwrap();
            assert_eq!(response.error_code, "architect_transition_unavailable");
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        });
    }

    #[test]
    fn campaign_cost_and_scheduler_explanations_are_closed_and_provider_free() {
        assert_eq!(
            campaign_cost_projection(
                TerminalCostV2::Known(factory_protocol::MicroUsd::new(4)),
                factory_protocol::MicroUsd::new(10)
            ),
            ("known", Some(4), Some(6))
        );
        assert_eq!(
            campaign_cost_projection(TerminalCostV2::Unknown, factory_protocol::MicroUsd::new(10)),
            ("unknown", None, None)
        );
        assert_eq!(
            scheduler_projection(&SchedulerNextAction::Blocked(
                SchedulerConstraint::InFlightTicketLimitReached {
                    in_flight_count: 1,
                    maximum: 1,
                },
            )),
            (
                "blocked".to_owned(),
                Some("in_flight_ticket_limit_reached".to_owned())
            )
        );
    }
}
