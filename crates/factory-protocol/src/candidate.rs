//! Closed candidate and independent Quality-review contracts.
//!
//! The Engineering actor can submit only bounded text and sealed report
//! artifacts. The kernel captures every tree, patch, validation, and commit
//! identity itself, then exposes one immutable [`CandidatePacketV1`] to the
//! fresh Quality assignment. Quality prose is likewise sealed before the
//! review becomes durable. Neither office can turn a narrative into a hard
//! validation pass or a delivery decision.

use crate::{
    AggregateRevision, CandidateId, ContractError, ReviewVerdict, SealedArtifactReferenceV1,
    SessionId, TicketAttemptId, TicketRevisionId, ValidationId,
};

pub const CANDIDATE_COMMIT_SUBJECT_BYTE_LIMIT: usize = 120;
pub const CANDIDATE_COMMIT_BODY_BYTE_LIMIT: usize = 8 * 1024;
pub const CANDIDATE_REGRESSION_IDENTITY_BYTE_LIMIT: usize = 4 * 1024;
pub const CANDIDATE_REPORT_BYTE_LIMIT: u64 = 128 * 1024;
pub const CANDIDATE_RISKS_BYTE_LIMIT: u64 = 64 * 1024;
pub const QUALITY_RATIONALE_BYTE_LIMIT: u64 = 128 * 1024;
pub const QUALITY_RISKS_BYTE_LIMIT: u64 = 64 * 1024;
pub const QUALITY_PROBES_BYTE_LIMIT: u64 = 128 * 1024;
pub const QUALITY_VALIDATION_PROFILE_BYTE_LIMIT: usize = 160;

/// A Product-repository object identifier. Git may use SHA-1 or SHA-256; the
/// wire therefore admits precisely lower-case 40- or 64-hex identities rather
/// than pretending they are Factory content digests.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct RepositoryObjectIdV1(String);

impl RepositoryObjectIdV1 {
    pub fn parse(value: impl Into<String>) -> Result<Self, ContractError> {
        let value = value.into();
        if !matches!(value.len(), 40 | 64)
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(ContractError::InvalidValue {
                field: "repository object identity",
                reason: "must be a lower-case 40- or 64-hex object ID",
            });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The non-actor-owned candidate evidence handed to a Quality assignment.
/// It contains identities and sealed artifacts only; the Quality worktree is
/// independently materialized from `candidate_tree` by the kernel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidatePacketV1 {
    pub candidate_id: CandidateId,
    pub ticket_attempt_id: TicketAttemptId,
    pub ticket_revision_id: TicketRevisionId,
    pub base_commit: RepositoryObjectIdV1,
    pub base_tree: RepositoryObjectIdV1,
    pub regression_tree: RepositoryObjectIdV1,
    pub candidate_tree: RepositoryObjectIdV1,
    /// The portable binary patch proving the accepted pre-fix regression tree.
    pub regression_patch: SealedArtifactReferenceV1,
    /// Kernel-owned targeted-command set and complete failure receipt for the
    /// accepted regression checkpoint.
    pub regression_command_set: SealedArtifactReferenceV1,
    pub regression_log: SealedArtifactReferenceV1,
    pub candidate_patch: SealedArtifactReferenceV1,
    pub engineering_session_id: SessionId,
    pub engineering_report: SealedArtifactReferenceV1,
    pub hard_validation_id: ValidationId,
    pub candidate_commit: RepositoryObjectIdV1,
    pub candidate_revision: AggregateRevision,
}

impl CandidatePacketV1 {
    /// Checks the self-contained packet shape. The kernel separately proves
    /// that the patch reconstructs the tree and the hard validation belongs
    /// to that same exact candidate before it issues this packet.
    pub fn validate(&self) -> Result<(), ContractError> {
        self.regression_patch
            .validate("regression binary patch", 16 * 1024 * 1024, false)?;
        self.regression_command_set.validate(
            "regression checkpoint command set",
            256 * 1024,
            false,
        )?;
        self.regression_log
            .validate("regression checkpoint log", 16 * 1024 * 1024, true)?;
        self.candidate_patch
            .validate("candidate binary patch", 16 * 1024 * 1024, false)?;
        self.engineering_report
            .validate("Engineering report", CANDIDATE_REPORT_BYTE_LIMIT, false)
    }
}

/// The actor-visible Engineering terminal payload. Tree identities are not
/// accepted from the actor: the kernel captures them from the exact owned
/// worktree after this operation is submitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateSubmissionV1 {
    pub engineering_report: SealedArtifactReferenceV1,
    pub commit_subject: String,
    pub commit_body: String,
    pub regression_test_identity: String,
    pub risks: SealedArtifactReferenceV1,
}

