//! Closed, product-neutral Product proposal and reproducer contracts.
//!
//! A Product actor may describe a user-visible defect, but it cannot turn a
//! narrative into a ticket merely by persuasion.  The large inputs are sealed
//! artifacts, while the few fields needed for authority, buffer admission,
//! duplicate lookup, and reproducibility remain explicit and bounded here.

use std::{collections::BTreeSet, str::FromStr};

use crate::{ArtifactId, ContentDigest, ContractError, RepositoryRelativePath, TicketBoundsV1};

/// The only comparison rule currently understood for a ticket reproducer.
/// A future comparison needs a named protocol revision; it cannot be hidden
/// in command output or application policy prose.
pub const EXACT_OBSERVATION_COMPARISON_V1: u16 = 1;

pub const PRODUCT_TICKET_TITLE_BYTE_LIMIT: usize = 240;
pub const PRODUCT_TICKET_MISSION_VALUE_BYTE_LIMIT: usize = 4096;
pub const PRODUCT_TICKET_SCOPE_BYTE_LIMIT: usize = 4096;
pub const PRODUCT_TICKET_CONTRACT_OWNER_BYTE_LIMIT: usize = 240;
pub const PRODUCT_TICKET_RISK_BYTE_LIMIT: usize = 4096;
pub const PRODUCT_TICKET_ACCEPTANCE_ITEM_BYTE_LIMIT: usize = 4096;
/// Ticket contract reads become exact assignment reads for downstream work,
/// whose closed packet permits at most 240 bytes of reason text. Admission
/// must reject a proposal that cannot later be materialized.
pub const PRODUCT_TICKET_CONTRACT_READ_REASON_BYTE_LIMIT: usize = 240;
pub const PRODUCT_TICKET_EVIDENCE_BYTE_LIMIT: u64 = 64 * 1024;
pub const PRODUCT_REPRODUCER_COMMAND_BYTE_LIMIT: u64 = 64 * 1024;
pub const PRODUCT_REPRODUCER_STDIN_BYTE_LIMIT: u64 = 256 * 1024;
pub const PRODUCT_REPRODUCER_STREAM_BYTE_LIMIT: u64 = 4 * 1024 * 1024;
pub const PRODUCT_REPRODUCER_PROFILE_BYTE_LIMIT: usize = 160;
pub const PRODUCT_DUPLICATE_SEARCH_QUERY_BYTE_LIMIT: usize = 4096;
pub const PRODUCT_DUPLICATE_SEARCH_LIMIT_MAXIMUM: u8 = 20;

/// A reference to an immutable kernel-adopted artifact. The byte length is
/// repeated in the proposal so a protocol implementation can reject an
/// over-bound reference before reading it; the kernel independently checks it
/// against the artifacts relation before accepting any transition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedArtifactReferenceV1 {
    pub artifact_id: ArtifactId,
    pub digest: ContentDigest,
    pub byte_length: u64,
}

impl SealedArtifactReferenceV1 {
    pub fn validate(
        &self,
        field: &'static str,
        maximum: u64,
        allow_empty: bool,
    ) -> Result<(), ContractError> {
        if self.byte_length > maximum {
            return Err(ContractError::ByteLimitExceeded {
                field,
                maximum: usize::try_from(maximum).unwrap_or(usize::MAX),
            });
        }
        if !allow_empty && self.byte_length == 0 {
            return Err(ContractError::InvalidValue {
                field,
                reason: "must not be empty",
            });
        }
        Ok(())
    }
}

/// The exact observable outcome of one command run. Output streams are sealed
/// separately: neither unbounded stdout nor stderr crosses the local JSON
/// protocol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandObservationV1 {
    pub exit_status: i32,
    pub stdout: SealedArtifactReferenceV1,
    pub stderr: SealedArtifactReferenceV1,
}

