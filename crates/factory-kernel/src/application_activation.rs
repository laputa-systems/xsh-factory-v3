//! Explicit operator activation for an already-admitted application revision.
//!
//! Application bundles and templates are immutable CAS inputs.  The only
//! mutable application control is the one active pointer per application, and
//! it is changed only between campaigns under the same process-transition
//! lock used by campaign start.  This module therefore cannot be reached by
//! an actor or by an application bundle callback.

use factory_protocol::{
    ARCHITECT_RATIONALE_BYTE_LIMIT, AggregateRevision, ApplicationKey, ApplicationRevisionId,
    ArchitectPrincipalV1, ContentDigest, ExpectedRevision, SealedArtifactReferenceV1,
};
use sqlx::{Postgres, Transaction};

use super::{
    ACTIVATE_APPLICATION_REVISION_OPERATION, APPLICATION_REVISION_SUBJECT, ApplicationRevisionView,
    KernelStore, StoreError, aggregate_revision_from_sql, find_idempotent_audit, hash_string,
    insert_audit_receipt, require_subject_kind, validate_command_component,
};

/// The same singleton lock used by `ProcessStore::start_campaign`.  Holding
/// it means a campaign cannot begin after this command has observed no running
/// campaign but before it moves the application active pointer.
const PROCESS_TRANSITION_LOCK_KEY: i64 = i64::MIN + 5;
const RUNNING_CAMPAIGN: i16 = 0;

/// Explicit Grand Architect selection of one immutable application revision.
/// `expected_revision` is the selected revision's aggregate revision; a
/// caller cannot activate an arbitrary ID after observing a different lineage
/// point.  The sealed rationale is provenance only and never contains code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivateApplicationRevision {
    pub principal: ArchitectPrincipalV1,
    pub command_id: String,
    pub expected_revision: ExpectedRevision,
    pub application_key: ApplicationKey,
    pub application_revision_id: ApplicationRevisionId,
    pub rationale: SealedArtifactReferenceV1,
}

impl ActivateApplicationRevision {
    fn validate(&self) -> Result<(), StoreError> {
        if self.principal.as_str().len() > 160 || self.principal.as_str().contains('\0') {
            return Err(StoreError::InvalidCommandComponent {
                field: "Architect principal",
            });
        }
        validate_command_component("command ID", &self.command_id)?;
        self.rationale
            .validate(
                "application activation rationale",
                ARCHITECT_RATIONALE_BYTE_LIMIT,
                false,
            )
            .map_err(StoreError::Contract)
    }

    fn fingerprint(&self) -> ContentDigest {
        let mut hasher = blake3::Hasher::new();
        hasher.update(ACTIVATE_APPLICATION_REVISION_OPERATION.as_bytes());
        hash_string(&mut hasher, self.principal.as_str());
        hash_string(&mut hasher, &self.command_id);
        hash_string(&mut hasher, self.application_key.as_str());
        hasher.update(&self.application_revision_id.get().to_be_bytes());
        hasher.update(&self.expected_revision.get().get().to_be_bytes());
        hasher.update(&self.rationale.artifact_id.get().to_be_bytes());
        hasher.update(&self.rationale.digest.as_bytes());
        hasher.update(&self.rationale.byte_length.to_be_bytes());
        ContentDigest::from_bytes(*hasher.finalize().as_bytes())
    }
}

/// The bounded receipt for an explicit active-pointer decision.  This is an
/// audit-backed transition, not a new application bundle revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationActivationReceipt {
    pub application_revision_id: ApplicationRevisionId,
    pub resulting_revision: AggregateRevision,
    pub audit_log_id: i64,
    pub is_active: bool,
    pub was_idempotent_retry: bool,
}

