//! Durable custody for reproducible harness compilations.
//!
//! This is a named persistence seam, not a generic repository abstraction.
//! It stores only the immutable compiler inputs/outputs required to explain
//! and replay an admitted assignment packet.

use factory_protocol::{
    ApplicationRevisionId, ArtifactId, AssignmentId, AssignmentRole, ContentDigest,
    ContextInclusionClassV1, ContextItemV1, ContextReferenceV1, HARNESS_COMPILER_VERSION_V1,
    HarnessCompilationId, HarnessCompilationV1, OfficeId,
};
use sqlx::{PgPool, Postgres, Row, Transaction};
use thiserror::Error;

use crate::storage::KernelStore;

#[derive(Clone, Debug)]
pub struct HarnessStore {
    pool: PgPool,
}

impl KernelStore {
    #[must_use]
    pub fn harness_store(&self) -> HarnessStore {
        HarnessStore::from_kernel_pool(self.pool_for_authority())
    }
}

impl HarnessStore {
    pub(crate) fn from_kernel_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Resolves the one active root office selected by the closed assignment
    /// role. The compiler receives this durable ID; actors never do.
    pub async fn active_office(
        &self,
        application_revision_id: ApplicationRevisionId,
        assignment_role: AssignmentRole,
    ) -> Result<OfficeId, HarnessStoreError> {
        let office_id: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM factory.offices
              WHERE application_revision_id = $1 AND assignment_role = $2 AND lifecycle = 0",
        )
        .bind(application_revision_id.get())
        .bind(assignment_role_code(assignment_role))
        .fetch_optional(&self.pool)
        .await?;
        office_id
            .ok_or(HarnessStoreError::ActiveOfficeMissing {
                application_revision_id,
                assignment_role,
            })
            .and_then(|id| OfficeId::new(id).map_err(Into::into))
    }

    /// Reads the immutable compiler receipt for one assignment. This is the
    /// narrow inspection seam used by replay and operator views; it does not
    /// expose a mutable persistence abstraction.
    pub async fn compilation_for_assignment(
        &self,
        assignment_id: AssignmentId,
    ) -> Result<Option<HarnessCompilationV1>, HarnessStoreError> {
        let row = sqlx::query(
            "SELECT id, application_revision_id, office_id, assignment_role,
                            compiler_version, spec_artifact_id, system_prompt_artifact_id,
                            assignment_prompt_artifact_id, packet_artifact_id, packet_digest
                       FROM factory.harness_compilations WHERE assignment_id = $1",
        )
        .bind(assignment_id.get())
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let packet_digest: Vec<u8> = row.try_get("packet_digest")?;
        let packet_digest = ContentDigest::from_bytes(
            packet_digest
                .as_slice()
                .try_into()
                .map_err(|_| HarnessStoreError::CorruptStoredPacketDigest)?,
        );
        let id = HarnessCompilationId::new(row.try_get("id")?)?;
        Ok(Some(HarnessCompilationV1 {
            id,
            assignment_id,
            application_revision_id: ApplicationRevisionId::new(
                row.try_get("application_revision_id")?,
            )?,
            office_id: OfficeId::new(row.try_get("office_id")?)?,
            assignment_role: assignment_role_from_code(row.try_get("assignment_role")?)?,
            compiler_version: u16::try_from(row.try_get::<i16, _>("compiler_version")?)
                .map_err(|_| HarnessStoreError::CorruptStoredCompilerVersion)?,
            spec_artifact_id: ArtifactId::new(row.try_get("spec_artifact_id")?)?,
            system_prompt_artifact_id: ArtifactId::new(row.try_get("system_prompt_artifact_id")?)?,
            assignment_prompt_artifact_id: ArtifactId::new(
                row.try_get("assignment_prompt_artifact_id")?,
            )?,
            packet_artifact_id: ArtifactId::new(row.try_get("packet_artifact_id")?)?,
            packet_digest,
            context_items: context_items_for_compilation(&self.pool, id).await?,
        }))
    }

    /// Persists exactly one immutable compiler receipt for an already admitted
    /// assignment. A retry may observe the same row, but cannot replace its
    /// spec, context, prompts, or packet digest.
    pub async fn record(
        &self,
        command: &RecordHarnessCompilation,
    ) -> Result<HarnessCompilationV1, HarnessStoreError> {
        command.validate()?;
        let mut tx = self.pool.begin().await?;
        let assignment = sqlx::query(
            "SELECT office_id, assignment_role FROM factory.assignments
              WHERE id = $1 AND application_revision_id = $2",
        )
        .bind(command.assignment_id.get())
        .bind(command.application_revision_id.get())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(HarnessStoreError::AssignmentMissing {
            assignment_id: command.assignment_id,
        })?;
        let office_id = OfficeId::new(assignment.try_get("office_id")?)?;
        let assignment_role: i16 = assignment.try_get("assignment_role")?;
        if office_id != command.office_id
            || assignment_role != assignment_role_code(command.assignment_role)
        {
            return Err(HarnessStoreError::AssignmentIdentityMismatch);
        }

        let receipt = persist_harness_compilation(&mut tx, command).await?;
        tx.commit().await?;
        Ok(receipt)
    }
}