impl CommandObservationV1 {
    fn validate(&self, field: &'static str) -> Result<(), ContractError> {
        self.stdout
            .validate(field, PRODUCT_REPRODUCER_STREAM_BYTE_LIMIT, true)?;
        self.stderr
            .validate(field, PRODUCT_REPRODUCER_STREAM_BYTE_LIMIT, true)
    }

    /// Artifact IDs are custody details; equivalence is the exact output
    /// identity and status, so independently adopted identical bytes compare
    /// equal even if an implementation allocated different IDs.
    #[must_use]
    pub fn same_outcome_as(&self, other: &Self) -> bool {
        self.exit_status == other.exit_status
            && self.stdout.digest == other.stdout.digest
            && self.stdout.byte_length == other.stdout.byte_length
            && self.stderr.digest == other.stderr.digest
            && self.stderr.byte_length == other.stderr.byte_length
    }
}

/// A sealed process contract and exactly two observations at one discovery
/// base. The bytes identified by `command` are a closed, versioned command
/// specification adopted by the kernel; Product cannot replace it with shell
/// interpolation after proposing the ticket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TwoRunReproducerV1 {
    pub comparison_rule_version: u16,
    pub command: SealedArtifactReferenceV1,
    /// Optional exact standard input. This is a sealed artifact rather than
    /// inline JSON so a reproducer can carry a bounded source fixture without
    /// dirtying the discovery checkout or introducing shell interpolation.
    pub stdin: Option<SealedArtifactReferenceV1>,
    pub expected_observation: CommandObservationV1,
    pub first_observation: CommandObservationV1,
    pub second_observation: CommandObservationV1,
}

impl TwoRunReproducerV1 {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.comparison_rule_version != EXACT_OBSERVATION_COMPARISON_V1 {
            return Err(ContractError::InvalidValue {
                field: "reproducer comparison rule version",
                reason: "is not the exact-observation V1 rule",
            });
        }
        self.command.validate(
            "reproducer command",
            PRODUCT_REPRODUCER_COMMAND_BYTE_LIMIT,
            false,
        )?;
        if let Some(stdin) = &self.stdin {
            stdin.validate(
                "reproducer stdin",
                PRODUCT_REPRODUCER_STDIN_BYTE_LIMIT,
                false,
            )?;
        }
        self.expected_observation
            .validate("expected reproducer observation")?;
        self.first_observation
            .validate("first reproducer observation")?;
        self.second_observation
            .validate("second reproducer observation")?;

        if !self
            .first_observation
            .same_outcome_as(&self.second_observation)
        {
            return Err(ContractError::InvalidValue {
                field: "reproducer observations",
                reason: "the two discovery runs do not match",
            });
        }
        if self
            .first_observation
            .same_outcome_as(&self.expected_observation)
        {
            return Err(ContractError::InvalidValue {
                field: "reproducer observations",
                reason: "the observed failure already matches the expected behavior",
            });
        }
        Ok(())
    }
}

/// A duplicate lookup that the kernel executes against the live ticket buffer
/// while admitting a proposal. Product supplies a bounded search expression;
/// it never supplies a trusted statement that no duplicate exists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DuplicateSearchInputV1 {
    pub query: String,
    pub limit: u8,
}

impl DuplicateSearchInputV1 {
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_text(
            "duplicate search query",
            &self.query,
            PRODUCT_DUPLICATE_SEARCH_QUERY_BYTE_LIMIT,
        )?;
        if self.limit == 0 || self.limit > PRODUCT_DUPLICATE_SEARCH_LIMIT_MAXIMUM {
            return Err(ContractError::InvalidValue {
                field: "duplicate search limit",
                reason: "must be between 1 and 20",
            });
        }
        Ok(())
    }
}

/// A product-contract file that Product believes constrains the defect. These
/// are proposal evidence, distinct from an assignment's required-read proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TicketContractReadV1 {
    pub path: RepositoryRelativePath,
    pub reason: String,
}

