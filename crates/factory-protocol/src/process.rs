//! Closed values for one paid assignment and its supervised process.
//!
//! These values are deliberately independent of PostgreSQL and provider
//! implementation details.
//! The packet is the immutable authority input for a host process; a terminal
//! report is only a claim which the kernel rechecks against the packet and
//! sealed artifacts before it advances durable state.

use std::collections::BTreeSet;

use crate::{
    AbsoluteHostPath, AggregateRevision, ApplicationRevisionId, ArtifactId, AssignmentRole,
    CandidateId, ContentDigest, ContractError, ExpectedRevision, KernelBuildId,
    MAX_POLICY_ARTIFACT_BYTES, MAX_SESSION_OUTPUT_BYTES, MicroUsd, PROTOCOL_VERSION_V2,
    PolicyEntrypointV2, RepositoryRelativePath, SessionLimitsV2, TicketAttemptId,
};

pub const ASSIGNMENT_PACKET_V2_FORMAT: u16 = 2;

/// One-time daemon-to-host admission gate sent over inherited FD 0 before Pi
/// construction. The packet bytes and digest are both present so the host
/// can verify the exact immutable input; `session_revision` is the only
/// revision accepted by subsequent session-scoped RPCs.
#[derive(Clone, Debug, PartialEq, Eq, miniserde::Serialize, miniserde::Deserialize)]
pub struct SessionAdmissionFrameV2 {
    pub r#type: String,
    pub protocol_version: u16,
    pub assignment_id: String,
    pub session_id: i64,
    pub session_revision: u64,
    pub packet_digest: String,
    pub packet_b64: String,
}

impl SessionAdmissionFrameV2 {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.r#type != "session.admitted" || self.protocol_version != PROTOCOL_VERSION_V2 {
            return Err(ContractError::InvalidValue {
                field: "session admission frame",
                reason: "type or protocol version is unsupported",
            });
        }
        if self.assignment_id.is_empty()
            || self.session_id < 1
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

/// Closed JSON identity carried to the Rust host. This is separate from the
/// kernel's persistence-facing `AssignmentPacketV2`: the host needs sealed
/// prompt bytes and generic repository/factory base identities, while the
/// kernel domain object intentionally remains free of transport details.
/// Every field is explicit; there is no metadata or application-specific map.
#[derive(Clone, Debug, PartialEq, Eq, miniserde::Serialize, miniserde::Deserialize)]
pub struct AssignmentPacketWireV2 {
    pub format_version: u16,
    pub campaign_id: i64,
    pub assignment_id: i64,
    pub application_revision_id: i64,
    pub kernel_build_id: String,
    pub assignment_role: String,
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
    /// Sealed Luau policy source. It is carried inline so the host never
    /// reads application files from disk after admission.
    pub policy_digest: String,
    pub policy_byte_limit: u32,
    pub policy_bytes_b64: String,
    pub policy_entrypoint: String,
    pub workspace_root: String,
    pub staging_root: String,
    pub model: AssignmentModelWireV2,
    pub limits: AssignmentLimitsWireV2,
    pub runtime: AssignmentRuntimeWireV2,
    pub required_reads: Vec<AssignmentReadWireV2>,
    /// Closed, named upstream evidence available through `artifact.read`.
    /// This is packet authority, not explanatory target prose.
    pub assignment_evidence: Vec<AssignmentEvidenceWireV2>,
    pub tools: Vec<String>,
    pub terminal_operations: Vec<String>,
    pub remaining_campaign_allowance_micro_usd: u64,
    pub aggregate_revision: u64,
    pub packet_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq, miniserde::Serialize, miniserde::Deserialize)]
