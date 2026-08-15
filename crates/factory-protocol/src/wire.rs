//! Closed local-socket wire values and frame boundaries.
//!
//! The daemon's Unix socket carries one length-prefixed JSON message at a
//! time.  The length prefix is deliberately outside JSON so a peer can reject
//! an oversized or truncated payload before allocating a parser buffer.  The
//! operation messages below are flat on purpose: miniserde's wire contract is
//! operation-specific rather than a dynamically tagged payload map.

use std::{fmt::Write as _, str::FromStr};

use miniserde::{Deserialize, Serialize, json};
use thiserror::Error;

use crate::{
    ASSIGNMENT_PACKET_V2_FORMAT, AbsoluteHostPath, ActorPolicyArtifactV2, ActorToolV2,
    ApplicationBundleV2, ApplicationKey, ApplicationRelativePath, ApprovedToolV2,
    ArchitectPrincipalV2, AssignmentPacketWireV2, AssignmentRole, AssignmentRoleProfileV2,
    CandidateDecisionRequestV2, CandidateDecisionV2, CandidateSubmissionV2, CommandObservationV2,
    CommandProfileV2, CommitMessagePolicyV2, ContentDigest, ContractError, DeliveryModeV2,
    DuplicateSearchInputV2, DurationMillis, EnvironmentAdditionV2, ExecutableV2, GitPolicyV2,
    InstitutionalObjectKind, InstitutionalReference, KernelBuildId, MAX_POLICY_ARTIFACT_BYTES,
    MAX_SESSION_OUTPUT_BYTES, MicroUsd, ModelCapabilityV2, ModelProfileV2,
    OP_FORUM_CREATE_THREAD_V2, OP_FORUM_CREATE_TOPIC_V2, OP_FORUM_LIST_THREADS_V2,
    OP_FORUM_LIST_TOPICS_V2, OP_FORUM_POST_V2, OP_FORUM_READ_THREAD_V2, OP_FORUM_SEARCH_V2,
    PolicyEntrypointV2, ProductTicketProposalV2, PublicationId, QualityFullSuiteRequestV2,
    QualityReviewSubmissionV2, ReleaseDecisionV2, RepositoryBindingV2, RepositoryRelativePath,
    RequiredReadV2, ReviewId, ReviewVerdict, RuntimeRelativePath, SealedArtifactReferenceV2,
    SessionLimitsV2, SponsorshipDecisionV2, TemplateArtifactV2, TemplatePlaceholderV2,
    ThinkingLevelV2, TicketAttemptId, TicketBoundsV2, TicketContractReadV2, TicketPolicyV2,
    TicketRevisionId, TwoRunReproducerV2, ValidationId, ValidationProfilesV2,
};

pub const PROTOCOL_VERSION_V2: u16 = 2;
pub const REQUEST_FRAME_MAX_BYTES: usize = 1 << 20;
pub const RESPONSE_FRAME_MAX_BYTES: usize = 4 << 20;
pub const FRAME_PREFIX_BYTES: usize = 4;

pub const OP_WORKSPACE_READ: &str = "workspace.read";
pub const OP_ARTIFACT_SEAL_WORKSPACE_FILE: &str = "artifact.seal_workspace_file";
pub const OP_ARTIFACT_READ: &str = "artifact.read";
pub const OP_PRODUCT_SUBMIT_TICKET: &str = "product.submit_ticket";
pub const OP_CANDIDATE_CHECKPOINT_REGRESSION: &str = "candidate.checkpoint_regression";
pub const OP_CANDIDATE_SUBMIT: &str = "candidate.submit";
pub const OP_QUALITY_RUN_FULL_SUITE: &str = "quality.run_full_suite";
pub const OP_QUALITY_SUBMIT_REVIEW: &str = "quality.submit_review";
pub const OP_WORK_COMPLETE: &str = "work.complete";
pub const OP_ARCHITECT_SPONSOR_TICKET_REVISION: &str = "architect.sponsor_ticket_revision";
pub const OP_ARCHITECT_RELEASE_TICKET_ATTEMPT: &str = "architect.release_ticket_attempt";
pub const OP_ARCHITECT_DECIDE_CANDIDATE: &str = "architect.decide_candidate";
/// Transport-owned daemon liveness probe. It has no actor identity or durable
/// side effect, but remains a closed typed operator frame for SDK parity.
pub const OP_FACTORYD_STATUS: &str = "factoryd.status";
/// Authenticated local-operator campaign admission. The daemon, rather than
/// the wire client, resolves the installed build and repository pin.
pub const OP_OPERATOR_START_CAMPAIGN: &str = "operator.campaign.start";
pub const OP_OPERATOR_CAMPAIGN_STATUS: &str = "operator.campaign.status";
pub const OP_OPERATOR_CANCEL_CAMPAIGN: &str = "operator.campaign.cancel";
/// Read-only inspection of one generic application revision on the local
/// operator socket.  The daemon returns identifiers only; it never executes
/// or expands application source for this operation.
pub const OP_OPERATOR_SHOW_APPLICATION: &str = "operator.application.show";
/// Operator-host bundle admission.  Rust/CAS re-reads and seals every named
/// byte beneath the supplied absolute root; factoryctl never sends bytes.
pub const OP_OPERATOR_REGISTER_APPLICATION: &str = "operator.application.register";
/// Explicit Grand Architect selection of the one active revision for an
/// application, allowed only when no campaign is running.
pub const OP_OPERATOR_ACTIVATE_APPLICATION: &str = "operator.application.activate";
/// Seals exactly one regular evidence file below an operator-selected root,
/// then records its immutable CAS identity. The client supplies no bytes.
pub const OP_OPERATOR_SEAL_ARTIFACT: &str = "operator.artifact.seal";
/// Bounded, read-only operator navigation over durable ticket state. These
/// operations deliberately name fixed projections rather than accepting a
/// filter language or a raw database query.
pub const OP_OPERATOR_LIST_TICKETS: &str = "operator.ticket.list";
pub const OP_OPERATOR_SHOW_TICKET: &str = "operator.ticket.show";
pub const OP_OPERATOR_SHOW_CANDIDATE: &str = "operator.candidate.show";
pub const OP_OPERATOR_SHOW_AUDIT: &str = "operator.audit.show";
/// Bounded read-only navigation over the concrete institutional relations.
/// The operation accepts only closed kind/id references; it is not a generic
/// object directory or query language.
pub const OP_OPERATOR_INSTITUTIONAL_SEARCH: &str = "operator.institutional.search";
pub const OP_OPERATOR_INSTITUTIONAL_SHOW: &str = "operator.institutional.show";
/// One local-operator authored immutable publication. Actor publication
/// creation never accepts office/session provenance; this separate command is
/// intentionally limited to the daemon's authenticated operator socket.
pub const OP_OPERATOR_PUBLICATION_CREATE: &str = "operator.publication.create";
/// One actor- or operator-authorized immutable publication. The connection
/// determines attribution; the wire may name only an anchor and sealed facts.
pub const OP_PUBLICATION_CREATE: &str = "publication.create";
pub const OP_SESSION_VERIFY_PACKET: &str = "session.verify_packet";
pub const OP_SESSION_SEAL_ARTIFACT: &str = "session.seal_artifact";
pub const OP_SESSION_SUBMIT_TERMINAL: &str = "session.submit_terminal";
pub const OP_FORUM_LIST_TOPICS: &str = OP_FORUM_LIST_TOPICS_V2;
pub const OP_FORUM_LIST_THREADS: &str = OP_FORUM_LIST_THREADS_V2;
pub const OP_FORUM_SEARCH: &str = OP_FORUM_SEARCH_V2;
pub const OP_FORUM_READ_THREAD: &str = OP_FORUM_READ_THREAD_V2;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FrameError {
    #[error("frame is truncated: expected {expected} bytes, received {received}")]
    Truncated { expected: usize, received: usize },
    #[error("frame has trailing bytes: expected {expected} bytes, received {received}")]
    TrailingBytes { expected: usize, received: usize },
    #[error("frame payload is {actual} bytes, exceeding the {maximum}-byte limit")]
    Oversized { actual: usize, maximum: usize },
    #[error("frame length prefix cannot be read")]
    MissingLength,
    #[error("frame payload is not valid UTF-8")]
    InvalidUtf8,
    #[error("frame JSON is invalid for {operation}: {detail}")]
    InvalidJson {
        operation: &'static str,
        detail: String,
    },
    #[error("frame operation is {actual:?}, expected {expected:?}")]
    WrongOperation {
        expected: &'static str,
        actual: String,
    },
    #[error("unknown frame operation {0:?}")]
    UnknownOperation(String),
    #[error("unsupported protocol version {0}")]
    UnsupportedProtocol(u16),
}

/// Encodes exactly one frame. `maximum` must be either the request or response
/// limit selected by the socket owner.
pub fn encode_frame(payload: &[u8], maximum: usize) -> Result<Vec<u8>, FrameError> {
    if payload.len() > maximum {
        return Err(FrameError::Oversized {
            actual: payload.len(),
            maximum,
        });
    }
    let length = u32::try_from(payload.len()).map_err(|_| FrameError::Oversized {
        actual: payload.len(),
        maximum,
    })?;
    let mut frame = Vec::with_capacity(FRAME_PREFIX_BYTES + payload.len());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// Decodes exactly one complete frame and borrows its payload from `frame`.
/// A stream reader should first collect the four-byte prefix and then exactly
/// that many bytes before calling this function; accepting a suffix here would
/// make request boundaries ambiguous.
pub fn decode_frame(frame: &[u8], maximum: usize) -> Result<&[u8], FrameError> {
    if frame.len() < FRAME_PREFIX_BYTES {
        return Err(FrameError::MissingLength);
    }
    let payload_length = u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
    if payload_length > maximum {
        return Err(FrameError::Oversized {
            actual: payload_length,
            maximum,
        });
    }
    let expected = FRAME_PREFIX_BYTES + payload_length;
    if frame.len() < expected {
        return Err(FrameError::Truncated {
            expected,
            received: frame.len(),
        });
    }
    if frame.len() > expected {
        return Err(FrameError::TrailingBytes {
            expected,
            received: frame.len(),
        });
    }
    Ok(&frame[FRAME_PREFIX_BYTES..])
}

pub fn encode_json_frame<T: Serialize>(value: &T, maximum: usize) -> Result<Vec<u8>, FrameError> {
    let payload = json::to_string(value);
    encode_frame(payload.as_bytes(), maximum)
}

pub fn decode_json_frame<T: Deserialize>(
    frame: &[u8],
    maximum: usize,
    operation: &'static str,
) -> Result<T, FrameError> {
    let payload = decode_frame(frame, maximum)?;
    let payload = std::str::from_utf8(payload).map_err(|_| FrameError::InvalidUtf8)?;
    json::from_str(payload).map_err(|error| FrameError::InvalidJson {
        operation,
        detail: format!("{error:?}"),
    })
}

/// The first parse performed by the daemon. The same bytes are then parsed
/// into one operation-specific request struct.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingEnvelope {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
}

pub fn decode_routing_envelope(
    frame: &[u8],
    maximum: usize,
) -> Result<RoutingEnvelope, FrameError> {
    let envelope: RoutingEnvelope = decode_json_frame(frame, maximum, "routing envelope")?;
    if envelope.protocol_version != PROTOCOL_VERSION_V2 {
        return Err(FrameError::UnsupportedProtocol(envelope.protocol_version));
    }
    if !is_known_operation(&envelope.operation) {
        return Err(FrameError::UnknownOperation(envelope.operation));
    }
    Ok(envelope)
}

pub fn decode_operation_request<T: Deserialize + Serialize>(
    frame: &[u8],
    maximum: usize,
    expected: &'static str,
) -> Result<T, FrameError> {
    let envelope = decode_routing_envelope(frame, maximum)?;
    require_operation(&envelope, expected)?;
    let payload = decode_frame(frame, maximum)?;
    let request: T = decode_json_frame(frame, maximum, expected)?;
    // miniserde's DTO decoder intentionally ignores unrecognized keys. At a
    // transport authority boundary that would turn an accidentally expanded
    // command into a silently accepted one. Re-serializing the closed DTO and
    // requiring byte identity rejects unknown/duplicate fields, alternate key
    // order, and whitespace without introducing an untyped JSON value into
    // the protocol domain.
    if json::to_string(&request).as_bytes() != payload {
        return Err(FrameError::InvalidJson {
            operation: expected,
            detail: "request bytes are not canonical V2 JSON or contain unknown fields".into(),
        });
    }
    Ok(request)
}

