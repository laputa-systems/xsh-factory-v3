//! Resident Unix-only Factory daemon.
//!
//! The binary receives the database URL only at startup, opens the kernel's
//! fixed pool, verifies the installed schema, acquires both singleton locks,
//! then serves the local operator socket. It intentionally exposes no TCP or
//! HTTP listener.

use std::{
    env, io,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    sync::Arc,
    time::{Duration, SystemTime},
};

#[cfg(test)]
use std::fs;

use factory_kernel::{
    campaign_driver::{CampaignDriver, CampaignDriverOutcome},
    cas::CasStore,
    durable_authority::DurableAuthorityResolver,
    installed_runtime::{
        InstalledApprovedToolsQualificationV2, InstalledKernelBuildReceiptV2,
        InstalledRuntimeManifest, InstalledRuntimeQualification, qualify_kernel_binary_v2,
        qualify_kernel_source_v2,
    },
    local_transport::{LocalDaemon, LocalTransportConfig},
    restart_recovery::{RestartRecoveryPolicy, reconcile_daemon_restart},
    storage::{InstallQualifiedKernelBuild, KernelBuildStatus, KernelStore, SCHEMA_IDENTITY},
};
use factory_protocol::{AggregateRevision, ExpectedRevision, KernelBuildId, RuntimeRelativePath};
use factory_settings::{
    DEFAULT_OPERATION_DEADLINE, DEFAULT_READ_DEADLINE, DEFAULT_WRITE_DEADLINE,
    FACTORYD_ASSIGNMENT_POLL_INTERVAL, FACTORYD_PRINTENV_COMMAND, FACTORYD_VAULT_COMMAND,
};

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
    // restore its exact sealed receipt and requalify every local kernel/host
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
    let running_binary = qualify_kernel_binary_v2(&env::current_exe()?)?
        .path()
        .to_owned();
    verify_serve_preflight(&installed, current_build, &running_binary)?;
    let credential_environment = installed.openrouter_credential_environment().to_owned();
    verify_vault_credential_preflight(&credential_environment)?;
    // The receipt is the only source of Cargo/Git/host paths. Reconstructing
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
        .with_publication_control(store.publication_store())
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
    let driver = CampaignDriver::with_credential_lookup(
        store.clone(),
        cas.as_ref().clone(),
        installed.clone(),
        execution,
        Arc::clone(&authority_resolver),
        |name| vault_credential(name).map_err(|error| error.to_string()),
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
                    FACTORYD_ASSIGNMENT_POLL_INTERVAL
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
                    FACTORYD_ASSIGNMENT_POLL_INTERVAL
                }
            };
            if !wait.is_zero() {
                smol::Timer::after(wait).await;
            }
        }
    });
    tracing::info!(socket = %daemon.operator_socket_path().display(), "factoryd ready on local Unix socket");
    let served = daemon.serve().await;
    let active_sessions = daemon.cancel_active_sessions().await;
    let _ = driver_task.cancel().await;
    let shutdown = match Arc::try_unwrap(daemon) {
        Ok(daemon) => daemon.shutdown().await,
        Err(_) => Err(io::Error::other("factoryd driver retained daemon ownership").into()),
    };
    store.close().await;
    served
        .and(active_sessions)
        .and(shutdown)
        .map_err(Into::into)
}

