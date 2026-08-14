//! Closed protocol values for Factory's institutional records.
//!
//! These records describe durable responsibility and inquiry.  They are not
//! workflow steps and they do not carry mutable prompt assembly.  Long-form
//! bodies are sealed artifacts; only bounded summaries and search fields are
//! represented inline so a database projection can remain small and useful.
//!
//! The kernel is responsible for proving that the IDs in these values belong
//! to the selected application revision and that links are legal.  This
//! module owns the closed vocabulary and the local shape/bound checks.  It
//! intentionally does not define a generic `kind + id` pair.

use crate::{
    AggregateRevision, ApplicationRevisionId, CandidateId, ClaimId, ContractError, DecisionId,
    ExperimentId, ExperimentRunId, OfficeId, ProjectId, PublicationId, RepositoryObjectIdV1, RfcId,
    RfcRevisionId, SealedArtifactReferenceV1, SessionId, TicketId, TicketRevisionId,
};

/// Inline fields are searchable projections, not substitutes for the sealed
/// body artifact.  These limits are protocol limits; an application may admit
/// smaller limits in its policy.
pub const INSTITUTIONAL_TITLE_MAX_BYTES: usize = 240;
pub const INSTITUTIONAL_SUMMARY_MAX_BYTES: usize = 4 * 1024;
pub const INSTITUTIONAL_PROPOSITION_MAX_BYTES: usize = 4 * 1024;
pub const INSTITUTIONAL_BODY_MAX_BYTES: u64 = 1024 * 1024;
pub const INSTITUTIONAL_EVALUATION_PLAN_MAX_BYTES: u64 = 256 * 1024;
pub const INSTITUTIONAL_INVOCATION_MAX_BYTES: u64 = 256 * 1024;
pub const INSTITUTIONAL_RECEIPT_MAX_BYTES: u64 = 256 * 1024;
pub const PUBLICATION_ATTACHMENT_LABEL_MAX_BYTES: usize = 160;
pub const PUBLICATION_MAX_ATTACHMENTS: usize = 8;

/// A project is a bounded area of institutional responsibility.  Its body is
/// immutable evidence; changing the searchable fields is an aggregate
/// transition and never an in-place rewrite of that body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    pub id: ProjectId,
    pub application_revision_id: ApplicationRevisionId,
    pub owner_office_id: OfficeId,
    pub title: String,
    pub summary: String,
    pub body: SealedArtifactReferenceV1,
    pub state: ProjectState,
    pub aggregate_revision: AggregateRevision,
}

impl Project {
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_text(
            "project title",
            &self.title,
            INSTITUTIONAL_TITLE_MAX_BYTES,
            false,
        )?;
        validate_text(
            "project summary",
            &self.summary,
            INSTITUTIONAL_SUMMARY_MAX_BYTES,
            false,
        )?;
        validate_body(&self.body, "project body")
    }
}

/// Project lifecycle is deliberately closed.  A new state requires a named
/// transition and an authority decision rather than a new database string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProjectState {
    Proposed,
    Active,
    Paused,
    Completed,
    Archived,
}

/// An RFC is the stable identity for a proposal.  Its immutable revisions
/// carry the historical proposal bodies; `current_revision_id` is only the
/// kernel-maintained pointer to the selected revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rfc {
    pub id: RfcId,
    pub application_revision_id: ApplicationRevisionId,
    pub owner_office_id: OfficeId,
    pub project_id: Option<ProjectId>,
    pub title: String,
    pub summary: String,
    pub state: RfcState,
    pub current_revision_id: Option<RfcRevisionId>,
    pub aggregate_revision: AggregateRevision,
}