#[must_use]
pub fn is_known_operation(operation: &str) -> bool {
    matches!(
        operation,
        OP_WORKSPACE_READ
            | OP_ARTIFACT_SEAL_WORKSPACE_FILE
            | OP_ARTIFACT_READ
            | OP_PRODUCT_SUBMIT_TICKET
            | OP_CANDIDATE_CHECKPOINT_REGRESSION
            | OP_CANDIDATE_SUBMIT
            | OP_QUALITY_RUN_FULL_SUITE
            | OP_QUALITY_SUBMIT_REVIEW
            | OP_WORK_COMPLETE
            | OP_ARCHITECT_SPONSOR_TICKET_REVISION
            | OP_ARCHITECT_RELEASE_TICKET_ATTEMPT
            | OP_ARCHITECT_DECIDE_CANDIDATE
            | OP_FACTORYD_STATUS
            | OP_OPERATOR_START_CAMPAIGN
            | OP_OPERATOR_CAMPAIGN_STATUS
            | OP_OPERATOR_CANCEL_CAMPAIGN
            | OP_OPERATOR_SHOW_APPLICATION
            | OP_OPERATOR_REGISTER_APPLICATION
            | OP_OPERATOR_ACTIVATE_APPLICATION
            | OP_OPERATOR_SEAL_ARTIFACT
            | OP_OPERATOR_LIST_TICKETS
            | OP_OPERATOR_SHOW_TICKET
            | OP_OPERATOR_SHOW_CANDIDATE
            | OP_OPERATOR_SHOW_AUDIT
            | OP_OPERATOR_INSTITUTIONAL_SEARCH
            | OP_OPERATOR_INSTITUTIONAL_SHOW
            | OP_OPERATOR_PUBLICATION_CREATE
            | OP_PUBLICATION_CREATE
            | OP_SESSION_VERIFY_PACKET
            | OP_SESSION_SEAL_ARTIFACT
            | OP_SESSION_SUBMIT_TERMINAL
            | OP_FORUM_LIST_TOPICS
            | OP_FORUM_LIST_THREADS
            | OP_FORUM_SEARCH
            | OP_FORUM_READ_THREAD
            // Retired mutations remain parseable solely so an already-bound
            // actor receives a typed rejection rather than losing its socket.
            // They are absent from every SDK/host/application operation map
            // and no actor or operator router dispatches them.
            | OP_FORUM_CREATE_TOPIC_V2
            | OP_FORUM_CREATE_THREAD_V2
            | OP_FORUM_POST_V2
    )
}

pub fn require_operation(
    envelope: &RoutingEnvelope,
    expected: &'static str,
) -> Result<(), FrameError> {
    if envelope.operation == expected {
        Ok(())
    } else {
        Err(FrameError::WrongOperation {
            expected,
            actual: envelope.operation.clone(),
        })
    }
}

/// Common fields for a mutating operation. They are repeated in each flat
/// operation struct rather than represented by an untyped map or tagged union.
pub trait MutatingRequest {
    fn client_command_id(&self) -> &str;
    fn expected_revision(&self) -> u64;
}

macro_rules! mutating_request {
    ($name:ident { $($field:ident : $ty:ty),* $(,)? }) => {
        #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
        pub struct $name {
            pub protocol_version: u16,
            pub request_id: String,
            pub operation: String,
            pub client_command_id: String,
            pub expected_revision: u64,
            $(pub $field: $ty,)*
        }
        impl MutatingRequest for $name {
            fn client_command_id(&self) -> &str { &self.client_command_id }
            fn expected_revision(&self) -> u64 { self.expected_revision }
        }
    };
}

macro_rules! read_request {
    ($name:ident { $($field:ident : $ty:ty),* $(,)? }) => {
        #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
        pub struct $name {
            pub protocol_version: u16,
            pub request_id: String,
            pub operation: String,
            $(pub $field: $ty,)*
        }
    };
}

read_request!(WorkspaceReadRequest {
    repository_relative_path: String,
});
mutating_request!(ArtifactSealWorkspaceFileRequest {
    workspace_relative_path: String,
    byte_limit: u64,
});
read_request!(ArtifactReadRequest {
    artifact_id: i64,
    expected_digest: String,
});
mutating_request!(ProductSubmitTicketRequest {
    title: String,
    mission_value: String,
    scope: String,
    contract_owner: String,
    risk: String,
    narrative: SealedArtifactReferenceWireV2,
    evidence: SealedArtifactReferenceWireV2,
    acceptance_criteria: Vec<String>,
    contract_reads: Vec<TicketContractReadWireV2>,
    duplicate_search: DuplicateSearchInputWireV2,
    reproducer_profile: String,
    reproducer: TwoRunReproducerWireV2,
});

/// The immutable proposal-only spelling retained in a ticket revision.  It
/// deliberately excludes actor routing and retry fields, so a later kernel
/// authority can re-read the exact problem/reproducer closure without
/// reconstructing it from untrusted transcript data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductTicketProposalWireV2 {
    pub title: String,
    pub mission_value: String,
    pub scope: String,
    pub contract_owner: String,
    pub risk: String,
    pub narrative: SealedArtifactReferenceWireV2,
    pub evidence: SealedArtifactReferenceWireV2,
    pub acceptance_criteria: Vec<String>,
    pub contract_reads: Vec<TicketContractReadWireV2>,
    pub duplicate_search: DuplicateSearchInputWireV2,
    pub reproducer_profile: String,
    pub reproducer: TwoRunReproducerWireV2,
}

