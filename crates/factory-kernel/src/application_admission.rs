//! Authoritative admission of one compiled application bundle.
//!
//! The daemon supplies an assigned staging root and a physical [`CasStore`].
//! This module reads the bundle through CAS, parses it into the closed
//! protocol domain, verifies each of the seven declared Markdown bytes, and
//! performs the artifact/revision/audit transition in one PostgreSQL
//! transaction. It never loads application code or accepts raw artifact IDs.

use std::path::PathBuf;

use factory_protocol::{
    ApplicationBundleV1, ApplicationRelativePath, AssignmentRole, ContentDigest, ExpectedRevision,
};
use sqlx::{Postgres, Transaction};

use super::{
    ADMIT_APPLICATION_REVISION_OPERATION, APPLICATION_REVISION_SUBJECT, CasArtifact, CasStore,
    KernelStore, REGISTER_REPOSITORY_OPERATION, REPOSITORY_SUBJECT, RegisterRepository, StoreError,
    aggregate_revision_from_sql, find_idempotent_audit, insert_audit_receipt, require_subject_kind,
};
use crate::storage::ApplicationRevisionReceipt;

/// The assigned physical inputs for one application admission. The bundle
/// path is relative to `source_root`; neither field is accepted from a raw
/// application callback or used to construct a database connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmitCompiledApplication {
    pub principal: String,
    pub command_id: String,
    pub expected_revision: ExpectedRevision,
    pub expected_kernel_build_revision: ExpectedRevision,
    pub kernel_build_id: factory_protocol::KernelBuildId,
    pub source_root: PathBuf,
    pub bundle_relative_path: PathBuf,
}

