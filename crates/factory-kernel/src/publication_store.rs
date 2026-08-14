//! Kernel authority for immutable, anchored institutional publications.
//!
//! Publications replace new unanchored Forum writes.  This is intentionally a
//! small command surface: a bound actor may create one sealed, office-attributed
//! publication; every queryable discussion projection remains a read-only
//! projection over this durable row.  There is no thread state, reputation, or
//! generic metadata graph here.

use factory_protocol::{
    AggregateRevision, ApplicationRevisionId, ArtifactId, AuditLogId, ContentDigest,
    InstitutionalReference, OfficeId, PublicationId, PublicationKind, SessionId,
};
use sqlx::{PgPool, Postgres, Row, Transaction};
use thiserror::Error;

use crate::{local_transport::ActorConnectionBinding, storage::KernelStore};

const PUBLICATION_SUBJECT: i16 = 60;
const CREATE_PUBLICATION_OPERATION: &str = "publication.create";

/// The concrete kernel command for one immutable anchored publication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatePublication {
    pub client_command_id: String,
    pub anchor: InstitutionalReference,
    pub kind: PublicationKind,
    pub summary: String,
    pub body_artifact_id: ArtifactId,
    pub attachments: Vec<PublicationAttachmentInput>,
    pub reply_to: Option<PublicationId>,
    pub supersedes: Option<PublicationId>,
}

/// One supporting artifact reference. The store proves the artifact exists;
/// its digest and byte length remain in the immutable artifact custody row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationAttachmentInput {
    pub artifact_id: ArtifactId,
    pub label: String,
}

/// The audit-backed identity allocated by an accepted publication command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationReceipt {
    pub publication_id: PublicationId,
    pub resulting_revision: AggregateRevision,
    pub audit_log_id: AuditLogId,
    pub was_idempotent_retry: bool,
}

/// Kernel-owned PostgreSQL authority for the Publication relation.
#[derive(Clone, Debug)]
pub struct PublicationStore {
    pool: PgPool,
}

impl KernelStore {
    /// Reuses the daemon's sole fixed PostgreSQL pool. No actor receives this
    /// value or a raw SQL capability.
    #[must_use]
    pub fn publication_store(&self) -> PublicationStore {
        PublicationStore::from_kernel_pool(self.pool_for_authority())
    }
}

impl PublicationStore {
    pub(crate) fn from_kernel_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Creates one immutable publication attributed only from the already
    /// bound actor connection. The command cannot assert a session, office,
    /// application revision, principal, or lifecycle result.
    pub async fn create_from_actor(
        &self,
        binding: ActorConnectionBinding,
        command: &CreatePublication,
    ) -> Result<PublicationReceipt, PublicationStoreError> {
        let authoring_office_id = bound_actor_office(&self.pool, binding).await?;
        self.create_from_provenance(
            binding.application_revision_id(),
            authoring_office_id,
            Some(binding.session_id()),
            format!("session:{}", binding.session_id().get()),
            command,
        )
        .await
    }

    /// Creates one local-operator publication. The operator socket is a
    /// distinct, kernel-minted authority: it may select an active durable
    /// office deliberately, but it cannot impersonate an actor session.
    pub async fn create_from_operator(
        &self,
        application_revision_id: ApplicationRevisionId,
        authoring_office_id: OfficeId,
        command: &CreatePublication,
    ) -> Result<PublicationReceipt, PublicationStoreError> {
        require_active_office(&self.pool, application_revision_id, authoring_office_id).await?;
        self.create_from_provenance(
            application_revision_id,
            authoring_office_id,
            None,
            "grand-architect".to_owned(),
            command,
        )
        .await
    }