impl Rfc {
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_text(
            "RFC title",
            &self.title,
            INSTITUTIONAL_TITLE_MAX_BYTES,
            false,
        )?;
        validate_text(
            "RFC summary",
            &self.summary,
            INSTITUTIONAL_SUMMARY_MAX_BYTES,
            false,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RfcState {
    Draft,
    Proposed,
    Accepted,
    Rejected,
    Superseded,
    Archived,
}

/// One immutable RFC body.  The revision number is scoped to its parent RFC
/// and must be positive; SQL additionally enforces uniqueness per RFC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RfcRevision {
    pub id: RfcRevisionId,
    pub rfc_id: RfcId,
    pub application_revision_id: ApplicationRevisionId,
    pub author_office_id: OfficeId,
    pub revision_number: u64,
    pub summary: String,
    pub body: SealedArtifactReferenceV1,
}

impl RfcRevision {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.revision_number == 0 {
            return Err(ContractError::InvalidValue {
                field: "RFC revision number",
                reason: "must be greater than zero",
            });
        }
        validate_text(
            "RFC revision summary",
            &self.summary,
            INSTITUTIONAL_SUMMARY_MAX_BYTES,
            false,
        )?;
        validate_body(&self.body, "RFC revision body")
    }
}

/// An experiment records a bounded question and its intended evaluation plan.
/// It does not schedule or execute anything; those are kernel runtime facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Experiment {
    pub id: ExperimentId,
    pub application_revision_id: ApplicationRevisionId,
    pub owner_office_id: OfficeId,
    pub project_id: Option<ProjectId>,
    pub question: String,
    pub summary: String,
    pub intended_base: Option<RepositoryObjectIdV1>,
    pub intended_target: InstitutionalReference,
    pub evaluation_plan: SealedArtifactReferenceV1,
    pub budget_micro_usd: u64,
    pub state: ExperimentState,
    pub aggregate_revision: AggregateRevision,
}

