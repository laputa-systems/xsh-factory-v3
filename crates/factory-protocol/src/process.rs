//! Closed values for one paid assignment and its supervised process.
//!
//! These values are deliberately independent of PostgreSQL and of the Pi SDK.
//! The packet is the immutable authority input for a host process; a terminal
//! report is only a claim which the kernel rechecks against the packet and
//! sealed artifacts before it advances durable state.

use std::collections::BTreeSet;

use crate::{
    AbsoluteHostPath, AggregateRevision, ApplicationRevisionId, ArtifactId, CandidateId,
    ContentDigest, ContractError, ExpectedRevision, KernelBuildId, MicroUsd, Office,
    PROTOCOL_VERSION_V1, RepositoryRelativePath, RuntimeRelativePath, SessionLimitsV1,
    TicketAttemptId,
};

const JAVASCRIPT_SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;

pub const ASSIGNMENT_PACKET_V1_FORMAT: u16 = 1;

/// One-time daemon-to-host admission gate sent over inherited FD 0 before Pi
/// construction. The packet bytes and digest are both present so the host
/// can verify the exact immutable input; `session_revision` is the only
/// revision accepted by subsequent session-scoped RPCs.
#[derive(Clone, Debug, PartialEq, Eq, miniserde::Serialize, miniserde::Deserialize)]
pub struct SessionAdmissionFrameV1 {
    pub r#type: String,
    pub protocol_version: u16,
    pub assignment_id: String,
    pub session_id: i64,
    pub session_revision: u64,
    pub packet_digest: String,
    pub packet_b64: String,
}

impl SessionAdmissionFrameV1 {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.r#type != "session.admitted" || self.protocol_version != PROTOCOL_VERSION_V1 {
            return Err(ContractError::InvalidValue {
                field: "session admission frame",
                reason: "type or protocol version is unsupported",
            });
        }
        if self.assignment_id.is_empty()
            || self.session_id < 1
            || self.session_id as u64 > JAVASCRIPT_SAFE_INTEGER_MAX
            || self.session_revision > JAVASCRIPT_SAFE_INTEGER_MAX
            || self.packet_digest.len() != 64
            || !self
                .packet_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || self.packet_b64.is_empty()
        {
            return Err(ContractError::InvalidValue {
                field: "session admission frame",
                reason: "identity, digest, and packet bytes are invalid",
            });
        }
        Ok(())
    }
}

/// Closed JSON identity carried to the generic Deno host.  This is separate
/// from the kernel's persistence-facing `AssignmentPacketV1`: the host needs
/// sealed prompt bytes and generic repository/factory base identities, while
/// the kernel domain object intentionally remains free of transport details.
/// Every field is explicit; there is no metadata or application-specific map.
#[derive(Clone, Debug, PartialEq, Eq, miniserde::Serialize, miniserde::Deserialize)]
pub struct AssignmentPacketWireV1 {
    pub format_version: u16,
    pub campaign_id: i64,
    pub assignment_id: i64,
    pub application_revision_id: i64,
    pub kernel_build_id: String,
    pub office: String,
    pub target: String,
    pub repository_base_identity: String,
    pub factory_base_identity: String,
    /// Durable assignment target, never a prompt-derived display reference.
    pub ticket_attempt_id: Option<i64>,
    /// Quality's exact candidate, paired with `ticket_attempt_id`.
    pub candidate_id: Option<i64>,
    pub system_prompt_artifact_id: i64,
    pub assignment_prompt_artifact_id: i64,
    pub required_read_manifest_artifact_id: i64,
    pub system_prompt_digest: String,
    pub assignment_prompt_digest: String,
    pub system_prompt_bytes_b64: String,
    pub assignment_prompt_bytes_b64: String,
    pub workspace_root: String,
    pub staging_root: String,
    pub model: AssignmentModelWireV1,
    pub limits: AssignmentLimitsWireV1,
    pub runtime: AssignmentRuntimeWireV1,
    pub required_reads: Vec<AssignmentReadWireV1>,
    /// Closed, named upstream evidence available through `artifact.read`.
    /// This is packet authority, not explanatory target prose.
    pub assignment_evidence: Vec<AssignmentEvidenceWireV1>,
    pub tools: Vec<String>,
    pub terminal_operations: Vec<String>,
    pub remaining_campaign_allowance_micro_usd: u64,
    pub aggregate_revision: u64,
    pub packet_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq, miniserde::Serialize, miniserde::Deserialize)]