    async fn create_from_provenance(
        &self,
        application_revision_id: ApplicationRevisionId,
        authoring_office_id: OfficeId,
        originating_session_id: Option<SessionId>,
        principal: String,
        command: &CreatePublication,
    ) -> Result<PublicationReceipt, PublicationStoreError> {
        validate_command(command)?;
        let fingerprint = fingerprint(command);
        let mut transaction = self.pool.begin().await?;
        if let Some(receipt) = find_idempotent(
            &mut transaction,
            &principal,
            &command.client_command_id,
            fingerprint,
        )
        .await?
        {
            require_subject(receipt.subject_kind)?;
            transaction.commit().await?;
            return Ok(PublicationReceipt {
                publication_id: PublicationId::new(receipt.subject_id)?,
                resulting_revision: receipt.resulting_revision,
                audit_log_id: AuditLogId::new(receipt.audit_log_id)?,
                was_idempotent_retry: true,
            });
        }
        require_artifact(&mut transaction, command.body_artifact_id).await?;
        for attachment in &command.attachments {
            require_artifact(&mut transaction, attachment.artifact_id).await?;
        }
        let anchor = anchor_columns(command.anchor);
        let publication_id: i64 = sqlx::query_scalar(
            "INSERT INTO factory.publications (
                 application_revision_id, authoring_office_id, originating_session_id,
                 publication_kind, summary, body_artifact_id,
                 project_id, rfc_id, rfc_revision_id, ticket_id, ticket_revision_id,
                 experiment_id, claim_id, decision_id, office_id,
                 reply_to_publication_id, supersedes_publication_id
             ) VALUES (
                 $1, $2, $3, $4, $5, $6,
                 $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17
             ) RETURNING id",
        )
        .bind(application_revision_id.get())
        .bind(authoring_office_id.get())
        .bind(originating_session_id.map(SessionId::get))
        .bind(publication_kind_code(command.kind))
        .bind(&command.summary)
        .bind(command.body_artifact_id.get())
        .bind(anchor.project_id)
        .bind(anchor.rfc_id)
        .bind(anchor.rfc_revision_id)
        .bind(anchor.ticket_id)
        .bind(anchor.ticket_revision_id)
        .bind(anchor.experiment_id)
        .bind(anchor.claim_id)
        .bind(anchor.decision_id)
        .bind(anchor.office_id)
        .bind(command.reply_to.map(PublicationId::get))
        .bind(command.supersedes.map(PublicationId::get))
        .fetch_one(&mut *transaction)
        .await?;
        for attachment in &command.attachments {
            sqlx::query(
                "INSERT INTO factory.publication_attachments (
                     publication_id, application_revision_id, artifact_id, label
                 ) VALUES ($1, $2, $3, $4)",
            )
            .bind(publication_id)
            .bind(application_revision_id.get())
            .bind(attachment.artifact_id.get())
            .bind(&attachment.label)
            .execute(&mut *transaction)
            .await?;
        }
        let resulting_revision = AggregateRevision::initial();
        let audit_log_id = insert_audit(
            &mut transaction,
            &principal,
            &command.client_command_id,
            fingerprint,
            publication_id,
            resulting_revision,
        )
        .await?;
        transaction.commit().await?;
        Ok(PublicationReceipt {
            publication_id: PublicationId::new(publication_id)?,
            resulting_revision,
            audit_log_id: AuditLogId::new(audit_log_id)?,
            was_idempotent_retry: false,
        })
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct AnchorColumns {
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

fn anchor_columns(reference: InstitutionalReference) -> AnchorColumns {
    match reference {
        InstitutionalReference::Project(id) => AnchorColumns {
            project_id: Some(id.get()),
            ..AnchorColumns::default()
        },
        InstitutionalReference::Rfc(id) => AnchorColumns {
            rfc_id: Some(id.get()),
            ..AnchorColumns::default()
        },
        InstitutionalReference::RfcRevision(id) => AnchorColumns {
            rfc_revision_id: Some(id.get()),
            ..AnchorColumns::default()
        },
        InstitutionalReference::Ticket(id) => AnchorColumns {
            ticket_id: Some(id.get()),
            ..AnchorColumns::default()
        },
        InstitutionalReference::TicketRevision(id) => AnchorColumns {
            ticket_revision_id: Some(id.get()),
            ..AnchorColumns::default()
        },
        InstitutionalReference::Experiment(id) => AnchorColumns {
            experiment_id: Some(id.get()),
            ..AnchorColumns::default()
        },
        InstitutionalReference::Claim(id) => AnchorColumns {
            claim_id: Some(id.get()),
            ..AnchorColumns::default()
        },
        InstitutionalReference::Decision(id) => AnchorColumns {
            decision_id: Some(id.get()),
            ..AnchorColumns::default()
        },
        InstitutionalReference::Office(id) => AnchorColumns {
            office_id: Some(id.get()),
            ..AnchorColumns::default()
        },
        InstitutionalReference::ExperimentRun(_) | InstitutionalReference::Publication(_) => {
            unreachable!("CreatePublication validates its anchor before storage")
        }
    }
}

fn publication_kind_code(kind: PublicationKind) -> i16 {
    match kind {
        PublicationKind::Finding => 0,
        PublicationKind::Question => 1,
        PublicationKind::Challenge => 2,
        PublicationKind::Correction => 3,
        PublicationKind::DecisionLink => 4,
        PublicationKind::Note => 5,
    }
}

fn validate_command(command: &CreatePublication) -> Result<(), PublicationStoreError> {
    if command.client_command_id.is_empty()
        || command.client_command_id.len() > 160
        || !command
            .client_command_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'_' | b'-'))
    {
        return Err(PublicationStoreError::InvalidCommandIdentity);
    }
    if !command.anchor.can_anchor_publication() {
        return Err(PublicationStoreError::InvalidAnchor);
    }
    if command.summary.is_empty()
        || command.summary.len() > factory_protocol::INSTITUTIONAL_SUMMARY_MAX_BYTES
        || command.summary.contains('\0')
    {
        return Err(PublicationStoreError::InvalidSummary);
    }
    if command.attachments.len() > factory_protocol::PUBLICATION_MAX_ATTACHMENTS {
        return Err(PublicationStoreError::AttachmentLimitExceeded);
    }
    let mut artifact_ids = std::collections::BTreeSet::new();
    artifact_ids.insert(command.body_artifact_id);
    for attachment in &command.attachments {
        if attachment.label.is_empty()
            || attachment.label.len() > factory_protocol::PUBLICATION_ATTACHMENT_LABEL_MAX_BYTES
            || attachment.label.contains('\0')
        {
            return Err(PublicationStoreError::InvalidAttachmentLabel);
        }
        if !artifact_ids.insert(attachment.artifact_id) {
            return Err(PublicationStoreError::DuplicateAttachmentArtifact);
        }
    }
    Ok(())
}