impl AdmitCompiledApplication {
    fn validate(&self) -> Result<(), StoreError> {
        super::validate_command_component("principal", &self.principal)?;
        super::validate_command_component("command ID", &self.command_id)?;
        if self.source_root.as_os_str().is_empty() {
            return Err(StoreError::InvalidApplicationSourceRoot);
        }
        ApplicationRelativePath::parse(self.bundle_relative_path.to_string_lossy().to_string())
            .map_err(StoreError::Contract)?;
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct SealedApplicationInputs {
    bundle: ApplicationBundleV1,
    bundle_seal: CasArtifact,
    mission_seal: CasArtifact,
    template_seals: Vec<CasArtifact>,
}

impl KernelStore {
    /// Reads, parses, and admits an application bundle using only the
    /// assigned source root and CAS custody. All physical objects are sealed
    /// before the transaction; a later database failure therefore leaves only
    /// safe, unreferenced append-only objects. The first admitted application
    /// for a repository atomically establishes the exact immutable binding
    /// declared by its bundle; later admissions must match it.
    pub async fn admit_compiled_application(
        &self,
        cas: &CasStore,
        command: &AdmitCompiledApplication,
    ) -> Result<ApplicationRevisionReceipt, StoreError> {
        command.validate()?;
        let inputs = seal_application_inputs(cas, command)?;
        let fingerprint = application_fingerprint(command, &inputs);
        let application_key = inputs.bundle.application_key.clone();

        let mut transaction = self.pool.begin().await?;
        sqlx::query!(
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            application_key.as_str()
        )
        .execute(&mut *transaction)
        .await?;

        if let Some(receipt) = find_idempotent_audit(
            &mut transaction,
            &command.principal,
            &command.command_id,
            ADMIT_APPLICATION_REVISION_OPERATION,
            fingerprint,
        )
        .await?
        {
            require_subject_kind(&receipt, APPLICATION_REVISION_SUBJECT)?;
            transaction.commit().await?;
            return Ok(ApplicationRevisionReceipt {
                application_revision_id: factory_protocol::ApplicationRevisionId::new(
                    receipt.subject_id,
                )?,
                resulting_revision: receipt.resulting_revision,
                audit_log_id: receipt.audit_log_id,
                was_idempotent_retry: true,
            });
        }

        let build_digest = command.kernel_build_id.digest().as_bytes();
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
        let build_database_id = build.id;
        let build_revision = aggregate_revision_from_sql(build.revision)?;
        if command.expected_kernel_build_revision.get() != build_revision {
            return Err(StoreError::RevisionConflict {
                expected: command.expected_kernel_build_revision,
                current: build_revision,
            });
        }

        let repository_key = inputs.bundle.repository.repository_key.as_str();
        sqlx::query!(
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            repository_key
        )
        .execute(&mut *transaction)
        .await?;
        let repository = sqlx::query!(
            "SELECT id, canonical_local_path, default_branch, delivery_mode
             FROM factory.repositories WHERE repository_key = $1",
            repository_key
        )
        .fetch_optional(&mut *transaction)
        .await?;
        let repository_id = match repository {
            Some(repository) => {
                if repository.canonical_local_path
                    != inputs.bundle.repository.canonical_local_path.as_str()
                    || repository.default_branch != inputs.bundle.repository.default_branch
                    || repository.delivery_mode != 0
                {
                    return Err(StoreError::RepositoryBindingMismatch);
                }
                repository.id
            }
            None => {
                let repository_command = RegisterRepository {
                    principal: command.principal.clone(),
                    command_id: format!("application-repository-{}", fingerprint.to_hex()),
                    expected_revision: ExpectedRevision::new(
                        factory_protocol::AggregateRevision::initial(),
                    ),
                    repository_key: inputs.bundle.repository.repository_key.clone(),
                    canonical_local_path: inputs
                        .bundle
                        .repository
                        .canonical_local_path
                        .as_str()
                        .to_owned(),
                    default_branch: inputs.bundle.repository.default_branch.clone(),
                };
                repository_command.validate()?;
                let repository_revision = factory_protocol::AggregateRevision::initial().next()?;
                let repository_id = sqlx::query_scalar!(
                    "INSERT INTO factory.repositories (
                         repository_key, canonical_local_path, default_branch,
                         delivery_mode, revision
                     ) VALUES ($1, $2, $3, 0, $4)
                     RETURNING id",
                    repository_command.repository_key,
                    repository_command.canonical_local_path,
                    repository_command.default_branch,
                    i64::try_from(repository_revision.get())
                        .map_err(|_| StoreError::RevisionOutOfRange)?,
                )
                .fetch_one(&mut *transaction)
                .await?;
                insert_audit_receipt(
                    &mut transaction,
                    &repository_command.principal,
                    &repository_command.command_id,
                    REGISTER_REPOSITORY_OPERATION,
                    repository_command.fingerprint(),
                    REPOSITORY_SUBJECT,
                    repository_id,
                    repository_revision,
                )
                .await?;
                repository_id
            }
        };

        let current = sqlx::query!(
            "SELECT ar.id, ar.aggregate_revision, ba.digest AS bundle_digest
             FROM factory.application_revisions ar
             JOIN factory.artifacts ba ON ba.id = ar.bundle_artifact_id
             WHERE ar.application_key = $1
             ORDER BY ar.aggregate_revision DESC LIMIT 1 FOR UPDATE",
            application_key.as_str()
        )
        .fetch_optional(&mut *transaction)
        .await?;
        let (current_id, current_revision, current_bundle) = match current {
            Some(row) => (
                Some(row.id),
                aggregate_revision_from_sql(row.aggregate_revision)?,
                Some(ContentDigest::from_bytes(super::bytes_to_digest(
                    row.bundle_digest.as_slice(),
                )?)),
            ),
            None => (None, factory_protocol::AggregateRevision::initial(), None),
        };
        if command.expected_revision.get() != current_revision {
            return Err(StoreError::RevisionConflict {
                expected: command.expected_revision,
                current: current_revision,
            });
        }
        if inputs.bundle.predecessor_bundle != current_bundle {
            return Err(StoreError::BundlePredecessorMismatch);
        }

        let resulting_revision = current_revision.next()?;
        let bundle_id = register_artifact_in_transaction(
            &mut transaction,
            cas,
            build_database_id,
            inputs.bundle_seal,
        )
        .await?;
        let mission_id = register_artifact_in_transaction(
            &mut transaction,
            cas,
            build_database_id,
            inputs.mission_seal,
        )
        .await?;
        let mut template_ids = Vec::with_capacity(inputs.template_seals.len());
        for seal in inputs.template_seals.iter().copied() {
            template_ids.push(
                register_artifact_in_transaction(&mut transaction, cas, build_database_id, seal)
                    .await?,
            );
        }

        let [
            product_system,
            product_assignment,
            engineering_system,
            engineering_assignment,
            quality_system,
            quality_assignment,
        ]: [i64; 6] = template_ids
            .try_into()
            .map_err(|_| StoreError::ApplicationTemplateCountMismatch)?;
        let ticket_policy = &inputs.bundle.ticket_policy;
        let application_revision_id: i64 = sqlx::query_scalar!(
            "INSERT INTO factory.application_revisions (
                 application_key, aggregate_revision,
                 predecessor_application_revision_id, bundle_artifact_id,
                 mission_artifact_id,
                 product_research_system_template_artifact_id,
                 product_research_assignment_template_artifact_id,
                 engineering_system_template_artifact_id,
                 engineering_assignment_template_artifact_id,
                 quality_system_template_artifact_id,
                 quality_assignment_template_artifact_id,
                 repository_id, ticket_low_water, ticket_target, ticket_maximum,
                 proposal_maximum, ticket_narrative_byte_limit,
                 ticket_acceptance_criteria_limit, ticket_contract_read_limit
             ) VALUES (
                 $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                 $17, $18, $19
             ) RETURNING id",
            application_key.as_str(),
            i64::try_from(resulting_revision.get()).map_err(|_| StoreError::RevisionOutOfRange)?,
            current_id,
            bundle_id,
            mission_id,
            product_system,
            product_assignment,
            engineering_system,
            engineering_assignment,
            quality_system,
            quality_assignment,
            repository_id,
            i32::from(ticket_policy.low_water),
            i32::from(ticket_policy.target),
            i32::from(ticket_policy.maximum),
            i32::from(ticket_policy.proposal_maximum),
            i32::try_from(ticket_policy.ticket_bounds.narrative_byte_limit)
                .map_err(|_| StoreError::RevisionOutOfRange)?,
            i32::from(ticket_policy.ticket_bounds.acceptance_criteria_limit),
            i32::from(ticket_policy.ticket_bounds.contract_read_limit),
        )
        .fetch_one(&mut *transaction)
        .await?;
        // Every admitted application revision receives its three fixed root
        // offices in the same transaction as the immutable bundle identity.
        // The office row is the durable owner; the role remains the closed
        // packet capability selected by the kernel.
        sqlx::query!(
            "INSERT INTO factory.offices (
                 application_revision_id, assignment_role, charter_artifact_id, authority_mask
             ) VALUES
                 ($1, 0, $2, 1),
                 ($1, 1, $3, 2),
                 ($1, 2, $4, 4)
             ON CONFLICT (application_revision_id, assignment_role) DO NOTHING",
            application_revision_id,
            product_system,
            engineering_system,
            quality_system,
        )
        .execute(&mut *transaction)
        .await?;
        let audit_log_id = insert_audit_receipt(
            &mut transaction,
            &command.principal,
            &command.command_id,
            ADMIT_APPLICATION_REVISION_OPERATION,
            fingerprint,
            APPLICATION_REVISION_SUBJECT,
            application_revision_id,
            resulting_revision,
        )
        .await?;
        transaction.commit().await?;
        Ok(ApplicationRevisionReceipt {
            application_revision_id: factory_protocol::ApplicationRevisionId::new(
                application_revision_id,
            )?,
            resulting_revision,
            audit_log_id,
            was_idempotent_retry: false,
        })
    }
}