/// Wire reference to a previously sealed artifact. Large proposal bytes never
/// travel in a Product submission frame; the kernel verifies this reference
/// against its artifact record before assigning it a proposal meaning.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedArtifactReferenceWireV2 {
    pub artifact_id: i64,
    pub digest: String,
    pub byte_length: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandObservationWireV2 {
    pub exit_status: i32,
    pub stdout: SealedArtifactReferenceWireV2,
    pub stderr: SealedArtifactReferenceWireV2,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TwoRunReproducerWireV2 {
    pub comparison_rule_version: u16,
    pub command: SealedArtifactReferenceWireV2,
    pub stdin: Option<SealedArtifactReferenceWireV2>,
    pub expected_observation: CommandObservationWireV2,
    pub first_observation: CommandObservationWireV2,
    pub second_observation: CommandObservationWireV2,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketContractReadWireV2 {
    pub path: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuplicateSearchInputWireV2 {
    pub query: String,
    pub limit: u8,
}

impl ProductSubmitTicketRequest {
    /// Converts the JSON DTO into the generic Product contract and validates
    /// it against the exact ticket bounds pinned by the application revision.
    /// The kernel must still verify every artifact ID/digest/length against
    /// custody before committing a proposal.
    pub fn proposal(
        &self,
        bounds: &TicketBoundsV2,
    ) -> Result<ProductTicketProposalV2, ContractError> {
        self.proposal_wire().into_domain(bounds)
    }

    #[must_use]
    pub fn proposal_wire(&self) -> ProductTicketProposalWireV2 {
        ProductTicketProposalWireV2 {
            title: self.title.clone(),
            mission_value: self.mission_value.clone(),
            scope: self.scope.clone(),
            contract_owner: self.contract_owner.clone(),
            risk: self.risk.clone(),
            narrative: self.narrative.clone(),
            evidence: self.evidence.clone(),
            acceptance_criteria: self.acceptance_criteria.clone(),
            contract_reads: self.contract_reads.clone(),
            duplicate_search: self.duplicate_search.clone(),
            reproducer_profile: self.reproducer_profile.clone(),
            reproducer: self.reproducer.clone(),
        }
    }
}

/// Decodes one closed Product request without needing the application bounds.
/// The caller must immediately invoke [`ProductSubmitTicketRequest::proposal`]
/// with the exact admitted revision before treating it as a proposal.
pub fn decode_product_submit_ticket_request_v2(
    frame: &[u8],
) -> Result<ProductSubmitTicketRequest, FrameError> {
    decode_operation_request(frame, REQUEST_FRAME_MAX_BYTES, OP_PRODUCT_SUBMIT_TICKET)
}

/// Canonical proposal-only bytes used for immutable ticket custody. Routing,
/// retry, and session revision fields are intentionally excluded: they prove
/// transport authority but are not part of the Product problem contract.
#[must_use]
pub fn canonical_product_ticket_proposal_json_v2(request: &ProductSubmitTicketRequest) -> Vec<u8> {
    canonical_product_ticket_proposal_wire_json_v2(&request.proposal_wire())
}

/// Canonical bytes for the stored proposal-only DTO.  This is public so a
/// durable reader can prove a retained CAS object has no unknown fields or
/// alternate JSON spelling before using it as a ticket contract.
#[must_use]
pub fn canonical_product_ticket_proposal_wire_json_v2(
    proposal: &ProductTicketProposalWireV2,
) -> Vec<u8> {
    json::to_string(proposal).into_bytes()
}

/// Parses the exact canonical proposal bytes stored by Product admission and
/// validates them against the selected immutable application bounds.
pub fn parse_product_ticket_proposal_v2(
    payload: &[u8],
    bounds: &TicketBoundsV2,
) -> Result<ProductTicketProposalV2, FrameError> {
    let proposal: ProductTicketProposalWireV2 = decode_closed_json(payload, "ticket proposal")?;
    let canonical = canonical_product_ticket_proposal_wire_json_v2(&proposal);
    if canonical != payload {
        return Err(FrameError::InvalidJson {
            operation: "ticket proposal",
            detail: "proposal bytes are not canonical V2 JSON or contain unknown fields".into(),
        });
    }
    proposal
        .into_domain(bounds)
        .map_err(|error| FrameError::InvalidJson {
            operation: "ticket proposal",
            detail: error.to_string(),
        })
}

impl SealedArtifactReferenceWireV2 {
    fn into_domain(self) -> Result<SealedArtifactReferenceV2, ContractError> {
        crate::sealed_artifact_reference_v2(self.artifact_id, &self.digest, self.byte_length)
    }
}

impl ProductTicketProposalWireV2 {
    fn into_domain(
        self,
        bounds: &TicketBoundsV2,
    ) -> Result<ProductTicketProposalV2, ContractError> {
        let proposal = ProductTicketProposalV2 {
            title: self.title,
            mission_value: self.mission_value,
            scope: self.scope,
            contract_owner: self.contract_owner,
            risk: self.risk,
            narrative: self.narrative.into_domain()?,
            evidence: self.evidence.into_domain()?,
            acceptance_criteria: self.acceptance_criteria,
            contract_reads: self
                .contract_reads
                .into_iter()
                .map(TicketContractReadWireV2::into_domain)
                .collect::<Result<_, _>>()?,
            duplicate_search: self.duplicate_search.into_domain(),
            reproducer_profile: self.reproducer_profile,
            reproducer: self.reproducer.into_domain()?,
        };
        proposal.validate(bounds)?;
        Ok(proposal)
    }
}

impl CommandObservationWireV2 {
    fn into_domain(self) -> Result<CommandObservationV2, ContractError> {
        Ok(CommandObservationV2 {
            exit_status: self.exit_status,
            stdout: self.stdout.into_domain()?,
            stderr: self.stderr.into_domain()?,
        })
    }
}

impl TwoRunReproducerWireV2 {
    fn into_domain(self) -> Result<TwoRunReproducerV2, ContractError> {
        Ok(TwoRunReproducerV2 {
            comparison_rule_version: self.comparison_rule_version,
            command: self.command.into_domain()?,
            stdin: self
                .stdin
                .map(SealedArtifactReferenceWireV2::into_domain)
                .transpose()?,
            expected_observation: self.expected_observation.into_domain()?,
            first_observation: self.first_observation.into_domain()?,
            second_observation: self.second_observation.into_domain()?,
        })
    }
}

impl TicketContractReadWireV2 {
    fn into_domain(self) -> Result<TicketContractReadV2, ContractError> {
        Ok(TicketContractReadV2 {
            path: RepositoryRelativePath::parse(self.path)?,
            reason: self.reason,
        })
    }
}

impl DuplicateSearchInputWireV2 {
    fn into_domain(self) -> DuplicateSearchInputV2 {
        DuplicateSearchInputV2 {
            query: self.query,
            limit: self.limit,
        }
    }
}

impl CandidateSubmitRequest {
    /// Converts the actor terminal payload. Candidate tree/patch/commit
    /// identity is deliberately absent: the kernel captures it from the owned
    /// Engineering worktree after this request has passed all input checks.
    pub fn submission(&self) -> Result<CandidateSubmissionV2, ContractError> {
        let submission = CandidateSubmissionV2 {
            commit_subject: self.commit_subject.clone(),
            commit_body: self.commit_body.clone(),
            regression_test_identity: self.regression_test_identity.clone(),
        };
        submission.validate()?;
        Ok(submission)
    }
}

impl QualityRunFullSuiteRequest {
    /// Validates the closed named profile before the kernel resolves it in the
    /// exact application revision pinned by the assignment.
    pub fn full_suite_request(&self) -> Result<QualityFullSuiteRequestV2, ContractError> {
        let request = QualityFullSuiteRequestV2 {
            validation_profile: self.validation_profile.clone(),
        };
        request.validate()?;
        Ok(request)
    }
}

impl QualitySubmitReviewRequest {
    /// Converts the terminal Quality payload. The kernel separately proves the
    /// referenced validation belongs to this Quality session/candidate and
    /// passed on the exact candidate tree.
    pub fn submission(&self) -> Result<QualityReviewSubmissionV2, ContractError> {
        let verdict = match self.verdict.as_str() {
            "accept" => ReviewVerdict::Accept,
            "reject" => ReviewVerdict::Reject,
            _ => {
                return Err(ContractError::InvalidValue {
                    field: "Quality review verdict",
                    reason: "must be accept or reject",
                });
            }
        };
        let submission = QualityReviewSubmissionV2 {
            full_suite_validation_id: ValidationId::new(self.full_suite_validation_id)?,
            verdict,
            rationale: self.rationale.clone().into_domain()?,
            risks: self.risks.clone().into_domain()?,
            additional_probes: self.additional_probes.clone().into_domain()?,
        };
        submission.validate()?;
        Ok(submission)
    }
}

impl ArchitectSponsorTicketRevisionRequest {
    /// Architect operations are accepted only over the operator connection;
    /// this conversion contains no actor/session identity.
    pub fn decision(&self) -> Result<SponsorshipDecisionV2, ContractError> {
        let decision = SponsorshipDecisionV2 {
            ticket_revision_id: TicketRevisionId::new(self.ticket_revision_id)?,
            rationale: self.rationale.clone().into_domain()?,
            principal: ArchitectPrincipalV2::parse(self.principal.clone())?,
        };
        decision.validate()?;
        Ok(decision)
    }
}

impl ArchitectReleaseTicketAttemptRequest {
    pub fn decision(&self) -> Result<ReleaseDecisionV2, ContractError> {
        let decision = ReleaseDecisionV2 {
            ticket_attempt_id: TicketAttemptId::new(self.ticket_attempt_id)?,
            rationale: self.rationale.clone().into_domain()?,
            principal: ArchitectPrincipalV2::parse(self.principal.clone())?,
        };
        decision.validate()?;
        Ok(decision)
    }
}

impl ArchitectDecideCandidateRequest {
    pub fn decision(&self) -> Result<CandidateDecisionRequestV2, ContractError> {
        let decision = match self.decision.as_str() {
            "deliver" => CandidateDecisionV2::Deliver,
            "rework" => CandidateDecisionV2::Rework,
            "reject" => CandidateDecisionV2::Reject,
            _ => {
                return Err(ContractError::InvalidValue {
                    field: "Architect candidate decision",
                    reason: "must be deliver, rework, or reject",
                });
            }
        };
        let request = CandidateDecisionRequestV2 {
            candidate_id: crate::CandidateId::new(self.candidate_id)?,
            review_id: ReviewId::new(self.review_id)?,
            decision,
            rationale: self.rationale.clone().into_domain()?,
            quality_rejection_override: self
                .quality_rejection_override_review_id
                .map(ReviewId::new)
                .transpose()?,
            principal: ArchitectPrincipalV2::parse(self.principal.clone())?,
        };
        request.validate()?;
        Ok(request)
    }
}

impl OperatorApplicationRegisterRequest {
    /// Parses the content-addressed installed build rather than accepting an
    /// untyped build database ID from an operator client.
    pub fn kernel_build_id(&self) -> Result<KernelBuildId, ContractError> {
        ContentDigest::from_str(&self.kernel_build_id)
            .map(KernelBuildId::new)
            .map_err(|_| ContractError::InvalidValue {
                field: "application registration kernel build ID",
                reason: "must be a 32-byte BLAKE3 hex digest",
            })
    }

    /// The source root is an operator-host path used only by the daemon's CAS
    /// admission read.  It is not persisted as application policy.
    pub fn source_root(&self) -> Result<AbsoluteHostPath, ContractError> {
        AbsoluteHostPath::parse(self.source_root.clone())
    }

    /// Bundle lookup remains canonical and relative to `source_root`.
    pub fn bundle_relative_path(&self) -> Result<ApplicationRelativePath, ContractError> {
        ApplicationRelativePath::parse(self.bundle_relative_path.clone())
    }
}

impl OperatorApplicationShowRequest {
    pub fn application_key(&self) -> Result<ApplicationKey, ContractError> {
        ApplicationKey::parse(self.application_key.clone())
    }

    pub fn application_revision_id(
        &self,
    ) -> Result<Option<crate::ApplicationRevisionId>, ContractError> {
        self.application_revision_id
            .map(crate::ApplicationRevisionId::new)
            .transpose()
    }
}

impl OperatorApplicationActivateRequest {
    pub fn application_key(&self) -> Result<ApplicationKey, ContractError> {
        ApplicationKey::parse(self.application_key.clone())
    }

    pub fn application_revision_id(&self) -> Result<crate::ApplicationRevisionId, ContractError> {
        crate::ApplicationRevisionId::new(self.application_revision_id)
    }

    pub fn rationale(&self) -> Result<SealedArtifactReferenceV2, ContractError> {
        self.rationale.clone().into_domain()
    }

    pub fn principal(&self) -> Result<ArchitectPrincipalV2, ContractError> {
        ArchitectPrincipalV2::parse(self.principal.clone())
    }
}

impl OperatorArtifactSealRequest {
    /// The daemon reopens the path through CAS; this host root is never
    /// retained as durable application or ticket policy.
    pub fn source_root(&self) -> Result<AbsoluteHostPath, ContractError> {
        AbsoluteHostPath::parse(self.source_root.clone())
    }

    /// Evidence must be one canonical relative regular-file name below the
    /// explicit source root. CAS rejects traversal, symlinks, and non-files.
    pub fn source_relative_path(&self) -> Result<RuntimeRelativePath, ContractError> {
        RuntimeRelativePath::parse(self.source_relative_path.clone())
    }
}
mutating_request!(CandidateCheckpointRegressionRequest {
    regression_command: String,
    expected_failure: String,
});
mutating_request!(CandidateSubmitRequest {
    commit_subject: String,
    commit_body: String,
    regression_test_identity: String,
});
mutating_request!(QualityRunFullSuiteRequest {
    validation_profile: String
});
mutating_request!(QualitySubmitReviewRequest {
    full_suite_validation_id: i64,
    verdict: String,
    rationale: SealedArtifactReferenceWireV2,
    risks: SealedArtifactReferenceWireV2,
    additional_probes: SealedArtifactReferenceWireV2,
});
mutating_request!(WorkCompleteRequest {
    result_artifact_id: i64
});
mutating_request!(ArchitectSponsorTicketRevisionRequest {
    ticket_revision_id: i64,
    rationale: SealedArtifactReferenceWireV2,
    principal: String,
});
mutating_request!(ArchitectReleaseTicketAttemptRequest {
    ticket_attempt_id: i64,
    rationale: SealedArtifactReferenceWireV2,
    principal: String,
});
mutating_request!(ArchitectDecideCandidateRequest {
    candidate_id: i64,
    review_id: i64,
    decision: String,
    rationale: SealedArtifactReferenceWireV2,
    quality_rejection_override_review_id: Option<i64>,
    principal: String,
});
/// The selected application revision is immutable, but its aggregate revision
/// remains an explicit optimistic-concurrency guard. Build and repository IDs
/// are deliberately absent: PostgreSQL resolves both under daemon authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorStartCampaignRequest {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub client_command_id: String,
    pub expected_application_revision: u64,
    pub application_revision_id: i64,
    pub aggregate_budget_micro_usd: u64,
    pub deadline_unix_millis: u64,
    pub delivery_target: u32,
    pub principal: String,
}
read_request!(OperatorCampaignStatusRequest { campaign_id: i64 });
read_request!(OperatorStatusRequest {});
mutating_request!(OperatorCancelCampaignRequest {
    campaign_id: i64,
    principal: String,
});
read_request!(OperatorApplicationShowRequest {
    application_key: String,
    application_revision_id: Option<i64>,
});
mutating_request!(OperatorApplicationRegisterRequest {
    expected_kernel_build_revision: u64,
    kernel_build_id: String,
    source_root: String,
    bundle_relative_path: String,
    principal: String,
});
mutating_request!(OperatorApplicationActivateRequest {
    application_key: String,
    application_revision_id: i64,
    rationale: SealedArtifactReferenceWireV2,
    principal: String,
});
/// Unlike ordinary aggregate mutations, artifact sealing is guarded by the
/// installed kernel-build revision that admits the object. There is no
/// arbitrary aggregate revision or payload byte field to smuggle across the
/// operator socket.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorArtifactSealRequest {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub client_command_id: String,
    pub expected_kernel_build_revision: u64,
    pub source_root: String,
    pub source_relative_path: String,
    pub principal: String,
}
// Lists at most twenty current ticket revisions in stable newest-first order.
// `state`, when supplied, is one closed lifecycle spelling.
read_request!(OperatorTicketListRequest {
    state: Option<String>,
});
read_request!(OperatorTicketShowRequest { ticket_id: i64 });
read_request!(OperatorCandidateShowRequest { candidate_id: i64 });
// A selector is one of the closed subject families such as `ticket:17`,
// `candidate:4`, `campaign:2`, `application-revision:8`, or `audit:99`.
// It is not a subject-kind number, SQL fragment, or search expression.
read_request!(OperatorAuditShowRequest { selector: String });

/// The only polymorphism permitted by institutional navigation.  A wire
/// reference remains a flat closed value so miniserde cannot accept a map of
/// arbitrary metadata in its place.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstitutionalReferenceWireV2 {
    pub kind: String,
    pub id: i64,
}

impl InstitutionalReferenceWireV2 {
    pub fn reference(&self) -> Result<InstitutionalReference, ContractError> {
        InstitutionalReference::from_kind_and_id(
            InstitutionalObjectKind::parse(&self.kind)?,
            self.id,
        )
    }

    #[must_use]
    pub fn from_reference(reference: InstitutionalReference) -> Self {
        Self {
            kind: reference.kind().as_str().to_owned(),
            id: reference.id(),
        }
    }
}

// Search is intentionally one fixed projection over one concrete
// institutional relation.  Requiring the kind keeps the `id` cursor stable;
// a mixed-noun feed would need a larger compound cursor and would be a second
// query language rather than useful navigation.
read_request!(OperatorInstitutionalSearchRequest {
    query: String,
    kind: String,
    project_id: Option<i64>,
    owner_office_id: Option<i64>,
    anchor: Option<InstitutionalReferenceWireV2>,
    limit: u8,
    cursor: Option<InstitutionalReferenceWireV2>,
});

impl OperatorInstitutionalSearchRequest {
    /// Validates the bounded values that remain ordinary transport scalars.
    /// The typed ID helpers below cover the object and scope identities.
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.query.len() > crate::INSTITUTIONAL_SUMMARY_MAX_BYTES || self.query.contains('\0') {
            return Err(ContractError::InvalidValue {
                field: "institutional search query",
                reason: "must be bounded UTF-8 without NUL",
            });
        }
        let kind = self.object_kind()?;
        let _ = self.project_id()?;
        let _ = self.owner_office_id()?;
        if let Some(anchor) = self.anchor()? {
            if kind != InstitutionalObjectKind::Publication {
                return Err(ContractError::InvalidValue {
                    field: "institutional search anchor",
                    reason: "is available only when searching publications",
                });
            }
            if !anchor.can_anchor_publication() {
                return Err(ContractError::InvalidValue {
                    field: "institutional search anchor",
                    reason: "must name one publishable institutional object",
                });
            }
        }
        if let Some(cursor) = self.cursor()?
            && cursor.kind() != kind
        {
            return Err(ContractError::InvalidValue {
                field: "institutional search cursor",
                reason: "must have the selected object kind",
            });
        }
        Ok(())
    }

    pub fn object_kind(&self) -> Result<InstitutionalObjectKind, ContractError> {
        InstitutionalObjectKind::parse(&self.kind)
    }

    pub fn project_id(&self) -> Result<Option<crate::ProjectId>, ContractError> {
        self.project_id.map(crate::ProjectId::new).transpose()
    }

    pub fn owner_office_id(&self) -> Result<Option<crate::OfficeId>, ContractError> {
        self.owner_office_id.map(crate::OfficeId::new).transpose()
    }

    pub fn anchor(&self) -> Result<Option<InstitutionalReference>, ContractError> {
        self.anchor
            .as_ref()
            .map(InstitutionalReferenceWireV2::reference)
            .transpose()
    }

    pub fn cursor(&self) -> Result<Option<InstitutionalReference>, ContractError> {
        self.cursor
            .as_ref()
            .map(InstitutionalReferenceWireV2::reference)
            .transpose()
    }
}

read_request!(OperatorInstitutionalShowRequest {
    reference: InstitutionalReferenceWireV2,
});

impl OperatorInstitutionalShowRequest {
    pub fn institutional_reference(&self) -> Result<InstitutionalReference, ContractError> {
        self.reference.reference()
    }
}

/// One supporting sealed artifact selected for a publication. The kernel
/// validates the artifact row and its bounded label before persistence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationAttachmentWireV2 {
    pub artifact_id: i64,
    pub label: String,
}

// Creates one immutable anchored publication. `authoring_office_id` and
// `originating_session_id` are intentionally absent: an actor obtains them
// from its bound connection, while an operator route has a separate typed
// authority adapter.
read_request!(PublicationCreateRequest {
    client_command_id: String,
    anchor: InstitutionalReferenceWireV2,
    kind: String,
    summary: String,
    body_artifact_id: i64,
    attachments: Vec<PublicationAttachmentWireV2>,
    reply_to: Option<i64>,
    supersedes: Option<i64>,
});

