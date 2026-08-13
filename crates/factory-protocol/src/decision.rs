//! External Grand Architect decisions.
//!
//! These values deliberately have no actor-tool representation. They travel
//! only on the operator connection and are checked by the kernel against the
//! exact ticket, attempt, candidate, review, and hard-validation state.

use crate::{
    ArchitectDecisionId, CandidateId, ContractError, ReviewId, SealedArtifactReferenceV1,
    TicketAttemptId, TicketRevisionId,
};

pub const ARCHITECT_PRINCIPAL_BYTE_LIMIT: usize = 240;
pub const ARCHITECT_RATIONALE_BYTE_LIMIT: u64 = 128 * 1024;

/// The immutable kind persisted for an accepted external decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArchitectDecisionKindV1 {
    Sponsor,
    Release,
    Deliver,
    Rework,
    Reject,
}

/// The only final candidate choices. A `Deliver` decision following a Quality
/// rejection must carry `quality_rejection_override`; hard failures are never
/// represented as an overridable decision input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CandidateDecisionV1 {
    Deliver,
    Rework,
    Reject,
}

impl CandidateDecisionV1 {
    #[must_use]
    pub const fn kind(self) -> ArchitectDecisionKindV1 {
        match self {
            Self::Deliver => ArchitectDecisionKindV1::Deliver,
            Self::Rework => ArchitectDecisionKindV1::Rework,
            Self::Reject => ArchitectDecisionKindV1::Reject,
        }
    }
}

/// A visible, bounded attribution for an external Grand Architect action.
/// It records provenance; the operator socket, not this string, is the
/// authority boundary that keeps actors from issuing these commands.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct ArchitectPrincipalV1(String);

impl ArchitectPrincipalV1 {
    pub fn parse(value: impl Into<String>) -> Result<Self, ContractError> {
        let value = value.into();
        if value.is_empty() || value.len() > ARCHITECT_PRINCIPAL_BYTE_LIMIT || value.contains('\0')
        {
            return Err(ContractError::InvalidValue {
                field: "Architect principal",
                reason: "must be nonempty, bounded UTF-8 without NUL",
            });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// External sponsorship of an immutable Product ticket revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SponsorshipDecisionV1 {
    pub ticket_revision_id: TicketRevisionId,
    pub rationale: SealedArtifactReferenceV1,
    pub principal: ArchitectPrincipalV1,
}

impl SponsorshipDecisionV1 {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.rationale.validate(
            "Architect sponsorship rationale",
            ARCHITECT_RATIONALE_BYTE_LIMIT,
            false,
        )
    }
}

/// An explicit release after a failed attempt. The kernel separately requires
/// current-head requalification; this is not an automatic retry request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseDecisionV1 {
    pub ticket_attempt_id: TicketAttemptId,
    pub rationale: SealedArtifactReferenceV1,
    pub principal: ArchitectPrincipalV1,
}

impl ReleaseDecisionV1 {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.rationale.validate(
            "Architect release rationale",
            ARCHITECT_RATIONALE_BYTE_LIMIT,
            false,
        )
    }
}

/// A final decision over one exact independently reviewed candidate.
///
/// `quality_rejection_override` is a relation, not a boolean escape hatch:
/// the kernel proves it names this candidate's rejected Quality review. It is
/// legal only for `Deliver`; it cannot waive a missing/failed hard validation,
/// candidate mismatch, cost stop, dirty checkout, or delivery guard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateDecisionRequestV1 {
    pub candidate_id: CandidateId,
    pub review_id: ReviewId,
    pub decision: CandidateDecisionV1,
    pub rationale: SealedArtifactReferenceV1,
    pub quality_rejection_override: Option<ReviewId>,
    pub principal: ArchitectPrincipalV1,
}

impl CandidateDecisionRequestV1 {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.rationale.validate(
            "Architect candidate-decision rationale",
            ARCHITECT_RATIONALE_BYTE_LIMIT,
            false,
        )?;
        if self.quality_rejection_override.is_some()
            && self.decision != CandidateDecisionV1::Deliver
        {
            return Err(ContractError::InvalidValue {
                field: "Quality rejection override",
                reason: "is only valid for a deliver decision",
            });
        }
        Ok(())
    }
}

/// The compact receipt returned after one immutable Architect decision is
/// stored. More detailed decisions are read-only status/audit views.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchitectDecisionReceiptV1 {
    pub architect_decision_id: ArchitectDecisionId,
    pub kind: ArchitectDecisionKindV1,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ArtifactId, ContentDigest};

    fn rationale() -> SealedArtifactReferenceV1 {
        SealedArtifactReferenceV1 {
            artifact_id: ArtifactId::new(1).unwrap(),
            digest: ContentDigest::from_bytes([1; 32]),
            byte_length: 10,
        }
    }

    #[test]
    fn only_delivery_can_link_a_quality_rejection_override() {
        let request = CandidateDecisionRequestV1 {
            candidate_id: CandidateId::new(1).unwrap(),
            review_id: ReviewId::new(2).unwrap(),
            decision: CandidateDecisionV1::Deliver,
            rationale: rationale(),
            quality_rejection_override: Some(ReviewId::new(2).unwrap()),
            principal: ArchitectPrincipalV1::parse("grand-architect").unwrap(),
        };
        assert!(request.validate().is_ok());

        let invalid = CandidateDecisionRequestV1 {
            decision: CandidateDecisionV1::Rework,
            ..request
        };
        assert!(invalid.validate().is_err());
    }
}