/// The pure, provider-free startup gate. Keeping it separate from socket bind
/// means a test can prove source/host-binary drift is rejected before a
/// resident daemon acquires a listener.
fn verify_serve_preflight(
    installed: &InstalledKernelBuildReceiptV2,
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

fn verify_vault_credential_preflight(name: &str) -> Result<(), io::Error> {
    vault_credential(name).map(|_| ())
}

fn vault_credential(name: &str) -> Result<std::ffi::OsString, io::Error> {
    vault_credential_with_command(Path::new(FACTORYD_VAULT_COMMAND), name)
}

fn vault_credential_with_command(
    vault: &Path,
    name: &str,
) -> Result<std::ffi::OsString, io::Error> {
    let output = Command::new(vault)
        .arg(name)
        .arg("--")
        .arg(FACTORYD_PRINTENV_COMMAND)
        .arg(name)
        .env_remove(name)
        .output()
        .map_err(|error| {
            io::Error::other(format!("unable to execute Vault for {name:?}: {error}"))
        })?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "Vault did not provide required credential {name:?}"
        )));
    }
    let mut value = output.stdout;
    if value.last() == Some(&b'\n') {
        value.pop();
    }
    if value.is_empty() {
        return Err(io::Error::other(format!(
            "Vault provided an empty credential {name:?}"
        )));
    }
    let value = String::from_utf8(value)
        .map_err(|_| io::Error::other(format!("Vault provided a non-UTF-8 credential {name:?}")))?;
    Ok(std::ffi::OsString::from(value))
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

        let runtime = InstalledRuntimeManifest::qualify(InstalledRuntimeQualification {
            host_executable: args.host_executable.clone(),
            host_source_root: args.host_source_root.clone(),
            host_source_files: args.host_source_files.clone(),
        })?;
        let source = qualify_kernel_source_v2(&args.kernel_source_root, &args.kernel_source_files)?;
        let binary = qualify_kernel_binary_v2(&args.kernel_binary)?;
        let approved_tools = InstalledApprovedToolsQualificationV2::qualify(
            &args.cargo_executable,
            &args.git_executable,
        )?;
        let running_binary = qualify_kernel_binary_v2(&env::current_exe()?)?;
        if binary.path() != running_binary.path() {
            return Err(boxed_init_error(
                "--kernel-binary must be the exact executable running `factoryd init`",
            ));
        }
        let build_receipt = InstalledKernelBuildReceiptV2::from_qualifications(
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
/// graph from a checkout or fill in a host executable from an ambient shell.
#[derive(Debug, PartialEq, Eq)]
struct InitArgs {
    database_url: String,
    runtime_root: PathBuf,
    kernel_source_root: PathBuf,
    kernel_source_files: Vec<RuntimeRelativePath>,
    kernel_binary: PathBuf,
    cargo_executable: PathBuf,
    git_executable: PathBuf,
    host_executable: PathBuf,
    host_source_root: PathBuf,
    host_source_files: Vec<RuntimeRelativePath>,
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
        read_deadline: read_deadline.unwrap_or(DEFAULT_READ_DEADLINE),
        operation_deadline: operation_deadline.unwrap_or(DEFAULT_OPERATION_DEADLINE),
        write_deadline: write_deadline.unwrap_or(DEFAULT_WRITE_DEADLINE),
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
    let mut host_executable = None;
    let mut host_source_root = None;
    let mut host_source_files = Vec::new();
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
            "--host-executable" => {
                set_absolute_path(&mut host_executable, value, "--host-executable")?;
            }
            "--host-source-root" => {
                set_absolute_path(&mut host_source_root, value, "--host-source-root")?;
            }
            "--host-source-file" => {
                host_source_files.push(parse_relative_path(value, "--host-source-file")?);
            }
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
    if host_source_files.is_empty() {
        return Err("at least one --host-source-file is required".to_owned());
    }
    Ok(InitArgs {
        database_url: required(database_url, "--database-url")?,
        runtime_root: required(runtime_root, "--runtime-root")?,
        kernel_source_root: required(kernel_source_root, "--kernel-source-root")?,
        kernel_source_files,
        kernel_binary: required(kernel_binary, "--kernel-binary")?,
        cargo_executable: required(cargo_executable, "--cargo-executable")?,
        git_executable: required(git_executable, "--git-executable")?,
        host_executable: required(host_executable, "--host-executable")?,
        host_source_root: required(host_source_root, "--host-source-root")?,
        host_source_files,
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
    factory_protocol::CredentialDescriptorV2::Environment {
        name: environment.clone(),
    }
    .validate()
    .map_err(|_| {
        "--provider-credential-environment must name a non-empty uppercase environment variable"
            .to_owned()
    })?;
    if matches!(environment.as_str(), "NO_COLOR" | "PATH") {
        return Err(
            "--provider-credential-environment cannot use the kernel-owned NO_COLOR or PATH name"
                .to_owned(),
        );
    }
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
    "usage:\n  factoryd serve --database-url <url> --runtime-root <absolute-path> [--read-deadline-ms <positive>] [--operation-deadline-ms <positive>] [--write-deadline-ms <positive>]\n  factoryd init --database-url <url> --runtime-root <absolute-path> --kernel-source-root <absolute-path> --kernel-source-file <safe-relative-path>... --kernel-binary <absolute-path> --cargo-executable <absolute-path> --git-executable <absolute-path> --host-executable <absolute-path> --host-source-root <absolute-path> --host-source-file <safe-relative-path>... --provider-credential-environment openrouter=<UPPERCASE_ENVIRONMENT_NAME>"
}

#[cfg(test)]
mod tests {
    use factory_protocol::ContentDigest;

    use super::*;
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
            "--host-executable".to_owned(),
            "/opt/factory/bin/factory-pi-host".to_owned(),
            "--host-source-root".to_owned(),
            "/opt/factory-source/crates/factory-pi-host".to_owned(),
            "--host-source-file".to_owned(),
            "Cargo.toml".to_owned(),
            "--host-source-file".to_owned(),
            "src/main.rs".to_owned(),
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

    #[cfg(unix)]
    #[test]
    fn vault_credential_resolution_uses_the_named_secret_without_daemon_environment() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "factoryd-vault-resolution-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("temporary root");
        let vault = root.join("vault");
        fs::write(
            &vault,
            "#!/bin/sh\n[ \"$1\" = OPENROUTER_API_KEY ] || exit 41\n[ \"$2\" = -- ] || exit 42\n[ -z \"$OPENROUTER_API_KEY\" ] || exit 43\nprintf 'fresh-vault-key\\n'\n",
        )
        .expect("fake vault");
        let mut permissions = fs::metadata(&vault).expect("vault metadata").permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&vault, permissions).expect("vault permissions");

        let value =
            vault_credential_with_command(&vault, "OPENROUTER_API_KEY").expect("vault credential");
        assert_eq!(value, "fresh-vault-key");
        fs::remove_dir_all(root).expect("remove temporary root");
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
                host_source_files,
                kernel_binary,
                cargo_executable,
                git_executable,
                ..
            }) if kernel_source_files == vec![RuntimeRelativePath::parse("crates/factoryd/src/main.rs").unwrap()]
                && host_source_files == vec![
                    RuntimeRelativePath::parse("Cargo.toml").unwrap(),
                    RuntimeRelativePath::parse("src/main.rs").unwrap(),
                ]
                && kernel_binary == *"/opt/factory/bin/factoryd"
                && cargo_executable == *"/opt/rust/bin/cargo"
                && git_executable == *"/opt/git/bin/git"
        ));

        let mut missing_host_graph = init_arguments();
        while let Some(flag) = missing_host_graph
            .iter()
            .position(|value| value == "--host-source-file")
        {
            missing_host_graph.drain(flag..=flag + 1);
        }
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
        let before = qualify_kernel_source_v2(&source_root, &graph).expect("initial source graph");
        fs::write(&main, "fn main() { println!(\"drift\"); }\n").expect("changed source file");
        let after = qualify_kernel_source_v2(&source_root, &graph).expect("changed source graph");
        assert_ne!(before.digest(), after.digest());

        let duplicate = vec![
            RuntimeRelativePath::parse("crates/factoryd/src/main.rs").unwrap(),
            RuntimeRelativePath::parse("crates/factoryd/src/main.rs").unwrap(),
        ];
        assert!(qualify_kernel_source_v2(&source_root, &duplicate).is_err());

        #[cfg(unix)]
        {
            let outside = root.join("outside.rs");
            fs::write(&outside, "outside\n").expect("outside source");
            std::os::unix::fs::symlink(&outside, source_root.join("escape.rs"))
                .expect("source escape symlink");
            assert!(
                qualify_kernel_source_v2(
                    &source_root,
                    &[RuntimeRelativePath::parse("escape.rs").unwrap()],
                )
                .is_err()
            );
        }
        fs::remove_dir_all(root).expect("remove temporary root");
    }
}