pub struct AssignmentModelWireV2 {
    pub provider: String,
    pub model_id: String,
    pub thinking_level: String,
    pub context_token_limit: u32,
    pub output_token_limit: u32,
    /// Legacy pricing metadata retained for packet identity compatibility;
    /// Factory accounting never derives cost from these fields.
    pub price_input_micro_usd_per_million_tokens: u64,
    pub price_output_micro_usd_per_million_tokens: u64,
    pub price_cache_read_micro_usd_per_million_tokens: u64,
    pub price_cache_write_micro_usd_per_million_tokens: u64,
    pub capability_flags: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, miniserde::Serialize, miniserde::Deserialize)]
pub struct AssignmentLimitsWireV2 {
    pub wall_limit_millis: u64,
    pub output_byte_limit: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, miniserde::Serialize, miniserde::Deserialize)]
pub struct AssignmentRuntimeWireV2 {
    /// Absolute path of the exact Rust host executable qualified by the
    /// installed runtime receipt.
    pub host_executable: String,
    /// Full Git commit of the local `pi-agent-core-rs` checkout.
    pub core_head: String,
    /// BLAKE3 identity of the closed core source inventory.
    pub core_source_digest: String,
    /// Exact Rust toolchain used to build the host and core.
    pub rust_toolchain: String,
    /// Name of the inherited environment variable containing the provider
    /// credential. The value itself never crosses this packet boundary.
    pub credential_env: String,
}

#[derive(Clone, Debug, PartialEq, Eq, miniserde::Serialize, miniserde::Deserialize)]
pub struct AssignmentReadWireV2 {
    pub path: String,
    pub digest: String,
    pub reason: String,
}

/// One closed evidence identity carried in the canonical assignment packet.
/// `role` selects a single durable semantic position; it is not an
/// application-controlled label or a generic artifact metadata key.
#[derive(Clone, Debug, PartialEq, Eq, miniserde::Serialize, miniserde::Deserialize)]
pub struct AssignmentEvidenceWireV2 {
    pub role: String,
    pub artifact_id: i64,
    pub digest: String,
    pub byte_length: u64,
}

/// The complete named evidence closure the generic Rust host may discover from
/// an assignment. The names intentionally distinguish stdout/stderr and each
/// reproduced observation: collapsing them would make the source evidence
/// ambiguous at the actor boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AssignmentEvidenceRoleV2 {
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

impl AssignmentEvidenceRoleV2 {
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
pub struct AssignmentEvidenceV2 {
    pub role: AssignmentEvidenceRoleV2,
    pub artifact_id: ArtifactId,
    pub digest: ContentDigest,
    pub byte_length: u64,
}

impl AssignmentEvidenceV2 {
    pub fn validate(&self) -> Result<(), ContractError> {
        Ok(())
    }
}

/// A read which the host must perform exactly, before it can submit terminal
/// state. Only the path, digest, and human reason cross the process boundary;
/// file bytes never become protocol metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadExactFileV2 {
    pub path: RepositoryRelativePath,
    pub digest: ContentDigest,
    pub reason: String,
}

/// The daemon's wrapped read result. It contains no file bytes and cannot be
/// satisfied by shell output or prompt text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadObservationV2 {
    pub path: RepositoryRelativePath,
    pub digest: ContentDigest,
}

impl ReadObservationV2 {
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

impl ReadExactFileV2 {
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
pub enum CredentialDescriptorV2 {
    Environment { name: String },
}

impl CredentialDescriptorV2 {
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
        }
    }
}

/// Runtime identity checked by the host immediately before spawn. The
/// credential field is a descriptor only; no secret bytes are represented.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeIdentityV2 {
    pub host_executable: AbsoluteHostPath,
    pub core_head: String,
    pub core_source_digest: ContentDigest,
    pub rust_toolchain: String,
    pub credential_env: String,
}