impl PublicationCreateRequest {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.client_command_id.is_empty()
            || self.client_command_id.len() > 160
            || !self.client_command_id.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'_' | b'-')
            })
        {
            return Err(ContractError::InvalidValue {
                field: "publication client command ID",
                reason: "must be a bounded closed command component",
            });
        }
        let anchor = self.anchor.reference()?;
        if !anchor.can_anchor_publication() {
            return Err(ContractError::InvalidValue {
                field: "publication anchor",
                reason: "must be an institutional object, not a run or publication",
            });
        }
        crate::PublicationKind::parse(&self.kind)?;
        validate_bounded_wire_text(
            &self.summary,
            "publication summary",
            crate::INSTITUTIONAL_SUMMARY_MAX_BYTES,
        )?;
        crate::ArtifactId::new(self.body_artifact_id)?;
        if self.attachments.len() > crate::PUBLICATION_MAX_ATTACHMENTS {
            return Err(ContractError::InvalidValue {
                field: "publication attachments",
                reason: "exceeds the closed attachment limit",
            });
        }
        let mut artifact_ids = std::collections::BTreeSet::new();
        artifact_ids.insert(self.body_artifact_id);
        for attachment in &self.attachments {
            let artifact_id = crate::ArtifactId::new(attachment.artifact_id)?;
            validate_bounded_wire_text(
                &attachment.label,
                "publication attachment label",
                crate::PUBLICATION_ATTACHMENT_LABEL_MAX_BYTES,
            )?;
            if !artifact_ids.insert(artifact_id.get()) {
                return Err(ContractError::InvalidValue {
                    field: "publication attachments",
                    reason: "must not repeat the body or another attachment",
                });
            }
        }
        self.reply_to.map(PublicationId::new).transpose()?;
        self.supersedes.map(PublicationId::new).transpose()?;
        Ok(())
    }

    pub fn anchor_reference(&self) -> Result<InstitutionalReference, ContractError> {
        self.anchor.reference()
    }

    pub fn publication_kind(&self) -> Result<crate::PublicationKind, ContractError> {
        crate::PublicationKind::parse(&self.kind)
    }
}

// Grand Architect publication command. The explicit office is meaningful
// operator intent, not an actor assertion; the local-only router validates it
// against the selected application revision before storage.
read_request!(OperatorPublicationCreateRequest {
    client_command_id: String,
    application_revision_id: i64,
    authoring_office_id: i64,
    anchor: InstitutionalReferenceWireV2,
    kind: String,
    summary: String,
    body_artifact_id: i64,
    attachments: Vec<PublicationAttachmentWireV2>,
    reply_to: Option<i64>,
    supersedes: Option<i64>,
});

impl OperatorPublicationCreateRequest {
    pub fn validate(&self) -> Result<(), ContractError> {
        let shared = PublicationCreateRequest {
            protocol_version: self.protocol_version,
            request_id: self.request_id.clone(),
            operation: self.operation.clone(),
            client_command_id: self.client_command_id.clone(),
            anchor: self.anchor.clone(),
            kind: self.kind.clone(),
            summary: self.summary.clone(),
            body_artifact_id: self.body_artifact_id,
            attachments: self.attachments.clone(),
            reply_to: self.reply_to,
            supersedes: self.supersedes,
        };
        shared.validate()?;
        crate::ApplicationRevisionId::new(self.application_revision_id)?;
        crate::OfficeId::new(self.authoring_office_id)?;
        Ok(())
    }

    pub fn publication_command(&self) -> Result<PublicationCreateRequest, ContractError> {
        self.validate()?;
        Ok(PublicationCreateRequest {
            protocol_version: self.protocol_version,
            request_id: self.request_id.clone(),
            operation: self.operation.clone(),
            client_command_id: self.client_command_id.clone(),
            anchor: self.anchor.clone(),
            kind: self.kind.clone(),
            summary: self.summary.clone(),
            body_artifact_id: self.body_artifact_id,
            attachments: self.attachments.clone(),
            reply_to: self.reply_to,
            supersedes: self.supersedes,
        })
    }

    pub fn application_revision_id(&self) -> Result<crate::ApplicationRevisionId, ContractError> {
        crate::ApplicationRevisionId::new(self.application_revision_id)
    }

    pub fn authoring_office_id(&self) -> Result<crate::OfficeId, ContractError> {
        crate::OfficeId::new(self.authoring_office_id)
    }
}
read_request!(SessionVerifyPacketRequest {
    packet_digest: String,
    packet_bytes_b64: String,
});
mutating_request!(SessionSealArtifactRequest {
    staging_relative_path: String,
    role: String,
    byte_limit: u64,
});
mutating_request!(SessionSubmitTerminalRequest {
    terminal_operation: Option<String>,
    terminal_payload_b64: String,
    transcript_artifact_id: i64,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    reasoning_tokens: Option<u64>,
    reported_cost_micro_usd: Option<u64>,
    stop_reason: String,
});

// Forum operations use the rich operation-specific contracts in `forum.rs`.

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactReceiptResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub artifact_id: i64,
    pub digest: String,
    pub byte_length: u64,
    pub aggregate_revision: u64,
}

/// Immutable receipt for the operator-only evidence adoption bridge. Unlike
/// actor sealing, this records the ordinary artifact-registration audit row
/// so a Grand Architect can cite the exact provenance command later.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorArtifactSealReceiptResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub audit_id: i64,
    pub aggregate_revision: u64,
    pub artifact_id: i64,
    pub digest: String,
    pub byte_length: u64,
    pub was_idempotent_retry: bool,
    pub was_reused: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPacketVerificationResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub packet_digest: String,
    pub verified: bool,
}

/// Exact bytes returned by the kernel-owned workspace reader. Base64 keeps
/// the JSON frame valid for arbitrary file bytes; the digest is over the
/// decoded bytes and is also recorded in the daemon-side session ledger.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceReadResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub canonical_path: String,
    pub blake3: String,
    pub byte_length: u64,
    pub content_base64: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactReadResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub artifact_id: i64,
    pub digest: String,
    pub byte_length: u64,
    pub content_base64: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationReceiptResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub audit_id: i64,
    pub aggregate_revision: u64,
}

/// Receipt for an immutable institutional publication. The ID is returned so
/// an actor can cite the durable record without guessing a database sequence
/// or inventing a Forum path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationReceiptResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub audit_id: i64,
    pub aggregate_revision: u64,
    pub publication_id: i64,
    pub was_idempotent_retry: bool,
}

/// Receipt for a kernel-captured Engineering candidate. The actor never
/// supplies any tree/commit identity in its terminal payload.  A candidate
/// commit is intentionally absent: the kernel attaches it later, after the
/// successful Engineering terminal transcript exists for provenance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateReceiptResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub audit_id: i64,
    pub aggregate_revision: u64,
    pub candidate_id: i64,
    pub validation_id: i64,
    pub candidate_tree: String,
}

/// Receipt for the one opaque Engineering regression checkpoint retained by
/// the daemon for this session. It has no candidate transition/audit; these
/// identities are evidence navigation only and cannot recreate the internal
/// checkpoint capability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegressionCheckpointReceiptResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub regression_tree: String,
    pub regression_patch_artifact_id: i64,
    pub regression_command_set_artifact_id: i64,
    pub regression_log_artifact_id: i64,
}

/// Receipt for a Quality-owned full-suite invocation. A later review must
/// reference this `validation_id`; a generic success receipt is insufficient.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityValidationReceiptResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub audit_id: i64,
    pub aggregate_revision: u64,
    pub validation_id: i64,
    pub candidate_id: i64,
    pub candidate_tree: String,
}

/// Receipt for the exact immutable Quality review now eligible for the
/// external Architect's final decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityReviewReceiptResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub audit_id: i64,
    pub aggregate_revision: u64,
    pub review_id: i64,
    pub candidate_id: i64,
    pub verdict: String,
}

/// Receipt for one immutable external Architect decision. The decision kind
/// is returned so clients cannot mistake a stale/replayed receipt for a
/// different requested action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchitectDecisionReceiptResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub audit_id: i64,
    pub aggregate_revision: u64,
    pub architect_decision_id: i64,
    pub decision_kind: String,
}

/// Durable receipt for local campaign start or cancellation. The resolved
/// pins are returned on both a first acceptance and an idempotent replay so
/// an operator can inspect exactly what PostgreSQL admitted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignReceiptResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub audit_id: i64,
    pub aggregate_revision: u64,
    pub campaign_id: i64,
    pub kernel_build_id: String,
    pub application_revision_id: i64,
    pub repository_id: i64,
    pub was_idempotent_retry: bool,
}

/// Read-only liveness response for the transport-owned `factoryd.status`
/// frame. A status probe is deliberately not an audit-producing operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorStatusResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub state: String,
    /// The installed build currently selected by PostgreSQL. A resident
    /// daemon always reports one; `None` is retained for typed bootstrap
    /// clients probing an uninitialized authority in tests or tooling.
    pub current_kernel_build_id: Option<String>,
    /// Current revision guard for kernel-build-scoped operator commands.
    pub aggregate_revision: u64,
}

/// One bounded, read-only campaign projection. Its ticket counts and
/// scheduler explanation let an operator explain a paused MVP campaign
/// without materializing a poll receipt or starting a provider session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignStatusResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub campaign_id: i64,
    pub state: String,
    pub aggregate_revision: u64,
    pub kernel_build_id: String,
    pub application_revision_id: i64,
    pub repository_id: i64,
    pub aggregate_budget_micro_usd: u64,
    pub measured_cost_state: String,
    pub measured_cost_micro_usd: Option<u64>,
    pub remaining_budget_micro_usd: Option<u64>,
    pub deadline_unix_millis: u64,
    pub delivery_target: u32,
    /// Present only for a failed campaign; it is the bounded terminal daemon
    /// fault, not a mutable operator note.
    pub failure_reason: Option<String>,
    /// Most recent claimed base in this campaign, even before a candidate is
    /// submitted.
    pub base_commit: Option<String>,
    /// Most recent candidate's immutable tree and attached commit.
    pub candidate_tree: Option<String>,
    pub candidate_commit: Option<String>,
    /// Most recent immutable delivery result in this campaign.
    pub delivered_commit: Option<String>,
    /// Factory's final known aggregate spend for `delivered_commit`, in
    /// micro-USD. The two fields are both absent until a delivery exists.
    pub delivered_factory_cost_micro_usd: Option<u64>,
    pub delivered_attempt_count: u32,
    pub ready_ticket_count: u32,
    pub proposed_ticket_count: u32,
    pub in_flight_ticket_count: u32,
    pub downstream_ticket_attempt_count: u32,
    pub downstream_action_stage: Option<String>,
    pub downstream_ticket_attempt_id: Option<i64>,
    pub downstream_ticket_attempt_revision: Option<u64>,
    pub downstream_candidate_id: Option<i64>,
    pub downstream_candidate_revision: Option<u64>,
    pub downstream_evidence: Option<DownstreamEvidenceResponse>,
    pub ready_low_water: u32,
    pub ready_target: u32,
    pub ready_maximum: u32,
    pub proposal_maximum: u32,
    pub oldest_sponsored_ticket_revision_id: Option<i64>,
    pub oldest_sponsored_ticket_revision: Option<u64>,
    pub scheduler_next_action: String,
    pub scheduler_constraint: Option<String>,
    /// At most twenty durable session facts, ordered by `session_id`. This is
    /// a read-only cost and current-work explanation, not a pagination API.
    pub session_costs: Vec<CampaignSessionCostResponse>,
    /// Complete spend aggregation over every campaign session. Application
    /// pinning permits one model per office, so office/model/outcome grouping
    /// has a hard maximum of eighteen rows.
    pub session_cost_aggregates: Vec<CampaignSessionCostAggregateResponse>,
}

/// One exact session cost/outcome fact within a campaign status projection.
/// `elapsed_millis` is present only for the currently running session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignSessionCostResponse {
    pub session_id: i64,
    pub assignment_id: i64,
    pub assignment_role: String,
    pub model_provider: String,
    pub model_id: String,
    pub outcome: String,
    pub cost_state: String,
    pub cost_micro_usd: Option<u64>,
    pub elapsed_millis: Option<u64>,
}

/// Complete campaign spend grouped by the identities an operator uses to
/// reconcile provider invoices. Unknown, exceeded, and still-running costs
/// remain explicit counts rather than being silently folded into a sum.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignSessionCostAggregateResponse {
    pub assignment_role: String,
    pub model_provider: String,
    pub model_id: String,
    pub outcome: String,
    pub session_count: u32,
    pub accounted_cost_micro_usd: u64,
    pub pending_cost_session_count: u32,
    pub unknown_cost_session_count: u32,
    pub exceeded_cost_session_count: u32,
}