fn fingerprint(command: &CreatePublication) -> ContentDigest {
    let mut hasher = blake3::Hasher::new();
    hash(&mut hasher, CREATE_PUBLICATION_OPERATION);
    hash(&mut hasher, &command.client_command_id);
    hash(&mut hasher, command.anchor.kind().as_str());
    hasher.update(&command.anchor.id().to_be_bytes());
    hash(&mut hasher, command.kind.as_str());
    hash(&mut hasher, &command.summary);
    hasher.update(&command.body_artifact_id.get().to_be_bytes());
    for attachment in &command.attachments {
        hasher.update(&attachment.artifact_id.get().to_be_bytes());
        hash(&mut hasher, &attachment.label);
    }
    hasher.update(
        &command
            .reply_to
            .map(PublicationId::get)
            .unwrap_or_default()
            .to_be_bytes(),
    );
    hasher.update(
        &command
            .supersedes
            .map(PublicationId::get)
            .unwrap_or_default()
            .to_be_bytes(),
    );
    ContentDigest::from_bytes(*hasher.finalize().as_bytes())
}

fn hash(hasher: &mut blake3::Hasher, value: &str) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

async fn bound_actor_office(
    pool: &PgPool,
    binding: ActorConnectionBinding,
) -> Result<OfficeId, PublicationStoreError> {
    let row = sqlx::query(
        "SELECT office_id
           FROM factory.sessions
          WHERE id = $1 AND assignment_id = $2
            AND application_revision_id = $3 AND lifecycle = 1",
    )
    .bind(binding.session_id().get())
    .bind(binding.assignment_id().get())
    .bind(binding.application_revision_id().get())
    .fetch_optional(pool)
    .await?
    .ok_or(PublicationStoreError::BoundSessionUnavailable)?;
    Ok(OfficeId::new(row.try_get("office_id")?)?)
}

async fn require_active_office(
    pool: &PgPool,
    application_revision_id: ApplicationRevisionId,
    office_id: OfficeId,
) -> Result<(), PublicationStoreError> {
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
             SELECT 1 FROM factory.offices
              WHERE id = $1 AND application_revision_id = $2 AND lifecycle = 0
         )",
    )
    .bind(office_id.get())
    .bind(application_revision_id.get())
    .fetch_one(pool)
    .await?;
    if exists {
        Ok(())
    } else {
        Err(PublicationStoreError::InactiveOrUnknownOffice { office_id })
    }
}