pub struct AssignmentModelWireV1 {
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

#[derive(Clone, Debug, PartialEq, Eq, miniserde::Serialize, miniserde::Deserialize)]
pub struct AssignmentLimitsWireV1 {
    pub turn_limit: u32,
    pub wall_limit_millis: u64,
    pub output_byte_limit: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, miniserde::Serialize, miniserde::Deserialize)]
pub struct AssignmentRuntimeWireV1 {
    pub deno_executable: String,
    pub deno_version: String,
    pub source_graph_digest: String,
    pub resolved_dependency_graph_digest: String,
    pub deno_json_digest: String,
    pub deno_lock_digest: String,
    pub pi_version: String,
    pub credential_source: AssignmentCredentialWireV1,
}

#[derive(Clone, Debug, PartialEq, Eq, miniserde::Serialize, miniserde::Deserialize)]
pub struct AssignmentCredentialWireV1 {
    pub kind: String,
    pub name: Option<String>,
    pub path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, miniserde::Serialize, miniserde::Deserialize)]
pub struct AssignmentReadWireV1 {
    pub path: String,
    pub digest: String,
    pub reason: String,
}

/// One closed evidence identity carried in the canonical assignment packet.
/// `role` selects a single durable semantic position; it is not an
/// application-controlled label or a generic artifact metadata key.
#[derive(Clone, Debug, PartialEq, Eq, miniserde::Serialize, miniserde::Deserialize)]
pub struct AssignmentEvidenceWireV1 {
    pub role: String,
    pub artifact_id: i64,
    pub digest: String,
    pub byte_length: u64,
}

/// The complete named evidence closure the generic SDK host may discover from
/// an assignment. The names intentionally distinguish stdout/stderr and each
/// reproduced observation: collapsing them would make the source evidence
/// ambiguous at the actor boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AssignmentEvidenceRoleV1 {
    TicketProposal,
    TicketNarrative,
    TicketEvidence,
    ReproducerCommand,
    ReproducerStdin,
    ReproducerExpectedStdout,
    ReproducerExpectedStderr,
    ReproducerFirstActualStdout,
    ReproducerFirstActualStderr,
    ReproducerSecondActualStdout,
    ReproducerSecondActualStderr,
    RegressionPatch,
    RegressionCommandSet,
    RegressionLog,
    ChangedPaths,
    CandidatePatch,
    EngineeringReport,
    EngineeringRisks,
    HardValidationCommandSet,
    HardValidationLog,
    QualityAdditionalProbes,
    QualityRationale,
    QualityRisks,
    ExternalDecisionRationale,
}

impl AssignmentEvidenceRoleV1 {
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::TicketProposal => "ticket_proposal",
            Self::TicketNarrative => "ticket_narrative",
            Self::TicketEvidence => "ticket_evidence",
            Self::ReproducerCommand => "reproducer_command",
            Self::ReproducerStdin => "reproducer_stdin",
            Self::ReproducerExpectedStdout => "reproducer_expected_stdout",
            Self::ReproducerExpectedStderr => "reproducer_expected_stderr",
            Self::ReproducerFirstActualStdout => "reproducer_first_actual_stdout",
            Self::ReproducerFirstActualStderr => "reproducer_first_actual_stderr",
            Self::ReproducerSecondActualStdout => "reproducer_second_actual_stdout",
            Self::ReproducerSecondActualStderr => "reproducer_second_actual_stderr",
            Self::RegressionPatch => "regression_patch",
            Self::RegressionCommandSet => "regression_command_set",
            Self::RegressionLog => "regression_log",
            Self::ChangedPaths => "changed_paths",
            Self::CandidatePatch => "candidate_patch",
            Self::EngineeringReport => "engineering_report",
            Self::EngineeringRisks => "engineering_risks",
            Self::HardValidationCommandSet => "hard_validation_command_set",
            Self::HardValidationLog => "hard_validation_log",
            Self::QualityAdditionalProbes => "quality_additional_probes",
            Self::QualityRationale => "quality_rationale",
            Self::QualityRisks => "quality_risks",
            Self::ExternalDecisionRationale => "external_decision_rationale",
        }
    }

    pub fn parse_wire_name(value: &str) -> Result<Self, ContractError> {
        match value {
            "ticket_proposal" => Ok(Self::TicketProposal),
            "ticket_narrative" => Ok(Self::TicketNarrative),
            "ticket_evidence" => Ok(Self::TicketEvidence),
            "reproducer_command" => Ok(Self::ReproducerCommand),
            "reproducer_stdin" => Ok(Self::ReproducerStdin),
            "reproducer_expected_stdout" => Ok(Self::ReproducerExpectedStdout),
            "reproducer_expected_stderr" => Ok(Self::ReproducerExpectedStderr),
            "reproducer_first_actual_stdout" => Ok(Self::ReproducerFirstActualStdout),
            "reproducer_first_actual_stderr" => Ok(Self::ReproducerFirstActualStderr),
            "reproducer_second_actual_stdout" => Ok(Self::ReproducerSecondActualStdout),
            "reproducer_second_actual_stderr" => Ok(Self::ReproducerSecondActualStderr),
            "regression_patch" => Ok(Self::RegressionPatch),
            "regression_command_set" => Ok(Self::RegressionCommandSet),
            "regression_log" => Ok(Self::RegressionLog),
            "changed_paths" => Ok(Self::ChangedPaths),
            "candidate_patch" => Ok(Self::CandidatePatch),
            "engineering_report" => Ok(Self::EngineeringReport),
            "engineering_risks" => Ok(Self::EngineeringRisks),
            "hard_validation_command_set" => Ok(Self::HardValidationCommandSet),
            "hard_validation_log" => Ok(Self::HardValidationLog),
            "quality_additional_probes" => Ok(Self::QualityAdditionalProbes),
            "quality_rationale" => Ok(Self::QualityRationale),
            "quality_risks" => Ok(Self::QualityRisks),
            "external_decision_rationale" => Ok(Self::ExternalDecisionRationale),
            _ => Err(ContractError::InvalidValue {
                field: "assignment evidence role",
                reason: "is not a closed assignment evidence role",
            }),
        }
    }
}