/// Immutable evidence already attached to the exact downstream candidate.
/// It contains only closed navigation identities; `operator.candidate.show`
/// remains the detailed evidence route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownstreamEvidenceResponse {
    pub candidate_commit: Option<String>,
    pub latest_validation: Option<DownstreamValidationEvidenceResponse>,
    pub review: Option<DownstreamReviewEvidenceResponse>,
    pub architect_decision: Option<DownstreamArchitectDecisionEvidenceResponse>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownstreamValidationEvidenceResponse {
    pub validation_id: i64,
    pub state: String,
    pub log_artifact_id: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownstreamReviewEvidenceResponse {
    pub review_id: i64,
    pub review_revision: u64,
    pub verdict: String,
    pub rationale_artifact_id: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownstreamArchitectDecisionEvidenceResponse {
    pub architect_decision_id: i64,
    pub decision_kind: String,
    pub rationale_artifact_id: i64,
}

/// Read-only application projection.  A registration stays visible while
/// inactive so an operator can verify its exact CAS bundle before activation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationShowResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub application_key: String,
    pub application_revision_id: i64,
    pub aggregate_revision: u64,
    pub bundle_artifact_id: i64,
    pub is_active: bool,
}

/// Receipt for application admission or explicit activation.  `is_active`
/// reflects the selected pointer at the time the command was accepted; an
/// idempotent replay can report false if a later activation superseded it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationRevisionReceiptResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub audit_id: i64,
    pub aggregate_revision: u64,
    pub application_revision_id: i64,
    pub is_active: bool,
    pub was_idempotent_retry: bool,
}

/// One sealed artifact referenced by a fixed operator navigation projection.
/// `role` is server-generated from a closed field name, never supplied by an
/// operator query.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceArtifactResponse {
    pub role: String,
    pub artifact_id: i64,
    pub digest: String,
    pub byte_length: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketListItemResponse {
    pub ticket_id: i64,
    pub ticket_revision_id: i64,
    pub ticket_revision: u64,
    pub application_revision_id: i64,
    pub state: String,
    pub proposal_artifact_id: i64,
    pub created_at_micros: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketListResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub items: Vec<TicketListItemResponse>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketAttemptNavigationResponse {
    pub ticket_attempt_id: i64,
    pub attempt_revision: u64,
    pub campaign_id: i64,
    pub stage: String,
    pub candidate_id: Option<i64>,
}

/// Full bounded view of one ticket's current immutable contract, latest
/// requalification evidence, and at most twenty attempts.  The exact current
/// ticket revision/revision pair is the sponsorship guard.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketShowResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub ticket_id: i64,
    pub ticket_revision_id: i64,
    pub ticket_revision: u64,
    pub application_revision_id: i64,
    pub state: String,
    pub sponsorship_reason: Option<String>,
    pub blocked_reason: Option<String>,
    pub evidence: Vec<EvidenceArtifactResponse>,
    pub attempts: Vec<TicketAttemptNavigationResponse>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateValidationNavigationResponse {
    pub validation_id: i64,
    pub scope: String,
    pub state: String,
    pub log: EvidenceArtifactResponse,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateReviewNavigationResponse {
    pub review_id: i64,
    pub review_revision: u64,
    pub verdict: String,
    pub rationale: EvidenceArtifactResponse,
    pub risks: EvidenceArtifactResponse,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateDecisionNavigationResponse {
    pub architect_decision_id: i64,
    pub decision_kind: String,
    pub rationale: EvidenceArtifactResponse,
}

/// Immutable delivery identity and the final known Factory spend attached to
/// the delivered product commit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryNavigationResponse {
    pub delivery_id: i64,
    pub resulting_commit: String,
    pub factory_cost_micro_usd: u64,
}

/// Full bounded candidate evidence view.  Its candidate/review revision IDs
/// are the values required to form a later final Architect decision; no
/// hidden lookup or inferred current revision is needed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateShowResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub candidate_id: i64,
    pub candidate_revision: u64,
    pub state: String,
    pub ticket_attempt_id: i64,
    pub ticket_revision_id: i64,
    pub ticket_revision: u64,
    pub base_commit: String,
    pub candidate_tree: String,
    pub candidate_commit: Option<String>,
    pub evidence: Vec<EvidenceArtifactResponse>,
    pub validations: Vec<CandidateValidationNavigationResponse>,
    pub review: Option<CandidateReviewNavigationResponse>,
    pub latest_architect_decision: Option<CandidateDecisionNavigationResponse>,
    pub delivery_receipt: Option<EvidenceArtifactResponse>,
    pub delivery: Option<DeliveryNavigationResponse>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntryResponse {
    pub audit_id: i64,
    pub principal: String,
    pub operation: String,
    pub subject_kind: i16,
    pub subject_id: i64,
    pub aggregate_revision: u64,
}

/// At most twenty newest audit entries for one closed selector family.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditShowResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub selector: String,
    pub items: Vec<AuditEntryResponse>,
}

/// One small, common search hit.  Detailed bodies remain sealed artifacts;
/// this projection exists only to make durable institutional identities
/// discoverable without exposing an unbounded query surface.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstitutionalSearchHitResponse {
    pub reference: InstitutionalReferenceWireV2,
    pub title: String,
    pub summary: String,
    pub created_at_micros: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstitutionalSearchResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub items: Vec<InstitutionalSearchHitResponse>,
    pub next_cursor: Option<InstitutionalReferenceWireV2>,
}

/// Fixed common projection returned by `operator.institutional.show`.
/// `lifecycle` is a closed noun-specific spelling produced by the kernel,
/// while the identity and scope fields stay numeric and typed at the wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstitutionalShowResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub reference: InstitutionalReferenceWireV2,
    pub application_revision_id: i64,
    pub owner_office_id: Option<i64>,
    pub title: String,
    pub summary: String,
    pub lifecycle: String,
    pub revision: u64,
    pub created_at_micros: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub error_code: String,
    pub current_revision: u64,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub protocol_version: u16,
    pub request_id: String,
    pub operation: String,
    pub error_code: String,
    pub message: String,
}

// -------------------------------------------------------------------------
// Canonical assignment admission
// -------------------------------------------------------------------------

/// Returns the recursively key-sorted JSON spelling used by the Rust host.
/// `miniserde::json::Value` uses `BTreeMap` objects, so
/// this remains a closed, dependency-free canonicalizer rather than an
/// untyped value crossing into the kernel domain.
pub fn canonical_assignment_packet_json_v2(
    packet: &AssignmentPacketWireV2,
) -> Result<String, FrameError> {
    validate_assignment_packet_wire_v2(packet, true)?;
    let encoded = json::to_string(packet);
    canonical_json_value(encoded.as_bytes(), "assignment packet")
}

/// Computes the packet seal over canonical unsigned JSON. The signed packet
/// carries this digest in `packet_digest`; hashing the signed bytes would be
/// self-referential and is therefore deliberately not the contract.
pub fn unsigned_assignment_packet_digest_v2(
    packet: &AssignmentPacketWireV2,
) -> Result<ContentDigest, FrameError> {
    let mut unsigned = packet.clone();
    unsigned.packet_digest.clear();
    let canonical = canonical_assignment_packet_json_v2(&unsigned)?;
    Ok(ContentDigest::of_bytes(canonical.as_bytes()))
}

/// Parses only canonical closed packet bytes. Re-serialization is compared
/// byte-for-byte, which rejects whitespace, reordered keys, numeric aliases,
/// and unknown fields that miniserde would otherwise ignore on a DTO.
pub fn parse_assignment_packet_v2(payload: &[u8]) -> Result<AssignmentPacketWireV2, FrameError> {
    let packet: AssignmentPacketWireV2 = decode_closed_json(payload, "assignment packet")?;
    let canonical = canonical_assignment_packet_json_v2(&packet)?;
    if canonical.as_bytes() != payload {
        return Err(FrameError::InvalidJson {
            operation: "assignment packet",
            detail: "packet bytes are not canonical V2 JSON or contain unknown fields".into(),
        });
    }
    validate_assignment_packet_wire_v2(&packet, false)?;
    Ok(packet)
}

/// Verifies both the packet's internal seal and the daemon's out-of-band
/// attestation digest before a host may construct Pi.
pub fn verify_assignment_packet_v2(
    payload: &[u8],
    expected_digest: &str,
) -> Result<AssignmentPacketWireV2, FrameError> {
    let packet = parse_assignment_packet_v2(payload)?;
    let computed = unsigned_assignment_packet_digest_v2(&packet)?;
    let packet_digest = ContentDigest::from_str(&packet.packet_digest).map_err(|error| {
        FrameError::InvalidJson {
            operation: "assignment packet",
            detail: format!("packet_digest is invalid: {error}"),
        }
    })?;
    let expected =
        ContentDigest::from_str(expected_digest).map_err(|error| FrameError::InvalidJson {
            operation: "assignment packet",
            detail: format!("expected packet digest is invalid: {error}"),
        })?;
    if computed != packet_digest || packet_digest != expected {
        return Err(FrameError::InvalidJson {
            operation: "assignment packet",
            detail: "packet digest does not match canonical unsigned bytes".into(),
        });
    }
    Ok(packet)
}

pub fn decode_assignment_packet_v2(
    frame: &[u8],
    maximum: usize,
) -> Result<AssignmentPacketWireV2, FrameError> {
    parse_assignment_packet_v2(decode_frame(frame, maximum)?)
}

fn decode_closed_json<T: Deserialize>(
    payload: &[u8],
    operation: &'static str,
) -> Result<T, FrameError> {
    let payload = std::str::from_utf8(payload).map_err(|_| FrameError::InvalidUtf8)?;
    json::from_str(payload).map_err(|error| FrameError::InvalidJson {
        operation,
        detail: format!("{error:?}"),
    })
}

fn canonical_json_value(payload: &[u8], operation: &'static str) -> Result<String, FrameError> {
    let payload = std::str::from_utf8(payload).map_err(|_| FrameError::InvalidUtf8)?;
    let value: json::Value = json::from_str(payload).map_err(|error| FrameError::InvalidJson {
        operation,
        detail: format!("{error:?}"),
    })?;
    Ok(json::to_string(&value))
}