/// Inserts the immutable compiler receipt inside the caller's transaction.
/// `ProcessStore::create_assignment` uses this before its audit receipt so an
/// admitted assignment and its harness are one durable transition.
pub(crate) async fn persist_harness_compilation(
    tx: &mut Transaction<'_, Postgres>,
    command: &RecordHarnessCompilation,
) -> Result<HarnessCompilationV1, HarnessStoreError> {
    command.validate()?;
    let inserted: Option<i64> = sqlx::query_scalar(
        "INSERT INTO factory.harness_compilations (
                 assignment_id, application_revision_id, office_id, assignment_role,
                 compiler_version, spec_artifact_id, system_prompt_artifact_id,
                 assignment_prompt_artifact_id, packet_artifact_id, packet_digest
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
             ON CONFLICT (assignment_id) DO NOTHING
             RETURNING id",
    )
    .bind(command.assignment_id.get())
    .bind(command.application_revision_id.get())
    .bind(command.office_id.get())
    .bind(assignment_role_code(command.assignment_role))
    .bind(
        i16::try_from(command.compiler_version)
            .map_err(|_| HarnessStoreError::CompilerVersionOutOfRange)?,
    )
    .bind(command.spec_artifact_id.get())
    .bind(command.system_prompt_artifact_id.get())
    .bind(command.assignment_prompt_artifact_id.get())
    .bind(command.packet_artifact_id.get())
    .bind(command.packet_digest.as_bytes().as_slice())
    .fetch_optional(&mut **tx)
    .await?;

    let (id, created) = match inserted {
        Some(id) => (HarnessCompilationId::new(id)?, true),
        None => {
            let row = sqlx::query(
                "SELECT id, application_revision_id, office_id, assignment_role,
                            compiler_version, spec_artifact_id, system_prompt_artifact_id,
                            assignment_prompt_artifact_id, packet_artifact_id, packet_digest
                       FROM factory.harness_compilations WHERE assignment_id = $1",
            )
            .bind(command.assignment_id.get())
            .fetch_one(&mut **tx)
            .await?;
            let digest: Vec<u8> = row.try_get("packet_digest")?;
            if row.try_get::<i64, _>("application_revision_id")?
                != command.application_revision_id.get()
                || row.try_get::<i64, _>("office_id")? != command.office_id.get()
                || row.try_get::<i16, _>("assignment_role")?
                    != assignment_role_code(command.assignment_role)
                || row.try_get::<i16, _>("compiler_version")?
                    != i16::try_from(command.compiler_version)
                        .map_err(|_| HarnessStoreError::CompilerVersionOutOfRange)?
                || row.try_get::<i64, _>("spec_artifact_id")? != command.spec_artifact_id.get()
                || row.try_get::<i64, _>("system_prompt_artifact_id")?
                    != command.system_prompt_artifact_id.get()
                || row.try_get::<i64, _>("assignment_prompt_artifact_id")?
                    != command.assignment_prompt_artifact_id.get()
                || row.try_get::<i64, _>("packet_artifact_id")? != command.packet_artifact_id.get()
                || digest.as_slice() != command.packet_digest.as_bytes()
            {
                return Err(HarnessStoreError::IdempotencyConflict);
            }
            (HarnessCompilationId::new(row.try_get("id")?)?, false)
        }
    };
    if created {
        for (ordinal, item) in command.context_items.iter().enumerate() {
            let columns = context_columns(item.reference);
            sqlx::query(
                "INSERT INTO factory.harness_context_items (
                         compilation_id, application_revision_id, ordinal, inclusion_class, reason,
                         artifact_id, project_id, rfc_id, rfc_revision_id, ticket_id,
                         ticket_revision_id, experiment_id, claim_id, decision_id, office_id
                     ) VALUES (
                         $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15
                     )",
            )
            .bind(id.get())
            .bind(command.application_revision_id.get())
            .bind(i16::try_from(ordinal).map_err(|_| HarnessStoreError::ContextOrdinalOutOfRange)?)
            .bind(inclusion_class_code(item.inclusion))
            .bind(&item.reason)
            .bind(columns.artifact_id)
            .bind(columns.project_id)
            .bind(columns.rfc_id)
            .bind(columns.rfc_revision_id)
            .bind(columns.ticket_id)
            .bind(columns.ticket_revision_id)
            .bind(columns.experiment_id)
            .bind(columns.claim_id)
            .bind(columns.decision_id)
            .bind(columns.office_id)
            .execute(&mut **tx)
            .await?;
        }
    }
    Ok(HarnessCompilationV1 {
        id,
        assignment_id: command.assignment_id,
        application_revision_id: command.application_revision_id,
        office_id: command.office_id,
        assignment_role: command.assignment_role,
        compiler_version: command.compiler_version,
        spec_artifact_id: command.spec_artifact_id,
        system_prompt_artifact_id: command.system_prompt_artifact_id,
        assignment_prompt_artifact_id: command.assignment_prompt_artifact_id,
        packet_artifact_id: command.packet_artifact_id,
        packet_digest: command.packet_digest,
        context_items: command.context_items.clone(),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordHarnessCompilation {
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
    pub context_items: Vec<ContextItemV1>,
}

impl RecordHarnessCompilation {
    pub(crate) fn validate(&self) -> Result<(), HarnessStoreError> {
        if self.compiler_version != HARNESS_COMPILER_VERSION_V1 {
            return Err(HarnessStoreError::UnsupportedCompilerVersion);
        }
        if self.context_items.len() > factory_protocol::HARNESS_CONTEXT_MAX_ITEMS {
            return Err(HarnessStoreError::ContextLimitExceeded);
        }
        for item in &self.context_items {
            item.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ContextColumns {
    artifact_id: Option<i64>,
    project_id: Option<i64>,
    rfc_id: Option<i64>,
    rfc_revision_id: Option<i64>,
    ticket_id: Option<i64>,
    ticket_revision_id: Option<i64>,
    experiment_id: Option<i64>,
    claim_id: Option<i64>,
    decision_id: Option<i64>,
    office_id: Option<i64>,
}

fn context_columns(reference: ContextReferenceV1) -> ContextColumns {
    match reference {
        ContextReferenceV1::Artifact(id) => ContextColumns {
            artifact_id: Some(id.get()),
            ..ContextColumns::default()
        },
        ContextReferenceV1::Project(id) => ContextColumns {
            project_id: Some(id.get()),
            ..ContextColumns::default()
        },
        ContextReferenceV1::Rfc(id) => ContextColumns {
            rfc_id: Some(id.get()),
            ..ContextColumns::default()
        },
        ContextReferenceV1::RfcRevision(id) => ContextColumns {
            rfc_revision_id: Some(id.get()),
            ..ContextColumns::default()
        },
        ContextReferenceV1::Ticket(id) => ContextColumns {
            ticket_id: Some(id.get()),
            ..ContextColumns::default()
        },
        ContextReferenceV1::TicketRevision(id) => ContextColumns {
            ticket_revision_id: Some(id.get()),
            ..ContextColumns::default()
        },
        ContextReferenceV1::Experiment(id) => ContextColumns {
            experiment_id: Some(id.get()),
            ..ContextColumns::default()
        },
        ContextReferenceV1::Claim(id) => ContextColumns {
            claim_id: Some(id.get()),
            ..ContextColumns::default()
        },
        ContextReferenceV1::Decision(id) => ContextColumns {
            decision_id: Some(id.get()),
            ..ContextColumns::default()
        },
        ContextReferenceV1::Office(id) => ContextColumns {
            office_id: Some(id.get()),
            ..ContextColumns::default()
        },
    }
}

async fn context_items_for_compilation(
    pool: &PgPool,
    compilation_id: HarnessCompilationId,
) -> Result<Vec<ContextItemV1>, HarnessStoreError> {
    let rows = sqlx::query(
        "SELECT inclusion_class, reason, artifact_id, project_id, rfc_id, rfc_revision_id,
                ticket_id, ticket_revision_id, experiment_id, claim_id, decision_id, office_id
           FROM factory.harness_context_items
          WHERE compilation_id = $1 ORDER BY ordinal ASC",
    )
    .bind(compilation_id.get())
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            let reference = if let Some(id) = row.try_get::<Option<i64>, _>("artifact_id")? {
                ContextReferenceV1::Artifact(ArtifactId::new(id)?)
            } else if let Some(id) = row.try_get("project_id")? {
                ContextReferenceV1::Project(factory_protocol::ProjectId::new(id)?)
            } else if let Some(id) = row.try_get("rfc_id")? {
                ContextReferenceV1::Rfc(factory_protocol::RfcId::new(id)?)
            } else if let Some(id) = row.try_get("rfc_revision_id")? {
                ContextReferenceV1::RfcRevision(factory_protocol::RfcRevisionId::new(id)?)
            } else if let Some(id) = row.try_get("ticket_id")? {
                ContextReferenceV1::Ticket(factory_protocol::TicketId::new(id)?)
            } else if let Some(id) = row.try_get("ticket_revision_id")? {
                ContextReferenceV1::TicketRevision(factory_protocol::TicketRevisionId::new(id)?)
            } else if let Some(id) = row.try_get("experiment_id")? {
                ContextReferenceV1::Experiment(factory_protocol::ExperimentId::new(id)?)
            } else if let Some(id) = row.try_get("claim_id")? {
                ContextReferenceV1::Claim(factory_protocol::ClaimId::new(id)?)
            } else if let Some(id) = row.try_get("decision_id")? {
                ContextReferenceV1::Decision(factory_protocol::DecisionId::new(id)?)
            } else if let Some(id) = row.try_get("office_id")? {
                ContextReferenceV1::Office(OfficeId::new(id)?)
            } else {
                return Err(HarnessStoreError::ContextReferenceMissing);
            };
            Ok(ContextItemV1 {
                reference,
                inclusion: inclusion_class_from_code(row.try_get("inclusion_class")?)?,
                reason: row.try_get("reason")?,
            })
        })
        .collect()
}

fn assignment_role_from_code(value: i16) -> Result<AssignmentRole, HarnessStoreError> {
    match value {
        0 => Ok(AssignmentRole::ProductResearch),
        1 => Ok(AssignmentRole::Engineering),
        2 => Ok(AssignmentRole::Quality),
        _ => Err(HarnessStoreError::CorruptStoredAssignmentRole),
    }
}

fn inclusion_class_from_code(value: i16) -> Result<ContextInclusionClassV1, HarnessStoreError> {
    match value {
        0 => Ok(ContextInclusionClassV1::DirectTarget),
        1 => Ok(ContextInclusionClassV1::RequiredConstraint),
        2 => Ok(ContextInclusionClassV1::DirectEvidence),
        3 => Ok(ContextInclusionClassV1::CurrentDecision),
        _ => Err(HarnessStoreError::CorruptStoredInclusionClass),
    }
}

const fn assignment_role_code(role: AssignmentRole) -> i16 {
    match role {
        AssignmentRole::ProductResearch => 0,
        AssignmentRole::Engineering => 1,
        AssignmentRole::Quality => 2,
    }
}

const fn inclusion_class_code(class: ContextInclusionClassV1) -> i16 {
    match class {
        ContextInclusionClassV1::DirectTarget => 0,
        ContextInclusionClassV1::RequiredConstraint => 1,
        ContextInclusionClassV1::DirectEvidence => 2,
        ContextInclusionClassV1::CurrentDecision => 3,
    }
}

#[derive(Debug, Error)]
pub enum HarnessStoreError {
    #[error(
        "no active {assignment_role:?} office exists for application revision {application_revision_id}"
    )]
    ActiveOfficeMissing {
        application_revision_id: ApplicationRevisionId,
        assignment_role: AssignmentRole,
    },
    #[error("assignment {assignment_id} does not exist for harness persistence")]
    AssignmentMissing { assignment_id: AssignmentId },
    #[error("assignment office or role does not match the compiled harness")]
    AssignmentIdentityMismatch,
    #[error("harness compilation retry differs from the immutable stored compilation")]
    IdempotencyConflict,
    #[error("harness compiler version is unsupported")]
    UnsupportedCompilerVersion,
    #[error("harness context exceeds its fixed item limit")]
    ContextLimitExceeded,
    #[error("harness compiler version is outside SQL range")]
    CompilerVersionOutOfRange,
    #[error("harness context ordinal is outside SQL range")]
    ContextOrdinalOutOfRange,
    #[error("stored harness packet digest violates its 32-byte invariant")]
    CorruptStoredPacketDigest,
    #[error("stored harness compiler version is invalid")]
    CorruptStoredCompilerVersion,
    #[error("stored harness assignment role is invalid")]
    CorruptStoredAssignmentRole,
    #[error("stored harness inclusion class is invalid")]
    CorruptStoredInclusionClass,
    #[error("stored harness context violates its exactly-one-reference invariant")]
    ContextReferenceMissing,
    #[error(transparent)]
    Contract(#[from] factory_protocol::ContractError),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}
