//! PostgreSQL-backed admission judge for the actual XSH application bundle.
//!
//! The generic kernel never imports XSH policy. This test stays at the one
//! intended boundary: Deno compiles the real closed application declaration,
//! then typed Rust custody admits the emitted bytes and the seven declared
//! template files. The temporary source root prevents the test from writing a
//! generated bundle into the checked-out application directory.

use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use factory_kernel::{
    cas::CasStore,
    storage::{
        ActivateApplicationRevision, AdmitCompiledApplication, InstallKernelBuild, KernelStore,
        RegisterArtifact, SCHEMA_IDENTITY,
    },
};
use factory_protocol::{
    AggregateRevision, ApplicationBundleV1, ArchitectPrincipalV1, ContentDigest, ExpectedRevision,
    KernelBuildId, Office, SealedArtifactReferenceV1, TemplateArtifactV1,
    parse_application_bundle_v1,
};
use sqlx::{PgPool, Row};

static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(1);

#[test]
#[ignore = "requires FACTORY_TEST_DATABASE_URL and a populated Deno 2.9.4 frozen cache"]
fn real_xsh_bundle_compiles_twice_and_admits_through_typed_cas_authority() {
    smol::block_on(async {
        let workspace_root = workspace_root();
        let deno = deno_executable();
        let deno_version = require_deno_2_9_4(&deno);
        let first_bundle = compile_xsh_bundle(&workspace_root, &deno);
        let second_bundle = compile_xsh_bundle(&workspace_root, &deno);
        assert_eq!(
            first_bundle, second_bundle,
            "XSH bundle bytes must be canonical"
        );

        let bundle = parse_application_bundle_v1(&first_bundle)
            .expect("Deno compiler output must be a canonical V1 application bundle");
        assert_eq!(bundle.application_key.as_str(), "xsh");
        let root = temporary_root("xsh-bundle-admission");
        let source_root = root.join("application-source");
        let templates = copy_exact_xsh_inputs(&workspace_root, &source_root, &bundle);
        fs::write(source_root.join("bundle.v1.json"), &first_bundle)
            .expect("write generated bundle only to the temporary source root");

        let database_url = test_database_url();
        let store = KernelStore::connect(&database_url)
            .await
            .expect("connect disposable PostgreSQL database");
        store
            .migrate_and_verify()
            .await
            .expect("migrate canonical fresh PostgreSQL fixture");

        let cas = CasStore::new_with_seed(root.join("runtime"), 4 * 1024 * 1024, 17)
            .expect("create isolated CAS");
        let build = install_kernel_build(&store, &cas, &deno, &deno_version, &workspace_root).await;
        let admitted = store
            .admit_compiled_application(
                &cas,
                &AdmitCompiledApplication {
                    principal: "operator".to_owned(),
                    command_id: unique("admit-real-xsh-bundle"),
                    expected_revision: ExpectedRevision::new(AggregateRevision::initial()),
                    expected_kernel_build_revision: ExpectedRevision::new(build.resulting_revision),
                    kernel_build_id: build.kernel_build_id,
                    source_root: source_root.clone(),
                    bundle_relative_path: PathBuf::from("bundle.v1.json"),
                },
            )
            .await
            .expect("admit the XSH bundle and its first repository binding atomically");
        assert!(!admitted.was_idempotent_retry);

        activate_xsh_revision(
            &store,
            &cas,
            &build,
            &bundle,
            admitted.application_revision_id,
            admitted.resulting_revision,
        )
        .await;
        let view = store
            .active_application_view(
                &bundle.application_key,
                Some(admitted.application_revision_id),
            )
            .await
            .expect("active XSH application view");
        assert!(view.bundle_artifact_id.get() > 0);
        assert!(
            view.is_active,
            "typed activation must select the admitted XSH revision"
        );
        assert!(
            store
                .audit_is_consistent()
                .await
                .expect("read application material-state audit consistency"),
            "the derived repository binding must retain its own creation receipt"
        );

        assert_repository_binding_and_audit(&database_url, &bundle).await;

        assert_sealed_template_references(
            &database_url,
            &cas,
            admitted.application_revision_id.get(),
            &templates,
        )
        .await;

        store.close().await;
        fs::remove_dir_all(root).expect("remove isolated XSH bundle fixture");
    });
}