fn validate_assignment_packet_wire_v2(
    packet: &AssignmentPacketWireV2,
    allow_empty_digest: bool,
) -> Result<(), FrameError> {
    if packet.format_version != ASSIGNMENT_PACKET_V2_FORMAT {
        return packet_error("format_version", "unsupported packet version");
    }
    for (field, value) in [
        ("campaign_id", packet.campaign_id),
        ("assignment_id", packet.assignment_id),
        ("application_revision_id", packet.application_revision_id),
        (
            "system_prompt_artifact_id",
            packet.system_prompt_artifact_id,
        ),
        (
            "assignment_prompt_artifact_id",
            packet.assignment_prompt_artifact_id,
        ),
        (
            "required_read_manifest_artifact_id",
            packet.required_read_manifest_artifact_id,
        ),
    ] {
        if value <= 0 {
            return packet_error(field, "must be greater than zero");
        }
    }
    for (field, value) in [
        ("ticket_attempt_id", packet.ticket_attempt_id),
        ("candidate_id", packet.candidate_id),
    ] {
        if let Some(value) = value
            && value <= 0
        {
            return packet_error(field, "must be greater than zero when present");
        }
    }
    for (field, value) in [
        ("kernel_build_id", packet.kernel_build_id.as_str()),
        (
            "repository_base_identity",
            packet.repository_base_identity.as_str(),
        ),
        (
            "factory_base_identity",
            packet.factory_base_identity.as_str(),
        ),
        ("system_prompt_digest", packet.system_prompt_digest.as_str()),
        (
            "assignment_prompt_digest",
            packet.assignment_prompt_digest.as_str(),
        ),
        ("policy_digest", packet.policy_digest.as_str()),
        (
            "runtime.core_source_digest",
            packet.runtime.core_source_digest.as_str(),
        ),
    ] {
        validate_digest(field, value)?;
    }
    if !allow_empty_digest || !packet.packet_digest.is_empty() {
        validate_digest("packet_digest", &packet.packet_digest)?;
    }
    if !matches!(
        packet.assignment_role.as_str(),
        "product_research" | "engineering" | "quality"
    ) {
        return packet_error("office", "unknown office");
    }
    if !matches!(
        (
            packet.assignment_role.as_str(),
            packet.ticket_attempt_id.is_some(),
            packet.candidate_id.is_some()
        ),
        ("product_research", false, false) | ("engineering", true, false) | ("quality", true, true)
    ) {
        return packet_error(
            "assignment target identity",
            "office requires its exact durable target shape",
        );
    }
    bounded_packet_text("target", &packet.target, 4096)?;
    bounded_packet_text("workspace_root", &packet.workspace_root, 4096)?;
    bounded_packet_text("staging_root", &packet.staging_root, 4096)?;
    validate_base64("system_prompt_bytes_b64", &packet.system_prompt_bytes_b64)?;
    validate_base64(
        "assignment_prompt_bytes_b64",
        &packet.assignment_prompt_bytes_b64,
    )?;
    let policy_bytes = decode_base64("policy_bytes_b64", &packet.policy_bytes_b64)?;
    if packet.policy_byte_limit == 0
        || packet.policy_byte_limit as usize > MAX_POLICY_ARTIFACT_BYTES
    {
        return packet_error(
            "policy_byte_limit",
            "must be positive and within the policy ceiling",
        );
    }
    if policy_bytes.is_empty() || policy_bytes.len() > packet.policy_byte_limit as usize {
        return packet_error("policy_bytes_b64", "source exceeds its declared byte limit");
    }
    if std::str::from_utf8(&policy_bytes).is_err() || policy_bytes.contains(&0) {
        return packet_error("policy_bytes_b64", "source must be UTF-8 without NUL");
    }
    if ContentDigest::of_bytes(&policy_bytes).to_hex() != packet.policy_digest {
        return packet_error("policy_digest", "does not match policy source bytes");
    }
    PolicyEntrypointV2::parse(&packet.policy_entrypoint).map_err(|error| {
        FrameError::InvalidJson {
            operation: "assignment packet",
            detail: format!("policy_entrypoint: {error}"),
        }
    })?;
    if packet.model.provider.is_empty() || packet.model.model_id.is_empty() {
        return packet_error("model", "provider and model_id are required");
    }
    if !matches!(
        packet.model.thinking_level.as_str(),
        "none" | "low" | "medium" | "high" | "xhigh"
    ) {
        return packet_error("model.thinking_level", "unknown thinking level");
    }
    if packet.model.context_token_limit == 0 || packet.model.output_token_limit == 0 {
        return packet_error("model limits", "must be positive");
    }
    if packet.limits.turn_limit == 0
        || packet.limits.wall_limit_millis == 0
        || packet.limits.output_byte_limit == 0
        || u64::from(packet.limits.output_byte_limit) > MAX_SESSION_OUTPUT_BYTES
    {
        return packet_error(
            "limits",
            "must be positive and output must fit one CAS object",
        );
    }
    if packet.remaining_campaign_allowance_micro_usd == 0 {
        return packet_error("remaining_campaign_allowance_micro_usd", "must be positive");
    }
    if packet.required_reads.is_empty()
        || packet.tools.is_empty()
        || packet.terminal_operations.is_empty()
    {
        return packet_error(
            "assignment sets",
            "reads, tools, and terminal operations are required",
        );
    }
    validate_unique_strings("tools", &packet.tools)?;
    for tool in &packet.tools {
        if !is_known_assignment_tool(tool) {
            return packet_error("tools", "unknown host tool");
        }
    }
    validate_unique_strings("model.capability_flags", &packet.model.capability_flags)?;
    for flag in &packet.model.capability_flags {
        if flag != "reasoning" {
            return Err(FrameError::InvalidJson {
                operation: "application bundle",
                detail: "model.capability_flags: unknown model capability".into(),
            });
        }
    }
    validate_unique_strings("terminal_operations", &packet.terminal_operations)?;
    for operation in &packet.terminal_operations {
        if !is_known_terminal_operation(operation) {
            return packet_error("terminal_operations", "unknown terminal operation");
        }
    }
    let mut paths = std::collections::BTreeSet::new();
    for read in &packet.required_reads {
        if !paths.insert(read.path.clone()) {
            return packet_error("required_reads", "paths must be unique");
        }
        bounded_packet_text("required read path", &read.path, 4096)?;
        validate_digest("required read digest", &read.digest)?;
        bounded_packet_text("required read reason", &read.reason, 240)?;
    }
    if packet.assignment_evidence.len() > 24 {
        return packet_error(
            "assignment_evidence",
            "exceeds the closed evidence reference limit",
        );
    }
    if packet.assignment_role == "product_research" && !packet.assignment_evidence.is_empty() {
        return packet_error(
            "assignment_evidence",
            "Product has no upstream assignment evidence",
        );
    }
    if packet.assignment_role != "product_research" && packet.assignment_evidence.is_empty() {
        return packet_error(
            "assignment_evidence",
            "Engineering and Quality require upstream evidence",
        );
    }
    let mut evidence_roles = std::collections::BTreeSet::new();
    for evidence in &packet.assignment_evidence {
        if !is_known_assignment_evidence_role(&evidence.role) {
            return packet_error("assignment_evidence.role", "is not a closed evidence role");
        }
        if !evidence_roles.insert(evidence.role.as_str()) {
            return packet_error("assignment_evidence", "roles must be unique");
        }
        if evidence.artifact_id <= 0 {
            return packet_error(
                "assignment_evidence.artifact_id",
                "is not a positive identity",
            );
        }
        validate_digest("assignment_evidence.digest", &evidence.digest)?;
    }
    bounded_packet_text(
        "runtime.host_executable",
        &packet.runtime.host_executable,
        4096,
    )?;
    if !packet.runtime.host_executable.starts_with('/') {
        return packet_error("runtime.host_executable", "must be an absolute host path");
    }
    if packet.runtime.core_head.len() != 40
        || !packet
            .runtime
            .core_head
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return packet_error(
            "runtime.core_head",
            "must be exactly 40 lower-case hexadecimal characters",
        );
    }
    bounded_packet_text(
        "runtime.rust_toolchain",
        &packet.runtime.rust_toolchain,
        240,
    )?;
    validate_credential_environment("runtime.credential_env", &packet.runtime.credential_env)?;
    Ok(())
}

fn is_known_assignment_evidence_role(role: &str) -> bool {
    matches!(
        role,
        "ticket_proposal"
            | "ticket_narrative"
            | "ticket_evidence"
            | "reproducer_command"
            | "reproducer_stdin"
            | "reproducer_expected_stdout"
            | "reproducer_expected_stderr"
            | "reproducer_first_actual_stdout"
            | "reproducer_first_actual_stderr"
            | "reproducer_second_actual_stdout"
            | "reproducer_second_actual_stderr"
            | "regression_patch"
            | "regression_command_set"
            | "regression_log"
            | "changed_paths"
            | "candidate_patch"
            | "engineering_report"
            | "engineering_risks"
            | "hard_validation_command_set"
            | "hard_validation_log"
            | "quality_additional_probes"
            | "quality_rationale"
            | "quality_risks"
            | "external_decision_rationale"
    )
}

fn packet_error(field: &'static str, detail: &'static str) -> Result<(), FrameError> {
    Err(FrameError::InvalidJson {
        operation: "assignment packet",
        detail: format!("{field}: {detail}"),
    })
}

fn bounded_packet_text(field: &'static str, value: &str, maximum: usize) -> Result<(), FrameError> {
    if value.is_empty() || value.len() > maximum || value.contains('\0') {
        return packet_error(field, "text is empty, oversized, or contains NUL");
    }
    Ok(())
}

fn validate_digest(field: &'static str, value: &str) -> Result<(), FrameError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return packet_error(field, "must be a lower-case 32-byte hex digest");
    }
    Ok(())
}

fn validate_base64(field: &'static str, value: &str) -> Result<(), FrameError> {
    if value.is_empty() || !value.len().is_multiple_of(4) {
        return packet_error(field, "must be nonempty canonical base64");
    }
    let bytes = value.as_bytes();
    let padding = bytes.iter().rev().take_while(|byte| **byte == b'=').count();
    let body = &bytes[..bytes.len() - padding];
    if padding > 2 || body.contains(&b'=') {
        return packet_error(field, "must be canonical base64");
    }
    if !body
        .iter()
        .copied()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/')
    {
        return packet_error(field, "must be canonical base64");
    }
    Ok(())
}