fn seal_application_inputs(
    cas: &CasStore,
    command: &AdmitCompiledApplication,
) -> Result<SealedApplicationInputs, StoreError> {
    let bundle_seal = cas.adopt(&command.source_root, &command.bundle_relative_path)?;
    let bundle_bytes = cas.read_verified(bundle_seal.digest())?;
    let (bundle, bundle_digest) = factory_protocol::admit_application_bundle_v1(&bundle_bytes)
        .map_err(|error| StoreError::InvalidApplicationBundle(error.to_string()))?;
    if bundle_digest != bundle_seal.digest() {
        return Err(StoreError::ApplicationBundleDigestMismatch);
    }
    let mission_seal =
        adopt_declared_template(cas, &command.source_root, &bundle.mission_template, None)?;
    let mut seals = Vec::with_capacity(6);
    for office in AssignmentRole::ALL {
        let profile = bundle
            .assignment_role_profiles
            .iter()
            .find(|profile| profile.assignment_role == office)
            .ok_or(StoreError::ApplicationTemplateCountMismatch)?;
        seals.push(adopt_declared_template(
            cas,
            &command.source_root,
            &profile.system_template,
            Some(office),
        )?);
        seals.push(adopt_declared_template(
            cas,
            &command.source_root,
            &profile.assignment_template,
            Some(office),
        )?);
    }
    Ok(SealedApplicationInputs {
        bundle,
        bundle_seal,
        mission_seal,
        template_seals: seals,
    })
}

fn adopt_declared_template(
    cas: &CasStore,
    source_root: &std::path::Path,
    template: &factory_protocol::TemplateArtifactV1,
    assignment_role: Option<AssignmentRole>,
) -> Result<CasArtifact, StoreError> {
    let seal = cas.adopt(source_root, template.source_path.as_str())?;
    if seal.digest() != template.digest {
        return Err(StoreError::ApplicationTemplateDigestMismatch {
            path: template.source_path.as_str().to_owned(),
        });
    }
    let bytes = cas.read_verified(seal.digest())?;
    validate_template_bytes(&bytes, template, assignment_role)?;
    Ok(seal)
}

