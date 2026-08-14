//! Closed, reproducible harness compilation values.
//!
//! A harness is a materialized computation over admitted policy and durable
//! references. It is neither agent memory nor a generic prompt extension
//! mechanism: its context is an ordered, bounded collection of typed durable
//! references with a named inclusion class.

use std::collections::BTreeSet;

use crate::{
    ApplicationRevisionId, ArtifactId, AssignmentId, AssignmentRole, ClaimId, ContentDigest,
    ContractError, DecisionId, ExperimentId, MicroUsd, OfficeId, ProjectId, RfcId, RfcRevisionId,
    TicketId, TicketRevisionId,
};

pub const HARNESS_COMPILER_VERSION_V2: u16 = 2;
pub const HARNESS_CONTEXT_REASON_MAX_BYTES: usize = 512;
pub const HARNESS_CONTEXT_MAX_ITEMS: usize = 32;

/// Why the compiler selected a context reference. New inclusion behavior
/// requires a closed protocol change rather than a retrieval score or map.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ContextInclusionClassV2 {
    DirectTarget,
    RequiredConstraint,
    DirectEvidence,
    CurrentDecision,
}

/// A context reference is one immutable artifact or one typed institutional
/// identity. There is no raw text and no generic `kind + id` pair here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ContextReferenceV2 {
    Artifact(ArtifactId),
    Project(ProjectId),
    Rfc(RfcId),
    RfcRevision(RfcRevisionId),
    Ticket(TicketId),
    TicketRevision(TicketRevisionId),
    Experiment(ExperimentId),
    Claim(ClaimId),
    Decision(DecisionId),
    Office(OfficeId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextItemV2 {
    pub reference: ContextReferenceV2,
    pub inclusion: ContextInclusionClassV2,
    pub reason: String,
}

impl ContextItemV2 {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.reason.is_empty()
            || self.reason.len() > HARNESS_CONTEXT_REASON_MAX_BYTES
            || self.reason.contains('\0')
            || self.reason.contains('\n')
            || self.reason.contains('\r')
        {
            return Err(ContractError::InvalidValue {
                field: "harness context reason",
                reason: "must be one bounded nonempty line without NUL",
            });
        }
        Ok(())
    }
}

/// The complete, deterministic input contract for one compiled harness. The
/// rendered prompts and packet are outputs, not fields actors can alter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HarnessSpecV2 {
    pub compiler_version: u16,
    pub application_revision_id: ApplicationRevisionId,
    pub office_id: OfficeId,
    pub assignment_role: AssignmentRole,
    pub objective: String,
    pub context_items: Vec<ContextItemV2>,
    pub capabilities: Vec<crate::ActorToolV2>,
    pub remaining_campaign_allowance: MicroUsd,
}

impl HarnessSpecV2 {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.compiler_version != HARNESS_COMPILER_VERSION_V2 {
            return Err(ContractError::InvalidValue {
                field: "harness compiler version",
                reason: "is not supported",
            });
        }
        if self.objective.is_empty() || self.objective.len() > 4096 || self.objective.contains('\0')
        {
            return Err(ContractError::InvalidValue {
                field: "harness objective",
                reason: "must be bounded nonempty UTF-8 without NUL",
            });
        }
        if self.context_items.len() > HARNESS_CONTEXT_MAX_ITEMS {
            return Err(ContractError::InvalidValue {
                field: "harness context items",
                reason: "exceeds the fixed context reference limit",
            });
        }
        let mut references = BTreeSet::new();
        for item in &self.context_items {
            item.validate()?;
            if !references.insert(item.reference) {
                return Err(ContractError::InvalidValue {
                    field: "harness context items",
                    reason: "must not repeat one durable reference",
                });
            }
        }
        if self.capabilities.is_empty() {
            return Err(ContractError::InvalidValue {
                field: "harness capabilities",
                reason: "must contain at least one admitted tool",
            });
        }
        let mut tools = BTreeSet::new();
        if self.capabilities.iter().any(|tool| !tools.insert(*tool)) {
            return Err(ContractError::InvalidValue {
                field: "harness capabilities",
                reason: "must not repeat an admitted tool",
            });
        }
        Ok(())
    }
}

/// Immutable receipt of a stored harness compilation. Its artifact IDs point
/// to kernel-sealed canonical inputs/outputs and its digest identifies the
/// resulting actor packet exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HarnessCompilationV2 {
    pub id: crate::HarnessCompilationId,
    pub assignment_id: AssignmentId,
    pub application_revision_id: ApplicationRevisionId,
    pub office_id: OfficeId,
    pub assignment_role: AssignmentRole,
    pub compiler_version: u16,
    pub spec_artifact_id: ArtifactId,
    pub system_prompt_artifact_id: ArtifactId,
    pub assignment_prompt_artifact_id: ArtifactId,
    pub packet_artifact_id: ArtifactId,
    pub packet_digest: ContentDigest,
    /// Ordered operator-facing explanation of the durable references selected
    /// by this compiler invocation.
    pub context_items: Vec<ContextItemV2>,
}