#[derive(Clone, Debug)]
struct ExpectedTemplate {
    source_path: String,
    digest: ContentDigest,
    bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
struct InstalledBuild {
    kernel_build_id: KernelBuildId,
    resulting_revision: AggregateRevision,
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("factory-kernel lives beneath the workspace root")
        .to_owned()
}

fn deno_executable() -> PathBuf {
    if let Some(path) = env::var_os("DENO").map(PathBuf::from) {
        return fs::canonicalize(path).expect("DENO must name an executable Deno path");
    }
    env::var_os("PATH")
        .as_deref()
        .and_then(|paths| {
            env::split_paths(paths)
                .map(|directory| directory.join("deno"))
                .find(|candidate| candidate.is_file())
        })
        .and_then(|path| fs::canonicalize(path).ok())
        .expect("Deno 2.9.4 must be available through DENO or PATH")
}

fn require_deno_2_9_4(deno: &Path) -> String {
    let output = Command::new(deno)
        .arg("--version")
        .env("DENO_NO_UPDATE_CHECK", "1")
        .output()
        .expect("run Deno version probe");
    assert!(
        output.status.success(),
        "Deno version probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let version = String::from_utf8(output.stdout).expect("Deno version is UTF-8");
    let version_fields = version
        .lines()
        .next()
        .expect("Deno version has a first line")
        .split_whitespace()
        .collect::<Vec<_>>();
    assert_eq!(
        version_fields.get(0),
        Some(&"deno"),
        "Deno version output must begin with its executable name"
    );
    assert_eq!(
        version_fields.get(1),
        Some(&"2.9.4"),
        "this qualification must use the pinned Deno 2.9.4 compiler"
    );
    version
}

fn compile_xsh_bundle(workspace_root: &Path, deno: &Path) -> Vec<u8> {
    let output = Command::new(deno)
        .current_dir(workspace_root)
        .args([
            "run",
            "--allow-read",
            "--no-prompt",
            "--frozen",
            "--cached-only",
            "--config",
        ])
        .arg(workspace_root.join("deno.json"))
        .arg("--lock")
        .arg(workspace_root.join("deno.lock"))
        .arg(workspace_root.join("applications/xsh/mod.ts"))
        .env("DENO_NO_UPDATE_CHECK", "1")
        .env("NO_COLOR", "1")
        .output()
        .expect("run the XSH application compiler");
    assert!(
        output.status.success(),
        "frozen cached-only XSH bundle compilation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn copy_exact_xsh_inputs(
    workspace_root: &Path,
    temporary_source_root: &Path,
    bundle: &ApplicationBundleV1,
) -> Vec<ExpectedTemplate> {
    let application_root = workspace_root.join("applications/xsh");
    let mut declared = Vec::with_capacity(7);
    declared.push(&bundle.mission_template);
    for office in Office::ALL {
        let profile = bundle
            .office_profiles
            .iter()
            .find(|profile| profile.office == office)
            .expect("canonical XSH bundle has every fixed office");
        declared.push(&profile.system_template);
        declared.push(&profile.assignment_template);
    }
    assert_eq!(
        declared.len(),
        7,
        "XSH declares exactly seven template inputs"
    );

    let mut paths = BTreeSet::new();
    declared
        .into_iter()
        .map(|template| {
            copy_declared_template(
                &application_root,
                temporary_source_root,
                template,
                &mut paths,
            )
        })
        .collect()
}

fn copy_declared_template(
    application_root: &Path,
    temporary_source_root: &Path,
    template: &TemplateArtifactV1,
    paths: &mut BTreeSet<String>,
) -> ExpectedTemplate {
    let source_path = template.source_path.as_str();
    assert!(
        paths.insert(source_path.to_owned()),
        "duplicate template path {source_path}"
    );
    let bytes =
        fs::read(application_root.join(source_path)).expect("read real XSH template source");
    assert_eq!(
        ContentDigest::of_bytes(&bytes),
        template.digest,
        "compiled XSH bundle digest must match {source_path}"
    );
    let copied = temporary_source_root.join(source_path);
    fs::create_dir_all(
        copied
            .parent()
            .expect("declared application template has a parent directory"),
    )
    .expect("create temporary template parent");
    fs::write(&copied, &bytes).expect("copy exact XSH template into temporary admission root");
    ExpectedTemplate {
        source_path: source_path.to_owned(),
        digest: template.digest,
        bytes,
    }
}

async fn install_kernel_build(
    store: &KernelStore,
    cas: &CasStore,
    deno: &Path,
    deno_version: &str,
    workspace_root: &Path,
) -> InstalledBuild {
    let staging = cas.runtime_root().join("staging");
    fs::create_dir_all(&staging).expect("kernel-build staging root");
    fs::write(
        staging.join("qualification.txt"),
        b"provider-free XSH bundle admission judge\n",
    )
    .expect("kernel-build qualification receipt");
    let qualification_receipt = cas
        .adopt(&staging, "qualification.txt")
        .expect("seal kernel-build qualification receipt");
    let receipt = store
        .install_kernel_build(
            cas,
            &InstallKernelBuild {
                principal: "operator".to_owned(),
                command_id: unique("install-kernel-build"),
                expected_revision: ExpectedRevision::new(
                    store
                        .kernel_build_status()
                        .await
                        .expect("kernel-build status")
                        .aggregate_revision,
                ),
                build_id: KernelBuildId::new(digest("kernel-build")),
                source_digest: digest("kernel-source"),
                binary_digest: digest("kernel-binary"),
                schema_identity: SCHEMA_IDENTITY.to_owned(),
                deno_executable_path: deno.to_string_lossy().into_owned(),
                deno_version: deno_version.to_owned(),
                deno_lock_digest: ContentDigest::of_bytes(
                    &fs::read(workspace_root.join("deno.lock")).expect("read root Deno lock"),
                ),
                qualification_receipt,
            },
        )
        .await
        .expect("install provider-free typed kernel build");
    InstalledBuild {
        kernel_build_id: receipt.kernel_build_id,
        resulting_revision: receipt.resulting_revision,
    }
}

async fn activate_xsh_revision(
    store: &KernelStore,
    cas: &CasStore,
    build: &InstalledBuild,
    bundle: &ApplicationBundleV1,
    application_revision_id: factory_protocol::ApplicationRevisionId,
    application_revision: AggregateRevision,
) {
    let staging = cas.runtime_root().join("staging");
    fs::write(
        staging.join("activation-rationale.txt"),
        b"activate exact admitted XSH bundle\n",
    )
    .expect("activation rationale");
    let sealed = cas
        .adopt(&staging, "activation-rationale.txt")
        .expect("seal activation rationale");
    let rationale_artifact = store
        .register_artifact(
            cas,
            &RegisterArtifact {
                principal: "operator".to_owned(),
                command_id: unique("register-activation-rationale"),
                expected_kernel_build_revision: ExpectedRevision::new(build.resulting_revision),
                kernel_build_id: build.kernel_build_id,
                sealed,
            },
        )
        .await
        .expect("register typed activation rationale");
    let activation = store
        .activate_application_revision(&ActivateApplicationRevision {
            principal: ArchitectPrincipalV1::parse("architect").expect("Architect principal"),
            command_id: unique("activate-xsh-bundle"),
            expected_revision: ExpectedRevision::new(application_revision),
            application_key: bundle.application_key.clone(),
            application_revision_id,
            rationale: SealedArtifactReferenceV1 {
                artifact_id: rationale_artifact.artifact_id,
                digest: sealed.digest(),
                byte_length: sealed.byte_length(),
            },
        })
        .await
        .expect("activate exact admitted XSH revision");
    assert!(activation.is_active);
}

async fn assert_repository_binding_and_audit(database_url: &str, bundle: &ApplicationBundleV1) {
    let pool = PgPool::connect(database_url)
        .await
        .expect("open repository inspection connection");
    let row = sqlx::query(
        "SELECT r.id, r.canonical_local_path, r.default_branch, r.delivery_mode,
                count(a.id) AS audit_count
         FROM factory.repositories AS r
         JOIN factory.audit_log AS a
           ON a.subject_kind = 2 AND a.subject_id = r.id
          AND a.operation = 'repository.register'
         WHERE r.repository_key = $1
         GROUP BY r.id",
    )
    .bind(bundle.repository.repository_key.as_str())
    .fetch_one(&pool)
    .await
    .expect("read the atomically admitted repository binding and receipt");
    assert_eq!(
        row.try_get::<String, _>("canonical_local_path")
            .expect("repository path"),
        bundle.repository.canonical_local_path.as_str()
    );
    assert_eq!(
        row.try_get::<String, _>("default_branch")
            .expect("repository branch"),
        bundle.repository.default_branch
    );
    assert_eq!(
        row.try_get::<i16, _>("delivery_mode")
            .expect("repository delivery mode"),
        0
    );
    assert_eq!(
        row.try_get::<i64, _>("audit_count")
            .expect("repository audit count"),
        1
    );
    pool.close().await;
}

async fn assert_sealed_template_references(
    database_url: &str,
    cas: &CasStore,
    application_revision_id: i64,
    expected: &[ExpectedTemplate],
) {
    // Mutation has already gone through `KernelStore`; this independent,
    // read-only connection is solely the integration test's exact PostgreSQL
    // inspection of the seven fixed reference columns.  The public operator
    // projection deliberately does not expand template identities.
    let pool = PgPool::connect(database_url)
        .await
        .expect("open read-only inspection connection to disposable database");
    let references = sqlx::query(
        "SELECT mission_artifact_id,
                product_research_system_template_artifact_id,
                product_research_assignment_template_artifact_id,
                engineering_system_template_artifact_id,
                engineering_assignment_template_artifact_id,
                quality_system_template_artifact_id,
                quality_assignment_template_artifact_id
         FROM factory.application_revisions WHERE id = $1",
    )
    .bind(application_revision_id)
    .fetch_one(&pool)
    .await
    .expect("read sealed template references after typed admission");
    let artifact_ids = [
        references
            .try_get::<i64, _>("mission_artifact_id")
            .expect("mission artifact reference"),
        references
            .try_get::<i64, _>("product_research_system_template_artifact_id")
            .expect("Product system artifact reference"),
        references
            .try_get::<i64, _>("product_research_assignment_template_artifact_id")
            .expect("Product assignment artifact reference"),
        references
            .try_get::<i64, _>("engineering_system_template_artifact_id")
            .expect("Engineering system artifact reference"),
        references
            .try_get::<i64, _>("engineering_assignment_template_artifact_id")
            .expect("Engineering assignment artifact reference"),
        references
            .try_get::<i64, _>("quality_system_template_artifact_id")
            .expect("Quality system artifact reference"),
        references
            .try_get::<i64, _>("quality_assignment_template_artifact_id")
            .expect("Quality assignment artifact reference"),
    ];
    assert_eq!(artifact_ids.len(), expected.len());

    for (artifact_id, template) in artifact_ids.into_iter().zip(expected) {
        let artifact =
            sqlx::query("SELECT digest, byte_length FROM factory.artifacts WHERE id = $1")
                .bind(artifact_id)
                .fetch_one(&pool)
                .await
                .expect("template reference must resolve to a sealed artifact");
        let digest_bytes: Vec<u8> = artifact.try_get("digest").expect("artifact digest bytes");
        let digest_bytes: [u8; 32] = digest_bytes
            .as_slice()
            .try_into()
            .expect("artifact digest has BLAKE3 length");
        let digest = ContentDigest::from_bytes(digest_bytes);
        let byte_length: i64 = artifact
            .try_get("byte_length")
            .expect("artifact byte length");
        assert_eq!(
            digest,
            template.digest,
            "sealed {path} digest differs from the real XSH declaration",
            path = template.source_path
        );
        assert_eq!(
            byte_length,
            i64::try_from(template.bytes.len()).expect("template byte length fits PostgreSQL"),
            "sealed {path} byte length differs from the real XSH source",
            path = template.source_path
        );
        assert_eq!(
            cas.read_verified(digest)
                .expect("read verified sealed template"),
            template.bytes,
            "sealed {path} CAS bytes differ from the real XSH source",
            path = template.source_path
        );
    }
    pool.close().await;
}

fn temporary_root(label: &str) -> PathBuf {
    let root = env::temp_dir().join(format!(
        "factory-{label}-{}-{}",
        std::process::id(),
        NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("create isolated test root");
    root
}

fn test_database_url() -> String {
    let database_url = env::var("FACTORY_TEST_DATABASE_URL")
        .expect("FACTORY_TEST_DATABASE_URL must name a disposable factory v12 PostgreSQL database");
    let database_name = database_url
        .rsplit('/')
        .next()
        .and_then(|value| value.split('?').next())
        .expect("database URL has a final path component");
    assert!(
        database_name
            .strip_prefix("factory_test_v3_")
            .is_some_and(|suffix| {
                !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
            }),
        "FACTORY_TEST_DATABASE_URL must name exactly factory_test_v3_<digits>"
    );
    database_url
}

fn digest(label: &str) -> ContentDigest {
    ContentDigest::of_bytes(label.as_bytes())
}

fn unique(label: &str) -> String {
    format!("{label}-{}", NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed))
}