fn validate_template_bytes(
    bytes: &[u8],
    template: &factory_protocol::TemplateArtifactV1,
    assignment_role: Option<AssignmentRole>,
) -> Result<(), StoreError> {
    let source = std::str::from_utf8(bytes).map_err(|_| StoreError::InvalidTemplateUtf8 {
        path: template.source_path.as_str().to_owned(),
    })?;
    let declared = template
        .placeholders
        .iter()
        .map(|placeholder| placeholder.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let allowed = allowed_placeholders(assignment_role);
    let mut found = std::collections::BTreeSet::new();
    let mut cursor = 0;
    while let Some(offset) = source[cursor..].find("${") {
        let start = cursor + offset;
        let end = source[start + 2..]
            .find('}')
            .map(|value| start + 2 + value)
            .ok_or_else(|| StoreError::InvalidTemplateSyntax {
                path: template.source_path.as_str().to_owned(),
            })?;
        let name = &source[start + 2..end];
        if name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
            || !declared.contains(name)
            || !allowed.contains(name)
        {
            return Err(StoreError::InvalidTemplatePlaceholder {
                path: template.source_path.as_str().to_owned(),
            });
        }
        found.insert(name);
        cursor = end + 1;
    }
    if found.len() != declared.len() {
        return Err(StoreError::MissingTemplatePlaceholder {
            path: template.source_path.as_str().to_owned(),
        });
    }
    Ok(())
}

fn allowed_placeholders(
    assignment_role: Option<AssignmentRole>,
) -> std::collections::BTreeSet<&'static str> {
    let mut allowed: std::collections::BTreeSet<&'static str> =
        ["ASSIGNMENT_ID", "MISSION", "TARGET"].into_iter().collect();
    match assignment_role {
        Some(AssignmentRole::ProductResearch) => {}
        Some(AssignmentRole::Engineering) => allowed.extend([
            "TICKET_ID",
            "TICKET_REVISION_ID",
            "REGRESSION_COMMAND",
            "REGRESSION_EXPECTED_FAILURE",
        ]),
        Some(AssignmentRole::Quality) => allowed.extend([
            "TICKET_ID",
            "TICKET_REVISION_ID",
            "CANDIDATE_ID",
            "VALIDATION_ID",
        ]),
        None => {}
    }
    allowed
}

fn application_fingerprint(
    command: &AdmitCompiledApplication,
    inputs: &SealedApplicationInputs,
) -> ContentDigest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(ADMIT_APPLICATION_REVISION_OPERATION.as_bytes());
    super::hash_string(&mut hasher, &command.principal);
    super::hash_string(&mut hasher, &command.command_id);
    hasher.update(&command.expected_revision.get().get().to_be_bytes());
    hasher.update(
        &command
            .expected_kernel_build_revision
            .get()
            .get()
            .to_be_bytes(),
    );
    hasher.update(&command.kernel_build_id.digest().as_bytes());
    super::hash_string(&mut hasher, inputs.bundle.application_key.as_str());
    hasher.update(&inputs.bundle_seals());
    ContentDigest::from_bytes(*hasher.finalize().as_bytes())
}

impl SealedApplicationInputs {
    fn bundle_seals(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(32 * 8);
        for seal in std::iter::once(self.bundle_seal)
            .chain(std::iter::once(self.mission_seal))
            .chain(self.template_seals.iter().copied())
        {
            bytes.extend_from_slice(&seal.digest().as_bytes());
            bytes.extend_from_slice(&seal.byte_length().to_be_bytes());
        }
        bytes
    }
}