fn decode_base64(field: &'static str, value: &str) -> Result<Vec<u8>, FrameError> {
    validate_base64(field, value)?;
    let bytes = value.as_bytes();
    let padding = bytes.iter().rev().take_while(|byte| **byte == b'=').count();
    let body = &bytes[..bytes.len() - padding];
    let mut output = Vec::with_capacity(value.len() / 4 * 3);
    let (chunks, remainder) = bytes.as_chunks::<4>();
    debug_assert!(
        remainder.is_empty(),
        "validated base64 has complete quartets"
    );
    for chunk in chunks {
        let first = base64_value(chunk[0]).ok_or_else(|| invalid_base64(field))?;
        let second = base64_value(chunk[1]).ok_or_else(|| invalid_base64(field))?;
        output.push((first << 2) | (second >> 4));
        if chunk[2] != b'=' {
            let third = base64_value(chunk[2]).ok_or_else(|| invalid_base64(field))?;
            output.push((second << 4) | (third >> 2));
            if chunk[3] != b'=' {
                let fourth = base64_value(chunk[3]).ok_or_else(|| invalid_base64(field))?;
                output.push((third << 6) | fourth);
            }
        }
    }
    // Canonical base64 requires unused trailing bits to be zero. This also
    // prevents multiple encodings of one policy source from crossing the
    // packet identity boundary.
    if !body.is_empty() {
        let remainder = body.len() % 4;
        if remainder == 2 {
            let value = base64_value(body[body.len() - 1]).ok_or_else(|| invalid_base64(field))?;
            if value & 0x0f != 0 {
                return Err(invalid_base64(field));
            }
        } else if remainder == 3 {
            let value = base64_value(body[body.len() - 1]).ok_or_else(|| invalid_base64(field))?;
            if value & 0x03 != 0 {
                return Err(invalid_base64(field));
            }
        }
    }
    Ok(output)
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn invalid_base64(field: &'static str) -> FrameError {
    FrameError::InvalidJson {
        operation: "assignment packet",
        detail: format!("{field}: invalid canonical base64"),
    }
}

fn validate_unique_strings(field: &'static str, values: &[String]) -> Result<(), FrameError> {
    let mut seen = std::collections::BTreeSet::new();
    if values
        .iter()
        .any(|value| value.is_empty() || !seen.insert(value))
    {
        return packet_error(field, "values must be nonempty and unique");
    }
    Ok(())
}

fn validate_credential_environment(field: &'static str, value: &str) -> Result<(), FrameError> {
    if value.is_empty()
        || value.len() > 160
        || !value.bytes().enumerate().all(|(index, byte)| {
            (index == 0 && (byte.is_ascii_uppercase() || byte == b'_'))
                || (index > 0
                    && (byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'))
        })
    {
        return packet_error(field, "must be a valid upper-case environment name");
    }
    Ok(())
}

fn is_known_assignment_tool(value: &str) -> bool {
    matches!(
        value,
        "workspace_read"
            | "workspace_write"
            | "workspace_edit"
            | "workspace_search"
            | "workspace_list"
            | "shell"
            | "forum_search"
            | "forum_list_topics"
            | "forum_list_threads"
            | "forum_read_thread"
            | "publication_create"
            | "artifact_seal"
            | "artifact_read"
            | "product_submit_ticket"
            | "candidate_checkpoint_regression"
            | "candidate_submit"
            | "quality_run_full_suite"
            | "quality_submit_review"
            | "work_complete"
    )
}

fn is_known_terminal_operation(value: &str) -> bool {
    matches!(
        value,
        "work_complete" | "candidate_submit" | "quality_submit_review"
    )
}

// -------------------------------------------------------------------------
// Canonical application admission
// -------------------------------------------------------------------------

/// The wire-only representation of the closed application bundle. Keeping
/// this DTO beside the frame parser makes the admission seam explicit while
/// leaving application source outside the kernel. It has no metadata map
/// and no executable callback field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationBundleWireV2 {
    pub format_version: u16,
    pub application_key: String,
    pub predecessor_bundle: Option<String>,
    pub repository: RepositoryWireV2,
    pub mission_template: TemplateWireV2,
    pub assignment_role_profiles: Vec<AssignmentRoleWireV2>,
    pub ticket_policy: TicketPolicyWireV2,
    pub required_reads: Vec<RequiredReadWireV2>,
    pub reproducer_profiles: Vec<CommandWireV2>,
    pub validation_profiles: ValidationWireV2,
    pub git_policy: GitWireV2,
    pub commit_message_policy: CommitMessageWireV2,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryWireV2 {
    pub repository_key: String,
    pub canonical_local_path: String,
    pub default_branch: String,
    pub delivery_mode: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateWireV2 {
    pub source_path: String,
    pub digest: String,
    pub placeholders: Vec<String>,
    pub rendered_byte_limit: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssignmentRoleWireV2 {
    pub assignment_role: String,
    pub system_template: TemplateWireV2,
    pub assignment_template: TemplateWireV2,
    pub policy: PolicyWireV2,
    pub tools: Vec<String>,
    pub model: ModelWireV2,
    pub limits: LimitsWireV2,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyWireV2 {
    pub source_path: String,
    pub digest: String,
    pub byte_limit: u32,
    pub entrypoint: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelWireV2 {
    pub provider: String,
    pub model_id: String,
    pub thinking_level: String,
    pub context_token_limit: u32,
    pub output_token_limit: u32,
    pub price_input_micro_usd_per_million_tokens: u64,
    pub price_output_micro_usd_per_million_tokens: u64,
    pub price_cache_read_micro_usd_per_million_tokens: u64,
    pub price_cache_write_micro_usd_per_million_tokens: u64,
    pub capability_flags: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitsWireV2 {
    pub turn_limit: u32,
    pub wall_limit_millis: u64,
    pub output_byte_limit: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketPolicyWireV2 {
    pub low_water: u16,
    pub target: u16,
    pub maximum: u16,
    pub proposal_maximum: u16,
    pub ticket_bounds: TicketBoundsWireV2,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketBoundsWireV2 {
    pub narrative_byte_limit: u32,
    pub acceptance_criteria_limit: u16,
    pub contract_read_limit: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequiredReadWireV2 {
    pub path: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandWireV2 {
    pub name: String,
    pub executable: ExecutableWireV2,
    pub argv: Vec<String>,
    pub working_directory: String,
    pub environment: Vec<EnvironmentWireV2>,
    pub timeout_millis: u64,
    pub stdout_byte_limit: u32,
    pub stderr_byte_limit: u32,
    pub expected_exit_status: i32,
}

/// Canonical bytes for one sealed deterministic command profile.
pub fn canonical_command_profile_json_v2(command: &CommandWireV2) -> Result<String, FrameError> {
    canonical_command(command)
}

/// Parses exactly the canonical V2 command profile bytes used by Product
/// reproducer custody. Unknown fields, alternate key order, or whitespace are
/// rejected rather than normalized.
pub fn parse_command_profile_v2(payload: &[u8]) -> Result<CommandProfileV2, FrameError> {
    let payload = std::str::from_utf8(payload).map_err(|_| FrameError::InvalidUtf8)?;
    let wire: CommandWireV2 = json::from_str(payload).map_err(|error| FrameError::InvalidJson {
        operation: "command profile",
        detail: format!("{error:?}"),
    })?;
    if canonical_command_profile_json_v2(&wire)? != payload {
        return Err(FrameError::InvalidJson {
            operation: "command profile",
            detail: "command bytes are not canonical V2 JSON or contain unknown fields".into(),
        });
    }
    wire.into_domain()
        .map_err(|detail| FrameError::InvalidJson {
            operation: "command profile",
            detail,
        })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutableWireV2 {
    pub approved_tool: Option<String>,
    pub repository_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentWireV2 {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationWireV2 {
    pub focused: Vec<CommandWireV2>,
    pub full: Vec<CommandWireV2>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitWireV2 {
    pub forbidden_paths: Vec<String>,
    pub delivery_mode: String,
    pub provenance_trailers_required: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitMessageWireV2 {
    pub subject_byte_limit: u16,
    pub body_byte_limit: u16,
}

/// Serializes the closed DTO in the exact canonical key order. This is
/// deliberately handwritten rather than
/// accepting a dynamic JSON value: admission must reject unknown fields and
/// alternate byte representations at the protocol boundary.
pub fn canonical_application_bundle_json_v2(
    bundle: &ApplicationBundleWireV2,
) -> Result<String, FrameError> {
    for profile in &bundle.assignment_role_profiles {
        validate_unique_strings("model.capability_flags", &profile.model.capability_flags)?;
        if profile
            .model
            .capability_flags
            .iter()
            .any(|flag| flag != "reasoning")
        {
            return Err(FrameError::InvalidJson {
                operation: "application bundle",
                detail: "model.capability_flags: unknown model capability".into(),
            });
        }
    }
    let mut out = String::new();
    out.push('{');
    field_string(
        &mut out,
        "application_key",
        &json_quote(&bundle.application_key),
        true,
    );
    field_string(
        &mut out,
        "assignment_role_profiles",
        &canonical_assignment_role_profiles(&bundle.assignment_role_profiles),
        false,
    );
    field_string(
        &mut out,
        "commit_message_policy",
        &canonical_commit_message_policy(&bundle.commit_message_policy),
        false,
    );
    field_u16(&mut out, "format_version", bundle.format_version, false);
    field_string(
        &mut out,
        "git_policy",
        &canonical_git_policy(&bundle.git_policy),
        false,
    );
    field_string(
        &mut out,
        "mission_template",
        &canonical_template(&bundle.mission_template),
        false,
    );
    field_optional_string(
        &mut out,
        "predecessor_bundle",
        bundle.predecessor_bundle.as_deref(),
        false,
    );
    field_string(
        &mut out,
        "repository",
        &canonical_repository(&bundle.repository),
        false,
    );
    field_string(
        &mut out,
        "reproducer_profiles",
        &canonical_commands(&bundle.reproducer_profiles)?,
        false,
    );
    field_string(
        &mut out,
        "required_reads",
        &canonical_required_reads(&bundle.required_reads),
        false,
    );
    field_string(
        &mut out,
        "ticket_policy",
        &canonical_ticket_policy(&bundle.ticket_policy),
        false,
    );
    field_string(
        &mut out,
        "validation_profiles",
        &canonical_validation(&bundle.validation_profiles)?,
        false,
    );
    out.push('}');
    Ok(out)
}

fn json_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if character < '\u{20}' => {
                let _ = write!(out, "\\u{:04x}", character as u32);
            }
            character => out.push(character),
        }
    }
    out.push('"');
    out
}

fn field_string(out: &mut String, name: &str, value: &str, first: bool) {
    if !first {
        out.push(',');
    }
    out.push_str(&json_quote(name));
    out.push(':');
    out.push_str(value);
}

fn field_u16(out: &mut String, name: &str, value: u16, first: bool) {
    field_string(out, name, &value.to_string(), first);
}

fn field_optional_string(out: &mut String, name: &str, value: Option<&str>, first: bool) {
    if !first {
        out.push(',');
    }
    out.push_str(&json_quote(name));
    out.push(':');
    match value {
        Some(value) => out.push_str(&json_quote(value)),
        None => out.push_str("null"),
    }
}

fn canonical_template(value: &TemplateWireV2) -> String {
    let placeholders = value
        .placeholders
        .iter()
        .map(|value| json_quote(value))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"digest\":{},\"placeholders\":[{}],\"rendered_byte_limit\":{},\"source_path\":{}}}",
        json_quote(&value.digest),
        placeholders,
        value.rendered_byte_limit,
        json_quote(&value.source_path)
    )
}

fn canonical_repository(value: &RepositoryWireV2) -> String {
    format!(
        "{{\"canonical_local_path\":{},\"default_branch\":{},\"delivery_mode\":{},\"repository_key\":{}}}",
        json_quote(&value.canonical_local_path),
        json_quote(&value.default_branch),
        json_quote(&value.delivery_mode),
        json_quote(&value.repository_key)
    )
}

fn canonical_model(value: &ModelWireV2) -> String {
    format!(
        "{{\"capability_flags\":[{}],\"context_token_limit\":{},\"model_id\":{},\"output_token_limit\":{},\"price_cache_read_micro_usd_per_million_tokens\":{},\"price_cache_write_micro_usd_per_million_tokens\":{},\"price_input_micro_usd_per_million_tokens\":{},\"price_output_micro_usd_per_million_tokens\":{},\"provider\":{},\"thinking_level\":{}}}",
        value
            .capability_flags
            .iter()
            .map(|flag| json_quote(flag))
            .collect::<Vec<_>>()
            .join(","),
        value.context_token_limit,
        json_quote(&value.model_id),
        value.output_token_limit,
        value.price_cache_read_micro_usd_per_million_tokens,
        value.price_cache_write_micro_usd_per_million_tokens,
        value.price_input_micro_usd_per_million_tokens,
        value.price_output_micro_usd_per_million_tokens,
        json_quote(&value.provider),
        json_quote(&value.thinking_level)
    )
}

fn canonical_limits(value: &LimitsWireV2) -> String {
    format!(
        "{{\"output_byte_limit\":{},\"turn_limit\":{},\"wall_limit_millis\":{}}}",
        value.output_byte_limit, value.turn_limit, value.wall_limit_millis
    )
}

fn canonical_assignment_role_profile(value: &AssignmentRoleWireV2) -> String {
    format!(
        "{{\"assignment_role\":{},\"assignment_template\":{},\"limits\":{},\"model\":{},\"policy\":{},\"system_template\":{},\"tools\":[{}]}}",
        json_quote(&value.assignment_role),
        canonical_template(&value.assignment_template),
        canonical_limits(&value.limits),
        canonical_model(&value.model),
        canonical_policy(&value.policy),
        canonical_template(&value.system_template),
        value
            .tools
            .iter()
            .map(|value| json_quote(value))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn canonical_policy(value: &PolicyWireV2) -> String {
    format!(
        "{{\"byte_limit\":{},\"digest\":{},\"entrypoint\":{},\"source_path\":{}}}",
        value.byte_limit,
        json_quote(&value.digest),
        json_quote(&value.entrypoint),
        json_quote(&value.source_path)
    )
}

fn canonical_assignment_role_profiles(values: &[AssignmentRoleWireV2]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(canonical_assignment_role_profile)
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn canonical_ticket_policy(value: &TicketPolicyWireV2) -> String {
    format!(
        "{{\"low_water\":{},\"maximum\":{},\"proposal_maximum\":{},\"target\":{},\"ticket_bounds\":{{\"acceptance_criteria_limit\":{},\"contract_read_limit\":{},\"narrative_byte_limit\":{}}}}}",
        value.low_water,
        value.maximum,
        value.proposal_maximum,
        value.target,
        value.ticket_bounds.acceptance_criteria_limit,
        value.ticket_bounds.contract_read_limit,
        value.ticket_bounds.narrative_byte_limit
    )
}

fn canonical_required_reads(values: &[RequiredReadWireV2]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| format!(
                "{{\"path\":{},\"reason\":{}}}",
                json_quote(&value.path),
                json_quote(&value.reason)
            ))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn canonical_executable(value: &ExecutableWireV2) -> Result<String, FrameError> {
    match (&value.approved_tool, &value.repository_path) {
        (Some(tool), None) => Ok(format!("{{\"approved_tool\":{}}}", json_quote(tool))),
        (None, Some(path)) => Ok(format!("{{\"repository_path\":{}}}", json_quote(path))),
        _ => Err(FrameError::InvalidJson {
            operation: "application bundle",
            detail: "executable must contain exactly one closed variant".into(),
        }),
    }
}

fn canonical_command(value: &CommandWireV2) -> Result<String, FrameError> {
    Ok(format!(
        "{{\"argv\":[{}],\"environment\":[{}],\"executable\":{},\"expected_exit_status\":{},\"name\":{},\"stderr_byte_limit\":{},\"stdout_byte_limit\":{},\"timeout_millis\":{},\"working_directory\":{}}}",
        value
            .argv
            .iter()
            .map(|value| json_quote(value))
            .collect::<Vec<_>>()
            .join(","),
        value
            .environment
            .iter()
            .map(|value| format!(
                "{{\"name\":{},\"value\":{}}}",
                json_quote(&value.name),
                json_quote(&value.value)
            ))
            .collect::<Vec<_>>()
            .join(","),
        canonical_executable(&value.executable)?,
        value.expected_exit_status,
        json_quote(&value.name),
        value.stderr_byte_limit,
        value.stdout_byte_limit,
        value.timeout_millis,
        json_quote(&value.working_directory)
    ))
}

fn canonical_commands(values: &[CommandWireV2]) -> Result<String, FrameError> {
    Ok(format!(
        "[{}]",
        values
            .iter()
            .map(canonical_command)
            .collect::<Result<Vec<_>, _>>()?
            .join(",")
    ))
}

fn canonical_validation(value: &ValidationWireV2) -> Result<String, FrameError> {
    let focused = value
        .focused
        .iter()
        .map(canonical_command)
        .collect::<Result<Vec<_>, _>>()?
        .join(",");
    let full = value
        .full
        .iter()
        .map(canonical_command)
        .collect::<Result<Vec<_>, _>>()?
        .join(",");
    Ok(format!("{{\"focused\":[{focused}],\"full\":[{full}]}}"))
}

fn canonical_git_policy(value: &GitWireV2) -> String {
    format!(
        "{{\"delivery_mode\":{},\"forbidden_paths\":[{}],\"provenance_trailers_required\":{}}}",
        json_quote(&value.delivery_mode),
        value
            .forbidden_paths
            .iter()
            .map(|value| json_quote(value))
            .collect::<Vec<_>>()
            .join(","),
        value.provenance_trailers_required
    )
}

fn canonical_commit_message_policy(value: &CommitMessageWireV2) -> String {
    format!(
        "{{\"body_byte_limit\":{},\"subject_byte_limit\":{}}}",
        value.body_byte_limit, value.subject_byte_limit
    )
}

/// Parses a canonical JSON bundle and admits it into the closed Rust domain
/// values. The caller still owns CAS adoption: every template and policy
/// path/digest must be read from the explicit application source bundle and
/// sealed independently before an application revision is written.
pub fn parse_application_bundle_v2(payload: &[u8]) -> Result<ApplicationBundleV2, FrameError> {
    let payload = std::str::from_utf8(payload).map_err(|_| FrameError::InvalidUtf8)?;
    let wire: ApplicationBundleWireV2 =
        json::from_str(payload).map_err(|error| FrameError::InvalidJson {
            operation: "application bundle",
            detail: format!("{error:?}"),
        })?;
    let canonical = canonical_application_bundle_json_v2(&wire)?;
    if canonical.as_bytes() != payload.as_bytes() {
        return Err(FrameError::InvalidJson {
            operation: "application bundle",
            detail: "bundle bytes are not canonical V2 JSON or contain unknown fields".into(),
        });
    }
    let bundle = wire
        .into_domain()
        .map_err(|detail| FrameError::InvalidJson {
            operation: "application bundle",
            detail,
        })?;
    bundle.validate().map_err(|error| FrameError::InvalidJson {
        operation: "application bundle",
        detail: error.to_string(),
    })?;
    Ok(bundle)
}

/// Admits a canonical bundle and returns the immutable application-revision
/// identity input. The caller must pass exact compiler bytes; whitespace or
/// key-order changes therefore produce a different identity and are not
/// silently normalized at the authority boundary.
pub fn admit_application_bundle_v2(
    payload: &[u8],
) -> Result<(ApplicationBundleV2, ContentDigest), FrameError> {
    let bundle = parse_application_bundle_v2(payload)?;
    Ok((bundle, ContentDigest::of_bytes(payload)))
}

pub fn decode_application_bundle_v2(
    frame: &[u8],
    maximum: usize,
) -> Result<ApplicationBundleV2, FrameError> {
    parse_application_bundle_v2(decode_frame(frame, maximum)?)
}

impl ApplicationBundleWireV2 {
    fn into_domain(self) -> Result<ApplicationBundleV2, String> {
        Ok(ApplicationBundleV2 {
            format_version: self.format_version,
            application_key: ApplicationKey::parse(self.application_key)
                .map_err(contract_detail)?,
            predecessor_bundle: self
                .predecessor_bundle
                .map(|value| ContentDigest::from_str(&value).map_err(contract_detail))
                .transpose()?,
            repository: self.repository.into_domain()?,
            mission_template: self.mission_template.into_domain()?,
            assignment_role_profiles: self
                .assignment_role_profiles
                .into_iter()
                .map(AssignmentRoleWireV2::into_domain)
                .collect::<Result<_, _>>()?,
            ticket_policy: self.ticket_policy.into_domain()?,
            required_reads: self
                .required_reads
                .into_iter()
                .map(RequiredReadWireV2::into_domain)
                .collect::<Result<_, _>>()?,
            reproducer_profiles: self
                .reproducer_profiles
                .into_iter()
                .map(CommandWireV2::into_domain)
                .collect::<Result<_, _>>()?,
            validation_profiles: self.validation_profiles.into_domain()?,
            git_policy: self.git_policy.into_domain()?,
            commit_message_policy: self.commit_message_policy.into_domain()?,
        })
    }
}

impl RepositoryWireV2 {
    fn into_domain(self) -> Result<RepositoryBindingV2, String> {
        Ok(RepositoryBindingV2 {
            repository_key: self.repository_key,
            canonical_local_path: AbsoluteHostPath::parse(self.canonical_local_path)
                .map_err(contract_detail)?,
            default_branch: self.default_branch,
            delivery_mode: parse_delivery_mode(&self.delivery_mode)?,
        })
    }
}

impl TemplateWireV2 {
    fn into_domain(self) -> Result<TemplateArtifactV2, String> {
        Ok(TemplateArtifactV2 {
            source_path: ApplicationRelativePath::parse(self.source_path)
                .map_err(contract_detail)?,
            digest: ContentDigest::from_str(&self.digest).map_err(contract_detail)?,
            placeholders: self
                .placeholders
                .into_iter()
                .map(|value| TemplatePlaceholderV2::parse(value).map_err(contract_detail))
                .collect::<Result<_, _>>()?,
            rendered_byte_limit: self.rendered_byte_limit,
        })
    }
}

impl AssignmentRoleWireV2 {
    fn into_domain(self) -> Result<AssignmentRoleProfileV2, String> {
        Ok(AssignmentRoleProfileV2 {
            assignment_role: parse_assignment_role(&self.assignment_role)?,
            system_template: self.system_template.into_domain()?,
            assignment_template: self.assignment_template.into_domain()?,
            policy: self.policy.into_domain()?,
            tools: self
                .tools
                .into_iter()
                .map(|value| parse_tool(&value))
                .collect::<Result<_, _>>()?,
            model: self.model.into_domain()?,
            limits: self.limits.into_domain()?,
        })
    }
}

impl PolicyWireV2 {
    fn into_domain(self) -> Result<ActorPolicyArtifactV2, String> {
        Ok(ActorPolicyArtifactV2 {
            source_path: ApplicationRelativePath::parse(self.source_path)
                .map_err(contract_detail)?,
            digest: ContentDigest::from_str(&self.digest).map_err(contract_detail)?,
            byte_limit: self.byte_limit,
            entrypoint: PolicyEntrypointV2::parse(&self.entrypoint).map_err(contract_detail)?,
        })
    }
}

impl ModelWireV2 {
    fn into_domain(self) -> Result<ModelProfileV2, String> {
        Ok(ModelProfileV2 {
            provider: self.provider,
            model_id: self.model_id,
            thinking_level: parse_thinking_level(&self.thinking_level)?,
            context_token_limit: self.context_token_limit,
            output_token_limit: self.output_token_limit,
            price_input_micro_usd_per_million_tokens: MicroUsd::new(
                self.price_input_micro_usd_per_million_tokens,
            ),
            price_output_micro_usd_per_million_tokens: MicroUsd::new(
                self.price_output_micro_usd_per_million_tokens,
            ),
            price_cache_read_micro_usd_per_million_tokens: MicroUsd::new(
                self.price_cache_read_micro_usd_per_million_tokens,
            ),
            price_cache_write_micro_usd_per_million_tokens: MicroUsd::new(
                self.price_cache_write_micro_usd_per_million_tokens,
            ),
            capability_flags: self
                .capability_flags
                .iter()
                .map(|value| parse_model_capability(value))
                .collect::<Result<_, _>>()?,
        })
    }
}

impl LimitsWireV2 {
    fn into_domain(self) -> Result<SessionLimitsV2, String> {
        Ok(SessionLimitsV2 {
            turn_limit: self.turn_limit,
            wall_limit: DurationMillis::new(self.wall_limit_millis),
            output_byte_limit: self.output_byte_limit,
        })
    }
}

impl TicketPolicyWireV2 {
    fn into_domain(self) -> Result<TicketPolicyV2, String> {
        Ok(TicketPolicyV2 {
            low_water: self.low_water,
            target: self.target,
            maximum: self.maximum,
            proposal_maximum: self.proposal_maximum,
            ticket_bounds: self.ticket_bounds.into_domain(),
        })
    }
}

impl TicketBoundsWireV2 {
    fn into_domain(self) -> TicketBoundsV2 {
        TicketBoundsV2 {
            narrative_byte_limit: self.narrative_byte_limit,
            acceptance_criteria_limit: self.acceptance_criteria_limit,
            contract_read_limit: self.contract_read_limit,
        }
    }
}

impl RequiredReadWireV2 {
    fn into_domain(self) -> Result<RequiredReadV2, String> {
        Ok(RequiredReadV2 {
            path: RepositoryRelativePath::parse(self.path).map_err(contract_detail)?,
            reason: self.reason,
        })
    }
}

impl CommandWireV2 {
    fn into_domain(self) -> Result<CommandProfileV2, String> {
        Ok(CommandProfileV2 {
            name: self.name,
            executable: self.executable.into_domain()?,
            argv: self.argv,
            working_directory: RepositoryRelativePath::parse(self.working_directory)
                .map_err(contract_detail)?,
            environment: self
                .environment
                .into_iter()
                .map(EnvironmentWireV2::into_domain)
                .collect::<Result<_, _>>()?,
            timeout: DurationMillis::new(self.timeout_millis),
            stdout_byte_limit: self.stdout_byte_limit,
            stderr_byte_limit: self.stderr_byte_limit,
            expected_exit_status: self.expected_exit_status,
        })
    }
}

impl ExecutableWireV2 {
    fn into_domain(self) -> Result<ExecutableV2, String> {
        match (self.approved_tool, self.repository_path) {
            (Some(tool), None) => Ok(ExecutableV2::ApprovedTool(parse_approved_tool(&tool)?)),
            (None, Some(path)) => Ok(ExecutableV2::RepositoryPath(
                RepositoryRelativePath::parse(path).map_err(contract_detail)?,
            )),
            _ => Err("executable must contain exactly one closed variant".to_owned()),
        }
    }
}

impl EnvironmentWireV2 {
    fn into_domain(self) -> Result<EnvironmentAdditionV2, String> {
        Ok(EnvironmentAdditionV2 {
            name: self.name,
            value: self.value,
        })
    }
}

impl ValidationWireV2 {
    fn into_domain(self) -> Result<ValidationProfilesV2, String> {
        Ok(ValidationProfilesV2 {
            focused: self
                .focused
                .into_iter()
                .map(CommandWireV2::into_domain)
                .collect::<Result<_, _>>()?,
            full: self
                .full
                .into_iter()
                .map(CommandWireV2::into_domain)
                .collect::<Result<_, _>>()?,
        })
    }
}

impl GitWireV2 {
    fn into_domain(self) -> Result<GitPolicyV2, String> {
        Ok(GitPolicyV2 {
            forbidden_paths: self
                .forbidden_paths
                .into_iter()
                .map(|value| RepositoryRelativePath::parse(value).map_err(contract_detail))
                .collect::<Result<_, _>>()?,
            delivery_mode: parse_delivery_mode(&self.delivery_mode)?,
            provenance_trailers_required: self.provenance_trailers_required,
        })
    }
}

impl CommitMessageWireV2 {
    fn into_domain(self) -> Result<CommitMessagePolicyV2, String> {
        Ok(CommitMessagePolicyV2 {
            subject_byte_limit: self.subject_byte_limit,
            body_byte_limit: self.body_byte_limit,
        })
    }
}

fn contract_detail(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn validate_bounded_wire_text(
    value: &str,
    field: &'static str,
    maximum: usize,
) -> Result<(), ContractError> {
    if value.is_empty() || value.len() > maximum || value.contains('\0') {
        return Err(ContractError::InvalidValue {
            field,
            reason: "must be nonempty, bounded UTF-8 without NUL",
        });
    }
    Ok(())
}

fn parse_delivery_mode(value: &str) -> Result<DeliveryModeV2, String> {
    if value == "local_fast_forward_only" {
        Ok(DeliveryModeV2::LocalFastForwardOnly)
    } else {
        Err(format!("unsupported delivery mode {value:?}"))
    }
}

fn parse_assignment_role(value: &str) -> Result<AssignmentRole, String> {
    match value {
        "product_research" => Ok(AssignmentRole::ProductResearch),
        "engineering" => Ok(AssignmentRole::Engineering),
        "quality" => Ok(AssignmentRole::Quality),
        _ => Err(format!("unsupported office {value:?}")),
    }
}

fn parse_thinking_level(value: &str) -> Result<ThinkingLevelV2, String> {
    match value {
        "none" => Ok(ThinkingLevelV2::None),
        "low" => Ok(ThinkingLevelV2::Low),
        "medium" => Ok(ThinkingLevelV2::Medium),
        "high" => Ok(ThinkingLevelV2::High),
        "xhigh" => Ok(ThinkingLevelV2::XHigh),
        _ => Err(format!("unsupported thinking level {value:?}")),
    }
}

fn parse_model_capability(value: &str) -> Result<ModelCapabilityV2, String> {
    match value {
        "reasoning" => Ok(ModelCapabilityV2::Reasoning),
        _ => Err(format!("unsupported model capability {value:?}")),
    }
}

fn parse_approved_tool(value: &str) -> Result<ApprovedToolV2, String> {
    match value {
        "cargo" => Ok(ApprovedToolV2::Cargo),
        "git" => Ok(ApprovedToolV2::Git),
        _ => Err(format!("unsupported approved tool {value:?}")),
    }
}

fn parse_tool(value: &str) -> Result<ActorToolV2, String> {
    match value {
        "workspace_read" => Ok(ActorToolV2::WorkspaceRead),
        "workspace_write" => Ok(ActorToolV2::WorkspaceWrite),
        "workspace_edit" => Ok(ActorToolV2::WorkspaceEdit),
        "workspace_search" => Ok(ActorToolV2::WorkspaceSearch),
        "workspace_list" => Ok(ActorToolV2::WorkspaceList),
        "shell" => Ok(ActorToolV2::Shell),
        "forum_search" => Ok(ActorToolV2::ForumSearch),
        "forum_list_topics" => Ok(ActorToolV2::ForumListTopics),
        "forum_list_threads" => Ok(ActorToolV2::ForumListThreads),
        "forum_read_thread" => Ok(ActorToolV2::ForumReadThread),
        "publication_create" => Ok(ActorToolV2::PublicationCreate),
        "artifact_seal" => Ok(ActorToolV2::ArtifactSeal),
        "artifact_read" => Ok(ActorToolV2::ArtifactRead),
        "product_submit_ticket" => Ok(ActorToolV2::ProductSubmitTicket),
        "candidate_checkpoint_regression" => Ok(ActorToolV2::CandidateCheckpointRegression),
        "candidate_submit" => Ok(ActorToolV2::CandidateSubmit),
        "quality_run_full_suite" => Ok(ActorToolV2::QualityRunFullSuite),
        "quality_submit_review" => Ok(ActorToolV2::QualitySubmitReview),
        "work_complete" => Ok(ActorToolV2::WorkComplete),
        _ => Err(format!("unsupported actor tool {value:?}")),
    }
}
