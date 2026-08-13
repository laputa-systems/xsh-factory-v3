//! PostgreSQL authority for the first durable Factory V3 facts.
//!
//! This module deliberately owns a small command surface instead of a generic
//! repository/service layer. Each accepted command inserts its named domain
//! fact and one audit receipt in the same transaction; the audit receipt is
//! also its bounded retry identity.

use std::{str::FromStr, time::Duration};

use crate::cas::{CasArtifact, CasStore};
use crate::installed_runtime::InstalledKernelBuildReceiptV1;
use factory_protocol::{
    AggregateRevision, ApplicationKey, ApplicationRevisionId, ArtifactId, ContentDigest,
    ExpectedRevision, KernelBuildId, RepositoryId,
};
use sqlx::{
    PgPool, Postgres,
    migrate::{MigrateError, Migrator},
    postgres::{PgConnectOptions, PgPoolOptions},
};
use thiserror::Error;

/// Comment installed on the factory-owned schema by the canonical migration.
pub const SCHEMA_IDENTITY: &str = "factory-v3-schema:initial-authority-v1";

/// A fixed kernel-local key. PostgreSQL holds it per connection until explicit
/// release or connection death, so a daemon restart cannot inherit a stale lock.
const DAEMON_ADVISORY_LOCK_KEY: i64 = 0x4656_335f_4441_454d;
const KERNEL_BUILD_SUBJECT: i16 = 0;
const APPLICATION_REVISION_SUBJECT: i16 = 1;
const REPOSITORY_SUBJECT: i16 = 2;
const ARTIFACT_SUBJECT: i16 = 3;
const INSTALL_KERNEL_BUILD_OPERATION: &str = "kernel_build.install";
const ADMIT_APPLICATION_REVISION_OPERATION: &str = "application_revision.admit";
pub(crate) const ACTIVATE_APPLICATION_REVISION_OPERATION: &str = "application_revision.activate";
const REGISTER_REPOSITORY_OPERATION: &str = "repository.register";
const REGISTER_ARTIFACT_OPERATION: &str = "artifact.register";

static MIGRATOR: Migrator = sqlx::migrate!("../../schema/migrations");

#[path = "application_admission.rs"]
mod application_admission;
pub use application_admission::AdmitCompiledApplication;
#[path = "application_activation.rs"]
mod application_activation;
pub use application_activation::{ActivateApplicationRevision, ApplicationActivationReceipt};

/// The narrow physical connection owner used by the kernel. Actors and Deno
/// code never receive its database URL or pool.
#[derive(Clone, Debug)]
pub struct KernelStore {
    pool: PgPool,
}