impl KernelStore {
    /// Activates one already-admitted application revision only when no
    /// campaign is running.  The command owns one audit receipt and its
    /// idempotency identity; it never admits source bytes or invokes the
    /// actor host.
    pub async fn activate_application_revision(
        &self,
        command: &ActivateApplicationRevision,
    ) -> Result<ApplicationActivationReceipt, StoreError> {
        command.validate()?;
        let fingerprint = command.fingerprint();
        let mut transaction = self.pool.begin().await?;
        sqlx::query!(
            "SELECT pg_advisory_xact_lock($1)",
            PROCESS_TRANSITION_LOCK_KEY
        )
        .execute(&mut *transaction)
        .await?;
        sqlx::query!(
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            command.application_key.as_str()
        )
        .execute(&mut *transaction)
        .await?;

        if let Some(receipt) = find_idempotent_audit(
            &mut transaction,
            command.principal.as_str(),
            &command.command_id,
            ACTIVATE_APPLICATION_REVISION_OPERATION,
            fingerprint,
        )
        .await?
        {
            require_subject_kind(&receipt, APPLICATION_REVISION_SUBJECT)?;
            let active = selected_is_active(&mut transaction, receipt.subject_id).await?;
            transaction.commit().await?;
            return Ok(ApplicationActivationReceipt {
                application_revision_id: ApplicationRevisionId::new(receipt.subject_id)?,
                resulting_revision: receipt.resulting_revision,
                audit_log_id: receipt.audit_log_id,
                is_active: active,
                was_idempotent_retry: true,
            });
        }

        let target = sqlx::query!(
            "SELECT id, aggregate_revision, is_active
             FROM factory.application_revisions
             WHERE application_key = $1 AND id = $2
             FOR UPDATE",
            command.application_key.as_str(),
            command.application_revision_id.get(),
        )
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or_else(|| StoreError::UnknownApplicationRevisionForKey {
            application_key: command.application_key.clone(),
            application_revision_id: Some(command.application_revision_id),
        })?;
        let resulting_revision = aggregate_revision_from_sql(target.aggregate_revision)?;
        if command.expected_revision.get() != resulting_revision {
            return Err(StoreError::RevisionConflict {
                expected: command.expected_revision,
                current: resulting_revision,
            });
        }
        require_sealed_rationale(&mut transaction, &command.rationale).await?;

        if let Some(campaign_id) = sqlx::query_scalar!(
            "SELECT id FROM factory.campaigns WHERE lifecycle = $1 LIMIT 1 FOR UPDATE",
            RUNNING_CAMPAIGN,
        )
        .fetch_optional(&mut *transaction)
        .await?
        {
            return Err(StoreError::ApplicationActivationCampaignRunning {
                campaign_id: factory_protocol::CampaignId::new(campaign_id)?,
            });
        }

        if !target.is_active {
            sqlx::query!(
                "UPDATE factory.application_revisions
                 SET is_active = FALSE
                 WHERE application_key = $1 AND is_active",
                command.application_key.as_str(),
            )
            .execute(&mut *transaction)
            .await?;
            sqlx::query!(
                "UPDATE factory.application_revisions SET is_active = TRUE WHERE id = $1",
                command.application_revision_id.get(),
            )
            .execute(&mut *transaction)
            .await?;
        }
        let audit_log_id = insert_audit_receipt(
            &mut transaction,
            command.principal.as_str(),
            &command.command_id,
            ACTIVATE_APPLICATION_REVISION_OPERATION,
            fingerprint,
            APPLICATION_REVISION_SUBJECT,
            target.id,
            resulting_revision,
        )
        .await?;
        transaction.commit().await?;
        Ok(ApplicationActivationReceipt {
            application_revision_id: command.application_revision_id,
            resulting_revision,
            audit_log_id,
            is_active: true,
            was_idempotent_retry: false,
        })
    }

    /// Loads the status projection used by the operator adapter.  Keeping the
    /// public type in this module makes the storage command surface discoverable
    /// next to its activation transition.
    pub async fn active_application_view(
        &self,
        application_key: &ApplicationKey,
        application_revision_id: Option<ApplicationRevisionId>,
    ) -> Result<ApplicationRevisionView, StoreError> {
        self.application_revision_view(application_key, application_revision_id)
            .await
    }
}

async fn selected_is_active(
    transaction: &mut Transaction<'_, Postgres>,
    application_revision_id: i64,
) -> Result<bool, StoreError> {
    sqlx::query_scalar!(
        "SELECT is_active FROM factory.application_revisions WHERE id = $1",
        application_revision_id,
    )
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or_else(|| StoreError::UnknownApplicationRevision {
        application_revision_id: ApplicationRevisionId::new(application_revision_id)
            .expect("id came from a checked audit receipt"),
    })
}

async fn require_sealed_rationale(
    transaction: &mut Transaction<'_, Postgres>,
    rationale: &SealedArtifactReferenceV1,
) -> Result<(), StoreError> {
    let row = sqlx::query!(
        "SELECT digest, byte_length FROM factory.artifacts WHERE id = $1",
        rationale.artifact_id.get(),
    )
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(StoreError::ApplicationActivationRationaleMismatch)?;
    if row.digest.as_slice() != rationale.digest.as_bytes()
        || u64::try_from(row.byte_length).map_err(|_| StoreError::RevisionOutOfRange)?
            != rationale.byte_length
    {
        return Err(StoreError::ApplicationActivationRationaleMismatch);
    }
    Ok(())
}