impl RuntimeIdentityV2 {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.core_head.len() != 40
            || !self
                .core_head
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(ContractError::InvalidValue {
                field: "pi-agent-core HEAD",
                reason: "must be exactly 40 lower-case hexadecimal characters",
            });
        }
        for (field, value) in [("Rust toolchain", self.rust_toolchain.as_str())] {
            if value.is_empty() || value.len() > 240 || value.contains('\0') {
                return Err(ContractError::InvalidValue {
                    field,
                    reason: "must be 1 through 240 bytes without NUL",
                });
            }
        }
        CredentialDescriptorV2::Environment {
            name: self.credential_env.clone(),
        }
        .validate()
    }
}

/// The exact terminal operation selected by the assignment. The host may not
/// invent another terminal operation after it has been admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TerminalOperationV2 {
    WorkComplete,
    CandidateSubmit,
    QualitySubmitReview,
}

/// Why the supervised host stopped. This is closed so a new stop reason is a
/// protocol change rather than an unvalidated string in an audit row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StopReasonV2 {
    Completed,
    Cancelled,
    Deadline,
    DaemonDisconnected,
    NonZeroExit,
    OutputLimit,
    ProtocolError,
    UnknownCost,
    /// The host stopped after live provider-reported spend reached the
    /// campaign allowance.
    CostLimit,
}

/// Normalized provider usage. Provider-specific event streams are not stored
/// in PostgreSQL; this bounded summary is the only cost evidence at terminal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UsageTotalsV2 {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: Option<u64>,
    /// Optional provider-reported terminal total, already normalized to
    /// integral micro-USD by the host. A complete reported total is the sole
    /// authoritative Factory-cost input; token counters remain diagnostics.
    pub reported_cost_micro_usd: Option<MicroUsd>,
}

impl UsageTotalsV2 {
    /// Returns the complete provider-reported terminal total. Token usage is
    /// retained for diagnostics and evidence, but is never substituted for a
    /// missing provider total.
    pub fn provider_cost(self) -> Result<MicroUsd, ContractError> {
        self.reported_cost_micro_usd.ok_or(ContractError::InvalidValue {
            field: "session cost",
            reason: "complete provider-reported terminal cost is absent",
        })
    }
}

/// Cost is explicit and tri-state. Unknown is not zero and freezes later paid
/// admission at the campaign boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalCostV2 {
    Known(MicroUsd),
    Unknown,
    Exceeded(MicroUsd),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssignmentPacketV2 {
    pub format_version: u16,
    pub campaign_id: crate::CampaignId,
    pub assignment_id: crate::AssignmentId,
    pub kernel_build_id: KernelBuildId,
    pub application_revision_id: ApplicationRevisionId,
    pub assignment_role: AssignmentRole,
    pub target: String,
    /// Product has no target. Engineering names one attempt. Quality names an
    /// attempt and a candidate from that attempt. These IDs are packet facts
    /// rechecked against the durable assignment row before actor dispatch.
    pub ticket_attempt_id: Option<TicketAttemptId>,
    pub candidate_id: Option<CandidateId>,
    pub system_prompt_artifact_id: ArtifactId,
    pub assignment_prompt_artifact_id: ArtifactId,
    pub required_read_manifest_artifact_id: ArtifactId,
    /// The sealed Luau artifact identity and source delivered inline to the
    /// host. The digest is the artifact identity; no CAS lookup is required
    /// at actor runtime.
    pub policy_digest: ContentDigest,
    pub policy_byte_limit: u32,
    pub policy_bytes: Vec<u8>,
    pub policy_entrypoint: PolicyEntrypointV2,
    pub workspace_root: AbsoluteHostPath,
    pub staging_root: AbsoluteHostPath,
    pub model: crate::ModelProfileV2,
    pub limits: SessionLimitsV2,
    pub runtime: RuntimeIdentityV2,
    pub required_reads: Vec<ReadExactFileV2>,
    pub assignment_evidence: Vec<AssignmentEvidenceV2>,
    pub terminal_operations: Vec<TerminalOperationV2>,
    pub remaining_campaign_allowance: MicroUsd,
    pub revision: AggregateRevision,
    pub packet_digest: ContentDigest,
}