impl CandidateSubmissionV1 {
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_text(
            "candidate commit subject",
            &self.commit_subject,
            CANDIDATE_COMMIT_SUBJECT_BYTE_LIMIT,
            false,
        )?;
        if self.commit_subject.contains(['\r', '\n']) {
            return Err(ContractError::InvalidValue {
                field: "candidate commit subject",
                reason: "must be exactly one line",
            });
        }
        validate_text(
            "candidate commit body",
            &self.commit_body,
            CANDIDATE_COMMIT_BODY_BYTE_LIMIT,
            true,
        )?;
        validate_text(
            "candidate regression test identity",
            &self.regression_test_identity,
            CANDIDATE_REGRESSION_IDENTITY_BYTE_LIMIT,
            false,
        )?;
        self.engineering_report.validate(
            "Engineering report",
            CANDIDATE_REPORT_BYTE_LIMIT,
            false,
        )?;
        self.risks
            .validate("candidate risks", CANDIDATE_RISKS_BYTE_LIMIT, false)
    }
}

/// A Quality-owned full-suite invocation. It is intentionally nonterminal:
/// Quality must inspect the returned validation receipt before it can submit
/// its one terminal review.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QualityFullSuiteRequestV1 {
    pub validation_profile: String,
}

impl QualityFullSuiteRequestV1 {
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_text(
            "Quality full-suite validation profile",
            &self.validation_profile,
            QUALITY_VALIDATION_PROFILE_BYTE_LIMIT,
            false,
        )
    }
}

/// The actor-visible Quality terminal payload. `full_suite_validation_id`
/// must name a separately kernel-run, passed validation on the exact
/// candidate; prose cannot waive that requirement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QualityReviewSubmissionV1 {
    pub full_suite_validation_id: ValidationId,
    pub verdict: ReviewVerdict,
    pub rationale: SealedArtifactReferenceV1,
    pub risks: SealedArtifactReferenceV1,
    pub additional_probes: SealedArtifactReferenceV1,
}

impl QualityReviewSubmissionV1 {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.rationale
            .validate("Quality rationale", QUALITY_RATIONALE_BYTE_LIMIT, false)?;
        self.risks
            .validate("Quality risks", QUALITY_RISKS_BYTE_LIMIT, false)?;
        self.additional_probes.validate(
            "Quality additional probes",
            QUALITY_PROBES_BYTE_LIMIT,
            false,
        )
    }
}

/// Exact evidence produced by the kernel-owned full-suite runner. A review
/// stores this identity rather than an actor's claim that it ran a command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QualityValidationReceiptV1 {
    pub validation_id: ValidationId,
    pub candidate_id: CandidateId,
    pub candidate_tree: RepositoryObjectIdV1,
    pub log_artifact: SealedArtifactReferenceV1,
    pub revision: AggregateRevision,
}

impl QualityValidationReceiptV1 {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.log_artifact
            .validate("Quality full-suite log", 16 * 1024 * 1024, true)
    }
}

/// Converts a wire artifact reference exactly once at the typed boundary.
pub fn candidate_artifact_reference_v1(
    artifact_id: i64,
    digest: &str,
    byte_length: u64,
) -> Result<SealedArtifactReferenceV1, ContractError> {
    crate::sealed_artifact_reference_v1(artifact_id, digest, byte_length)
}

fn validate_text(
    field: &'static str,
    value: &str,
    maximum: usize,
    allow_empty: bool,
) -> Result<(), ContractError> {
    if (!allow_empty && value.is_empty()) || value.len() > maximum || value.contains('\0') {
        return Err(ContractError::InvalidValue {
            field,
            reason: "must be bounded UTF-8 without NUL",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ArtifactId, ContentDigest};

    fn artifact(id: i64, length: u64) -> SealedArtifactReferenceV1 {
        SealedArtifactReferenceV1 {
            artifact_id: ArtifactId::new(id).unwrap(),
            digest: ContentDigest::from_bytes([id as u8; 32]),
            byte_length: length,
        }
    }

    #[test]
    fn candidate_submission_is_bounded_and_never_accepts_a_tree_from_an_actor() {
        let submission = CandidateSubmissionV1 {
            engineering_report: artifact(1, 4),
            commit_subject: "Fix visible behavior".into(),
            commit_body: String::new(),
            regression_test_identity: "cargo test regression".into(),
            risks: artifact(2, 4),
        };
        assert!(submission.validate().is_ok());

        let mut invalid = submission;
        invalid.commit_subject = "x\ntrailer: actor-controlled".into();
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn repository_object_ids_are_not_confused_with_blake3_artifact_digests() {
        assert!(RepositoryObjectIdV1::parse("a".repeat(40)).is_ok());
        assert!(RepositoryObjectIdV1::parse("A".repeat(40)).is_err());
        assert!(RepositoryObjectIdV1::parse("a".repeat(64)).is_ok());
    }
}
