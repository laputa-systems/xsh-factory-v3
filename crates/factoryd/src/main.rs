//! Resident Unix-only Factory daemon.
//!
//! The binary receives the database URL only at startup, opens the kernel's
//! fixed pool, verifies the installed schema, acquires both singleton locks,
//! then serves the local operator socket. It intentionally exposes no TCP or
//! HTTP listener.

use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
    time::{Duration, SystemTime},
};

use factory_kernel::{
    campaign_driver::{CampaignDriver, CampaignDriverOutcome},
    cas::CasStore,
    durable_authority::DurableAuthorityResolver,
    installed_runtime::{
        InstalledApprovedToolsQualificationV1, InstalledKernelBuildReceiptV1,
        InstalledRuntimeManifest, InstalledRuntimeQualification, qualify_kernel_binary_v1,
        qualify_kernel_source_v1,
    },
    local_transport::{LocalDaemon, LocalTransportConfig},
    restart_recovery::{RestartRecoveryPolicy, reconcile_daemon_restart},
    storage::{InstallQualifiedKernelBuild, KernelBuildStatus, KernelStore, SCHEMA_IDENTITY},
};
use factory_protocol::{AggregateRevision, ExpectedRevision, KernelBuildId, RuntimeRelativePath};

fn main() -> ExitCode {
    tracing_subscriber::fmt().with_target(false).init();
    match parse_args(env::args().skip(1).collect()) {
        Ok(command) => match smol::block_on(run(command)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                tracing::error!(%error, "factoryd stopped");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("factoryd: {error}\n{}", usage());
            ExitCode::FAILURE
        }
    }
}

async fn run(command: DaemonCommand) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        DaemonCommand::Serve(args) => run_serve(args).await,
        DaemonCommand::Init(args) => run_init(args).await,
    }
}