impl Experiment {
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_text(
            "experiment question",
            &self.question,
            INSTITUTIONAL_PROPOSITION_MAX_BYTES,
            false,
        )?;
        validate_text(
            "experiment summary",
            &self.summary,
            INSTITUTIONAL_SUMMARY_MAX_BYTES,
            false,
        )?;
        if !matches!(
            self.intended_target,
            InstitutionalReference::Claim(_) | InstitutionalReference::RfcRevision(_)
        ) {
            return Err(ContractError::InvalidValue {
                field: "experiment target",
                reason: "must be one claim or RFC revision",
            });
        }
        if self.budget_micro_usd == 0 {
            return Err(ContractError::InvalidValue {
                field: "experiment budget",
                reason: "must be greater than zero",
            });
        }
        self.evaluation_plan.validate(
            "experiment evaluation plan",
            INSTITUTIONAL_EVALUATION_PLAN_MAX_BYTES,
            false,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExperimentState {
    Proposed,
    Ready,
    Running,
    Completed,
    Failed,
    Cancelled,
    Archived,
}

/// One exact execution of an experiment.  The base commit/tree and invocation
/// are captured facts.  Optional result references are populated only when a
/// run actually produces the corresponding kernel-owned evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExperimentRun {
    pub id: ExperimentRunId,
    pub experiment_id: ExperimentId,
    pub application_revision_id: ApplicationRevisionId,
    pub owner_office_id: OfficeId,
    pub base_commit: RepositoryObjectIdV1,
    pub base_tree: RepositoryObjectIdV1,
    pub invocation: SealedArtifactReferenceV1,
    pub candidate_id: Option<CandidateId>,
    pub result_artifact: Option<SealedArtifactReferenceV1>,
    pub evaluator_receipt: Option<SealedArtifactReferenceV1>,
    pub state: ExperimentRunState,
    pub aggregate_revision: AggregateRevision,
}

impl ExperimentRun {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.invocation.validate(
            "experiment run invocation",
            INSTITUTIONAL_INVOCATION_MAX_BYTES,
            false,
        )?;
        if let Some(result) = &self.result_artifact {
            result.validate("experiment run result", INSTITUTIONAL_BODY_MAX_BYTES, true)?;
        }
        if let Some(receipt) = &self.evaluator_receipt {
            receipt.validate(
                "experiment evaluator receipt",
                INSTITUTIONAL_RECEIPT_MAX_BYTES,
                false,
            )?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExperimentRunState {
    Prepared,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

/// A claim is one immutable bounded proposition.  Support and challenge are
/// explicit links to evidence in the institutional graph, not mutable prose
/// appended to this record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    pub id: ClaimId,
    pub application_revision_id: ApplicationRevisionId,
    pub owner_office_id: OfficeId,
    pub proposition: String,
    pub body: SealedArtifactReferenceV1,
    pub state: ClaimState,
    pub aggregate_revision: AggregateRevision,
}

impl Claim {
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_text(
            "claim proposition",
            &self.proposition,
            INSTITUTIONAL_PROPOSITION_MAX_BYTES,
            false,
        )?;
        validate_body(&self.body, "claim body")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ClaimState {
    Proposed,
    Supported,
    Challenged,
    Retracted,
}

/// The closed set of dispositions represented by a durable Decision.  A
/// decision's target is a typed reference and its legal target kinds are
/// enforced by the kernel's edge constraints.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DecisionKind {
    Approve,
    Reject,
    Defer,
    Supersede,
}

/// Decisions are immutable records. A later disposition supersedes its
/// predecessor instead of rewriting it; this is only a small lifecycle
/// projection and never mutable decision prose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DecisionState {
    Proposed,
    Final,
    Superseded,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub id: DecisionId,
    pub application_revision_id: ApplicationRevisionId,
    pub deciding_office_id: OfficeId,
    pub title: String,
    pub summary: String,
    pub target: InstitutionalReference,
    pub kind: DecisionKind,
    pub state: DecisionState,
    pub rationale: SealedArtifactReferenceV1,
    pub aggregate_revision: AggregateRevision,
}

impl Decision {
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_text(
            "decision title",
            &self.title,
            INSTITUTIONAL_TITLE_MAX_BYTES,
            false,
        )?;
        validate_text(
            "decision summary",
            &self.summary,
            INSTITUTIONAL_SUMMARY_MAX_BYTES,
            false,
        )?;
        if !matches!(
            self.target,
            InstitutionalReference::RfcRevision(_)
                | InstitutionalReference::TicketRevision(_)
                | InstitutionalReference::Experiment(_)
        ) {
            return Err(ContractError::InvalidValue {
                field: "decision target",
                reason: "must be an RFC revision, ticket revision, or experiment",
            });
        }
        validate_body(&self.rationale, "decision rationale")
    }
}

/// Publication kinds are semantic discourse records.  They do not grant
/// authority and they cannot be used as a hidden workflow state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PublicationKind {
    Finding,
    Question,
    Challenge,
    Correction,
    DecisionLink,
    Note,
}

impl PublicationKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Finding => "finding",
            Self::Question => "question",
            Self::Challenge => "challenge",
            Self::Correction => "correction",
            Self::DecisionLink => "decision_link",
            Self::Note => "note",
        }
    }

    pub fn parse(value: &str) -> Result<Self, ContractError> {
        match value {
            "finding" => Ok(Self::Finding),
            "question" => Ok(Self::Question),
            "challenge" => Ok(Self::Challenge),
            "correction" => Ok(Self::Correction),
            "decision_link" => Ok(Self::DecisionLink),
            "note" => Ok(Self::Note),
            _ => Err(ContractError::InvalidValue {
                field: "publication kind",
                reason: "is not a closed publication kind",
            }),
        }
    }
}

/// A supporting sealed artifact with a bounded display label. Attachments are
/// explicit evidence links, not a generic metadata bag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationAttachment {
    pub artifact: SealedArtifactReferenceV1,
    pub label: String,
}