async fn require_artifact(
    tx: &mut Transaction<'_, Postgres>,
    artifact_id: ArtifactId,
) -> Result<(), PublicationStoreError> {
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM factory.artifacts WHERE id = $1)",
    )
    .bind(artifact_id.get())
    .fetch_one(&mut **tx)
    .await?;
    if exists {
        Ok(())
    } else {
        Err(PublicationStoreError::UnknownArtifact { artifact_id })
    }
}

#[derive(Clone, Copy, Debug)]
struct AuditReceipt {
    audit_log_id: i64,
    subject_kind: i16,
    subject_id: i64,
    resulting_revision: AggregateRevision,
}

async fn find_idempotent(
    tx: &mut Transaction<'_, Postgres>,
    principal: &str,
    command_id: &str,
    fingerprint: ContentDigest,
) -> Result<Option<AuditReceipt>, PublicationStoreError> {
    let row = sqlx::query(
        "SELECT id, operation, command_fingerprint, subject_kind, subject_id, resulting_revision
           FROM factory.audit_log WHERE principal = $1 AND command_id = $2",
    )
    .bind(principal)
    .bind(command_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let operation: String = row.try_get("operation")?;
    let stored: Vec<u8> = row.try_get("command_fingerprint")?;
    if operation != CREATE_PUBLICATION_OPERATION || stored.as_slice() != fingerprint.as_bytes() {
        return Err(PublicationStoreError::IdempotencyConflict {
            principal: principal.to_owned(),
            command_id: command_id.to_owned(),
        });
    }
    Ok(Some(AuditReceipt {
        audit_log_id: row.try_get("id")?,
        subject_kind: row.try_get("subject_kind")?,
        subject_id: row.try_get("subject_id")?,
        resulting_revision: AggregateRevision::from_persisted(
            u64::try_from(row.try_get::<i64, _>("resulting_revision")?)
                .map_err(|_| PublicationStoreError::RevisionOutOfRange)?,
        ),
    }))
}

fn require_subject(subject_kind: i16) -> Result<(), PublicationStoreError> {
    if subject_kind == PUBLICATION_SUBJECT {
        Ok(())
    } else {
        Err(PublicationStoreError::AuditSubjectKindMismatch)
    }
}

async fn insert_audit(
    tx: &mut Transaction<'_, Postgres>,
    principal: &str,
    command_id: &str,
    fingerprint: ContentDigest,
    publication_id: i64,
    resulting_revision: AggregateRevision,
) -> Result<i64, PublicationStoreError> {
    let fingerprint_bytes = fingerprint.as_bytes();
    Ok(sqlx::query_scalar(
        "INSERT INTO factory.audit_log (
             principal, command_id, operation, command_fingerprint,
             subject_kind, subject_id, resulting_revision
         ) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(principal)
    .bind(command_id)
    .bind(CREATE_PUBLICATION_OPERATION)
    .bind(fingerprint_bytes.as_slice())
    .bind(PUBLICATION_SUBJECT)
    .bind(publication_id)
    .bind(
        i64::try_from(resulting_revision.get())
            .map_err(|_| PublicationStoreError::RevisionOutOfRange)?,
    )
    .fetch_one(&mut **tx)
    .await?)
}

#[derive(Debug, Error)]
pub enum PublicationStoreError {
    #[error("invalid publication command identity")]
    InvalidCommandIdentity,
    #[error("publication anchor must be one supported institutional object")]
    InvalidAnchor,
    #[error("publication summary is invalid")]
    InvalidSummary,
    #[error("publication attachment limit exceeded")]
    AttachmentLimitExceeded,
    #[error("publication attachment label is invalid")]
    InvalidAttachmentLabel,
    #[error("publication body and attachments must not repeat an artifact")]
    DuplicateAttachmentArtifact,
    #[error("the bound session is not running with its admitted office")]
    BoundSessionUnavailable,
    #[error("office {office_id} is not active for the selected application revision")]
    InactiveOrUnknownOffice { office_id: OfficeId },
    #[error("unknown sealed artifact {artifact_id}")]
    UnknownArtifact { artifact_id: ArtifactId },
    #[error("idempotency conflict for principal {principal:?} and command ID {command_id:?}")]
    IdempotencyConflict {
        principal: String,
        command_id: String,
    },
    #[error("publication retry audit has a different subject kind")]
    AuditSubjectKindMismatch,
    #[error("aggregate revision is outside SQL range")]
    RevisionOutOfRange,
    #[error(transparent)]
    Contract(#[from] factory_protocol::ContractError),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}