impl KernelStore {
    pub(crate) fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }
    /// Connects with the small fixed pool allowed by the MVP baseline.
    pub async fn connect(database_url: &str) -> Result<Self, StoreError> {
        let options = PgConnectOptions::from_str(database_url)
            .map_err(|source| StoreError::InvalidDatabaseUrl { source })?;
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(options)
            .await?;
        Ok(Self { pool })
    }

    /// Applies the canonical migration then proves its schema
    /// identity comment. A binary that sees another schema fails before serving.
    pub async fn migrate_and_verify(&self) -> Result<(), StoreError> {
        self.verify_postgres_baseline().await?;
        MIGRATOR.run(&self.pool).await?;
        self.verify_schema_identity().await
    }

    /// The first authority lineage is exercised and supported only on the
    /// PostgreSQL 18 baseline. Rejecting an older server before migration keeps
    /// a partial, unsupported schema from becoming durable state.
    async fn verify_postgres_baseline(&self) -> Result<(), StoreError> {
        let server_version_num = sqlx::query_scalar!(
            "SELECT current_setting('server_version_num') AS \"server_version_num!\""
        )
        .fetch_one(&self.pool)
        .await?;
        let server_version_num = server_version_num
            .parse::<u32>()
            .map_err(|_| StoreError::UnparseablePostgresVersion { server_version_num })?;
        if server_version_num < 180_000 {
            return Err(StoreError::UnsupportedPostgresVersion { server_version_num });
        }
        Ok(())
    }

    /// Checks the factory-owned schema identity without performing a write.
    pub async fn verify_schema_identity(&self) -> Result<(), StoreError> {
        let observed = sqlx::query_scalar!(
            "SELECT obj_description('factory'::regnamespace, 'pg_namespace') AS \"description?\""
        )
        .fetch_one(&self.pool)
        .await?;
        match observed.as_deref() {
            Some(SCHEMA_IDENTITY) => Ok(()),
            _ => Err(StoreError::SchemaIdentityMismatch { observed }),
        }
    }

    /// Acquires the PostgreSQL singleton lock for one resident daemon.
    pub async fn acquire_daemon_lock(&self) -> Result<DaemonLock, StoreError> {
        let mut connection = self.pool.acquire().await?;
        let acquired = sqlx::query_scalar!(
            "SELECT pg_try_advisory_lock($1) AS \"acquired!\"",
            DAEMON_ADVISORY_LOCK_KEY
        )
        .fetch_one(&mut *connection)
        .await?;
        if !acquired {
            return Err(StoreError::DaemonAlreadyRunning);
        }
        Ok(DaemonLock {
            connection: Some(connection),
        })
    }

    /// Closes every kernel-owned connection. A stopped daemon uses this path
    /// so PostgreSQL releases its session-scoped advisory singleton lock.
    pub async fn close(&self) {
        self.pool.close().await;
    }

    /// Creates the narrow Forum authority over this kernel's existing fixed
    /// PostgreSQL pool. No second database URL or connection pool is opened.
    #[must_use]
    pub fn forum_store(&self) -> crate::forum_store::ForumStore {
        crate::forum_store::ForumStore::from_kernel_pool(self.pool.clone())
    }

    /// Shares the already-owned fixed pool with another named kernel
    /// authority. The pool itself never crosses the public storage boundary.
    pub(crate) fn pool_for_authority(&self) -> PgPool {
        self.pool.clone()
    }

    /// Installs one manually qualified kernel build. Installation is a typed
    /// durable transition, not an in-daemon self-upgrade mechanism.
    pub async fn install_kernel_build(
        &self,
        cas: &CasStore,
        command: &InstallKernelBuild,
    ) -> Result<KernelBuildReceipt, StoreError> {
        command.validate()?;
        let qualification = cas.verify(command.qualification_receipt.digest())?;
        if qualification != command.qualification_receipt {
            return Err(StoreError::QualificationReceiptChanged);
        }
        let fingerprint = command.fingerprint();
        let mut transaction = self.pool.begin().await?;
        sqlx::query!("SELECT pg_advisory_xact_lock($1)", i64::MIN)
            .execute(&mut *transaction)
            .await?;
        if let Some(receipt) = find_idempotent_audit(
            &mut transaction,
            &command.principal,
            &command.command_id,
            INSTALL_KERNEL_BUILD_OPERATION,
            fingerprint,
        )
        .await?
        {
            require_subject_kind(&receipt, KERNEL_BUILD_SUBJECT)?;
            transaction.commit().await?;
            return Ok(KernelBuildReceipt {
                kernel_build_id: command.build_id,
                resulting_revision: receipt.resulting_revision,
                audit_log_id: receipt.audit_log_id,
                was_idempotent_retry: true,
            });
        }

        let current = sqlx::query_scalar!(
            "SELECT COALESCE(MAX(revision), 0)::BIGINT AS \"revision!\" FROM factory.kernel_builds"
        )
        .fetch_one(&mut *transaction)
        .await?;
        let current_revision = aggregate_revision_from_sql(current)?;
        if command.expected_revision.get() != current_revision {
            return Err(StoreError::RevisionConflict {
                expected: command.expected_revision,
                current: current_revision,
            });
        }
        let resulting_revision = current_revision.next()?;
        let build_digest = command.build_id.digest().as_bytes();
        let source_digest = command.source_digest.as_bytes();
        let binary_digest = command.binary_digest.as_bytes();
        let deno_lock_digest = command.deno_lock_digest.as_bytes();
        let qualification_digest = qualification.digest().as_bytes();
        let qualification_path = cas.object_relative_path(qualification.digest())?;
        let existing_qualification = sqlx::query!(
            "SELECT id, byte_length, cas_relative_path
             FROM factory.artifacts WHERE digest = $1",
            &qualification_digest[..]
        )
        .fetch_optional(&mut *transaction)
        .await?;
        let existing_qualification_id = if let Some(row) = existing_qualification {
            let expected_length = i64::try_from(qualification.byte_length())
                .map_err(|_| StoreError::ArtifactLengthOutOfRange)?;
            if row.byte_length != expected_length
                || row.cas_relative_path != qualification_path.as_str()
            {
                return Err(StoreError::ArtifactIdentityConflict {
                    digest: qualification.digest(),
                });
            }
            Some(row.id)
        } else {
            None
        };
        sqlx::query!("UPDATE factory.kernel_builds SET is_current = FALSE WHERE is_current")
            .execute(&mut *transaction)
            .await?;
        let build_id: i64 = sqlx::query_scalar!(
            "SELECT nextval(pg_get_serial_sequence('factory.kernel_builds', 'id')) AS \"id!\""
        )
        .fetch_one(&mut *transaction)
        .await?;
        let qualification_artifact_id: i64 =
            match existing_qualification_id {
                Some(id) => id,
                None => sqlx::query_scalar!(
                    "SELECT nextval(pg_get_serial_sequence('factory.artifacts', 'id')) AS \"id!\""
                )
                .fetch_one(&mut *transaction)
                .await?,
            };
        let resulting_revision =
            i64::try_from(resulting_revision.get()).map_err(|_| StoreError::RevisionOutOfRange)?;
        sqlx::query!(
            "INSERT INTO factory.kernel_builds (
                id, build_digest, source_digest, binary_digest, schema_identity,
                deno_executable_path, deno_version, deno_lock_digest,
                qualification_receipt_artifact_id, is_current, revision
             ) OVERRIDING SYSTEM VALUE
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, TRUE, $10)",
            build_id,
            &build_digest[..],
            &source_digest[..],
            &binary_digest[..],
            command.schema_identity,
            command.deno_executable_path,
            command.deno_version,
            &deno_lock_digest[..],
            qualification_artifact_id,
            resulting_revision,
        )
        .execute(&mut *transaction)
        .await?;
        if existing_qualification_id.is_none() {
            sqlx::query!(
                "INSERT INTO factory.artifacts (
                id, digest, byte_length, cas_relative_path,
                creating_kernel_build_id
             ) OVERRIDING SYSTEM VALUE
             VALUES ($1, $2, $3, $4, $5)",
                qualification_artifact_id,
                &qualification_digest[..],
                i64::try_from(qualification.byte_length())
                    .map_err(|_| StoreError::ArtifactLengthOutOfRange)?,
                qualification_path.as_str(),
                build_id,
            )
            .execute(&mut *transaction)
            .await?;
        }
        let audit_log_id = insert_audit_receipt(
            &mut transaction,
            &command.principal,
            &command.command_id,
            INSTALL_KERNEL_BUILD_OPERATION,
            fingerprint,
            KERNEL_BUILD_SUBJECT,
            build_id,
            AggregateRevision::from_persisted(
                u64::try_from(resulting_revision).map_err(|_| StoreError::RevisionOutOfRange)?,
            ),
        )
        .await?;
        transaction.commit().await?;
        Ok(KernelBuildReceipt {
            kernel_build_id: command.build_id,
            resulting_revision: AggregateRevision::from_persisted(
                u64::try_from(resulting_revision).map_err(|_| StoreError::RevisionOutOfRange)?,
            ),
            audit_log_id,
            was_idempotent_retry: false,
        })
    }

    /// Seals and installs one fully typed offline build qualification. The
    /// daemon entrypoint can neither publish arbitrary kernel bytes to CAS nor
    /// construct a second, partially overlapping build record.
    pub async fn install_qualified_kernel_build(
        &self,
        cas: &CasStore,
        command: &InstallQualifiedKernelBuild,
    ) -> Result<KernelBuildReceipt, StoreError> {
        let receipt_bytes = command.receipt.encode()?;
        let qualification_receipt = cas.adopt_kernel_bytes(&receipt_bytes)?;
        self.install_kernel_build(
            cas,
            &InstallKernelBuild {
                principal: command.principal.clone(),
                command_id: command.command_id.clone(),
                expected_revision: command.expected_revision,
                build_id: command.receipt.kernel_build_id(),
                source_digest: command.receipt.kernel_source_digest(),
                binary_digest: command.receipt.kernel_binary_digest(),
                schema_identity: command.receipt.schema_identity().to_owned(),
                deno_executable_path: installed_path(
                    "Deno executable",
                    command.receipt.runtime().deno_executable(),
                )?,
                deno_version: command.receipt.runtime().deno_version().to_owned(),
                deno_lock_digest: command.receipt.runtime().deno_lock_digest(),
                qualification_receipt,
            },
        )
        .await
    }

    /// Registers one immutable local repository binding. A later repository
    /// policy change needs its own explicit revision command.
    pub async fn register_repository(
        &self,
        command: &RegisterRepository,
    ) -> Result<RepositoryReceipt, StoreError> {
        command.validate()?;
        let fingerprint = command.fingerprint();
        let mut transaction = self.pool.begin().await?;
        sqlx::query!(
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            command.repository_key
        )
        .execute(&mut *transaction)
        .await?;
        if let Some(receipt) = find_idempotent_audit(
            &mut transaction,
            &command.principal,
            &command.command_id,
            REGISTER_REPOSITORY_OPERATION,
            fingerprint,
        )
        .await?
        {
            require_subject_kind(&receipt, REPOSITORY_SUBJECT)?;
            transaction.commit().await?;
            return Ok(RepositoryReceipt {
                repository_id: RepositoryId::new(receipt.subject_id)?,
                resulting_revision: receipt.resulting_revision,
                audit_log_id: receipt.audit_log_id,
                was_idempotent_retry: true,
            });
        }
        let current = sqlx::query!(
            "SELECT id, revision FROM factory.repositories WHERE repository_key = $1 FOR UPDATE",
            command.repository_key
        )
        .fetch_optional(&mut *transaction)
        .await?;
        let already_registered = current.is_some();
        let current_revision = match current.as_ref() {
            Some(row) => aggregate_revision_from_sql(row.revision)?,
            None => AggregateRevision::initial(),
        };
        if command.expected_revision.get() != current_revision {
            return Err(StoreError::RevisionConflict {
                expected: command.expected_revision,
                current: current_revision,
            });
        }
        if already_registered {
            return Err(StoreError::RepositoryAlreadyRegistered {
                repository_key: command.repository_key.clone(),
            });
        }
        let resulting_revision = current_revision.next()?;
        let repository_id = sqlx::query_scalar!(
            "INSERT INTO factory.repositories (
                repository_key, canonical_local_path, default_branch, delivery_mode, revision
             ) VALUES ($1, $2, $3, 0, $4)
             RETURNING id",
            command.repository_key,
            command.canonical_local_path,
            command.default_branch,
            i64::try_from(resulting_revision.get()).map_err(|_| StoreError::RevisionOutOfRange)?,
        )
        .fetch_one(&mut *transaction)
        .await?;
        let audit_log_id = insert_audit_receipt(
            &mut transaction,
            &command.principal,
            &command.command_id,
            REGISTER_REPOSITORY_OPERATION,
            fingerprint,
            REPOSITORY_SUBJECT,
            repository_id,
            resulting_revision,
        )
        .await?;
        transaction.commit().await?;
        Ok(RepositoryReceipt {
            repository_id: RepositoryId::new(repository_id)?,
            resulting_revision,
            audit_log_id,
            was_idempotent_retry: false,
        })
    }

    /// Records one already-sealed content object. The seal is an opaque
    /// capability returned by [`CasStore::adopt`] or [`CasStore::verify`]; the
    /// storage boundary re-verifies it and derives the canonical path itself.
    pub async fn register_artifact(
        &self,
        cas: &CasStore,
        command: &RegisterArtifact,
    ) -> Result<ArtifactReceipt, StoreError> {
        command.validate()?;
        let verified = cas.verify(command.sealed.digest())?;
        if verified != command.sealed {
            return Err(StoreError::ArtifactSealChanged);
        }
        let fingerprint = command.fingerprint();
        let byte_length = i64::try_from(command.sealed.byte_length())
            .map_err(|_| StoreError::ArtifactLengthOutOfRange)?;
        let build_digest = command.kernel_build_id.digest().as_bytes();
        let artifact_digest = command.sealed.digest().as_bytes();
        let cas_path = cas.object_relative_path(command.sealed.digest())?;
        let mut transaction = self.pool.begin().await?;
        if let Some(receipt) = find_idempotent_audit(
            &mut transaction,
            &command.principal,
            &command.command_id,
            REGISTER_ARTIFACT_OPERATION,
            fingerprint,
        )
        .await?
        {
            require_subject_kind(&receipt, ARTIFACT_SUBJECT)?;
            transaction.commit().await?;
            return Ok(ArtifactReceipt {
                artifact_id: ArtifactId::new(receipt.subject_id)?,
                audit_log_id: receipt.audit_log_id,
                was_idempotent_retry: true,
                was_reused: false,
            });
        }
        let build = sqlx::query!(
            "SELECT id, revision FROM factory.kernel_builds WHERE build_digest = $1",
            &build_digest[..]
        )
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(build) = build else {
            return Err(StoreError::UnknownKernelBuild {
                kernel_build_id: command.kernel_build_id,
            });
        };
        let kernel_build_database_id = build.id;
        let build_revision = aggregate_revision_from_sql(build.revision)?;
        if command.expected_kernel_build_revision.get() != build_revision {
            return Err(StoreError::RevisionConflict {
                expected: command.expected_kernel_build_revision,
                current: build_revision,
            });
        }
        if let Some(existing) = sqlx::query!(
            "SELECT id, byte_length, cas_relative_path, creating_kernel_build_id
             FROM factory.artifacts WHERE digest = $1",
            &artifact_digest[..]
        )
        .fetch_optional(&mut *transaction)
        .await?
        {
            if existing.byte_length != byte_length
                || existing.cas_relative_path != cas_path.as_str()
            {
                return Err(StoreError::ArtifactIdentityConflict {
                    digest: command.sealed.digest(),
                });
            }
            let artifact_id = existing.id;
            // A physical digest reuse is still a distinct authority command.
            // Persist its receipt before returning so a retry of this exact
            // command is distinguishable from another command that happens
            // to name the same immutable object.
            let audit_log_id = insert_audit_receipt(
                &mut transaction,
                &command.principal,
                &command.command_id,
                REGISTER_ARTIFACT_OPERATION,
                fingerprint,
                ARTIFACT_SUBJECT,
                artifact_id,
                build_revision,
            )
            .await?;
            transaction.commit().await?;
            return Ok(ArtifactReceipt {
                artifact_id: ArtifactId::new(artifact_id)?,
                audit_log_id,
                was_idempotent_retry: false,
                was_reused: true,
            });
        }
        let artifact_id: i64 = sqlx::query_scalar!(
            "INSERT INTO factory.artifacts (
                digest, byte_length, cas_relative_path, creating_kernel_build_id
             ) VALUES ($1, $2, $3, $4)
             RETURNING id",
            &artifact_digest[..],
            byte_length,
            cas_path.as_str(),
            kernel_build_database_id,
        )
        .fetch_one(&mut *transaction)
        .await?;
        let audit_log_id = insert_audit_receipt(
            &mut transaction,
            &command.principal,
            &command.command_id,
            REGISTER_ARTIFACT_OPERATION,
            fingerprint,
            ARTIFACT_SUBJECT,
            artifact_id,
            build_revision,
        )
        .await?;
        transaction.commit().await?;
        Ok(ArtifactReceipt {
            artifact_id: ArtifactId::new(artifact_id)?,
            audit_log_id,
            was_idempotent_retry: false,
            was_reused: false,
        })
    }

    /// Derives the installed kernel-build aggregate without writing a receipt.
    pub async fn kernel_build_status(&self) -> Result<KernelBuildStatus, StoreError> {
        let current = sqlx::query!(
            "SELECT build_digest, revision FROM factory.kernel_builds
             WHERE is_current
             LIMIT 1"
        )
        .fetch_optional(&self.pool)
        .await?;
        match current {
            Some(row) => Ok(KernelBuildStatus {
                current_kernel_build_id: Some(KernelBuildId::new(ContentDigest::from_bytes(
                    bytes_to_digest(&row.build_digest)?,
                ))),
                aggregate_revision: aggregate_revision_from_sql(row.revision)?,
            }),
            None => Ok(KernelBuildStatus {
                current_kernel_build_id: None,
                aggregate_revision: AggregateRevision::initial(),
            }),
        }
    }

    /// Resolves the immutable installed build that owns one exact build
    /// revision. Operator evidence retries use this rather than a moving
    /// "current" pointer, so their command fingerprint remains stable across
    /// a later kernel installation.
    pub async fn kernel_build_at_revision(
        &self,
        expected_revision: ExpectedRevision,
    ) -> Result<KernelBuildAtRevision, StoreError> {
        let revision = i64::try_from(expected_revision.get().get())
            .map_err(|_| StoreError::RevisionOutOfRange)?;
        let row = sqlx::query!(
            "SELECT build_digest, revision
             FROM factory.kernel_builds
             WHERE revision = $1",
            revision,
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::UnknownKernelBuildRevision { expected_revision })?;
        Ok(KernelBuildAtRevision {
            kernel_build_id: KernelBuildId::new(ContentDigest::from_bytes(bytes_to_digest(
                &row.build_digest,
            )?)),
            aggregate_revision: aggregate_revision_from_sql(row.revision)?,
        })
    }

    /// Restores the one current installed-build receipt from its durable CAS
    /// seal, proves it still names the current database build, and reruns the
    /// provider-free local material checks. This is the only read path by
    /// which later assignment composition recovers Deno/Pi identity and the
    /// configured credential *name*; secret values never enter this API.
    pub async fn load_current_installed_runtime(
        &self,
        cas: &CasStore,
    ) -> Result<Option<InstalledKernelBuildReceiptV1>, StoreError> {
        let current = sqlx::query!(
            "SELECT kb.build_digest, a.digest AS \"receipt_digest!\",
                    a.byte_length AS \"receipt_byte_length!\", a.cas_relative_path
             FROM factory.kernel_builds AS kb
             JOIN factory.artifacts AS a ON a.id = kb.qualification_receipt_artifact_id
             WHERE kb.is_current
             LIMIT 1"
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some(current) = current else {
            return Ok(None);
        };
        let build_id = KernelBuildId::new(ContentDigest::from_bytes(bytes_to_digest(
            &current.build_digest,
        )?));
        let receipt_digest = ContentDigest::from_bytes(bytes_to_digest(&current.receipt_digest)?);
        let receipt_byte_length = u64::try_from(current.receipt_byte_length)
            .map_err(|_| StoreError::ArtifactLengthOutOfRange)?;
        let expected_relative_path = cas.object_relative_path(receipt_digest)?;
        if current.cas_relative_path != expected_relative_path.as_str() {
            return Err(StoreError::QualificationReceiptChanged);
        }
        let bytes = cas.read_verified(receipt_digest)?;
        if bytes.len() as u64 != receipt_byte_length {
            return Err(StoreError::QualificationReceiptChanged);
        }
        let receipt = InstalledKernelBuildReceiptV1::decode(&bytes)?;
        if receipt.kernel_build_id() != build_id {
            return Err(StoreError::QualificationReceiptChanged);
        }
        receipt.verify_installed_material(SCHEMA_IDENTITY)?;
        Ok(Some(receipt))
    }

    /// Derives current application status without creating a read receipt.
    pub async fn application_status(
        &self,
        application_key: &ApplicationKey,
    ) -> Result<ApplicationStatus, StoreError> {
        let current = sqlx::query!(
            "SELECT id, aggregate_revision
             FROM factory.application_revisions
             WHERE application_key = $1
             ORDER BY aggregate_revision DESC
             LIMIT 1",
            application_key.as_str(),
        )
        .fetch_optional(&self.pool)
        .await?;
        let (application_revision_id, aggregate_revision) = match current {
            Some(row) => (
                Some(ApplicationRevisionId::new(row.id)?),
                aggregate_revision_from_sql(row.aggregate_revision)?,
            ),
            None => (None, AggregateRevision::initial()),
        };
        Ok(ApplicationStatus {
            application_key: application_key.clone(),
            application_revision_id,
            aggregate_revision,
        })
    }

    /// Resolves one registered application revision without inserting a
    /// receipt.  Omitting the revision chooses the newest admitted lineage
    /// entry so an operator can inspect an inert registration before any
    /// activation exists.
    pub async fn application_revision_view(
        &self,
        application_key: &ApplicationKey,
        application_revision_id: Option<ApplicationRevisionId>,
    ) -> Result<ApplicationRevisionView, StoreError> {
        let row = match application_revision_id {
            Some(application_revision_id) => sqlx::query!(
                "SELECT id, aggregate_revision, bundle_artifact_id, is_active
                 FROM factory.application_revisions
                 WHERE application_key = $1 AND id = $2",
                application_key.as_str(),
                application_revision_id.get(),
            )
            .fetch_optional(&self.pool)
            .await?
            .map(|row| {
                (
                    row.id,
                    row.aggregate_revision,
                    row.bundle_artifact_id,
                    row.is_active,
                )
            }),
            None => sqlx::query!(
                "SELECT id, aggregate_revision, bundle_artifact_id, is_active
                 FROM factory.application_revisions
                 WHERE application_key = $1
                 ORDER BY aggregate_revision DESC
                 LIMIT 1",
                application_key.as_str(),
            )
            .fetch_optional(&self.pool)
            .await?
            .map(|row| {
                (
                    row.id,
                    row.aggregate_revision,
                    row.bundle_artifact_id,
                    row.is_active,
                )
            }),
        };
        let (id, aggregate_revision, bundle_artifact_id, is_active) =
            row.ok_or_else(|| StoreError::UnknownApplicationRevisionForKey {
                application_key: application_key.clone(),
                application_revision_id,
            })?;
        Ok(ApplicationRevisionView {
            application_key: application_key.clone(),
            application_revision_id: ApplicationRevisionId::new(id)?,
            aggregate_revision: aggregate_revision_from_sql(aggregate_revision)?,
            bundle_artifact_id: ArtifactId::new(bundle_artifact_id)?,
            is_active,
        })
    }

    /// Checks that every independently created durable authority row has its
    /// matching creation receipt. Non-artifact facts have exactly one;
    /// content-addressed artifact rows may have multiple audited registration
    /// commands when later principals reuse the same immutable bytes. This is
    /// deliberately broader than audit-to-subject validation: a receipt can
    /// point to a real row while a copied database has lost the row's original
    /// creation proof.
    /// Child rows born inside a parent transition (tickets and Forum
    /// attachments) are excluded because they do not mint their own command.
    /// It is a read-only material-state/audit consistency probe.
    pub async fn audit_is_consistent(&self) -> Result<bool, StoreError> {
        Ok(sqlx::query_scalar!(
            "SELECT NOT EXISTS (
                 SELECT 1
                 FROM (
                    SELECT $1::SMALLINT AS subject_kind, id, $2::TEXT AS operation
                    FROM factory.kernel_builds
                    UNION ALL
                    SELECT $3::SMALLINT, id, $4::TEXT
                    FROM factory.application_revisions
                    UNION ALL
                    SELECT $5::SMALLINT, id, $6::TEXT
                    FROM factory.repositories
                    UNION ALL
                    SELECT $7::SMALLINT, a.id, $8::TEXT
                    FROM factory.artifacts AS a
                    WHERE NOT EXISTS (
                        SELECT 1 FROM factory.kernel_builds AS kb
                        WHERE kb.qualification_receipt_artifact_id = a.id
                    )
                    AND NOT EXISTS (
                        SELECT 1 FROM factory.application_revisions AS ar
                        WHERE ar.bundle_artifact_id = a.id
                           OR ar.mission_artifact_id = a.id
                           OR ar.product_research_system_template_artifact_id = a.id
                           OR ar.product_research_assignment_template_artifact_id = a.id
                           OR ar.engineering_system_template_artifact_id = a.id
                           OR ar.engineering_assignment_template_artifact_id = a.id
                           OR ar.quality_system_template_artifact_id = a.id
                           OR ar.quality_assignment_template_artifact_id = a.id
                    )
                    UNION ALL
                    SELECT 4::SMALLINT, id, 'campaign.start'::TEXT
                    FROM factory.campaigns
                    UNION ALL
                    SELECT 5::SMALLINT, id, 'assignment.create'::TEXT
                    FROM factory.assignments
                    UNION ALL
                    SELECT 6::SMALLINT, id, 'session.start'::TEXT
                    FROM factory.sessions
                    UNION ALL
                    SELECT 10::SMALLINT, id,
                           CASE WHEN supersedes_topic_id IS NULL
                                THEN 'forum.topic.create'
                                ELSE 'forum.topic.supersede'
                           END
                    FROM factory.forum_topics
                    UNION ALL
                    SELECT 11::SMALLINT, id,
                           CASE WHEN supersedes_thread_id IS NULL
                                THEN 'forum.thread.create'
                                ELSE 'forum.thread.supersede'
                           END
                    FROM factory.forum_threads
                    UNION ALL
                    SELECT 12::SMALLINT, id, 'forum.post.append'::TEXT
                    FROM factory.forum_posts
                    UNION ALL
                    SELECT 30::SMALLINT, id, 'ticket.propose'::TEXT
                    FROM factory.ticket_revisions
                    UNION ALL
                    SELECT 32::SMALLINT, id, 'ticket.claim'::TEXT
                    FROM factory.ticket_attempts
                    UNION ALL
                    SELECT 40::SMALLINT, id, 'candidate.submit'::TEXT
                    FROM factory.candidates
                    UNION ALL
                    SELECT 41::SMALLINT, id, 'validation.record'::TEXT
                    FROM factory.validations
                    UNION ALL
                    SELECT 42::SMALLINT, id, 'quality.review.submit'::TEXT
                    FROM factory.reviews
                    UNION ALL
                    SELECT 43::SMALLINT, id,
                           CASE decision_kind
                                WHEN 0 THEN 'architect.ticket.sponsor'
                                WHEN 1 THEN 'architect.ticket.release'
                                ELSE 'architect.candidate.decide'
                           END
                    FROM factory.architect_decisions
                    UNION ALL
                    SELECT 44::SMALLINT, id, 'delivery.record'::TEXT
                    FROM factory.deliveries
                 ) AS fact
                 LEFT JOIN factory.audit_log AS audit
                   ON audit.subject_kind = fact.subject_kind
                  AND audit.subject_id = fact.id
                  AND audit.operation = fact.operation
                 GROUP BY fact.subject_kind, fact.id
                 HAVING count(audit.id) = 0
                     OR (fact.subject_kind <> $7::SMALLINT AND count(audit.id) <> 1)
             ) AS \"consistent!\"",
            KERNEL_BUILD_SUBJECT,
            INSTALL_KERNEL_BUILD_OPERATION,
            APPLICATION_REVISION_SUBJECT,
            ADMIT_APPLICATION_REVISION_OPERATION,
            REPOSITORY_SUBJECT,
            REGISTER_REPOSITORY_OPERATION,
            ARTIFACT_SUBJECT,
            REGISTER_ARTIFACT_OPERATION,
        )
        .fetch_one(&self.pool)
        .await?)
    }

    /// Read-only restore gate for a copied PostgreSQL/CAS pair. Before a
    /// resident daemon exposes its socket, prove the expected schema, the
    /// first-line audit/material relation, and every registered CAS identity.
    /// This deliberately performs no migration, repair, adoption, or audit
    /// write: an operator must restore a mutually consistent pair first.
    pub async fn verify_restore_integrity(&self, cas: &CasStore) -> Result<(), StoreError> {
        self.verify_schema_identity().await?;
        if !self.audit_is_consistent().await? {
            return Err(StoreError::RestoreAuditInconsistent);
        }
        if let Some(invalid) = self.first_invalid_audit_subject().await? {
            return Err(StoreError::RestoreAuditSubjectInvalid {
                audit_log_id: invalid.audit_log_id,
                subject_kind: invalid.subject_kind,
                subject_id: invalid.subject_id,
            });
        }
        // Page rather than loading an unbounded artifact set into memory. The
        // query has a fixed shape and identifier ordering; it observes no
        // mutable daemon state and never creates restore bookkeeping.
        let mut after_id = 0_i64;
        loop {
            let rows = sqlx::query!(
                "SELECT id, digest, byte_length, cas_relative_path
                   FROM factory.artifacts
                  WHERE id > $1
                  ORDER BY id ASC
                  LIMIT 128",
                after_id,
            )
            .fetch_all(&self.pool)
            .await?;
            if rows.is_empty() {
                break;
            }
            for row in &rows {
                after_id = row.id;
                let digest = row.digest.clone();
                let digest = ContentDigest::from_bytes(bytes_to_digest(&digest)?);
                let byte_length = row.byte_length;
                let byte_length =
                    u64::try_from(byte_length).map_err(|_| StoreError::ArtifactLengthOutOfRange)?;
                let persisted_path = &row.cas_relative_path;
                let expected_path = cas.object_relative_path(digest)?;
                if persisted_path != expected_path.as_str() {
                    return Err(StoreError::RestoreArtifactPathMismatch { digest });
                }
                let verified = cas.verify(digest)?;
                if verified.byte_length() != byte_length {
                    return Err(StoreError::RestoreArtifactLengthMismatch {
                        digest,
                        expected: byte_length,
                        observed: verified.byte_length(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Returns the first audit receipt whose subject is not part of the
    /// closed durable subject registry or no longer names the row family that
    /// minted it.  This is deliberately separate from [`Self::audit_is_consistent`]:
    /// first-line facts need exactly one receipt, while every receipt needs a
    /// non-ambiguous, extant subject.
    async fn first_invalid_audit_subject(&self) -> Result<Option<InvalidAuditSubject>, StoreError> {
        let row = sqlx::query!(
            "SELECT audit.id, audit.subject_kind, audit.subject_id
             FROM factory.audit_log AS audit
             WHERE NOT (
                 (audit.subject_kind = 0
                  AND audit.operation = 'kernel_build.install'
                  AND EXISTS (
                     SELECT 1 FROM factory.kernel_builds AS subject
                     WHERE subject.id = audit.subject_id
                 ))
                 OR (audit.subject_kind = 1
                     AND audit.operation IN ('application_revision.admit', 'application_revision.activate')
                     AND EXISTS (
                     SELECT 1 FROM factory.application_revisions AS subject
                     WHERE subject.id = audit.subject_id
                 ))
                 OR (audit.subject_kind = 2
                     AND audit.operation = 'repository.register'
                     AND EXISTS (
                     SELECT 1 FROM factory.repositories AS subject
                     WHERE subject.id = audit.subject_id
                 ))
                 OR (audit.subject_kind = 3
                     AND audit.operation = 'artifact.register'
                     AND EXISTS (
                     SELECT 1 FROM factory.artifacts AS subject
                     WHERE subject.id = audit.subject_id
                 ))
                 OR (audit.subject_kind = 4
                     AND audit.operation IN ('campaign.start', 'campaign.cancel', 'campaign.fail')
                     AND EXISTS (
                     SELECT 1 FROM factory.campaigns AS subject
                     WHERE subject.id = audit.subject_id
                 ))
                 OR (audit.subject_kind = 5
                     AND audit.operation = 'assignment.create'
                     AND EXISTS (
                     SELECT 1 FROM factory.assignments AS subject
                     WHERE subject.id = audit.subject_id
                 ))
                 OR (audit.subject_kind = 6
                     AND audit.operation IN ('session.start', 'session.terminal')
                     AND EXISTS (
                     SELECT 1 FROM factory.sessions AS subject
                     WHERE subject.id = audit.subject_id
                 ))
                 OR (audit.subject_kind = 10
                     AND audit.operation IN ('forum.topic.create', 'forum.topic.supersede')
                     AND EXISTS (
                     SELECT 1 FROM factory.forum_topics AS subject
                     WHERE subject.id = audit.subject_id
                 ))
                 OR (audit.subject_kind = 11
                     AND audit.operation IN ('forum.thread.create', 'forum.thread.supersede')
                     AND EXISTS (
                     SELECT 1 FROM factory.forum_threads AS subject
                     WHERE subject.id = audit.subject_id
                 ))
                 OR (audit.subject_kind = 12
                     AND audit.operation = 'forum.post.append'
                     AND EXISTS (
                     SELECT 1 FROM factory.forum_posts AS subject
                     WHERE subject.id = audit.subject_id
                 ))
                 OR (audit.subject_kind = 30
                     AND audit.operation = 'ticket.propose'
                     AND EXISTS (
                     SELECT 1 FROM factory.ticket_revisions AS subject
                     WHERE subject.id = audit.subject_id
                 ))
                 OR (audit.subject_kind = 31
                     AND audit.operation = 'ticket.sponsor'
                     AND EXISTS (
                     SELECT 1 FROM factory.ticket_revisions AS subject
                     WHERE subject.id = audit.subject_id
                 ))
                 OR (audit.subject_kind IN (33, 34)
                     AND audit.operation = 'ticket.claim'
                     AND EXISTS (
                     SELECT 1 FROM factory.ticket_revisions AS subject
                     WHERE subject.id = audit.subject_id
                 ))
                 OR (audit.subject_kind = 32
                     AND audit.operation = 'ticket.claim'
                     AND EXISTS (
                     SELECT 1 FROM factory.ticket_attempts AS subject
                     WHERE subject.id = audit.subject_id
                 ))
                 OR (audit.subject_kind = 35
                     AND audit.operation = 'ticket_attempt.fail'
                     AND EXISTS (
                     SELECT 1 FROM factory.ticket_attempts AS subject
                     WHERE subject.id = audit.subject_id
                 ))
                 OR (audit.subject_kind IN (36, 37, 38)
                     AND audit.operation = 'ticket_attempt.release'
                     AND EXISTS (
                     SELECT 1 FROM factory.ticket_attempts AS subject
                     WHERE subject.id = audit.subject_id
                 ))
                 OR (audit.subject_kind = 39
                     AND audit.operation = 'campaign.complete_delivery_target'
                     AND EXISTS (
                     SELECT 1 FROM factory.campaigns AS subject
                     WHERE subject.id = audit.subject_id
                 ))
                 OR (audit.subject_kind = 40
                     AND audit.operation IN ('candidate.submit', 'candidate.commit.attach')
                     AND EXISTS (
                     SELECT 1 FROM factory.candidates AS subject
                     WHERE subject.id = audit.subject_id
                 ))
                 OR (audit.subject_kind = 41
                     AND audit.operation = 'validation.record'
                     AND EXISTS (
                     SELECT 1 FROM factory.validations AS subject
                     WHERE subject.id = audit.subject_id
                 ))
                 OR (audit.subject_kind = 42
                     AND audit.operation = 'quality.review.submit'
                     AND EXISTS (
                     SELECT 1 FROM factory.reviews AS subject
                     WHERE subject.id = audit.subject_id
                 ))
                 OR (audit.subject_kind = 43
                     AND audit.operation IN ('architect.ticket.sponsor', 'architect.ticket.release', 'architect.candidate.decide')
                     AND EXISTS (
                     SELECT 1 FROM factory.architect_decisions AS subject
                     WHERE subject.id = audit.subject_id
                 ))
                 OR (audit.subject_kind = 44
                     AND audit.operation = 'delivery.record'
                     AND EXISTS (
                     SELECT 1 FROM factory.deliveries AS subject
                     WHERE subject.id = audit.subject_id
                 ))
             )
             ORDER BY audit.id ASC
             LIMIT 1"
        )
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(InvalidAuditSubject {
                audit_log_id: row.id,
                subject_kind: row.subject_kind,
                subject_id: row.subject_id,
            })
        })
        .transpose()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InvalidAuditSubject {
    audit_log_id: i64,
    subject_kind: i16,
    subject_id: i64,
}

/// Held singleton connection. Call [`Self::release`] before orderly shutdown;
/// an ungraceful daemon death closes the PostgreSQL session and releases it.
#[derive(Debug)]
pub struct DaemonLock {
    connection: Option<sqlx::pool::PoolConnection<Postgres>>,
}

impl DaemonLock {
    pub async fn release(mut self) -> Result<(), StoreError> {
        let mut connection = self
            .connection
            .take()
            .ok_or(StoreError::DaemonLockAlreadyReleased)?;
        let released = sqlx::query_scalar!(
            "SELECT pg_advisory_unlock($1) AS \"released!\"",
            DAEMON_ADVISORY_LOCK_KEY
        )
        .fetch_one(&mut *connection)
        .await?;
        if !released {
            return Err(StoreError::DaemonLockLost);
        }
        Ok(())
    }
}

/// Manual installation record for one externally qualified kernel build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallKernelBuild {
    pub principal: String,
    pub command_id: String,
    pub expected_revision: ExpectedRevision,
    pub build_id: KernelBuildId,
    pub source_digest: ContentDigest,
    pub binary_digest: ContentDigest,
    pub schema_identity: String,
    pub deno_executable_path: String,
    pub deno_version: String,
    pub deno_lock_digest: ContentDigest,
    /// Physically sealed qualification evidence. The install operation
    /// verifies it again before opening the durable transaction.
    pub qualification_receipt: CasArtifact,
}

/// Narrow bootstrap command for one closed installed-build receipt. Unlike
/// [`InstallKernelBuild`], callers cannot separately supply its source,
/// binary, Deno, or qualification-artifact facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallQualifiedKernelBuild {
    pub principal: String,
    pub command_id: String,
    pub expected_revision: ExpectedRevision,
    pub receipt: InstalledKernelBuildReceiptV1,
}

impl InstallKernelBuild {
    fn validate(&self) -> Result<(), StoreError> {
        validate_command_component("principal", &self.principal)?;
        validate_command_component("command ID", &self.command_id)?;
        validate_text("schema identity", &self.schema_identity, 160)?;
        if self.schema_identity != SCHEMA_IDENTITY {
            return Err(StoreError::InstalledSchemaIdentityMismatch);
        }
        if !self.deno_executable_path.starts_with('/') {
            return Err(StoreError::InvalidAbsolutePath {
                field: "Deno executable path",
            });
        }
        validate_text("Deno executable path", &self.deno_executable_path, 4096)?;
        validate_text("Deno version", &self.deno_version, 240)?;
        Ok(())
    }

    fn fingerprint(&self) -> ContentDigest {
        let mut hasher = blake3::Hasher::new();
        hasher.update(INSTALL_KERNEL_BUILD_OPERATION.as_bytes());
        hash_string(&mut hasher, &self.principal);
        hash_string(&mut hasher, &self.command_id);
        hasher.update(&self.expected_revision.get().get().to_be_bytes());
        hasher.update(&self.build_id.digest().as_bytes());
        hasher.update(&self.source_digest.as_bytes());
        hasher.update(&self.binary_digest.as_bytes());
        hash_string(&mut hasher, &self.schema_identity);
        hash_string(&mut hasher, &self.deno_executable_path);
        hash_string(&mut hasher, &self.deno_version);
        hasher.update(&self.deno_lock_digest.as_bytes());
        hasher.update(&self.qualification_receipt.digest().as_bytes());
        hasher.update(&self.qualification_receipt.byte_length().to_be_bytes());
        ContentDigest::from_bytes(*hasher.finalize().as_bytes())
    }
}

fn installed_path(field: &'static str, path: &std::path::Path) -> Result<String, StoreError> {
    let value = path
        .to_str()
        .filter(|value| value.starts_with('/') && !value.contains('\0'))
        .ok_or(StoreError::InvalidAbsolutePath { field })?;
    Ok(value.to_owned())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KernelBuildReceipt {
    pub kernel_build_id: KernelBuildId,
    pub resulting_revision: AggregateRevision,
    pub audit_log_id: i64,
    pub was_idempotent_retry: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KernelBuildStatus {
    pub current_kernel_build_id: Option<KernelBuildId>,
    pub aggregate_revision: AggregateRevision,
}

/// Exact immutable kernel build resolved from a caller-observed revision.
/// This is not a current-build status projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KernelBuildAtRevision {
    pub kernel_build_id: KernelBuildId,
    pub aggregate_revision: AggregateRevision,
}

/// First-time registration of one product repository binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisterRepository {
    pub principal: String,
    pub command_id: String,
    pub expected_revision: ExpectedRevision,
    pub repository_key: String,
    pub canonical_local_path: String,
    pub default_branch: String,
}

impl RegisterRepository {
    fn validate(&self) -> Result<(), StoreError> {
        validate_command_component("principal", &self.principal)?;
        validate_command_component("command ID", &self.command_id)?;
        validate_text("repository key", &self.repository_key, 160)?;
        if !self.canonical_local_path.starts_with('/') {
            return Err(StoreError::InvalidAbsolutePath {
                field: "repository path",
            });
        }
        validate_text("repository path", &self.canonical_local_path, 4096)?;
        validate_text("default branch", &self.default_branch, 240)?;
        if self.default_branch.contains(char::is_whitespace)
            || self.default_branch.contains("..")
            || self.default_branch.ends_with('/')
        {
            return Err(StoreError::InvalidDefaultBranch);
        }
        Ok(())
    }

    fn fingerprint(&self) -> ContentDigest {
        let mut hasher = blake3::Hasher::new();
        hasher.update(REGISTER_REPOSITORY_OPERATION.as_bytes());
        hash_string(&mut hasher, &self.principal);
        hash_string(&mut hasher, &self.command_id);
        hasher.update(&self.expected_revision.get().get().to_be_bytes());
        hash_string(&mut hasher, &self.repository_key);
        hash_string(&mut hasher, &self.canonical_local_path);
        hash_string(&mut hasher, &self.default_branch);
        ContentDigest::from_bytes(*hasher.finalize().as_bytes())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryReceipt {
    pub repository_id: RepositoryId,
    pub resulting_revision: AggregateRevision,
    pub audit_log_id: i64,
    pub was_idempotent_retry: bool,
}

/// Durable metadata for bytes already sealed by the CAS custody boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisterArtifact {
    pub principal: String,
    pub command_id: String,
    pub expected_kernel_build_revision: ExpectedRevision,
    pub kernel_build_id: KernelBuildId,
    pub sealed: CasArtifact,
}

impl RegisterArtifact {
    fn validate(&self) -> Result<(), StoreError> {
        validate_command_component("principal", &self.principal)?;
        validate_command_component("command ID", &self.command_id)?;
        Ok(())
    }

    fn fingerprint(&self) -> ContentDigest {
        let mut hasher = blake3::Hasher::new();
        hasher.update(REGISTER_ARTIFACT_OPERATION.as_bytes());
        hash_string(&mut hasher, &self.principal);
        hash_string(&mut hasher, &self.command_id);
        hasher.update(
            &self
                .expected_kernel_build_revision
                .get()
                .get()
                .to_be_bytes(),
        );
        hasher.update(&self.kernel_build_id.digest().as_bytes());
        hasher.update(&self.sealed.digest().as_bytes());
        hasher.update(&self.sealed.byte_length().to_be_bytes());
        ContentDigest::from_bytes(*hasher.finalize().as_bytes())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactReceipt {
    pub artifact_id: ArtifactId,
    pub audit_log_id: i64,
    pub was_idempotent_retry: bool,
    pub was_reused: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationRevisionReceipt {
    pub application_revision_id: ApplicationRevisionId,
    pub resulting_revision: AggregateRevision,
    pub audit_log_id: i64,
    pub was_idempotent_retry: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationStatus {
    pub application_key: ApplicationKey,
    pub application_revision_id: Option<ApplicationRevisionId>,
    pub aggregate_revision: AggregateRevision,
}

/// Exact, bounded read projection for one admitted application revision.
/// It contains only durable identities and the active pointer; the bundle and
/// templates remain in CAS and are never expanded by an operator status read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationRevisionView {
    pub application_key: ApplicationKey,
    pub application_revision_id: ApplicationRevisionId,
    pub aggregate_revision: AggregateRevision,
    pub bundle_artifact_id: ArtifactId,
    pub is_active: bool,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("invalid database URL: {source}")]
    InvalidDatabaseUrl { source: sqlx::Error },

    #[error(transparent)]
    Database(#[from] sqlx::Error),

    #[error(transparent)]
    Migration(#[from] MigrateError),

    #[error(transparent)]
    Contract(#[from] factory_protocol::ContractError),

    #[error(transparent)]
    Cas(#[from] crate::cas::CasError),

    #[error(transparent)]
    InstalledRuntime(#[from] crate::installed_runtime::InstalledRuntimeError),

    #[error("schema identity mismatch: expected {SCHEMA_IDENTITY:?}, observed {observed:?}")]
    SchemaIdentityMismatch { observed: Option<String> },

    #[error("restore audit/material facts are inconsistent")]
    RestoreAuditInconsistent,

    #[error(
        "restore audit receipt {audit_log_id} has an unknown, ambiguous, or orphaned subject ({subject_kind}, {subject_id})"
    )]
    RestoreAuditSubjectInvalid {
        audit_log_id: i64,
        subject_kind: i16,
        subject_id: i64,
    },

    #[error("restore artifact {digest} has a noncanonical persisted CAS path")]
    RestoreArtifactPathMismatch { digest: ContentDigest },

    #[error(
        "restore artifact {digest} length differs: PostgreSQL expects {expected}, CAS contains {observed}"
    )]
    RestoreArtifactLengthMismatch {
        digest: ContentDigest,
        expected: u64,
        observed: u64,
    },

    #[error("PostgreSQL reported an unparseable server version {server_version_num:?}")]
    UnparseablePostgresVersion { server_version_num: String },

    #[error(
        "PostgreSQL {server_version_num} is unsupported; Factory V3 requires PostgreSQL 18 or newer"
    )]
    UnsupportedPostgresVersion { server_version_num: u32 },

    #[error("another factory daemon owns the PostgreSQL singleton lock")]
    DaemonAlreadyRunning,

    #[error("daemon lock has already been released")]
    DaemonLockAlreadyReleased,

    #[error("daemon lock was not owned by this connection")]
    DaemonLockLost,

    #[error("installed kernel build does not name this binary's schema identity")]
    InstalledSchemaIdentityMismatch,

    #[error("{field} must be an absolute non-NUL UTF-8 path")]
    InvalidAbsolutePath { field: &'static str },

    #[error("repository default branch is unsafe")]
    InvalidDefaultBranch,

    #[error("repository {repository_key:?} is already registered")]
    RepositoryAlreadyRegistered { repository_key: String },

    #[error("kernel build {kernel_build_id} is not installed")]
    UnknownKernelBuild { kernel_build_id: KernelBuildId },

    #[error("no current installed kernel build is available for campaign pinning")]
    NoCurrentKernelBuild,

    #[error("kernel build revision {expected_revision:?} is not installed")]
    UnknownKernelBuildRevision { expected_revision: ExpectedRevision },

    #[error("artifact {digest} exists with different immutable metadata")]
    ArtifactIdentityConflict { digest: ContentDigest },

    #[error("the supplied CAS seal no longer verifies")]
    ArtifactSealChanged,

    #[error("the qualification receipt changed after it was sealed")]
    QualificationReceiptChanged,

    #[error("artifact byte length cannot be represented by PostgreSQL BIGINT")]
    ArtifactLengthOutOfRange,

    #[error("artifact media role must be between 0 and 255")]
    InvalidMediaRole,

    #[error("repository binding {repository_key:?} is not registered")]
    UnknownRepositoryBinding { repository_key: String },

    #[error("application bundle digest does not match its CAS object")]
    ApplicationBundleDigestMismatch,

    #[error("application template {path:?} digest does not match its declaration")]
    ApplicationTemplateDigestMismatch { path: String },

    #[error("application bundle must declare exactly seven template artifacts")]
    ApplicationTemplateCountMismatch,

    #[error("application source root is empty")]
    InvalidApplicationSourceRoot,

    #[error("application bundle is not valid closed Rust data: {0}")]
    InvalidApplicationBundle(String),

    #[error("application repository binding does not match the registered repository")]
    RepositoryBindingMismatch,

    #[error("application bundle predecessor does not match the current revision")]
    BundlePredecessorMismatch,

    #[error("application template {path:?} is not valid UTF-8")]
    InvalidTemplateUtf8 { path: String },

    #[error("application template {path:?} has invalid placeholder syntax")]
    InvalidTemplateSyntax { path: String },

    #[error("application template {path:?} has an undeclared or disallowed placeholder")]
    InvalidTemplatePlaceholder { path: String },

    #[error("application template {path:?} omits a declared placeholder")]
    MissingTemplatePlaceholder { path: String },

    #[error("audit receipt subject kind does not match its command operation")]
    AuditSubjectKindMismatch,

    #[error("revision conflict: expected {expected:?}, current {current:?}")]
    RevisionConflict {
        expected: ExpectedRevision,
        current: AggregateRevision,
    },

    #[error("idempotency conflict for principal {principal:?} and command ID {command_id:?}")]
    IdempotencyConflict {
        principal: String,
        command_id: String,
    },

    #[error(
        "application predecessor mismatch: expected {expected_current:?}, supplied {supplied_predecessor:?}"
    )]
    PredecessorMismatch {
        expected_current: Option<ApplicationRevisionId>,
        supplied_predecessor: Option<ApplicationRevisionId>,
    },

    #[error("{field} must use 1 through 160 ASCII letters, digits, '.', ':', '_', or '-'")]
    InvalidCommandComponent { field: &'static str },

    #[error("aggregate revision cannot be represented by PostgreSQL BIGINT")]
    RevisionOutOfRange,

    #[error("database digest column violated its 32-byte invariant")]
    CorruptDigestColumn,

    #[error("invalid process command field: {field}")]
    InvalidProcessCommand { field: &'static str },

    #[error("unknown campaign {campaign_id}")]
    UnknownCampaign {
        campaign_id: factory_protocol::CampaignId,
    },

    #[error("campaign {campaign_id} is no longer running")]
    CampaignClosed {
        campaign_id: factory_protocol::CampaignId,
    },

    #[error("another campaign is already running")]
    CampaignAlreadyRunning,

    #[error(
        "campaign singleton invariant is corrupt: observed {observed_running} running campaigns"
    )]
    RunningCampaignCardinality { observed_running: usize },

    #[error("campaign {campaign_id} has a running paid session")]
    CampaignHasRunningSession {
        campaign_id: factory_protocol::CampaignId,
    },

    #[error("campaign {campaign_id} has frozen cost admission")]
    CampaignCostFrozen {
        campaign_id: factory_protocol::CampaignId,
    },

    #[error("campaign deadline has elapsed")]
    CampaignDeadlineElapsed,

    #[error("unknown application revision {application_revision_id}")]
    UnknownApplicationRevision {
        application_revision_id: factory_protocol::ApplicationRevisionId,
    },

    #[error("application revision {application_revision_id:?} is not active")]
    ApplicationRevisionInactive {
        application_revision_id: factory_protocol::ApplicationRevisionId,
    },

    #[error(
        "application revision {application_revision_id:?} does not belong to application {application_key:?}"
    )]
    UnknownApplicationRevisionForKey {
        application_key: ApplicationKey,
        application_revision_id: Option<factory_protocol::ApplicationRevisionId>,
    },

    #[error("an application cannot be activated while campaign {campaign_id} is running")]
    ApplicationActivationCampaignRunning {
        campaign_id: factory_protocol::CampaignId,
    },

    #[error("application activation rationale does not match a sealed artifact")]
    ApplicationActivationRationaleMismatch,

    #[error("unknown repository {repository_id}")]
    UnknownRepositoryId {
        repository_id: factory_protocol::RepositoryId,
    },

    #[error("unknown assignment {assignment_id}")]
    UnknownAssignment {
        assignment_id: factory_protocol::AssignmentId,
    },

    #[error("unknown session {session_id}")]
    UnknownSession {
        session_id: factory_protocol::SessionId,
    },

    #[error("unknown artifact {artifact_id}")]
    UnknownArtifact {
        artifact_id: factory_protocol::ArtifactId,
    },

    #[error("assignment packet digest is invalid")]
    InvalidPacketDigest,

    #[error("assignment packet identity does not match its campaign/session")]
    PacketIdentityMismatch,

    #[error("packet remaining aggregate allowance does not match campaign state")]
    RemainingAllowanceMismatch,

    #[error("packet artifact digest does not match the packet seal")]
    PacketArtifactDigestMismatch,

    #[error("assignment is not in the prepared state")]
    AssignmentStateConflict {
        assignment_id: factory_protocol::AssignmentId,
    },

    #[error("session is already terminal")]
    SessionAlreadyTerminal {
        session_id: factory_protocol::SessionId,
    },

    #[error("another paid session is already running")]
    PaidSessionAlreadyRunning,

    #[error("terminal operation is not legal for this assignment")]
    TerminalOperationNotAllowed,

    #[error("terminal stop reason and provider cost evidence disagree")]
    TerminalCostMismatch,

    #[error("cost/lifecycle column is corrupt")]
    CorruptLifecycleColumn,

    #[error("campaign cost column is corrupt")]
    CorruptCostColumn,

    #[error("required-read assertion is incomplete")]
    RequiredReadIncomplete,

    #[error("required-read manifest does not belong to the assignment")]
    RequiredReadManifestMismatch,

    #[error("artifact was sealed by a different kernel build")]
    ArtifactBuildMismatch,

    #[error("terminal CAS artifact is not registered")]
    UnregisteredTerminalArtifact,

    #[error("unknown ticket revision {ticket_revision_id}")]
    UnknownTicketRevision {
        ticket_revision_id: factory_protocol::TicketRevisionId,
    },

    #[error("unknown ticket attempt {ticket_attempt_id}")]
    UnknownTicketAttempt {
        ticket_attempt_id: factory_protocol::TicketAttemptId,
    },

    #[error("ticket revision is not in the required {required:?} state (observed {observed:?})")]
    TicketStateConflict {
        required: factory_protocol::TicketState,
        observed: factory_protocol::TicketState,
    },

    #[error("ticket attempt is not in a releasable failed state")]
    TicketAttemptNotReleasable,

    #[error("ticket attempt has already been released")]
    TicketAttemptAlreadyReleased,

    #[error("a reproducible ticket already owns reproducer {reproducer_artifact_id}")]
    DuplicateTicketReproducer {
        reproducer_artifact_id: factory_protocol::ArtifactId,
    },

    #[error("ticket and campaign do not belong to the same application")]
    CampaignApplicationMismatch,

    #[error("ticket proposal observations are not byte-identical")]
    ProposalNotReproducible,

    #[error("ticket proposal reproducer does not demonstrate the declared failure")]
    ProposalDoesNotFail,

    #[error("the unsponsored proposal buffer is at its configured maximum")]
    ProposalBufferFull,

    #[error("the sponsored ready buffer is at its configured maximum")]
    ReadyTicketBufferFull,

    #[error("another ticket is already in flight for this application")]
    EngineeringTicketAlreadyInFlight,

    #[error("ticket requalification evidence is incomplete or inconsistent")]
    InvalidTicketRequalification,

    #[error("campaign delivery target has not been reached")]
    CampaignDeliveryTargetNotReached,

    #[error("invalid ticket authority field: {field}")]
    InvalidTicketField { field: &'static str },

    #[error("ticket authority state stored an invalid closed discriminant")]
    CorruptTicketState,
}

#[derive(Clone, Copy, Debug)]
struct AuditReceipt {
    audit_log_id: i64,
    subject_kind: i16,
    subject_id: i64,
    resulting_revision: AggregateRevision,
}

async fn find_idempotent_audit(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    principal: &str,
    command_id: &str,
    expected_operation: &'static str,
    fingerprint: ContentDigest,
) -> Result<Option<AuditReceipt>, StoreError> {
    let existing = sqlx::query!(
        "SELECT id, operation, command_fingerprint, subject_kind, subject_id, resulting_revision
         FROM factory.audit_log
         WHERE principal = $1 AND command_id = $2",
        principal,
        command_id,
    )
    .fetch_optional(&mut **transaction)
    .await?;
    let Some(row) = existing else {
        return Ok(None);
    };
    let operation = row.operation;
    let stored_fingerprint = row.command_fingerprint;
    if operation != expected_operation || stored_fingerprint.as_slice() != fingerprint.as_bytes() {
        return Err(StoreError::IdempotencyConflict {
            principal: principal.to_owned(),
            command_id: command_id.to_owned(),
        });
    }
    Ok(Some(AuditReceipt {
        audit_log_id: row.id,
        subject_kind: row.subject_kind,
        subject_id: row.subject_id,
        resulting_revision: aggregate_revision_from_sql(row.resulting_revision)?,
    }))
}

async fn insert_audit_receipt(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    principal: &str,
    command_id: &str,
    operation: &'static str,
    fingerprint: ContentDigest,
    subject_kind: i16,
    subject_id: i64,
    resulting_revision: AggregateRevision,
) -> Result<i64, StoreError> {
    let fingerprint_bytes = fingerprint.as_bytes();
    Ok(sqlx::query_scalar!(
        "INSERT INTO factory.audit_log (
             principal, command_id, operation, command_fingerprint,
             subject_kind, subject_id, resulting_revision
         ) VALUES ($1, $2, $3, $4, $5, $6, $7)
         RETURNING id",
        principal,
        command_id,
        operation,
        &fingerprint_bytes[..],
        subject_kind,
        subject_id,
        i64::try_from(resulting_revision.get()).map_err(|_| StoreError::RevisionOutOfRange)?,
    )
    .fetch_one(&mut **transaction)
    .await?)
}

fn require_subject_kind(receipt: &AuditReceipt, expected: i16) -> Result<(), StoreError> {
    if receipt.subject_kind == expected {
        Ok(())
    } else {
        Err(StoreError::AuditSubjectKindMismatch)
    }
}

fn validate_command_component(field: &'static str, value: &str) -> Result<(), StoreError> {
    if value.is_empty()
        || value.len() > 160
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'_' | b'-'))
    {
        return Err(StoreError::InvalidCommandComponent { field });
    }
    Ok(())
}

fn validate_text(field: &'static str, value: &str, maximum: usize) -> Result<(), StoreError> {
    if value.is_empty() || value.len() > maximum || value.contains('\0') {
        return Err(StoreError::InvalidCommandComponent { field });
    }
    Ok(())
}

fn hash_string(hasher: &mut blake3::Hasher, value: &str) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

fn aggregate_revision_from_sql(value: i64) -> Result<AggregateRevision, StoreError> {
    u64::try_from(value)
        .map(AggregateRevision::from_persisted)
        .map_err(|_| StoreError::RevisionOutOfRange)
}

pub(crate) fn aggregate_revision_from_sql_for_process(
    value: i64,
) -> Result<AggregateRevision, StoreError> {
    aggregate_revision_from_sql(value)
}

fn bytes_to_digest(bytes: &[u8]) -> Result<[u8; 32], StoreError> {
    bytes
        .try_into()
        .map_err(|_| StoreError::CorruptDigestColumn)
}