async fn register_artifact_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    cas: &CasStore,
    build_database_id: i64,
    seal: CasArtifact,
) -> Result<i64, StoreError> {
    let path = cas.object_relative_path(seal.digest())?;
    let digest = seal.digest().as_bytes();
    let existing = sqlx::query!(
        "SELECT id, byte_length, cas_relative_path
         FROM factory.artifacts WHERE digest = $1",
        &digest[..]
    )
    .fetch_optional(&mut **transaction)
    .await?;
    if let Some(existing) = existing {
        let existing_length = existing.byte_length;
        let existing_path = existing.cas_relative_path;
        if existing_length
            != i64::try_from(seal.byte_length())
                .map_err(|_| StoreError::ArtifactLengthOutOfRange)?
            || existing_path != path.as_str()
        {
            return Err(StoreError::ArtifactIdentityConflict {
                digest: seal.digest(),
            });
        }
        return Ok(existing.id);
    }
    let artifact_id: i64 = sqlx::query_scalar!(
        "INSERT INTO factory.artifacts
         (digest, byte_length, cas_relative_path, creating_kernel_build_id)
         VALUES ($1, $2, $3, $4) RETURNING id",
        &digest[..],
        i64::try_from(seal.byte_length()).map_err(|_| StoreError::ArtifactLengthOutOfRange)?,
        path.as_str(),
        build_database_id,
    )
    .fetch_one(&mut **transaction)
    .await?;
    Ok(artifact_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(1);

    fn make_template(
        path: &str,
        source: &[u8],
        placeholders: &[&str],
    ) -> (CasStore, PathBuf, factory_protocol::TemplateArtifactV1) {
        let root = std::env::temp_dir().join(format!(
            "factory-application-admission-{}-{}",
            std::process::id(),
            NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("test root");
        let cas = CasStore::new_with_seed(root.join("runtime"), 4096, 1).expect("CAS");
        let source_root = root.join("source");
        fs::create_dir(&source_root).expect("source root");
        fs::write(source_root.join(path), source).expect("template source");
        let template = factory_protocol::TemplateArtifactV1 {
            source_path: ApplicationRelativePath::parse(path).expect("path"),
            digest: ContentDigest::of_bytes(source),
            placeholders: placeholders
                .iter()
                .map(|value| {
                    factory_protocol::TemplatePlaceholderV1::parse(*value).expect("placeholder")
                })
                .collect(),
            rendered_byte_limit: 4096,
        };
        (cas, source_root, template)
    }

    #[test]
    fn declared_template_digest_mismatch_is_rejected() {
        let (cas, source_root, mut template) = make_template("template.md", b"actual", &[]);
        template.digest = ContentDigest::of_bytes(b"different");
        assert!(matches!(
            adopt_declared_template(&cas, &source_root, &template, None),
            Err(StoreError::ApplicationTemplateDigestMismatch { .. })
        ));
    }

    #[test]
    fn template_unknown_and_missing_placeholders_are_rejected() {
        let (cas, source_root, template) =
            make_template("template.md", b"${UNKNOWN}", &["UNKNOWN"]);
        assert!(matches!(
            adopt_declared_template(&cas, &source_root, &template, None),
            Err(StoreError::InvalidTemplatePlaceholder { .. })
        ));

        let (cas, source_root, template) = make_template("template.md", b"plain", &["MISSION"]);
        assert!(matches!(
            adopt_declared_template(&cas, &source_root, &template, None),
            Err(StoreError::MissingTemplatePlaceholder { .. })
        ));
    }

    #[test]
    fn template_cannot_require_unavailable_assignment_identity() {
        for (office, source, placeholder) in [
            (
                AssignmentRole::ProductResearch,
                b"${SESSION_ID}".as_slice(),
                "SESSION_ID",
            ),
            (
                AssignmentRole::ProductResearch,
                b"${CAMPAIGN_ID}".as_slice(),
                "CAMPAIGN_ID",
            ),
            (
                AssignmentRole::Engineering,
                b"${APPLICATION_REVISION_ID}".as_slice(),
                "APPLICATION_REVISION_ID",
            ),
            (AssignmentRole::Quality, b"${OFFICE}".as_slice(), "OFFICE"),
            (
                AssignmentRole::ProductResearch,
                b"${TICKET_ID}".as_slice(),
                "TICKET_ID",
            ),
            (
                AssignmentRole::ProductResearch,
                b"${TICKET_REVISION_ID}".as_slice(),
                "TICKET_REVISION_ID",
            ),
            (
                AssignmentRole::Engineering,
                b"${CANDIDATE_ID}".as_slice(),
                "CANDIDATE_ID",
            ),
        ] {
            let (cas, source_root, template) = make_template("template.md", source, &[placeholder]);
            assert!(matches!(
                adopt_declared_template(&cas, &source_root, &template, Some(office)),
                Err(StoreError::InvalidTemplatePlaceholder { .. })
            ));
        }
    }

    #[test]
    fn template_malformed_utf8_and_syntax_are_rejected() {
        let (cas, source_root, template) = make_template("template.md", b"${MISSION", &["MISSION"]);
        assert!(matches!(
            adopt_declared_template(&cas, &source_root, &template, None),
            Err(StoreError::InvalidTemplateSyntax { .. })
        ));

        let (cas, source_root, template) = make_template("template.md", &[0xff], &[]);
        assert!(matches!(
            adopt_declared_template(&cas, &source_root, &template, None),
            Err(StoreError::InvalidTemplateUtf8 { .. })
        ));
    }
}
