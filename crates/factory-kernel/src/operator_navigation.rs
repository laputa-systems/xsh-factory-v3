//! Bounded, read-only operator navigation over durable Factory facts.
//!
//! This is deliberately one concrete PostgreSQL projection rather than a
//! repository layer or a query language.  Every route below has a named,
//! finite shape, a stable order, and a hard row ceiling.  None creates an
//! audit receipt: inspection must not turn polling into durable work.

use factory_protocol::{
    AuditEntryResponse, AuditShowResponse, CandidateDecisionNavigationResponse,
    CandidateReviewNavigationResponse, CandidateShowResponse,
    CandidateValidationNavigationResponse, ContentDigest, ContractError,
    DeliveryNavigationResponse, ErrorResponse, EvidenceArtifactResponse, FrameError,
    OP_OPERATOR_LIST_TICKETS, OP_OPERATOR_SHOW_AUDIT, OP_OPERATOR_SHOW_CANDIDATE,
    OP_OPERATOR_SHOW_TICKET, OperatorAuditShowRequest, OperatorCandidateShowRequest,
    OperatorTicketListRequest, OperatorTicketShowRequest, PROTOCOL_VERSION_V1,
    TicketAttemptNavigationResponse, TicketListItemResponse, TicketListResponse,
    TicketShowResponse, decode_operation_request, decode_routing_envelope,
};
use miniserde::json;
use sqlx::PgPool;
use thiserror::Error;

use crate::storage::KernelStore;

macro_rules! audit_entry_from {
    ($row:expr) => {
        AuditEntryResponse {
            audit_id: positive($row.id, "audit ID")?,
            principal: $row.principal,
            operation: $row.operation,
            subject_kind: $row.subject_kind,
            subject_id: positive($row.subject_id, "audit subject ID")?,
            aggregate_revision: revision($row.resulting_revision)?,
        }
    };
}

/// Capability minted only after the operator socket is bound.  Actor and
/// application frames cannot manufacture navigation authority by spelling an
/// operation name in JSON.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OperatorNavigationCapability {
    _private: (),
}

impl OperatorNavigationCapability {
    pub(crate) const fn from_operator_transport() -> Self {
        Self { _private: () }
    }
}

/// Concrete read-only authority retained by the daemon.  The raw pool remains
/// private, so callers receive only the fixed protocol projections below.
#[derive(Clone, Debug)]
pub(crate) struct OperatorNavigationRpc {
    pool: PgPool,
}

impl OperatorNavigationRpc {
    pub(crate) fn from_operator_transport(
        _capability: OperatorNavigationCapability,
        store: KernelStore,
    ) -> Self {
        Self {
            pool: store.pool_for_authority(),
        }
    }

    pub(crate) async fn dispatch(
        &self,
        frame: &[u8],
    ) -> Result<Vec<u8>, OperatorNavigationRpcError> {
        let envelope = decode_routing_envelope(frame, factory_protocol::REQUEST_FRAME_MAX_BYTES)?;
        let request_id = envelope.request_id.clone();
        let operation = envelope.operation.clone();
        let response = match operation.as_str() {
            OP_OPERATOR_LIST_TICKETS => self.list_tickets(frame).await,
            OP_OPERATOR_SHOW_TICKET => self.show_ticket(frame).await,
            OP_OPERATOR_SHOW_CANDIDATE => self.show_candidate(frame).await,
            OP_OPERATOR_SHOW_AUDIT => self.show_audit(frame).await,
            _ => return Err(OperatorNavigationRpcError::OperationNotNavigation { operation }),
        };
        Ok(match response {
            Ok(response) => response,
            Err(rejection) => rejection.response(request_id, envelope.operation),
        })
    }