impl AssignmentPacketV2 {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.format_version != ASSIGNMENT_PACKET_V2_FORMAT {
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
        if self.limits.wall_limit.get() == 0
            || self.limits.output_byte_limit == 0
            || u64::from(self.limits.output_byte_limit) > MAX_SESSION_OUTPUT_BYTES
        {
            return Err(ContractError::InvalidValue {
                field: "assignment session limits",
                reason: "must be positive and output must fit one CAS object",
            });
        }
        if self.policy_byte_limit == 0
            || self.policy_byte_limit as usize > MAX_POLICY_ARTIFACT_BYTES
        {
            return Err(ContractError::InvalidValue {
                field: "assignment policy byte limit",
                reason: "must be positive and within the policy ceiling",
            });
        }
        if self.policy_bytes.is_empty() || self.policy_bytes.len() > self.policy_byte_limit as usize
        {
            return Err(ContractError::ByteLimitExceeded {
                field: "assignment policy bytes",
                maximum: self.policy_byte_limit as usize,
            });
        }
        if self.policy_bytes.contains(&0) || std::str::from_utf8(&self.policy_bytes).is_err() {
            return Err(ContractError::InvalidValue {
                field: "assignment policy bytes",
                reason: "must be nonempty UTF-8 without NUL",
            });
        }
        if ContentDigest::of_bytes(&self.policy_bytes) != self.policy_digest {
            return Err(ContractError::InvalidValue {
                field: "assignment policy digest",
                reason: "does not match sealed policy bytes",
            });
        }
        match (
            self.assignment_role,
            self.ticket_attempt_id,
            self.candidate_id,
        ) {
            (AssignmentRole::ProductResearch, None, None)
            | (AssignmentRole::Engineering, Some(_), None)
            | (AssignmentRole::Quality, Some(_), Some(_)) => {}
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
        if self.assignment_role == AssignmentRole::ProductResearch
            && !self.assignment_evidence.is_empty()
        {
            return Err(ContractError::InvalidValue {
                field: "assignment evidence",
                reason: "Product has no upstream assignment evidence",
            });
        }
        if self.assignment_role != AssignmentRole::ProductResearch
            && self.assignment_evidence.is_empty()
        {
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
/// normalized usage; the kernel verifies packet identity and accepts only a
/// complete provider-reported terminal cost for accounting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalReportV2 {
    pub packet_digest: ContentDigest,
    pub expected_session_revision: ExpectedRevision,
    /// Infrastructure shutdowns have no actor terminal operation. A supplied
    /// operation is checked against the assignment allowlist only for a
    /// successful actor submission.
    pub operation: Option<TerminalOperationV2>,
    pub stop_reason: StopReasonV2,
    pub report_digest: ContentDigest,
}

/// The only process state persisted by the kernel. PID and PGID are custody
/// evidence, never a permission supplied by an actor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessCustodyV2 {
    pub pid: u32,
    pub pgid: u32,
    pub started_at_unix_millis: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_cost_wins_over_token_usage() {
        let usage = UsageTotalsV2 {
            input_tokens: 1,
            output_tokens: 1_000_001,
            cache_read_tokens: 1,
            cache_write_tokens: 1,
            reasoning_tokens: None,
            reported_cost_micro_usd: Some(MicroUsd::new(1)),
            ..UsageTotalsV2::default()
        };
        assert_eq!(usage.provider_cost(), Ok(MicroUsd::new(1)));
    }

    #[test]
    fn absent_provider_cost_is_unknown_not_zero() {
        let usage = UsageTotalsV2::default();
        assert!(usage.provider_cost().is_err());
    }

    #[test]
    fn complete_provider_zero_cost_is_known_free_usage() {
        let usage = UsageTotalsV2 {
            reported_cost_micro_usd: Some(MicroUsd::new(0)),
            ..UsageTotalsV2::default()
        };
        assert_eq!(usage.provider_cost(), Ok(MicroUsd::new(0)));
    }
}