impl PublicationAttachment {
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_text(
            "publication attachment label",
            &self.label,
            PUBLICATION_ATTACHMENT_LABEL_MAX_BYTES,
            false,
        )?;
        self.artifact
            .validate("publication attachment", INSTITUTIONAL_BODY_MAX_BYTES, true)
    }
}

/// Anchored durable discourse. Its summary is the bounded database-side
/// search projection; the full immutable body remains a sealed artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Publication {
    pub id: PublicationId,
    pub application_revision_id: ApplicationRevisionId,
    pub authoring_office_id: OfficeId,
    pub originating_session_id: Option<SessionId>,
    pub anchor: InstitutionalReference,
    pub kind: PublicationKind,
    pub summary: String,
    pub body: SealedArtifactReferenceV1,
    pub attachments: Vec<PublicationAttachment>,
    pub reply_to: Option<PublicationId>,
    pub supersedes: Option<PublicationId>,
    pub aggregate_revision: AggregateRevision,
}

impl Publication {
    pub fn validate(&self) -> Result<(), ContractError> {
        if !self.anchor.can_anchor_publication() {
            return Err(ContractError::InvalidValue {
                field: "publication anchor",
                reason: "must be an institutional object, not a publication or run",
            });
        }
        validate_text(
            "publication summary",
            &self.summary,
            INSTITUTIONAL_SUMMARY_MAX_BYTES,
            false,
        )?;
        validate_body(&self.body, "publication body")?;
        if self.attachments.len() > PUBLICATION_MAX_ATTACHMENTS {
            return Err(ContractError::InvalidValue {
                field: "publication attachments",
                reason: "exceeds the closed attachment limit",
            });
        }
        let mut artifact_ids = std::collections::BTreeSet::new();
        artifact_ids.insert(self.body.artifact_id);
        for attachment in &self.attachments {
            attachment.validate()?;
            if !artifact_ids.insert(attachment.artifact.artifact_id) {
                return Err(ContractError::InvalidValue {
                    field: "publication attachments",
                    reason: "repeats an artifact",
                });
            }
        }
        Ok(())
    }
}

/// A closed, typed navigation reference.  Each variant carries the ID for
/// that noun directly, making dangling or cross-noun references visible to
/// Rust and to a database adapter.  This is deliberately not a string pair
/// or a map.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InstitutionalReference {
    Project(ProjectId),
    Rfc(RfcId),
    RfcRevision(RfcRevisionId),
    Ticket(TicketId),
    TicketRevision(TicketRevisionId),
    Experiment(ExperimentId),
    ExperimentRun(ExperimentRunId),
    Claim(ClaimId),
    Decision(DecisionId),
    Office(OfficeId),
    Publication(PublicationId),
}

/// Closed object kinds for the two navigation boundaries that intentionally
/// cross noun families: object lookup and bounded search.  This enum does not
/// create a generic storage table; it is only the typed discriminator for an
/// [`InstitutionalReference`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InstitutionalObjectKind {
    Project,
    Rfc,
    RfcRevision,
    Ticket,
    TicketRevision,
    Experiment,
    ExperimentRun,
    Claim,
    Decision,
    Office,
    Publication,
}

impl InstitutionalObjectKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Rfc => "rfc",
            Self::RfcRevision => "rfc_revision",
            Self::Ticket => "ticket",
            Self::TicketRevision => "ticket_revision",
            Self::Experiment => "experiment",
            Self::ExperimentRun => "experiment_run",
            Self::Claim => "claim",
            Self::Decision => "decision",
            Self::Office => "office",
            Self::Publication => "publication",
        }
    }

    pub fn parse(value: &str) -> Result<Self, ContractError> {
        match value {
            "project" => Ok(Self::Project),
            "rfc" => Ok(Self::Rfc),
            "rfc_revision" => Ok(Self::RfcRevision),
            "ticket" => Ok(Self::Ticket),
            "ticket_revision" => Ok(Self::TicketRevision),
            "experiment" => Ok(Self::Experiment),
            "experiment_run" => Ok(Self::ExperimentRun),
            "claim" => Ok(Self::Claim),
            "decision" => Ok(Self::Decision),
            "office" => Ok(Self::Office),
            "publication" => Ok(Self::Publication),
            _ => Err(ContractError::InvalidValue {
                field: "institutional object kind",
                reason: "is not a closed institutional object kind",
            }),
        }
    }
}