/// Kernel-domain form of one packet evidence reference. It retains the
/// artifact's sealed identity instead of accepting an opaque display label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssignmentEvidenceV1 {
    pub role: AssignmentEvidenceRoleV1,
    pub artifact_id: ArtifactId,
    pub digest: ContentDigest,
    pub byte_length: u64,
}

impl AssignmentEvidenceV1 {
    pub fn validate(&self) -> Result<(), ContractError> {
        Ok(())
    }
}

/// A read which the host must perform exactly, before it can submit terminal
/// state. Only the path, digest, and human reason cross the process boundary;
/// file bytes never become protocol metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadExactFileV1 {
    pub path: RepositoryRelativePath,
    pub digest: ContentDigest,
    pub reason: String,
}

/// The daemon's wrapped read result. It contains no file bytes and cannot be
/// satisfied by shell output or prompt text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadObservationV1 {
    pub path: RepositoryRelativePath,
    pub digest: ContentDigest,
}

impl ReadObservationV1 {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.path.as_str().contains('\0') {
            return Err(ContractError::InvalidValue {
                field: "required read observation path",
                reason: "must not contain NUL",
            });
        }
        Ok(())
    }
}

impl ReadExactFileV1 {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.reason.is_empty() || self.reason.len() > 240 || self.reason.contains('\0') {
            return Err(ContractError::InvalidValue {
                field: "required read reason",
                reason: "must be 1 through 240 bytes without NUL",
            });
        }
        Ok(())
    }
}

/// Names exactly one credential source without carrying a secret or an
/// environment value. The variant is part of the closed packet identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialDescriptorV1 {
    Environment { name: String },
    PiAuthStore { path: RuntimeRelativePath },
}

impl CredentialDescriptorV1 {
    pub fn validate(&self) -> Result<(), ContractError> {
        match self {
            Self::Environment { name } => {
                let valid = !name.is_empty()
                    && name.len() <= 160
                    && name.bytes().enumerate().all(|(index, byte)| {
                        (index == 0 && (byte.is_ascii_uppercase() || byte == b'_'))
                            || (index > 0
                                && (byte.is_ascii_uppercase()
                                    || byte.is_ascii_digit()
                                    || byte == b'_'))
                    });
                if !valid {
                    return Err(ContractError::InvalidValue {
                        field: "credential environment name",
                        reason: "must use 1 through 160 upper-case environment-name bytes",
                    });
                }
                Ok(())
            }
            Self::PiAuthStore { path } if !path.as_str().is_empty() => Ok(()),
            Self::PiAuthStore { .. } => Err(ContractError::InvalidValue {
                field: "credential auth-store path",
                reason: "must be a non-empty runtime-relative path",
            }),
        }
    }
}