async fn run_serve(args: DaemonArgs) -> Result<(), Box<dyn std::error::Error>> {
    let store = KernelStore::connect(&args.database_url).await?;
    // Resident startup verifies the explicitly installed schema. It does not
    // apply migrations or turn a normal daemon launch into an upgrade action.
    store.verify_schema_identity().await?;
    let cas_runtime_root = args.runtime_root.clone();
    // A resident daemon is admitted only after the current database build can
    // restore its exact sealed receipt and requalify every local kernel/Deno
    // input. This happens before `LocalDaemon::bind` exposes an operator
    // socket, so runtime drift fails closed rather than failing a paid launch.
    let build_status = store.kernel_build_status().await?;
    let current_build = build_status
        .current_kernel_build_id
        .ok_or_else(|| init_error("factoryd serve requires one installed kernel build"))?;
    let seed = CasStore::temporary_name_seed(current_build, std::process::id(), SystemTime::now())?;
    let cas = Arc::new(CasStore::with_default_limit(cas_runtime_root, seed)?);
    // A restored database and CAS directory are one provenance unit. Refuse
    // before binding the local socket if either audit/material facts or any
    // registered byte identity no longer agrees with the other.
    store.verify_restore_integrity(cas.as_ref()).await?;
    let installed = store
        .load_current_installed_runtime(cas.as_ref())
        .await?
        .ok_or_else(|| {
            init_error("factoryd serve found no sealed current installed-build receipt")
        })?;
    let running_binary = qualify_kernel_binary_v1(&env::current_exe()?)?
        .path()
        .to_owned();
    verify_serve_preflight(&installed, current_build, &running_binary)?;
    // The receipt is the only source of Cargo/Git/Deno paths. Reconstructing
    // these services here makes an executable or toolchain drift fail before
    // the daemon exposes operator authority or can launch a paid actor.
    let execution = installed.execution_tools(&args.runtime_root.join("git"))?;
    let authority_resolver = Arc::new(DurableAuthorityResolver::new(
        store.clone(),
        cas.as_ref().clone(),
        execution.command_runner().clone(),
        execution.git_custody(),
    ));
    let architect_resolver: Arc<dyn factory_kernel::operator_rpc::ArchitectTransitionResolver> =
        authority_resolver.clone();
    let config = LocalTransportConfig::new(args.runtime_root)
        .with_deadlines(args.read_deadline, args.operation_deadline)
        .with_write_deadline(args.write_deadline);
    // The only durable Architect authority is attached to the already-bound
    // mode-0600 operator socket. `factoryd` retains the database connection;
    // neither factoryctl nor an actor receives a database write surface.
    let daemon = LocalDaemon::bind(config, &store)
        .await?
        .with_architect_control(store.decision_store())
        .with_architect_transition_resolver(architect_resolver)
        .with_campaign_control(store.process_store(), store.ticket_store())
        .with_navigation_control(store.clone())
        .with_forum_control(store.forum_store())
        // Application source paths cross only this authenticated Unix socket;
        // daemon-side Rust/CAS re-reads every byte under the source root.
        .with_application_control(store.clone(), Arc::clone(&cas))
        // Rationale and other operator evidence use the same local custody
        // boundary: factoryctl supplies only one rooted relative filename.
        .with_operator_artifact_control(store.clone(), Arc::clone(&cas));
    // `LocalDaemon::bind` has now acquired both the runtime-root filesystem
    // lease and PostgreSQL singleton. Reconcile every exact persisted actor
    // group before exposing the operator socket; a restarted daemon never
    // resumes a paid session from an inherited staging directory.
    let recovery = reconcile_daemon_restart(
        &store.process_store(),
        cas.as_ref(),
        RestartRecoveryPolicy::bounded_default(),
    )
    .await?;
    if !recovery.recovered.is_empty() {
        tracing::warn!(
            session_count = recovery.recovered.len(),
            "reconciled interrupted actor sessions before serving"
        );
    }
    // One direct bounded driver consumes durable scheduler actions. It owns
    // no queue or poll state: every pass rereads the current campaign and all
    // launch/delivery identities. It sleeps only after a no-work or Architect
    // gate outcome; actionable work chains immediately through the next
    // durable scheduler read once its bounded operation returns.
    let daemon = Arc::new(daemon);
    let driver = CampaignDriver::new(
        store.clone(),
        cas.as_ref().clone(),
        installed.clone(),
        execution,
        Arc::clone(&authority_resolver),
    );
    let driver_daemon = Arc::clone(&daemon);
    let driver_task = smol::spawn(async move {
        let mut last_error = None::<String>;
        loop {
            let wait = match driver.run_next(driver_daemon.as_ref()).await {
                Ok(
                    CampaignDriverOutcome::NoRunningCampaign
                    | CampaignDriverOutcome::AwaitingArchitect { .. }
                    | CampaignDriverOutcome::Idle { .. }
                    | CampaignDriverOutcome::Blocked(_),
                ) => {
                    last_error = None;
                    Duration::from_millis(250)
                }
                Ok(_) => {
                    last_error = None;
                    Duration::ZERO
                }
                Err(error) => {
                    let message = error.to_string();
                    if last_error.as_deref() != Some(message.as_str()) {
                        tracing::error!(%error, "campaign driver action failed");
                        last_error = Some(message);
                    }
                    // A transition rejection is durable evidence, not a busy
                    // loop. The next poll rereads all authority after the
                    // bounded pause and lets an operator repair a real gate.
                    Duration::from_millis(250)
                }
            };
            if !wait.is_zero() {
                smol::Timer::after(wait).await;
            }
        }
    });
    tracing::info!(socket = %daemon.operator_socket_path().display(), "factoryd ready on local Unix socket");
    let served = daemon.serve().await;
    let _ = driver_task.cancel().await;
    store.close().await;
    served.map_err(Into::into)
}

/// The pure, provider-free startup gate. Keeping it separate from socket bind
/// means a test can prove source/cache/binary drift is rejected before a
/// resident daemon acquires a listener.
fn verify_serve_preflight(
    installed: &InstalledKernelBuildReceiptV1,
    current_build: factory_protocol::KernelBuildId,
    running_binary: &Path,
) -> Result<(), io::Error> {
    if installed.kernel_build_id() != current_build {
        return Err(init_error(
            "current installed-build receipt changed identity",
        ));
    }
    if running_binary != installed.kernel_binary() {
        return Err(init_error(
            "running factoryd binary is not the current installed-build binary",
        ));
    }
    installed
        .verify_installed_material(SCHEMA_IDENTITY)
        .map_err(|error| init_error(&format!("installed build preflight failed: {error}")))
}