impl TicketContractReadV1 {
    fn validate(&self) -> Result<(), ContractError> {
        validate_text(
            "ticket contract read reason",
            &self.reason,
            PRODUCT_TICKET_CONTRACT_READ_REASON_BYTE_LIMIT,
        )
    }
}

/// The complete generic Product submission. It intentionally has no product
/// taxonomy, application metadata, actor identity, or sponsorship field.
/// Sponsorship is an external Architect transition, not an actor capability.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProductTicketProposalV1 {
    pub title: String,
    pub mission_value: String,
    pub scope: String,
    pub contract_owner: String,
    pub risk: String,
    pub narrative: SealedArtifactReferenceV1,
    pub evidence: SealedArtifactReferenceV1,
    pub acceptance_criteria: Vec<String>,
    pub contract_reads: Vec<TicketContractReadV1>,
    pub duplicate_search: DuplicateSearchInputV1,
    pub reproducer_profile: String,
    pub reproducer: TwoRunReproducerV1,
}

impl ProductTicketProposalV1 {
    /// Validates the generic proposal shape plus the exact bounds pinned in
    /// the selected application revision.
    pub fn validate(&self, bounds: &TicketBoundsV1) -> Result<(), ContractError> {
        validate_text("ticket title", &self.title, PRODUCT_TICKET_TITLE_BYTE_LIMIT)?;
        validate_text(
            "ticket mission value",
            &self.mission_value,
            PRODUCT_TICKET_MISSION_VALUE_BYTE_LIMIT,
        )?;
        validate_text("ticket scope", &self.scope, PRODUCT_TICKET_SCOPE_BYTE_LIMIT)?;
        validate_text(
            "ticket contract owner",
            &self.contract_owner,
            PRODUCT_TICKET_CONTRACT_OWNER_BYTE_LIMIT,
        )?;
        validate_text("ticket risk", &self.risk, PRODUCT_TICKET_RISK_BYTE_LIMIT)?;
        self.narrative.validate(
            "ticket narrative",
            u64::from(bounds.narrative_byte_limit),
            false,
        )?;
        self.evidence
            .validate("ticket evidence", PRODUCT_TICKET_EVIDENCE_BYTE_LIMIT, false)?;

        if self.acceptance_criteria.is_empty()
            || self.acceptance_criteria.len() > usize::from(bounds.acceptance_criteria_limit)
        {
            return Err(ContractError::InvalidValue {
                field: "ticket acceptance criteria",
                reason: "count is outside the application bound",
            });
        }
        for criterion in &self.acceptance_criteria {
            validate_text(
                "ticket acceptance criterion",
                criterion,
                PRODUCT_TICKET_ACCEPTANCE_ITEM_BYTE_LIMIT,
            )?;
        }

        if self.contract_reads.is_empty()
            || self.contract_reads.len() > usize::from(bounds.contract_read_limit)
        {
            return Err(ContractError::InvalidValue {
                field: "ticket contract reads",
                reason: "count is outside the application bound",
            });
        }
        let mut paths = BTreeSet::new();
        for read in &self.contract_reads {
            read.validate()?;
            if !paths.insert(read.path.as_str()) {
                return Err(ContractError::InvalidValue {
                    field: "ticket contract reads",
                    reason: "paths must be unique",
                });
            }
        }

        self.duplicate_search.validate()?;
        validate_text(
            "reproducer profile",
            &self.reproducer_profile,
            PRODUCT_REPRODUCER_PROFILE_BYTE_LIMIT,
        )?;
        self.reproducer.validate()
    }
}

/// Converts a text-boundary artifact reference into the typed contract. This
/// lives here so every caller shares the same positive-ID and digest rules.
pub fn sealed_artifact_reference_v1(
    artifact_id: i64,
    digest: &str,
    byte_length: u64,
) -> Result<SealedArtifactReferenceV1, ContractError> {
    Ok(SealedArtifactReferenceV1 {
        artifact_id: ArtifactId::new(artifact_id)?,
        digest: ContentDigest::from_str(digest)?,
        byte_length,
    })
}