/// Runtime identity checked by the host immediately before spawn. The
/// credential field is a descriptor only; no secret bytes are represented.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeIdentityV1 {
    pub deno_executable: AbsoluteHostPath,
    pub deno_version: String,
    pub source_graph_digest: ContentDigest,
    /// Digest of the exact resolved dependency graph used by the installed
    /// host. Cache layout is execution material and is intentionally absent.
    pub resolved_dependency_graph_digest: ContentDigest,
    pub deno_json_digest: ContentDigest,
    pub deno_lock_digest: ContentDigest,
    pub pi_version: String,
    pub credential: CredentialDescriptorV1,
}

impl RuntimeIdentityV1 {
    pub fn validate(&self) -> Result<(), ContractError> {
        for (field, value) in [
            ("Deno version", self.deno_version.as_str()),
            ("Pi version", self.pi_version.as_str()),
        ] {
            if value.is_empty() || value.len() > 240 || value.contains('\0') {
                return Err(ContractError::InvalidValue {
                    field,
                    reason: "must be 1 through 240 bytes without NUL",
                });
            }
        }
        self.credential.validate()
    }
}

/// The exact terminal operation selected by the assignment. The host may not
/// invent another terminal operation after it has been admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TerminalOperationV1 {
    WorkComplete,
    CandidateSubmit,
    QualitySubmitReview,
}

/// Why the supervised host stopped. This is closed so a new stop reason is a
/// protocol change rather than an unvalidated string in an audit row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StopReasonV1 {
    Completed,
    Cancelled,
    Deadline,
    DaemonDisconnected,
    NonZeroExit,
    OutputLimit,
    ProtocolError,
    UnknownCost,
}

/// Normalized provider usage. Provider-specific event streams are not stored
/// in PostgreSQL; this bounded summary is the only cost input at terminal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UsageTotalsV1 {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: Option<u64>,
    /// Provider-reported terminal total, already normalized to integral
    /// micro-USD by the host (the host rounds upward from provider currency).
    /// `None` is unknown and must freeze campaign admission.
    pub reported_cost_micro_usd: Option<MicroUsd>,
}

impl UsageTotalsV1 {
    /// Computes integer micro-USD with each token class rounded upward at the
    /// million-token boundary. A missing usage report is represented by
    /// [`TerminalCostV1::Unknown`], never by zero usage.
    pub fn cost_at(
        self,
        input_price_per_million: MicroUsd,
        output_price_per_million: MicroUsd,
    ) -> Result<MicroUsd, ContractError> {
        self.cost_at_with_cache(
            input_price_per_million,
            output_price_per_million,
            MicroUsd::new(0),
            MicroUsd::new(0),
        )
    }

    pub fn cost_at_with_cache(
        self,
        input_price_per_million: MicroUsd,
        output_price_per_million: MicroUsd,
        cache_read_price_per_million: MicroUsd,
        cache_write_price_per_million: MicroUsd,
    ) -> Result<MicroUsd, ContractError> {
        let _ = (
            input_price_per_million,
            output_price_per_million,
            cache_read_price_per_million,
            cache_write_price_per_million,
        );
        self.reported_cost_micro_usd
            .ok_or(ContractError::InvalidValue {
                field: "session cost",
                reason: "provider terminal cost is absent",
            })
    }
}

/// Cost is explicit and tri-state. Unknown is not zero and freezes later paid
/// admission at the campaign boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalCostV1 {
    Known(MicroUsd),
    Unknown,
    Exceeded(MicroUsd),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssignmentPacketV1 {
    pub format_version: u16,
    pub campaign_id: crate::CampaignId,
    pub assignment_id: crate::AssignmentId,
    pub kernel_build_id: KernelBuildId,
    pub application_revision_id: ApplicationRevisionId,
    pub office: Office,
    pub target: String,
    /// Product has no target. Engineering names one attempt. Quality names an
    /// attempt and a candidate from that attempt. These IDs are packet facts
    /// rechecked against the durable assignment row before actor dispatch.
    pub ticket_attempt_id: Option<TicketAttemptId>,
    pub candidate_id: Option<CandidateId>,
    pub system_prompt_artifact_id: ArtifactId,
    pub assignment_prompt_artifact_id: ArtifactId,
    pub required_read_manifest_artifact_id: ArtifactId,
    pub workspace_root: AbsoluteHostPath,
    pub staging_root: AbsoluteHostPath,
    pub model: crate::ModelProfileV1,
    pub limits: SessionLimitsV1,
    pub runtime: RuntimeIdentityV1,
    pub required_reads: Vec<ReadExactFileV1>,
    pub assignment_evidence: Vec<AssignmentEvidenceV1>,
    pub terminal_operations: Vec<TerminalOperationV1>,
    pub remaining_campaign_allowance: MicroUsd,
    pub revision: AggregateRevision,
    pub packet_digest: ContentDigest,
}

