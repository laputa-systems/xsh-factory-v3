//! Offline restore-pair qualification and negative-corruption judge.
//!
//! The Deno operator wrapper creates the exact blank clone, restores the
//! custom PostgreSQL dump, and copies the append-only CAS before this test is
//! invoked.  This test never creates or drops a database.  Its one valid-pair
//! call is read-only; the following short-lived clone-only corruptions prove
//! that the same gate rejects missing, corrupt, and mismatched evidence.

use std::{env, fs, path::PathBuf};

use factory_kernel::{
    cas::{CasError, CasStore},
    storage::{KernelStore, StoreError},
};
use factory_protocol::ContentDigest;
use sqlx::Row;

#[test]
#[ignore = "requires FACTORY_RESTORE_DATABASE_URL and FACTORY_RESTORE_RUNTIME_ROOT"]
fn restored_database_and_cas_are_integrity_qualified() {
    smol::block_on(async {
        let database_url = restore_database_url();
        let runtime_root = restore_runtime_root();
        let store = KernelStore::connect(&database_url)
            .await
            .expect("connect the already-restored database");
        let cas = CasStore::with_default_limit(&runtime_root, 0x6261_636b_7570_7633)
            .expect("open the already-copied restore CAS");

        // This is the actual serving preflight: no SQL or filesystem mutation
        // occurs in `verify_restore_integrity`.
        store
            .verify_restore_integrity(&cas)
            .await
            .expect("valid restored PostgreSQL/CAS pair must qualify read-only");
        let restored_runtime = store
            .load_current_installed_runtime(&cas)
            .await
            .expect("read-only restored installed-build receipt must load")
            .expect("restored database must retain one installed-build receipt");
        // `load_current_installed_runtime` decodes the receipt from the
        // restored CAS, binds it to the current durable build identity, and
        // reruns the local Deno/Pi source, executable, and frozen-cache
        // checks. This keeps a structurally sound artifact ledger from being
        // mistaken for a restorable installed runtime.
        restored_runtime
            .verify_installed_material(factory_kernel::storage::SCHEMA_IDENTITY)
            .expect("restored installed material must requalify locally");

        // The remaining mutations are constrained to the disposable clone.
        // They exercise failure behavior that a valid operator restore cannot
        // otherwise produce, and each one is restored before the next probe.
        let inspection = sqlx::PgPool::connect(&database_url)
            .await
            .expect("clone-only corruption inspection pool");
        let artifact = sqlx::query(
            "SELECT id, digest, cas_relative_path
             FROM factory.artifacts
             WHERE byte_length > 0
             ORDER BY id ASC
             LIMIT 1",
        )
        .fetch_optional(&inspection)
        .await
        .expect("read one nonempty restored artifact")
        .expect("an installed build must retain a nonempty artifact");
        let artifact_id: i64 = artifact.try_get("id").expect("artifact id");
        let digest_bytes: Vec<u8> = artifact.try_get("digest").expect("artifact digest");
        let digest = ContentDigest::from_bytes(
            digest_bytes
                .as_slice()
                .try_into()
                .expect("artifact digest has 32 bytes"),
        );
        let canonical_path: String = artifact
            .try_get("cas_relative_path")
            .expect("persisted CAS path");
        let object_path = cas.object_path(digest);
        let original_bytes = fs::read(&object_path).expect("read restored CAS object");
        assert!(
            !original_bytes.is_empty(),
            "selected CAS object is nonempty"
        );

        let mut corrupted_bytes = original_bytes.clone();
        corrupted_bytes[0] ^= 0xFF;
        fs::write(&object_path, corrupted_bytes).expect("corrupt clone CAS object");
        assert!(matches!(
            store.verify_restore_integrity(&cas).await,
            Err(StoreError::Cas(CasError::CorruptObject { .. }))
        ));
        fs::write(&object_path, &original_bytes).expect("restore clone CAS bytes");
        store
            .verify_restore_integrity(&cas)
            .await
            .expect("repaired clone CAS object qualifies");

        let missing_path = object_path.with_file_name(format!(
            "{}.backup-restore-missing",
            object_path
                .file_name()
                .and_then(|name| name.to_str())
                .expect("CAS object has a UTF-8 digest name")
        ));
        fs::rename(&object_path, &missing_path).expect("hide clone CAS object");
        assert!(matches!(
            store.verify_restore_integrity(&cas).await,
            Err(StoreError::Cas(CasError::MissingObject { .. }))
        ));
        fs::rename(&missing_path, &object_path).expect("restore clone CAS object path");
        store
            .verify_restore_integrity(&cas)
            .await
            .expect("restored clone CAS path qualifies");

        sqlx::query("UPDATE factory.artifacts SET cas_relative_path = $1 WHERE id = $2")
            .bind("objects/blake3/00/not-the-recorded-digest")
            .bind(artifact_id)
            .execute(&inspection)
            .await
            .expect("alter clone-only persisted CAS path");
        assert!(matches!(
            store.verify_restore_integrity(&cas).await,
            Err(StoreError::RestoreArtifactPathMismatch { .. })
        ));
        sqlx::query("UPDATE factory.artifacts SET cas_relative_path = $1 WHERE id = $2")
            .bind(&canonical_path)
            .bind(artifact_id)
            .execute(&inspection)
            .await
            .expect("restore clone persisted CAS path");
        store
            .verify_restore_integrity(&cas)
            .await
            .expect("canonical clone CAS path qualifies");

        let audit = sqlx::query(
            "SELECT id, principal, command_id, operation, command_fingerprint,
                    subject_kind, subject_id, resulting_revision, accepted_at::TEXT AS accepted_at
             FROM factory.audit_log
             WHERE subject_kind = $1 AND operation = $2
             ORDER BY id ASC
             LIMIT 1",
        )
        .bind(0_i16)
        .bind("kernel_build.install")
        .fetch_optional(&inspection)
        .await
        .expect("read installed-build audit receipt")
        .expect("restored build must retain its audit receipt");
        let audit_id: i64 = audit.try_get("id").expect("audit id");
        let principal: String = audit.try_get("principal").expect("audit principal");
        let command_id: String = audit.try_get("command_id").expect("audit command id");
        let operation: String = audit.try_get("operation").expect("audit operation");
        let command_fingerprint: Vec<u8> = audit
            .try_get("command_fingerprint")
            .expect("audit fingerprint");
        let subject_kind: i16 = audit.try_get("subject_kind").expect("audit subject kind");
        let subject_id: i64 = audit.try_get("subject_id").expect("audit subject id");
        let resulting_revision: i64 = audit
            .try_get("resulting_revision")
            .expect("audit resulting revision");
        let accepted_at: String = audit.try_get("accepted_at").expect("audit acceptance time");
        sqlx::query("DELETE FROM factory.audit_log WHERE id = $1")
            .bind(audit_id)
            .execute(&inspection)
            .await
            .expect("delete clone-only audit receipt");
        assert!(matches!(
            store.verify_restore_integrity(&cas).await,
            Err(StoreError::RestoreAuditInconsistent)
        ));
        let expected_principal = principal.clone();
        let expected_command_id = command_id.clone();
        let expected_fingerprint = command_fingerprint.clone();
        let expected_operation = operation.clone();
        let expected_accepted_at = accepted_at.clone();
        sqlx::query(
            "INSERT INTO factory.audit_log (
                id, principal, command_id, operation, command_fingerprint,
                subject_kind, subject_id, resulting_revision, accepted_at
             ) OVERRIDING SYSTEM VALUE
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::TIMESTAMPTZ)",
        )
        .bind(audit_id)
        .bind(principal)
        .bind(command_id)
        .bind(&operation)
        .bind(command_fingerprint)
        .bind(subject_kind)
        .bind(subject_id)
        .bind(resulting_revision)
        .bind(accepted_at)
        .execute(&inspection)
        .await
        .expect("restore clone audit receipt");
        let restored_audit = sqlx::query(
            "SELECT principal, command_id, operation, command_fingerprint,
                    subject_kind, subject_id, resulting_revision, accepted_at::TEXT AS accepted_at
             FROM factory.audit_log
             WHERE id = $1",
        )
        .bind(audit_id)
        .fetch_one(&inspection)
        .await
        .expect("read restored clone audit receipt");
        assert_eq!(
            restored_audit.try_get::<String, _>("principal").unwrap(),
            expected_principal
        );
        assert_eq!(
            restored_audit.try_get::<String, _>("command_id").unwrap(),
            expected_command_id
        );
        assert_eq!(
            restored_audit.try_get::<String, _>("operation").unwrap(),
            expected_operation
        );
        assert_eq!(
            restored_audit
                .try_get::<Vec<u8>, _>("command_fingerprint")
                .unwrap(),
            expected_fingerprint
        );
        assert_eq!(
            restored_audit.try_get::<i16, _>("subject_kind").unwrap(),
            subject_kind
        );
        assert_eq!(
            restored_audit.try_get::<i64, _>("subject_id").unwrap(),
            subject_id
        );
        assert_eq!(
            restored_audit
                .try_get::<i64, _>("resulting_revision")
                .unwrap(),
            resulting_revision
        );
        assert_eq!(
            restored_audit.try_get::<String, _>("accepted_at").unwrap(),
            expected_accepted_at
        );
        store
            .verify_restore_integrity(&cas)
            .await
            .expect("fully repaired restored pair qualifies");

        // A downstream authority row needs its own creation receipt too.
        // The receipt→subject registry check alone would not detect this
        // loss, because the surviving candidate row remains perfectly valid.
        let candidate_audit = sqlx::query(
            "SELECT audit.id, audit.principal, audit.command_id, audit.operation,
                    audit.command_fingerprint, audit.subject_kind, audit.subject_id,
                    audit.resulting_revision, audit.accepted_at::TEXT AS accepted_at
               FROM factory.candidates AS candidate
               JOIN factory.audit_log AS audit
                 ON audit.subject_kind = 40
                AND audit.subject_id = candidate.id
                AND audit.operation = 'candidate.submit'
              ORDER BY candidate.id ASC
              LIMIT 1",
        )
        .fetch_optional(&inspection)
        .await
        .expect("read optional candidate creation receipt");
        if let Some(candidate_audit) = candidate_audit {
            let candidate_audit_id: i64 =
                candidate_audit.try_get("id").expect("candidate audit id");
            let candidate_principal: String = candidate_audit
                .try_get("principal")
                .expect("candidate audit principal");
            let candidate_command_id: String = candidate_audit
                .try_get("command_id")
                .expect("candidate audit command id");
            let candidate_operation: String = candidate_audit
                .try_get("operation")
                .expect("candidate audit operation");
            let candidate_fingerprint: Vec<u8> = candidate_audit
                .try_get("command_fingerprint")
                .expect("candidate audit fingerprint");
            let candidate_subject_kind: i16 = candidate_audit
                .try_get("subject_kind")
                .expect("candidate audit subject kind");
            let candidate_subject_id: i64 = candidate_audit
                .try_get("subject_id")
                .expect("candidate audit subject id");
            let candidate_revision: i64 = candidate_audit
                .try_get("resulting_revision")
                .expect("candidate audit revision");
            let candidate_accepted_at: String = candidate_audit
                .try_get("accepted_at")
                .expect("candidate audit acceptance time");
            sqlx::query("DELETE FROM factory.audit_log WHERE id = $1")
                .bind(candidate_audit_id)
                .execute(&inspection)
                .await
                .expect("delete candidate creation receipt from clone");
            assert!(matches!(
                store.verify_restore_integrity(&cas).await,
                Err(StoreError::RestoreAuditInconsistent)
            ));
            sqlx::query(
                "INSERT INTO factory.audit_log (
                id, principal, command_id, operation, command_fingerprint,
                subject_kind, subject_id, resulting_revision, accepted_at
             ) OVERRIDING SYSTEM VALUE
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::TIMESTAMPTZ)",
            )
            .bind(candidate_audit_id)
            .bind(candidate_principal)
            .bind(candidate_command_id)
            .bind(candidate_operation)
            .bind(candidate_fingerprint)
            .bind(candidate_subject_kind)
            .bind(candidate_subject_id)
            .bind(candidate_revision)
            .bind(candidate_accepted_at)
            .execute(&inspection)
            .await
            .expect("restore candidate creation receipt in clone");
            store
                .verify_restore_integrity(&cas)
                .await
                .expect("restored candidate audit receipt qualifies");
        }

        let audit_sequence =
            sqlx::query("SELECT last_value, is_called FROM factory.audit_log_id_seq")
                .fetch_one(&inspection)
                .await
                .expect("read clone audit sequence before corruption probe");
        let audit_sequence_value: i64 = audit_sequence
            .try_get("last_value")
            .expect("audit sequence value");
        let audit_sequence_called: bool = audit_sequence
            .try_get("is_called")
            .expect("audit sequence call state");
        // An extra receipt does not alter a first-line material fact, so it
        // reaches the closed subject/operation-family check rather than the
        // earlier exact-first-line audit count check.
        let invalid_audit_id: i64 = sqlx::query_scalar(
            "INSERT INTO factory.audit_log (
                principal, command_id, operation, command_fingerprint,
                subject_kind, subject_id, resulting_revision
             ) VALUES ($1, $2, $3, $4, $5, $6, $7)
             RETURNING id",
        )
        .bind("backup-restore-corruption-probe")
        .bind(format!("invalid-operation-{audit_id}"))
        .bind("forum.topic.create")
        .bind(vec![0_u8; 32])
        .bind(subject_kind)
        .bind(subject_id)
        .bind(resulting_revision)
        .fetch_one(&inspection)
        .await
        .expect("insert clone-only invalid audit receipt");
        assert!(matches!(
            store.verify_restore_integrity(&cas).await,
            Err(StoreError::RestoreAuditSubjectInvalid { .. })
        ));
        sqlx::query("DELETE FROM factory.audit_log WHERE id = $1")
            .bind(invalid_audit_id)
            .execute(&inspection)
            .await
            .expect("remove clone-only invalid audit receipt");
        sqlx::query_scalar::<_, i64>("SELECT setval('factory.audit_log_id_seq', $1, $2)")
            .bind(audit_sequence_value)
            .bind(audit_sequence_called)
            .fetch_one(&inspection)
            .await
            .expect("restore clone audit sequence");
        let restored_sequence =
            sqlx::query("SELECT last_value, is_called FROM factory.audit_log_id_seq")
                .fetch_one(&inspection)
                .await
                .expect("read repaired clone audit sequence");
        assert_eq!(
            restored_sequence.try_get::<i64, _>("last_value").unwrap(),
            audit_sequence_value
        );
        assert_eq!(
            restored_sequence.try_get::<bool, _>("is_called").unwrap(),
            audit_sequence_called
        );
        store
            .verify_restore_integrity(&cas)
            .await
            .expect("removed invalid audit receipt restores qualification");

        inspection.close().await;
        store.close().await;
    });
}

fn restore_database_url() -> String {
    let url = env::var("FACTORY_RESTORE_DATABASE_URL")
        .expect("FACTORY_RESTORE_DATABASE_URL must name the disposable restore database");
    let database_name = url
        .rsplit('/')
        .next()
        .and_then(|part| part.split('?').next())
        .expect("database URL has a final path component");
    assert!(
        database_name
            .strip_prefix("factory_restore_v3_")
            .is_some_and(
                |suffix| !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
            ),
        "FACTORY_RESTORE_DATABASE_URL must name exactly factory_restore_v3_<digits>"
    );
    url
}

fn restore_runtime_root() -> PathBuf {
    let root = PathBuf::from(
        env::var("FACTORY_RESTORE_RUNTIME_ROOT")
            .expect("FACTORY_RESTORE_RUNTIME_ROOT must name the copied restore runtime root"),
    );
    assert!(root.is_absolute(), "restore runtime root must be absolute");
    assert!(root.is_dir(), "restore runtime root must already exist");
    root
}