impl InstitutionalReference {
    #[must_use]
    pub const fn kind(self) -> InstitutionalObjectKind {
        match self {
            Self::Project(_) => InstitutionalObjectKind::Project,
            Self::Rfc(_) => InstitutionalObjectKind::Rfc,
            Self::RfcRevision(_) => InstitutionalObjectKind::RfcRevision,
            Self::Ticket(_) => InstitutionalObjectKind::Ticket,
            Self::TicketRevision(_) => InstitutionalObjectKind::TicketRevision,
            Self::Experiment(_) => InstitutionalObjectKind::Experiment,
            Self::ExperimentRun(_) => InstitutionalObjectKind::ExperimentRun,
            Self::Claim(_) => InstitutionalObjectKind::Claim,
            Self::Decision(_) => InstitutionalObjectKind::Decision,
            Self::Office(_) => InstitutionalObjectKind::Office,
            Self::Publication(_) => InstitutionalObjectKind::Publication,
        }
    }

    #[must_use]
    pub const fn id(self) -> i64 {
        match self {
            Self::Project(value) => value.get(),
            Self::Rfc(value) => value.get(),
            Self::RfcRevision(value) => value.get(),
            Self::Ticket(value) => value.get(),
            Self::TicketRevision(value) => value.get(),
            Self::Experiment(value) => value.get(),
            Self::ExperimentRun(value) => value.get(),
            Self::Claim(value) => value.get(),
            Self::Decision(value) => value.get(),
            Self::Office(value) => value.get(),
            Self::Publication(value) => value.get(),
        }
    }

    pub fn from_kind_and_id(kind: InstitutionalObjectKind, id: i64) -> Result<Self, ContractError> {
        Ok(match kind {
            InstitutionalObjectKind::Project => Self::Project(ProjectId::new(id)?),
            InstitutionalObjectKind::Rfc => Self::Rfc(RfcId::new(id)?),
            InstitutionalObjectKind::RfcRevision => Self::RfcRevision(RfcRevisionId::new(id)?),
            InstitutionalObjectKind::Ticket => Self::Ticket(TicketId::new(id)?),
            InstitutionalObjectKind::TicketRevision => {
                Self::TicketRevision(TicketRevisionId::new(id)?)
            }
            InstitutionalObjectKind::Experiment => Self::Experiment(ExperimentId::new(id)?),
            InstitutionalObjectKind::ExperimentRun => {
                Self::ExperimentRun(ExperimentRunId::new(id)?)
            }
            InstitutionalObjectKind::Claim => Self::Claim(ClaimId::new(id)?),
            InstitutionalObjectKind::Decision => Self::Decision(DecisionId::new(id)?),
            InstitutionalObjectKind::Office => Self::Office(OfficeId::new(id)?),
            InstitutionalObjectKind::Publication => Self::Publication(PublicationId::new(id)?),
        })
    }

    /// Publication anchors exclude discourse-about-discourse and execution
    /// runs.  The latter remain navigable references, but a publication must
    /// attach to the institutional object whose question or responsibility it
    /// illuminates.
    #[must_use]
    pub const fn can_anchor_publication(self) -> bool {
        !matches!(self, Self::ExperimentRun(_) | Self::Publication(_))
    }
}

fn validate_body(
    artifact: &SealedArtifactReferenceV1,
    field: &'static str,
) -> Result<(), ContractError> {
    artifact.validate(field, INSTITUTIONAL_BODY_MAX_BYTES, false)
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
            reason: "must be nonempty, bounded UTF-8 without NUL",
        });
    }
    Ok(())
}