impl AssignmentPacketV1 {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.format_version != ASSIGNMENT_PACKET_V1_FORMAT {
            return Err(ContractError::InvalidValue {
                field: "assignment packet format",
                reason: "unsupported assignment packet version",
            });
        }
        if self.target.is_empty() || self.target.len() > 4096 || self.target.contains('\0') {
            return Err(ContractError::InvalidValue {
                field: "assignment target",
                reason: "must be 1 through 4096 bytes without NUL",
            });
        }
        match (self.office, self.ticket_attempt_id, self.candidate_id) {
            (Office::ProductResearch, None, None)
            | (Office::Engineering, Some(_), None)
            | (Office::Quality, Some(_), Some(_)) => {}
            _ => {
                return Err(ContractError::InvalidValue {
                    field: "assignment target identity",
                    reason: "office requires its exact durable target shape",
                });
            }
        }
        if self.assignment_evidence.len() > 24 {
            return Err(ContractError::InvalidValue {
                field: "assignment evidence",
                reason: "exceeds the closed evidence reference limit",
            });
        }
        if self.office == Office::ProductResearch && !self.assignment_evidence.is_empty() {
            return Err(ContractError::InvalidValue {
                field: "assignment evidence",
                reason: "Product has no upstream assignment evidence",
            });
        }
        if self.office != Office::ProductResearch && self.assignment_evidence.is_empty() {
            return Err(ContractError::InvalidValue {
                field: "assignment evidence",
                reason: "Engineering and Quality require upstream evidence",
            });
        }
        let mut evidence_roles = BTreeSet::new();
        for evidence in &self.assignment_evidence {
            evidence.validate()?;
            if !evidence_roles.insert(evidence.role) {
                return Err(ContractError::InvalidValue {
                    field: "assignment evidence",
                    reason: "roles must be unique",
                });
            }
        }
        if self.required_reads.is_empty() {
            return Err(ContractError::InvalidValue {
                field: "assignment required reads",
                reason: "at least one required read is required",
            });
        }
        let mut read_paths = BTreeSet::new();
        for read in &self.required_reads {
            read.validate()?;
            if !read_paths.insert(read.path.as_str()) {
                return Err(ContractError::InvalidValue {
                    field: "assignment required reads",
                    reason: "paths must be unique",
                });
            }
        }
        if self.terminal_operations.is_empty() {
            return Err(ContractError::InvalidValue {
                field: "assignment terminal operations",
                reason: "at least one terminal operation is required",
            });
        }
        self.runtime.validate()
    }
}

/// Kernel-admitted terminal report. The host supplies sealed artifact IDs and
/// normalized usage, while the kernel recomputes packet identity and cost.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalReportV1 {
    pub packet_digest: ContentDigest,
    pub expected_session_revision: ExpectedRevision,
    /// Infrastructure shutdowns have no actor terminal operation. A supplied
    /// operation is checked against the assignment allowlist only for a
    /// successful actor submission.
    pub operation: Option<TerminalOperationV1>,
    pub stop_reason: StopReasonV1,
    pub report_digest: ContentDigest,
}

/// The only process state persisted by the kernel. PID and PGID are custody
/// evidence, never a permission supplied by an actor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessCustodyV1 {
    pub pid: u32,
    pub pgid: u32,
    pub started_at_unix_millis: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_rounds_each_class_upward() {
        let usage = UsageTotalsV1 {
            input_tokens: 1,
            output_tokens: 1_000_001,
            reasoning_tokens: None,
            reported_cost_micro_usd: Some(MicroUsd::new(13)),
            ..UsageTotalsV1::default()
        };
        assert_eq!(
            usage.cost_at_with_cache(
                MicroUsd::new(7),
                MicroUsd::new(11),
                MicroUsd::new(13),
                MicroUsd::new(17),
            ),
            Ok(MicroUsd::new(13))
        );
    }

    #[test]
    fn absent_usage_is_unknown_not_zero() {
        let usage = UsageTotalsV1::default();
        assert!(
            usage
                .cost_at_with_cache(
                    MicroUsd::new(10),
                    MicroUsd::new(10),
                    MicroUsd::new(10),
                    MicroUsd::new(10)
                )
                .is_err()
        );
    }
}