/// Applies the forward-only schema lineage and installs one qualified build.
/// This is intentionally an offline process: it never binds an operator socket
/// or starts the resident daemon loop. A later invocation may replace the
/// current build, but only from the kernel-build revision observed after schema
/// verification; a resident daemon can never upgrade itself.
async fn run_init(args: InitArgs) -> Result<(), Box<dyn std::error::Error>> {
    let store = KernelStore::connect(&args.database_url).await?;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        store.migrate_and_verify().await?;

        // `factoryctl` selects a build-specific cache beneath the runtime
        // root. Installation is the one phase allowed to populate it; later
        // daemon and assignment preflights are strictly cached-only.
        fs::create_dir_all(&args.deno_dir)?;

        let runtime = InstalledRuntimeManifest::qualify(InstalledRuntimeQualification {
            deno_executable: args.deno_executable.clone(),
            host_source_root: args.pi_host_source_root.clone(),
            host_entrypoint: args.pi_host_entrypoint.clone(),
            deno_config: args.deno_config.clone(),
            deno_lock: args.deno_lock.clone(),
            deno_dir: args.deno_dir.clone(),
            host_source_files: args.pi_host_source_files.clone(),
            cache_probe_module: args.pi_host_cache_probe.clone(),
            pi_version: args.pi_version.clone(),
        })?;
        let source = qualify_kernel_source_v1(&args.kernel_source_root, &args.kernel_source_files)?;
        let binary = qualify_kernel_binary_v1(&args.kernel_binary)?;
        let approved_tools = InstalledApprovedToolsQualificationV1::qualify(
            &args.cargo_executable,
            &args.git_executable,
        )?;
        let running_binary = qualify_kernel_binary_v1(&env::current_exe()?)?;
        if binary.path() != running_binary.path() {
            return Err(boxed_init_error(
                "--kernel-binary must be the exact executable running `factoryd init`",
            ));
        }
        let build_receipt = InstalledKernelBuildReceiptV1::from_qualifications(
            SCHEMA_IDENTITY.to_owned(),
            source,
            binary,
            approved_tools,
            runtime,
            args.openrouter_credential_environment.clone(),
        )?;
        let build_id = build_receipt.kernel_build_id();
        let seed = CasStore::temporary_name_seed(build_id, std::process::id(), SystemTime::now())?;
        let cas = CasStore::with_default_limit(&args.runtime_root, seed)?;
        // Installation is an offline deployment boundary. Read the current
        // build aggregate immediately before the guarded write so the first
        // install and a later exact-source replacement both use the same
        // optimistic-concurrency rule. The store makes an exact retry of this
        // build idempotent and rejects a concurrent deployment.
        let expected_revision =
            expected_install_revision(&store.kernel_build_status().await?, build_id)?;
        let receipt = store
            .install_qualified_kernel_build(
                &cas,
                &InstallQualifiedKernelBuild {
                    principal: "factoryd-init".to_owned(),
                    command_id: format!("factoryd-init-{}", build_id.digest().to_hex()),
                    expected_revision: ExpectedRevision::new(expected_revision),
                    receipt: build_receipt,
                },
            )
            .await?;
        if receipt.kernel_build_id != build_id {
            return Err(boxed_init_error(
                "installed kernel build receipt changed identity",
            ));
        }
        let status = store.kernel_build_status().await?;
        if status.current_kernel_build_id != Some(build_id) {
            return Err(boxed_init_error(
                "installed kernel build is not current after initialization",
            ));
        }
        println!(
            "factoryd initialized build {} (revision {}, audit #{})",
            build_id,
            receipt.resulting_revision.get(),
            receipt.audit_log_id
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    store.close().await;
    result
}

/// Returns the revision that originally admitted `build_id` when that exact
/// build is current, preserving its command fingerprint for an idempotent
/// offline-install retry. A different build advances from the observed
/// current revision. Kernel-build revisions are contiguous and begin at one.
fn expected_install_revision(
    status: &KernelBuildStatus,
    build_id: KernelBuildId,
) -> Result<AggregateRevision, Box<dyn std::error::Error>> {
    if status.current_kernel_build_id == Some(build_id) {
        let preceding = status
            .aggregate_revision
            .get()
            .checked_sub(1)
            .ok_or_else(|| boxed_init_error("current kernel build has no preceding revision"))?;
        return Ok(AggregateRevision::from_persisted(preceding));
    }
    Ok(status.aggregate_revision)
}

#[derive(Debug)]
enum DaemonCommand {
    Serve(DaemonArgs),
    Init(InitArgs),
}

#[derive(Debug)]
struct DaemonArgs {
    database_url: String,
    runtime_root: PathBuf,
    read_deadline: Duration,
    operation_deadline: Duration,
    write_deadline: Duration,
}

/// All build identity inputs are explicit. The daemon does not infer a source
/// graph from a checkout or fill in a Deno path/cache from an ambient shell.
#[derive(Debug, PartialEq, Eq)]
struct InitArgs {
    database_url: String,
    runtime_root: PathBuf,
    kernel_source_root: PathBuf,
    kernel_source_files: Vec<RuntimeRelativePath>,
    kernel_binary: PathBuf,
    cargo_executable: PathBuf,
    git_executable: PathBuf,
    deno_executable: PathBuf,
    pi_host_source_root: PathBuf,
    pi_host_source_files: Vec<RuntimeRelativePath>,
    pi_host_entrypoint: PathBuf,
    deno_config: PathBuf,
    deno_lock: PathBuf,
    deno_dir: PathBuf,
    pi_host_cache_probe: RuntimeRelativePath,
    pi_version: String,
    openrouter_credential_environment: String,
}

fn parse_args(arguments: Vec<String>) -> Result<DaemonCommand, String> {
    let mut values = arguments.into_iter();
    match values.next().as_deref() {
        Some("serve") => parse_serve_args(values.collect()).map(DaemonCommand::Serve),
        Some("init") => parse_init_args(values.collect()).map(DaemonCommand::Init),
        _ => Err("expected the `serve` or `init` subcommand".to_owned()),
    }
}

fn parse_serve_args(arguments: Vec<String>) -> Result<DaemonArgs, String> {
    let mut values = arguments.into_iter();
    let mut database_url = None;
    let mut runtime_root = None;
    let mut read_deadline = None;
    let mut operation_deadline = None;
    let mut write_deadline = None;
    while let Some(flag) = values.next() {
        let value = values
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        match flag.as_str() {
            "--database-url" => set_once(&mut database_url, value, "--database-url")?,
            "--runtime-root" => {
                set_once(&mut runtime_root, PathBuf::from(value), "--runtime-root")?;
            }
            "--read-deadline-ms" => {
                set_once(
                    &mut read_deadline,
                    positive_millis(&value, "--read-deadline-ms")?,
                    "--read-deadline-ms",
                )?;
            }
            "--operation-deadline-ms" => {
                set_once(
                    &mut operation_deadline,
                    positive_millis(&value, "--operation-deadline-ms")?,
                    "--operation-deadline-ms",
                )?;
            }
            "--write-deadline-ms" => {
                set_once(
                    &mut write_deadline,
                    positive_millis(&value, "--write-deadline-ms")?,
                    "--write-deadline-ms",
                )?;
            }
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    Ok(DaemonArgs {
        database_url: database_url.ok_or_else(|| "--database-url is required".to_owned())?,
        runtime_root: runtime_root.ok_or_else(|| "--runtime-root is required".to_owned())?,
        read_deadline: read_deadline.unwrap_or(Duration::from_secs(5)),
        operation_deadline: operation_deadline.unwrap_or(Duration::from_secs(30)),
        write_deadline: write_deadline.unwrap_or(Duration::from_secs(5)),
    })
}

fn set_once<T>(slot: &mut Option<T>, value: T, flag: &str) -> Result<(), String> {
    if slot.replace(value).is_some() {
        return Err(format!("{flag} was supplied more than once"));
    }
    Ok(())
}

fn positive_millis(value: &str, flag: &str) -> Result<Duration, String> {
    let millis = value
        .parse::<u64>()
        .map_err(|_| format!("{flag} must be a positive integer"))?;
    if millis == 0 {
        return Err(format!("{flag} must be greater than zero"));
    }
    Ok(Duration::from_millis(millis))
}

fn parse_init_args(arguments: Vec<String>) -> Result<InitArgs, String> {
    let mut values = arguments.into_iter();
    let mut database_url = None;
    let mut runtime_root = None;
    let mut kernel_source_root = None;
    let mut kernel_source_files = Vec::new();
    let mut kernel_binary = None;
    let mut cargo_executable = None;
    let mut git_executable = None;
    let mut deno_executable = None;
    let mut pi_host_source_root = None;
    let mut pi_host_source_files = Vec::new();
    let mut pi_host_entrypoint = None;
    let mut deno_config = None;
    let mut deno_lock = None;
    let mut deno_dir = None;
    let mut pi_host_cache_probe = None;
    let mut pi_version = None;
    let mut openrouter_credential_environment = None;
    while let Some(flag) = values.next() {
        let value = values
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        match flag.as_str() {
            "--database-url" => set_once(&mut database_url, value, "--database-url")?,
            "--runtime-root" => set_absolute_path(&mut runtime_root, value, "--runtime-root")?,
            "--kernel-source-root" => {
                set_absolute_path(&mut kernel_source_root, value, "--kernel-source-root")?;
            }
            "--kernel-source-file" => {
                kernel_source_files.push(parse_relative_path(value, "--kernel-source-file")?);
            }
            "--kernel-binary" => set_absolute_path(&mut kernel_binary, value, "--kernel-binary")?,
            "--cargo-executable" => {
                set_absolute_path(&mut cargo_executable, value, "--cargo-executable")?;
            }
            "--git-executable" => {
                set_absolute_path(&mut git_executable, value, "--git-executable")?;
            }
            "--deno-executable" => {
                set_absolute_path(&mut deno_executable, value, "--deno-executable")?;
            }
            "--pi-host-source-root" => {
                set_absolute_path(&mut pi_host_source_root, value, "--pi-host-source-root")?;
            }
            "--pi-host-source-file" => {
                pi_host_source_files.push(parse_relative_path(value, "--pi-host-source-file")?);
            }
            "--pi-host-entrypoint" => {
                set_absolute_path(&mut pi_host_entrypoint, value, "--pi-host-entrypoint")?;
            }
            "--deno-config" => set_absolute_path(&mut deno_config, value, "--deno-config")?,
            "--deno-lock" => set_absolute_path(&mut deno_lock, value, "--deno-lock")?,
            "--deno-dir" => set_absolute_path(&mut deno_dir, value, "--deno-dir")?,
            "--pi-host-cache-probe" => set_once(
                &mut pi_host_cache_probe,
                parse_relative_path(value, "--pi-host-cache-probe")?,
                "--pi-host-cache-probe",
            )?,
            "--pi-version" => set_once(&mut pi_version, value, "--pi-version")?,
            "--provider-credential-environment" => set_once(
                &mut openrouter_credential_environment,
                parse_openrouter_credential_environment(value)?,
                "--provider-credential-environment",
            )?,
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    if kernel_source_files.is_empty() {
        return Err("at least one --kernel-source-file is required".to_owned());
    }
    if pi_host_source_files.is_empty() {
        return Err("at least one --pi-host-source-file is required".to_owned());
    }
    Ok(InitArgs {
        database_url: required(database_url, "--database-url")?,
        runtime_root: required(runtime_root, "--runtime-root")?,
        kernel_source_root: required(kernel_source_root, "--kernel-source-root")?,
        kernel_source_files,
        kernel_binary: required(kernel_binary, "--kernel-binary")?,
        cargo_executable: required(cargo_executable, "--cargo-executable")?,
        git_executable: required(git_executable, "--git-executable")?,
        deno_executable: required(deno_executable, "--deno-executable")?,
        pi_host_source_root: required(pi_host_source_root, "--pi-host-source-root")?,
        pi_host_source_files,
        pi_host_entrypoint: required(pi_host_entrypoint, "--pi-host-entrypoint")?,
        deno_config: required(deno_config, "--deno-config")?,
        deno_lock: required(deno_lock, "--deno-lock")?,
        deno_dir: required(deno_dir, "--deno-dir")?,
        pi_host_cache_probe: required(pi_host_cache_probe, "--pi-host-cache-probe")?,
        pi_version: required(pi_version, "--pi-version")?,
        openrouter_credential_environment: required(
            openrouter_credential_environment,
            "--provider-credential-environment openrouter=<ENVIRONMENT_NAME>",
        )?,
    })
}

fn set_absolute_path(slot: &mut Option<PathBuf>, value: String, flag: &str) -> Result<(), String> {
    let path = PathBuf::from(&value);
    if value.is_empty() || value.contains('\0') || !path.is_absolute() {
        return Err(format!(
            "{flag} must be a non-empty absolute path without NUL"
        ));
    }
    set_once(slot, path, flag)
}

fn parse_relative_path(value: String, flag: &str) -> Result<RuntimeRelativePath, String> {
    RuntimeRelativePath::parse(value).map_err(|error| format!("{flag} must be safe: {error}"))
}

fn parse_openrouter_credential_environment(value: String) -> Result<String, String> {
    let environment = value
        .strip_prefix("openrouter=")
        .ok_or_else(|| {
            "--provider-credential-environment must use the only MVP provider shape `openrouter=<ENVIRONMENT_NAME>`"
                .to_owned()
        })?
        .to_owned();
    factory_protocol::CredentialDescriptorV1::Environment {
        name: environment.clone(),
    }
    .validate()
    .map_err(|_| {
        "--provider-credential-environment must name a non-empty uppercase environment variable"
            .to_owned()
    })?;
    Ok(environment)
}

fn required<T>(value: Option<T>, flag: &str) -> Result<T, String> {
    value.ok_or_else(|| format!("{flag} is required"))
}

fn init_error(message: &str) -> io::Error {
    io::Error::other(message.to_owned())
}

fn boxed_init_error(message: &str) -> Box<dyn std::error::Error> {
    Box::new(init_error(message))
}

fn usage() -> &'static str {
    "usage:\n  factoryd serve --database-url <url> --runtime-root <absolute-path> [--read-deadline-ms <positive>] [--operation-deadline-ms <positive>] [--write-deadline-ms <positive>]\n  factoryd init --database-url <url> --runtime-root <absolute-path> --kernel-source-root <absolute-path> --kernel-source-file <safe-relative-path>... --kernel-binary <absolute-path> --cargo-executable <absolute-path> --git-executable <absolute-path> --deno-executable <absolute-path> --pi-host-source-root <absolute-path> --pi-host-source-file <safe-relative-path>... --pi-host-entrypoint <absolute-path> --pi-host-cache-probe <safe-relative-path> --deno-config <absolute-path> --deno-lock <absolute-path> --deno-dir <absolute-path> --pi-version <version> --provider-credential-environment openrouter=<UPPERCASE_ENVIRONMENT_NAME>"
}

#[cfg(test)]
mod tests {
    use factory_protocol::ContentDigest;

    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    struct PreflightFixture {
        root: PathBuf,
        binary: PathBuf,
        cargo: PathBuf,
        git: PathBuf,
        source_root: PathBuf,
        host_root: PathBuf,
        cache: PathBuf,
    }

    impl PreflightFixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "factoryd-serve-preflight-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock after epoch")
                    .as_nanos(),
            ));
            let source_root = root.join("source");
            let host_root = root.join("host");
            let cache = root.join("deno-cache");
            fs::create_dir_all(source_root.join("crates/factoryd/src")).expect("kernel source");
            fs::create_dir_all(&host_root).expect("host source");
            fs::create_dir_all(&cache).expect("DENO_DIR");
            fs::write(cache.join("qualified"), b"cache material").expect("cache marker");
            fs::write(
                source_root.join("crates/factoryd/src/main.rs"),
                b"kernel source",
            )
            .expect("kernel source file");
            let binary = root.join("factoryd");
            fs::write(
                &binary,
                "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf 'deno 2.9.4\\n'; exit 0; fi\nif [ \"$1\" = \"info\" ] && [ -f \"$DENO_DIR/qualified\" ]; then for value in \"$@\"; do entrypoint=\"$value\"; done; printf '{\"modules\":[{\"specifier\":\"file://%s\",\"local\":\"%s\"}]}' \"$entrypoint\" \"$entrypoint\"; exit 0; fi\nif [ \"$1\" = \"cache\" ]; then exit 0; fi\nif { [ \"$1\" = \"check\" ] || [ \"$1\" = \"run\" ]; } && [ -f \"$DENO_DIR/qualified\" ]; then exit 0; fi\nexit 17\n",
            )
            .expect("fake Deno/kernel binary");
            let mut permissions = fs::metadata(&binary)
                .expect("binary metadata")
                .permissions();
            permissions.set_mode(0o700);
            fs::set_permissions(&binary, permissions).expect("make binary executable");
            let cargo = root.join("cargo");
            let git = root.join("git");
            for tool in [&cargo, &git, &root.join("rustc"), &root.join("rustdoc")] {
                fs::write(tool, "#!/bin/sh\nexit 0\n").expect("fake approved executable");
                let mut permissions = fs::metadata(tool)
                    .expect("approved executable metadata")
                    .permissions();
                permissions.set_mode(0o700);
                fs::set_permissions(tool, permissions)
                    .expect("make approved executable executable");
            }
            fs::write(host_root.join("main.ts"), "export const value = 1;\n")
                .expect("host entrypoint");
            fs::write(host_root.join("probe.ts"), "export {};\n").expect("safe cache probe");
            fs::write(root.join("deno.json"), "{}\n").expect("Deno config");
            fs::write(root.join("deno.lock"), "{}\n").expect("Deno lock");
            Self {
                root,
                binary,
                cargo,
                git,
                source_root,
                host_root,
                cache,
            }
        }

        fn receipt(&self) -> InstalledKernelBuildReceiptV1 {
            let runtime = InstalledRuntimeManifest::qualify(InstalledRuntimeQualification {
                deno_executable: self.binary.clone(),
                host_source_root: self.host_root.clone(),
                host_entrypoint: self.host_root.join("main.ts"),
                deno_config: self.root.join("deno.json"),
                deno_lock: self.root.join("deno.lock"),
                deno_dir: self.cache.clone(),
                host_source_files: vec![
                    RuntimeRelativePath::parse("main.ts").unwrap(),
                    RuntimeRelativePath::parse("probe.ts").unwrap(),
                ],
                cache_probe_module: RuntimeRelativePath::parse("probe.ts").unwrap(),
                pi_version: "0.84.1".to_owned(),
            })
            .expect("qualify runtime");
            let source = qualify_kernel_source_v1(
                &self.source_root,
                &[RuntimeRelativePath::parse("crates/factoryd/src/main.rs").unwrap()],
            )
            .expect("qualify kernel source");
            let binary = qualify_kernel_binary_v1(&self.binary).expect("qualify kernel binary");
            let approved_tools =
                InstalledApprovedToolsQualificationV1::qualify(&self.cargo, &self.git)
                    .expect("qualify approved tools");
            InstalledKernelBuildReceiptV1::from_qualifications(
                SCHEMA_IDENTITY.to_owned(),
                source,
                binary,
                approved_tools,
                runtime,
                "OPENROUTER_API_KEY".to_owned(),
            )
            .expect("build receipt")
        }
    }

    impl Drop for PreflightFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn init_arguments() -> Vec<String> {
        vec![
            "init".to_owned(),
            "--database-url".to_owned(),
            "postgresql://factory@localhost/factory_v3".to_owned(),
            "--runtime-root".to_owned(),
            "/tmp/factory-runtime".to_owned(),
            "--kernel-source-root".to_owned(),
            "/opt/factory-source".to_owned(),
            "--kernel-source-file".to_owned(),
            "crates/factoryd/src/main.rs".to_owned(),
            "--kernel-binary".to_owned(),
            "/opt/factory/bin/factoryd".to_owned(),
            "--cargo-executable".to_owned(),
            "/opt/rust/bin/cargo".to_owned(),
            "--git-executable".to_owned(),
            "/opt/git/bin/git".to_owned(),
            "--deno-executable".to_owned(),
            "/opt/deno/bin/deno".to_owned(),
            "--pi-host-source-root".to_owned(),
            "/opt/factory-source/packages".to_owned(),
            "--pi-host-source-file".to_owned(),
            "factory-pi-host/main.ts".to_owned(),
            "--pi-host-entrypoint".to_owned(),
            "/opt/factory-source/packages/factory-pi-host/main.ts".to_owned(),
            "--pi-host-cache-probe".to_owned(),
            "factory-pi-host/mod.ts".to_owned(),
            "--deno-config".to_owned(),
            "/opt/factory-source/deno.json".to_owned(),
            "--deno-lock".to_owned(),
            "/opt/factory-source/deno.lock".to_owned(),
            "--deno-dir".to_owned(),
            "/opt/factory-runtime/deno-cache".to_owned(),
            "--pi-version".to_owned(),
            "0.84.1".to_owned(),
            "--provider-credential-environment".to_owned(),
            "openrouter=OPENROUTER_API_KEY".to_owned(),
        ]
    }

    #[test]
    fn serve_configuration_requires_exact_local_inputs() {
        assert!(parse_args(vec!["serve".to_owned()]).is_err());
        assert!(
            parse_args(vec![
                "serve".to_owned(),
                "--database-url".to_owned(),
                "postgres://factory".to_owned(),
                "--runtime-root".to_owned(),
                "/tmp/factory".to_owned(),
                "--read-deadline-ms".to_owned(),
                "0".to_owned(),
            ])
            .is_err()
        );
        let parsed = parse_args(vec![
            "serve".to_owned(),
            "--database-url".to_owned(),
            "postgres://factory".to_owned(),
            "--runtime-root".to_owned(),
            "/tmp/factory".to_owned(),
        ])
        .expect("valid daemon config");
        assert!(matches!(
            parsed,
            DaemonCommand::Serve(DaemonArgs { runtime_root, .. })
                if runtime_root == *"/tmp/factory"
        ));
    }

    #[test]
    fn offline_install_uses_the_original_revision_for_an_exact_current_build_retry() {
        let current = KernelBuildId::new(ContentDigest::from_bytes([7; 32]));
        let other = KernelBuildId::new(ContentDigest::from_bytes([8; 32]));
        let status = KernelBuildStatus {
            current_kernel_build_id: Some(current),
            aggregate_revision: AggregateRevision::from_persisted(8),
        };

        assert_eq!(
            expected_install_revision(&status, current)
                .expect("current build retry")
                .get(),
            7
        );
        assert_eq!(
            expected_install_revision(&status, other)
                .expect("replacement build")
                .get(),
            8
        );
        assert_eq!(
            expected_install_revision(
                &KernelBuildStatus {
                    current_kernel_build_id: None,
                    aggregate_revision: AggregateRevision::initial(),
                },
                current,
            )
            .expect("first build")
            .get(),
            0
        );
    }

    #[test]
    fn init_requires_the_complete_closed_build_identity() {
        let parsed = parse_args(init_arguments()).expect("complete init command");
        assert!(matches!(
            parsed,
            DaemonCommand::Init(InitArgs {
                kernel_source_files,
                pi_host_source_files,
                kernel_binary,
                cargo_executable,
                git_executable,
                ..
            }) if kernel_source_files == vec![RuntimeRelativePath::parse("crates/factoryd/src/main.rs").unwrap()]
                && pi_host_source_files == vec![RuntimeRelativePath::parse("factory-pi-host/main.ts").unwrap()]
                && kernel_binary == *"/opt/factory/bin/factoryd"
                && cargo_executable == *"/opt/rust/bin/cargo"
                && git_executable == *"/opt/git/bin/git"
        ));

        let mut missing_host_graph = init_arguments();
        let flag = missing_host_graph
            .iter()
            .position(|value| value == "--pi-host-source-file")
            .expect("host graph flag");
        missing_host_graph.drain(flag..=flag + 1);
        assert!(parse_args(missing_host_graph).is_err());

        let mut relative_runtime_root = init_arguments();
        let runtime_root = relative_runtime_root
            .iter()
            .position(|value| value == "--runtime-root")
            .expect("runtime root flag");
        relative_runtime_root[runtime_root + 1] = "relative-runtime".to_owned();
        assert!(parse_args(relative_runtime_root).is_err());

        let mut unsafe_source = init_arguments();
        unsafe_source.extend([
            "--kernel-source-file".to_owned(),
            "../outside.rs".to_owned(),
        ]);
        assert!(parse_args(unsafe_source).is_err());

        let mut unsupported_credential = init_arguments();
        let credential = unsupported_credential
            .iter()
            .position(|value| value == "--provider-credential-environment")
            .expect("credential flag");
        unsupported_credential[credential + 1] = "other=OTHER_KEY".to_owned();
        assert!(parse_args(unsupported_credential).is_err());
    }

    #[test]
    fn kernel_source_qualification_is_closed_and_observes_drift() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "factoryd-init-source-{}-{unique}",
            std::process::id()
        ));
        let source_root = root.join("source");
        fs::create_dir_all(source_root.join("crates/factoryd/src")).expect("source root");
        let main = source_root.join("crates/factoryd/src/main.rs");
        fs::write(&main, "fn main() {}\n").expect("source file");
        let graph = vec![RuntimeRelativePath::parse("crates/factoryd/src/main.rs").unwrap()];
        let before = qualify_kernel_source_v1(&source_root, &graph).expect("initial source graph");
        fs::write(&main, "fn main() { println!(\"drift\"); }\n").expect("changed source file");
        let after = qualify_kernel_source_v1(&source_root, &graph).expect("changed source graph");
        assert_ne!(before.digest(), after.digest());

        let duplicate = vec![
            RuntimeRelativePath::parse("crates/factoryd/src/main.rs").unwrap(),
            RuntimeRelativePath::parse("crates/factoryd/src/main.rs").unwrap(),
        ];
        assert!(qualify_kernel_source_v1(&source_root, &duplicate).is_err());

        #[cfg(unix)]
        {
            let outside = root.join("outside.rs");
            fs::write(&outside, "outside\n").expect("outside source");
            std::os::unix::fs::symlink(&outside, source_root.join("escape.rs"))
                .expect("source escape symlink");
            assert!(
                qualify_kernel_source_v1(
                    &source_root,
                    &[RuntimeRelativePath::parse("escape.rs").unwrap()],
                )
                .is_err()
            );
        }
        fs::remove_dir_all(root).expect("remove temporary root");
    }

    #[test]
    fn serve_preflight_rejects_binary_source_and_cache_drift_before_socket_bind() {
        let fixture = PreflightFixture::new();
        let receipt = fixture.receipt();
        let binary = qualify_kernel_binary_v1(&fixture.binary)
            .expect("qualify kernel binary")
            .path()
            .to_owned();
        verify_serve_preflight(&receipt, receipt.kernel_build_id(), &binary)
            .expect("qualified build preflight");

        assert!(
            verify_serve_preflight(
                &receipt,
                receipt.kernel_build_id(),
                &fixture.root.join("other-binary"),
            )
            .is_err()
        );

        fs::write(
            fixture.source_root.join("crates/factoryd/src/main.rs"),
            b"changed",
        )
        .expect("change kernel source");
        assert!(verify_serve_preflight(&receipt, receipt.kernel_build_id(), &binary).is_err());

        let binary_fixture = PreflightFixture::new();
        let binary_receipt = binary_fixture.receipt();
        fs::write(&binary_fixture.binary, "#!/bin/sh\nexit 88\n").expect("change kernel binary");
        let mut permissions = fs::metadata(&binary_fixture.binary)
            .expect("changed binary metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&binary_fixture.binary, permissions).expect("preserve executable bit");
        assert!(
            verify_serve_preflight(
                &binary_receipt,
                binary_receipt.kernel_build_id(),
                binary_receipt.kernel_binary(),
            )
            .is_err()
        );

        let cache_fixture = PreflightFixture::new();
        let cache_receipt = cache_fixture.receipt();
        fs::remove_file(cache_fixture.cache.join("qualified")).expect("remove frozen cache marker");
        assert!(
            verify_serve_preflight(
                &cache_receipt,
                cache_receipt.kernel_build_id(),
                qualify_kernel_binary_v1(&cache_fixture.binary)
                    .expect("qualify cache binary")
                    .path(),
            )
            .is_err()
        );
    }
}