fn validate_text(field: &'static str, value: &str, maximum: usize) -> Result<(), ContractError> {
    if value.is_empty() || value.as_bytes().len() > maximum || value.contains('\0') {
        return Err(ContractError::InvalidValue {
            field,
            reason: "must be nonempty, bounded UTF-8 without NUL",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(id: i64, byte: u8, byte_length: u64) -> SealedArtifactReferenceV1 {
        SealedArtifactReferenceV1 {
            artifact_id: ArtifactId::new(id).unwrap(),
            digest: ContentDigest::from_bytes([byte; 32]),
            byte_length,
        }
    }

    fn observation(id: i64, status: i32, byte: u8) -> CommandObservationV1 {
        CommandObservationV1 {
            exit_status: status,
            stdout: artifact(id, byte, 1),
            stderr: artifact(id + 1, byte.wrapping_add(1), 1),
        }
    }

    fn proposal() -> ProductTicketProposalV1 {
        ProductTicketProposalV1 {
            title: "observable failure".to_owned(),
            mission_value: "users receive a correct result".to_owned(),
            scope: "public command behavior".to_owned(),
            contract_owner: "docs/contract.md".to_owned(),
            risk: "compatibility".to_owned(),
            narrative: artifact(1, 1, 32),
            evidence: artifact(2, 2, 32),
            acceptance_criteria: vec!["the public result is correct".to_owned()],
            contract_reads: vec![TicketContractReadV1 {
                path: RepositoryRelativePath::parse("docs/contract.md").unwrap(),
                reason: "defines the public behavior".to_owned(),
            }],
            duplicate_search: DuplicateSearchInputV1 {
                query: "observable failure".to_owned(),
                limit: 20,
            },
            reproducer_profile: "reproducer".to_owned(),
            reproducer: TwoRunReproducerV1 {
                comparison_rule_version: EXACT_OBSERVATION_COMPARISON_V1,
                command: artifact(3, 3, 16),
                stdin: Some(artifact(10, 10, 32)),
                expected_observation: observation(4, 0, 4),
                first_observation: observation(6, 1, 6),
                second_observation: observation(8, 1, 6),
            },
        }
    }

    fn bounds() -> TicketBoundsV1 {
        TicketBoundsV1 {
            narrative_byte_limit: 32,
            acceptance_criteria_limit: 1,
            contract_read_limit: 1,
        }
    }

    #[test]
    fn proposal_requires_two_equal_failing_observations() {
        assert_eq!(proposal().validate(&bounds()), Ok(()));

        let mut divergent = proposal();
        divergent.reproducer.second_observation.exit_status = 2;
        assert!(divergent.validate(&bounds()).is_err());

        let mut passing = proposal();
        passing.reproducer.first_observation = passing.reproducer.expected_observation.clone();
        passing.reproducer.second_observation = passing.reproducer.expected_observation.clone();
        assert!(passing.validate(&bounds()).is_err());
    }

    #[test]
    fn proposal_enforces_application_bounds_and_duplicate_input() {
        let mut oversized = proposal();
        oversized.narrative.byte_length = 33;
        assert!(oversized.validate(&bounds()).is_err());

        let mut too_many_reads = proposal();
        too_many_reads.contract_reads.push(TicketContractReadV1 {
            path: RepositoryRelativePath::parse("docs/other.md").unwrap(),
            reason: "another contract".to_owned(),
        });
        assert!(too_many_reads.validate(&bounds()).is_err());

        let mut invalid_search = proposal();
        invalid_search.duplicate_search.limit = 0;
        assert!(invalid_search.validate(&bounds()).is_err());

        let mut materialization_safe_reason = proposal();
        materialization_safe_reason.contract_reads[0].reason = "x".repeat(241);
        assert!(materialization_safe_reason.validate(&bounds()).is_err());
    }
}