    async fn list_tickets(&self, frame: &[u8]) -> Result<Vec<u8>, NavigationRejection> {
        let request: OperatorTicketListRequest = decode_operation_request(
            frame,
            factory_protocol::REQUEST_FRAME_MAX_BYTES,
            OP_OPERATOR_LIST_TICKETS,
        )
        .map_err(NavigationRejection::Frame)?;
        let state = request
            .state
            .as_deref()
            .map(ticket_state_code)
            .transpose()
            .map_err(NavigationRejection::Contract)?;
        let rows = sqlx::query!(
            "SELECT t.id AS ticket_id, tr.id AS ticket_revision_id, tr.revision,
                    tr.application_revision_id, tr.lifecycle, tr.proposal_artifact_id,
                    floor(extract(epoch FROM tr.created_at) * 1000000)::BIGINT
                        AS \"created_at_micros!\"
             FROM factory.tickets AS t
             JOIN factory.ticket_revisions AS tr ON tr.id = t.current_ticket_revision_id
             WHERE ($1::SMALLINT IS NULL OR tr.lifecycle = $1)
             ORDER BY tr.created_at DESC, tr.id DESC
             LIMIT 20",
            state,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(navigation_database)?;
        let items = rows
            .into_iter()
            .map(|row| {
                Ok(TicketListItemResponse {
                    ticket_id: positive(row.ticket_id, "ticket ID")?,
                    ticket_revision_id: positive(row.ticket_revision_id, "ticket revision ID")?,
                    ticket_revision: revision(row.revision)?,
                    application_revision_id: positive(
                        row.application_revision_id,
                        "application revision ID",
                    )?,
                    state: ticket_state_name(row.lifecycle)?.to_owned(),
                    proposal_artifact_id: positive(
                        row.proposal_artifact_id,
                        "proposal artifact ID",
                    )?,
                    created_at_micros: micros(row.created_at_micros)?,
                })
            })
            .collect::<Result<Vec<_>, NavigationError>>()
            .map_err(NavigationRejection::Navigation)?;
        Ok(json::to_string(&TicketListResponse {
            protocol_version: PROTOCOL_VERSION_V1,
            request_id: request.request_id,
            operation: OP_OPERATOR_LIST_TICKETS.to_owned(),
            items,
        })
        .into_bytes())
    }

    async fn show_ticket(&self, frame: &[u8]) -> Result<Vec<u8>, NavigationRejection> {
        let request: OperatorTicketShowRequest = decode_operation_request(
            frame,
            factory_protocol::REQUEST_FRAME_MAX_BYTES,
            OP_OPERATOR_SHOW_TICKET,
        )
        .map_err(NavigationRejection::Frame)?;
        let ticket_id =
            positive(request.ticket_id, "ticket ID").map_err(NavigationRejection::Navigation)?;
        let ticket = sqlx::query!(
            "SELECT t.id AS ticket_id, tr.id AS ticket_revision_id, tr.revision,
                    tr.application_revision_id, tr.lifecycle, tr.sponsorship_reason,
                    tr.blocked_reason,
                    proposal.id AS proposal_artifact_id, proposal.digest AS proposal_digest,
                    proposal.byte_length AS proposal_byte_length,
                    reproducer.id AS reproducer_artifact_id, reproducer.digest AS reproducer_digest,
                    reproducer.byte_length AS reproducer_byte_length,
                    expected_observation.id AS expected_observation_artifact_id,
                    expected_observation.digest AS expected_observation_digest,
                    expected_observation.byte_length AS expected_observation_byte_length,
                    discovery_observation.id AS discovery_observation_artifact_id,
                    discovery_observation.digest AS discovery_observation_digest,
                    discovery_observation.byte_length AS discovery_observation_byte_length,
                    requalification_first.id AS requalification_first_artifact_id,
                    requalification_first.digest AS requalification_first_digest,
                    requalification_first.byte_length AS requalification_first_byte_length,
                    requalification_second.id AS requalification_second_artifact_id,
                    requalification_second.digest AS requalification_second_digest,
                    requalification_second.byte_length AS requalification_second_byte_length
             FROM factory.tickets AS t
             JOIN factory.ticket_revisions AS tr ON tr.id = t.current_ticket_revision_id
             JOIN factory.artifacts AS proposal ON proposal.id = tr.proposal_artifact_id
             JOIN factory.artifacts AS reproducer ON reproducer.id = tr.reproducer_artifact_id
             JOIN factory.artifacts AS expected_observation
                 ON expected_observation.id = tr.expected_observation_artifact_id
             JOIN factory.artifacts AS discovery_observation
                 ON discovery_observation.id = tr.discovery_observation_artifact_id
             LEFT JOIN factory.artifacts AS requalification_first
                 ON requalification_first.id = tr.last_requalification_first_observation_artifact_id
             LEFT JOIN factory.artifacts AS requalification_second
                 ON requalification_second.id = tr.last_requalification_second_observation_artifact_id
             WHERE t.id = $1",
            ticket_id as i64,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(navigation_database)?
        .ok_or(NavigationRejection::Navigation(NavigationError::NotFound { subject: "ticket" }))?;
        let attempts = sqlx::query!(
            "SELECT attempt.id AS ticket_attempt_id, attempt.revision AS attempt_revision,
                    attempt.campaign_id, attempt.stage, candidate.id AS \"candidate_id?\"
             FROM factory.ticket_attempts AS attempt
             LEFT JOIN factory.candidates AS candidate ON candidate.ticket_attempt_id = attempt.id
             WHERE attempt.ticket_revision_id = $1
             ORDER BY attempt.created_at DESC, attempt.id DESC
             LIMIT 20",
            ticket.ticket_revision_id,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(navigation_database)?
        .into_iter()
        .map(|row| {
            Ok(TicketAttemptNavigationResponse {
                ticket_attempt_id: positive(row.ticket_attempt_id, "ticket attempt ID")?,
                attempt_revision: revision(row.attempt_revision)?,
                campaign_id: positive(row.campaign_id, "campaign ID")?,
                stage: attempt_stage_name(row.stage)?.to_owned(),
                candidate_id: row
                    .candidate_id
                    .map(|id| positive(id, "candidate ID"))
                    .transpose()?,
            })
        })
        .collect::<Result<Vec<_>, NavigationError>>()
        .map_err(NavigationRejection::Navigation)?;
        let mut evidence = vec![
            artifact(
                "proposal",
                ticket.proposal_artifact_id,
                ticket.proposal_digest,
                ticket.proposal_byte_length,
            )?,
            artifact(
                "reproducer",
                ticket.reproducer_artifact_id,
                ticket.reproducer_digest,
                ticket.reproducer_byte_length,
            )?,
            artifact(
                "expected_observation",
                ticket.expected_observation_artifact_id,
                ticket.expected_observation_digest,
                ticket.expected_observation_byte_length,
            )?,
            artifact(
                "discovery_observation",
                ticket.discovery_observation_artifact_id,
                ticket.discovery_observation_digest,
                ticket.discovery_observation_byte_length,
            )?,
        ];
        if let (Some(id), Some(digest), Some(length)) = (
            ticket.requalification_first_artifact_id,
            ticket.requalification_first_digest,
            ticket.requalification_first_byte_length,
        ) {
            evidence.push(artifact(
                "last_requalification_first_observation",
                id,
                digest,
                length,
            )?);
        }
        if let (Some(id), Some(digest), Some(length)) = (
            ticket.requalification_second_artifact_id,
            ticket.requalification_second_digest,
            ticket.requalification_second_byte_length,
        ) {
            evidence.push(artifact(
                "last_requalification_second_observation",
                id,
                digest,
                length,
            )?);
        }
        Ok(json::to_string(&TicketShowResponse {
            protocol_version: PROTOCOL_VERSION_V1,
            request_id: request.request_id,
            operation: OP_OPERATOR_SHOW_TICKET.to_owned(),
            ticket_id: positive(ticket.ticket_id, "ticket ID")?,
            ticket_revision_id: positive(ticket.ticket_revision_id, "ticket revision ID")?,
            ticket_revision: revision(ticket.revision)?,
            application_revision_id: positive(
                ticket.application_revision_id,
                "application revision ID",
            )?,
            state: ticket_state_name(ticket.lifecycle)?.to_owned(),
            sponsorship_reason: ticket.sponsorship_reason,
            blocked_reason: ticket.blocked_reason,
            evidence,
            attempts,
        })
        .into_bytes())
    }

    async fn show_candidate(&self, frame: &[u8]) -> Result<Vec<u8>, NavigationRejection> {
        let request: OperatorCandidateShowRequest = decode_operation_request(
            frame,
            factory_protocol::REQUEST_FRAME_MAX_BYTES,
            OP_OPERATOR_SHOW_CANDIDATE,
        )
        .map_err(NavigationRejection::Frame)?;
        let candidate_id = positive(request.candidate_id, "candidate ID")
            .map_err(NavigationRejection::Navigation)?;
        let candidate = sqlx::query!(
            "SELECT candidate.id AS candidate_id, candidate.revision AS candidate_revision,
                    candidate.lifecycle, candidate.ticket_attempt_id,
                    ticket_revision.id AS ticket_revision_id,
                    ticket_revision.revision AS ticket_revision,
                    candidate.base_commit, candidate.candidate_tree, candidate.candidate_commit,
                    changed_paths.id AS changed_paths_artifact_id,
                    changed_paths.digest AS changed_paths_digest,
                    changed_paths.byte_length AS changed_paths_byte_length,
                    patch.id AS patch_artifact_id, patch.digest AS patch_digest,
                    patch.byte_length AS patch_byte_length,
                    engineering_report.id AS engineering_report_artifact_id,
                    engineering_report.digest AS engineering_report_digest,
                    engineering_report.byte_length AS engineering_report_byte_length,
                    risks.id AS risks_artifact_id, risks.digest AS risks_digest,
                    risks.byte_length AS risks_byte_length
             FROM factory.candidates AS candidate
             JOIN factory.ticket_attempts AS attempt ON attempt.id = candidate.ticket_attempt_id
             JOIN factory.ticket_revisions AS ticket_revision ON ticket_revision.id = attempt.ticket_revision_id
             JOIN factory.artifacts AS changed_paths ON changed_paths.id = candidate.changed_paths_artifact_id
             JOIN factory.artifacts AS patch ON patch.id = candidate.patch_artifact_id
             JOIN factory.artifacts AS engineering_report ON engineering_report.id = candidate.engineering_report_artifact_id
             JOIN factory.artifacts AS risks ON risks.id = candidate.risks_artifact_id
             WHERE candidate.id = $1",
            candidate_id as i64,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(navigation_database)?
        .ok_or(NavigationRejection::Navigation(NavigationError::NotFound { subject: "candidate" }))?;
        let validations = sqlx::query!(
            "SELECT validation.id AS validation_id, validation.validation_scope,
                    validation.lifecycle, log.id AS log_artifact_id, log.digest AS log_digest,
                    log.byte_length AS log_byte_length
             FROM factory.validations AS validation
             JOIN factory.artifacts AS log ON log.id = validation.log_artifact_id
             WHERE validation.candidate_id = $1
             ORDER BY validation.validation_scope ASC, validation.id ASC
             LIMIT 2",
            candidate.candidate_id,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(navigation_database)?
        .into_iter()
        .map(|row| {
            Ok(CandidateValidationNavigationResponse {
                validation_id: positive(row.validation_id, "validation ID")?,
                scope: validation_scope_name(row.validation_scope)?.to_owned(),
                state: validation_state_name(row.lifecycle)?.to_owned(),
                log: artifact(
                    "validation_log",
                    row.log_artifact_id,
                    row.log_digest,
                    row.log_byte_length,
                )?,
            })
        })
        .collect::<Result<Vec<_>, NavigationError>>()
        .map_err(NavigationRejection::Navigation)?;
        let review = sqlx::query!(
            "SELECT review.id AS review_id, review.revision AS review_revision, review.verdict,
                    rationale.id AS rationale_artifact_id, rationale.digest AS rationale_digest,
                    rationale.byte_length AS rationale_byte_length,
                    risks.id AS risks_artifact_id, risks.digest AS risks_digest,
                    risks.byte_length AS risks_byte_length
             FROM factory.reviews AS review
             JOIN factory.artifacts AS rationale ON rationale.id = review.rationale_artifact_id
             JOIN factory.artifacts AS risks ON risks.id = review.risks_artifact_id
             WHERE review.candidate_id = $1",
            candidate.candidate_id,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(navigation_database)?
        .map(|row| {
            Ok(CandidateReviewNavigationResponse {
                review_id: positive(row.review_id, "review ID")?,
                review_revision: revision(row.review_revision)?,
                verdict: review_verdict_name(row.verdict)?.to_owned(),
                rationale: artifact(
                    "review_rationale",
                    row.rationale_artifact_id,
                    row.rationale_digest,
                    row.rationale_byte_length,
                )?,
                risks: artifact(
                    "review_risks",
                    row.risks_artifact_id,
                    row.risks_digest,
                    row.risks_byte_length,
                )?,
            })
        })
        .transpose()
        .map_err(NavigationRejection::Navigation)?;
        let decision = sqlx::query!(
            "SELECT decision.id AS architect_decision_id, decision.decision_kind,
                    rationale.id AS rationale_artifact_id, rationale.digest AS rationale_digest,
                    rationale.byte_length AS rationale_byte_length
             FROM factory.architect_decisions AS decision
             JOIN factory.artifacts AS rationale ON rationale.id = decision.rationale_artifact_id
             WHERE decision.candidate_id = $1
             ORDER BY decision.id DESC LIMIT 1",
            candidate.candidate_id,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(navigation_database)?
        .map(|row| {
            Ok(CandidateDecisionNavigationResponse {
                architect_decision_id: positive(
                    row.architect_decision_id,
                    "architect decision ID",
                )?,
                decision_kind: decision_kind_name(row.decision_kind)?.to_owned(),
                rationale: artifact(
                    "architect_decision_rationale",
                    row.rationale_artifact_id,
                    row.rationale_digest,
                    row.rationale_byte_length,
                )?,
            })
        })
        .transpose()
        .map_err(NavigationRejection::Navigation)?;
        let delivery = sqlx::query!(
            "SELECT delivery.id AS delivery_id, delivery.resulting_commit,
                    delivery.factory_cost_micro_usd,
                    receipt.id AS receipt_artifact_id, receipt.digest AS receipt_digest,
                    receipt.byte_length AS receipt_byte_length
             FROM factory.deliveries AS delivery
             JOIN factory.artifacts AS receipt ON receipt.id = delivery.receipt_artifact_id
             WHERE delivery.candidate_id = $1",
            candidate.candidate_id,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(navigation_database)?;
        let delivery_receipt = delivery
            .as_ref()
            .map(|row| {
                artifact(
                    "delivery_receipt",
                    row.receipt_artifact_id,
                    row.receipt_digest.clone(),
                    row.receipt_byte_length,
                )
            })
            .transpose()
            .map_err(NavigationRejection::Navigation)?;
        let delivery = delivery
            .map(|row| {
                Ok(DeliveryNavigationResponse {
                    delivery_id: positive(row.delivery_id, "delivery ID")?,
                    resulting_commit: row.resulting_commit,
                    factory_cost_micro_usd: u64::try_from(row.factory_cost_micro_usd).map_err(
                        |_| NavigationError::Corrupt {
                            field: "delivery Factory-Cost",
                        },
                    )?,
                })
            })
            .transpose()
            .map_err(NavigationRejection::Navigation)?;
        Ok(json::to_string(&CandidateShowResponse {
            protocol_version: PROTOCOL_VERSION_V1,
            request_id: request.request_id,
            operation: OP_OPERATOR_SHOW_CANDIDATE.to_owned(),
            candidate_id: positive(candidate.candidate_id, "candidate ID")?,
            candidate_revision: revision(candidate.candidate_revision)?,
            state: candidate_state_name(candidate.lifecycle)?.to_owned(),
            ticket_attempt_id: positive(candidate.ticket_attempt_id, "ticket attempt ID")?,
            ticket_revision_id: positive(candidate.ticket_revision_id, "ticket revision ID")?,
            ticket_revision: revision(candidate.ticket_revision)?,
            base_commit: candidate.base_commit,
            candidate_tree: candidate.candidate_tree,
            candidate_commit: candidate.candidate_commit,
            evidence: vec![
                artifact(
                    "changed_paths",
                    candidate.changed_paths_artifact_id,
                    candidate.changed_paths_digest,
                    candidate.changed_paths_byte_length,
                )?,
                artifact(
                    "candidate_patch",
                    candidate.patch_artifact_id,
                    candidate.patch_digest,
                    candidate.patch_byte_length,
                )?,
                artifact(
                    "engineering_report",
                    candidate.engineering_report_artifact_id,
                    candidate.engineering_report_digest,
                    candidate.engineering_report_byte_length,
                )?,
                artifact(
                    "candidate_risks",
                    candidate.risks_artifact_id,
                    candidate.risks_digest,
                    candidate.risks_byte_length,
                )?,
            ],
            validations,
            review,
            latest_architect_decision: decision,
            delivery_receipt,
            delivery,
        })
        .into_bytes())
    }

    async fn show_audit(&self, frame: &[u8]) -> Result<Vec<u8>, NavigationRejection> {
        let request: OperatorAuditShowRequest = decode_operation_request(
            frame,
            factory_protocol::REQUEST_FRAME_MAX_BYTES,
            OP_OPERATOR_SHOW_AUDIT,
        )
        .map_err(NavigationRejection::Frame)?;
        let selector =
            AuditSelector::parse(&request.selector).map_err(NavigationRejection::Navigation)?;
        let entries = self
            .audit_entries(selector)
            .await
            .map_err(NavigationRejection::Navigation)?;
        Ok(json::to_string(&AuditShowResponse {
            protocol_version: PROTOCOL_VERSION_V1,
            request_id: request.request_id,
            operation: OP_OPERATOR_SHOW_AUDIT.to_owned(),
            selector: request.selector,
            items: entries,
        })
        .into_bytes())
    }

    async fn audit_entries(
        &self,
        selector: AuditSelector,
    ) -> Result<Vec<AuditEntryResponse>, NavigationError> {
        match selector {
            AuditSelector::Audit(id) => sqlx::query!(
                "SELECT id, principal, operation, subject_kind, subject_id, resulting_revision
                 FROM factory.audit_log WHERE id = $1 ORDER BY id DESC LIMIT 1",
                id
            )
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(|row| Ok(audit_entry_from!(row)))
            .collect(),
            AuditSelector::ApplicationRevision(id) => sqlx::query!(
                "SELECT id, principal, operation, subject_kind, subject_id, resulting_revision
                 FROM factory.audit_log
                 WHERE subject_kind = 1 AND subject_id = $1
                 ORDER BY id DESC LIMIT 20",
                id
            )
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(|row| Ok(audit_entry_from!(row)))
            .collect(),
            AuditSelector::Campaign(id) => sqlx::query!(
                "SELECT id, principal, operation, subject_kind, subject_id, resulting_revision
                 FROM factory.audit_log
                 WHERE subject_kind = 4 AND subject_id = $1
                 ORDER BY id DESC LIMIT 20",
                id
            )
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(|row| Ok(audit_entry_from!(row)))
            .collect(),
            AuditSelector::Ticket(id) => sqlx::query!(
                "SELECT audit.id, audit.principal, audit.operation, audit.subject_kind,
                        audit.subject_id, audit.resulting_revision
                 FROM factory.audit_log AS audit
                 LEFT JOIN factory.ticket_revisions AS direct_revision
                   ON audit.subject_id = direct_revision.id
                  AND audit.subject_kind IN (30, 31, 33, 34, 39)
                 LEFT JOIN factory.ticket_attempts AS attempt
                   ON audit.subject_id = attempt.id
                  AND (audit.subject_kind IN (32, 35, 36, 37, 38, 45)
                       OR (audit.subject_kind = 40
                           AND audit.operation = 'ticket_attempt.retry_quality'))
                 LEFT JOIN factory.ticket_revisions AS attempt_revision
                   ON attempt_revision.id = attempt.ticket_revision_id
                 WHERE direct_revision.ticket_id = $1 OR attempt_revision.ticket_id = $1
                 ORDER BY audit.id DESC LIMIT 20",
                id
            )
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(|row| Ok(audit_entry_from!(row)))
            .collect(),
            AuditSelector::Candidate(id) => sqlx::query!(
                "SELECT audit.id, audit.principal, audit.operation, audit.subject_kind,
                        audit.subject_id, audit.resulting_revision
                 FROM factory.audit_log AS audit
                 LEFT JOIN factory.candidates AS direct_candidate
                   ON audit.subject_kind = 40
                  AND audit.operation IN ('candidate.submit', 'candidate.commit.attach')
                  AND audit.subject_id = direct_candidate.id
                 LEFT JOIN factory.validations AS validation
                   ON audit.subject_kind = 41 AND audit.subject_id = validation.id
                 LEFT JOIN factory.reviews AS review
                   ON audit.subject_kind = 42 AND audit.subject_id = review.id
                 LEFT JOIN factory.architect_decisions AS decision
                   ON audit.subject_kind = 43 AND audit.subject_id = decision.id
                 LEFT JOIN factory.deliveries AS delivery
                   ON audit.subject_kind = 44 AND audit.subject_id = delivery.id
                 WHERE direct_candidate.id = $1 OR validation.candidate_id = $1
                    OR review.candidate_id = $1 OR decision.candidate_id = $1
                    OR delivery.candidate_id = $1
                 ORDER BY audit.id DESC LIMIT 20",
                id
            )
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(|row| Ok(audit_entry_from!(row)))
            .collect(),
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum OperatorNavigationRpcError {
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error("operation {operation:?} is not an operator navigation operation")]
    OperationNotNavigation { operation: String },
}

#[derive(Debug)]
enum NavigationRejection {
    Frame(FrameError),
    Contract(ContractError),
    Navigation(NavigationError),
}

impl From<NavigationError> for NavigationRejection {
    fn from(error: NavigationError) -> Self {
        Self::Navigation(error)
    }
}

fn navigation_database(error: sqlx::Error) -> NavigationRejection {
    NavigationRejection::Navigation(NavigationError::Database(error))
}

impl NavigationRejection {
    fn response(self, request_id: String, operation: String) -> Vec<u8> {
        let (error_code, message) = match self {
            Self::Frame(error) => ("invalid_navigation_request", error.to_string()),
            Self::Contract(error) => ("invalid_navigation_request", error.to_string()),
            Self::Navigation(error) => (error.code(), error.to_string()),
        };
        json::to_string(&ErrorResponse {
            protocol_version: PROTOCOL_VERSION_V1,
            request_id,
            operation,
            error_code: error_code.to_owned(),
            message,
        })
        .into_bytes()
    }
}

#[derive(Debug, Error)]
enum NavigationError {
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error("requested {subject} does not exist")]
    NotFound { subject: &'static str },
    #[error("navigation selector must be one closed `kind:positive-id` form")]
    InvalidSelector,
    #[error("stored {field} is outside its closed protocol range")]
    Corrupt { field: &'static str },
}

impl NavigationError {
    const fn code(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "navigation_not_found",
            Self::InvalidSelector => "invalid_audit_selector",
            Self::Database(_) | Self::Corrupt { .. } => "navigation_unavailable",
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum AuditSelector {
    Ticket(i64),
    Candidate(i64),
    Campaign(i64),
    ApplicationRevision(i64),
    Audit(i64),
}

impl AuditSelector {
    fn parse(value: &str) -> Result<Self, NavigationError> {
        let (kind, raw_id) = value
            .split_once(':')
            .ok_or(NavigationError::InvalidSelector)?;
        if raw_id.contains(':') {
            return Err(NavigationError::InvalidSelector);
        }
        let id = raw_id
            .parse::<i64>()
            .ok()
            .filter(|id| *id > 0)
            .ok_or(NavigationError::InvalidSelector)?;
        match kind {
            "ticket" => Ok(Self::Ticket(id)),
            "candidate" => Ok(Self::Candidate(id)),
            "campaign" => Ok(Self::Campaign(id)),
            "application-revision" => Ok(Self::ApplicationRevision(id)),
            "audit" => Ok(Self::Audit(id)),
            _ => Err(NavigationError::InvalidSelector),
        }
    }
}

fn ticket_state_code(value: &str) -> Result<i16, ContractError> {
    match value {
        "proposed" => Ok(0),
        "sponsored" => Ok(1),
        "in_flight" => Ok(2),
        "delivered" => Ok(3),
        "blocked" => Ok(4),
        "resolved" => Ok(5),
        "superseded" => Ok(6),
        "rejected" => Ok(7),
        _ => Err(ContractError::InvalidValue {
            field: "ticket state",
            reason: "must be a closed ticket lifecycle spelling",
        }),
    }
}

fn ticket_state_name(value: i16) -> Result<&'static str, NavigationError> {
    match value {
        0 => Ok("proposed"),
        1 => Ok("sponsored"),
        2 => Ok("in_flight"),
        3 => Ok("delivered"),
        4 => Ok("blocked"),
        5 => Ok("resolved"),
        6 => Ok("superseded"),
        7 => Ok("rejected"),
        _ => Err(NavigationError::Corrupt {
            field: "ticket lifecycle",
        }),
    }
}
fn attempt_stage_name(value: i16) -> Result<&'static str, NavigationError> {
    match value {
        0 => Ok("engineering"),
        1 => Ok("hard_validation"),
        2 => Ok("quality"),
        3 => Ok("awaiting_architect"),
        4 => Ok("rework_engineering"),
        5 => Ok("rework_validation"),
        6 => Ok("rework_quality"),
        7 => Ok("delivered"),
        8 => Ok("failed"),
        9 => Ok("cancelled"),
        _ => Err(NavigationError::Corrupt {
            field: "ticket attempt stage",
        }),
    }
}
fn candidate_state_name(value: i16) -> Result<&'static str, NavigationError> {
    match value {
        0 => Ok("submitted"),
        1 => Ok("validated"),
        2 => Ok("rejected"),
        3 => Ok("accepted"),
        4 => Ok("delivered"),
        _ => Err(NavigationError::Corrupt {
            field: "candidate lifecycle",
        }),
    }
}
fn validation_scope_name(value: i16) -> Result<&'static str, NavigationError> {
    match value {
        0 => Ok("hard_candidate"),
        1 => Ok("quality_full_suite"),
        _ => Err(NavigationError::Corrupt {
            field: "validation scope",
        }),
    }
}
fn validation_state_name(value: i16) -> Result<&'static str, NavigationError> {
    match value {
        1 => Ok("passed"),
        2 => Ok("failed"),
        3 => Ok("interrupted"),
        _ => Err(NavigationError::Corrupt {
            field: "validation lifecycle",
        }),
    }
}
fn review_verdict_name(value: i16) -> Result<&'static str, NavigationError> {
    match value {
        0 => Ok("accept"),
        1 => Ok("reject"),
        _ => Err(NavigationError::Corrupt {
            field: "review verdict",
        }),
    }
}
fn decision_kind_name(value: i16) -> Result<&'static str, NavigationError> {
    match value {
        0 => Ok("sponsor"),
        1 => Ok("release"),
        2 => Ok("deliver"),
        3 => Ok("rework"),
        4 => Ok("reject"),
        _ => Err(NavigationError::Corrupt {
            field: "architect decision kind",
        }),
    }
}

fn artifact(
    role: &str,
    id: i64,
    digest: Vec<u8>,
    byte_length: i64,
) -> Result<EvidenceArtifactResponse, NavigationError> {
    let digest: [u8; 32] = digest.try_into().map_err(|_| NavigationError::Corrupt {
        field: "artifact digest",
    })?;
    Ok(EvidenceArtifactResponse {
        role: role.to_owned(),
        artifact_id: positive(id, "artifact ID")?,
        digest: ContentDigest::from_bytes(digest).to_hex(),
        byte_length: u64::try_from(byte_length).map_err(|_| NavigationError::Corrupt {
            field: "artifact byte length",
        })?,
    })
}
fn positive(value: i64, field: &'static str) -> Result<i64, NavigationError> {
    if value > 0 {
        Ok(value)
    } else {
        Err(NavigationError::Corrupt { field })
    }
}
fn revision(value: i64) -> Result<u64, NavigationError> {
    u64::try_from(value).map_err(|_| NavigationError::Corrupt {
        field: "aggregate revision",
    })
}
fn micros(value: i64) -> Result<u64, NavigationError> {
    u64::try_from(value).map_err(|_| NavigationError::Corrupt { field: "timestamp" })
}

#[cfg(test)]
mod tests {
    use factory_protocol::{
        OperatorAuditShowRequest, OperatorTicketListRequest, encode_json_frame,
    };

    use crate::storage::KernelStore;

    use super::*;

    #[test]
    fn audit_selectors_are_closed_and_ticket_states_have_no_fallback() {
        assert!(matches!(
            AuditSelector::parse("ticket:7"),
            Ok(AuditSelector::Ticket(7))
        ));
        assert!(AuditSelector::parse("subject_kind:7").is_err());
        assert!(AuditSelector::parse("ticket:7:drop").is_err());
        assert!(ticket_state_code("anything").is_err());
    }

    #[test]
    #[ignore = "requires FACTORY_TEST_DATABASE_URL for a disposable PostgreSQL 18 database"]
    fn all_navigation_reads_leave_audit_unchanged() {
        smol::block_on(async {
            let database_url = test_database_url();
            let store = KernelStore::connect(&database_url).await.expect("connect");
            store.migrate_and_verify().await.expect("migration");
            let router = OperatorNavigationRpc::from_operator_transport(
                OperatorNavigationCapability::from_operator_transport(),
                store.clone(),
            );
            let before = audit_count(&store).await;
            let requests = vec![
                encode_json_frame(
                    &OperatorTicketListRequest {
                        protocol_version: PROTOCOL_VERSION_V1,
                        request_id: "navigation-list".to_owned(),
                        operation: OP_OPERATOR_LIST_TICKETS.to_owned(),
                        state: None,
                    },
                    factory_protocol::REQUEST_FRAME_MAX_BYTES,
                )
                .expect("frame"),
                encode_json_frame(
                    &OperatorTicketShowRequest {
                        protocol_version: PROTOCOL_VERSION_V1,
                        request_id: "navigation-ticket".to_owned(),
                        operation: OP_OPERATOR_SHOW_TICKET.to_owned(),
                        ticket_id: 1,
                    },
                    factory_protocol::REQUEST_FRAME_MAX_BYTES,
                )
                .expect("frame"),
                encode_json_frame(
                    &OperatorCandidateShowRequest {
                        protocol_version: PROTOCOL_VERSION_V1,
                        request_id: "navigation-candidate".to_owned(),
                        operation: OP_OPERATOR_SHOW_CANDIDATE.to_owned(),
                        candidate_id: 1,
                    },
                    factory_protocol::REQUEST_FRAME_MAX_BYTES,
                )
                .expect("frame"),
                encode_json_frame(
                    &OperatorAuditShowRequest {
                        protocol_version: PROTOCOL_VERSION_V1,
                        request_id: "navigation-audit".to_owned(),
                        operation: OP_OPERATOR_SHOW_AUDIT.to_owned(),
                        selector: "audit:1".to_owned(),
                    },
                    factory_protocol::REQUEST_FRAME_MAX_BYTES,
                )
                .expect("frame"),
            ];
            for frame in requests {
                router.dispatch(&frame).await.expect("navigation response");
            }
            assert_eq!(
                audit_count(&store).await,
                before,
                "navigation reads create no audit receipt"
            );
            store.close().await;
        });
    }

    async fn audit_count(store: &KernelStore) -> i64 {
        sqlx::query_scalar!("SELECT count(*)::BIGINT AS \"count!\" FROM factory.audit_log")
            .fetch_one(&store.pool_for_authority())
            .await
            .expect("audit count")
    }

    fn test_database_url() -> String {
        let database_url =
            std::env::var("FACTORY_TEST_DATABASE_URL").expect("FACTORY_TEST_DATABASE_URL");
        let name = database_url
            .rsplit('/')
            .next()
            .and_then(|value| value.split('?').next())
            .expect("database name");
        assert!(name.strip_prefix("factory_test_v3_").is_some_and(
            |suffix| !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        ));
        database_url
    }
}
