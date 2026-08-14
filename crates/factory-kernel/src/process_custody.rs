//! Exact launch contract for one daemon-supervised Rust actor host.
//!
//! Spawning is deliberately split from durable session admission. The child
//! inherits one already-connected actor descriptor and must wait for the
//! daemon's `session.admitted` frame before constructing an agent. This
//! prevents a spawned child from making a provider request before its exact
//! PID/PGID and assignment identity have committed in PostgreSQL. The kernel
//! launches one already-qualified executable with no interpreter, package
//! cache, script, or resume arguments.

use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::fd::OwnedFd,
    os::fd::RawFd,
    os::unix::{
        net::UnixStream as StdUnixStream,
        process::{CommandExt, ExitStatusExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use factory_protocol::ProcessCustodyV1;
use rustix::process::{Pid, Signal, kill_process_group};
use thiserror::Error;

/// Environment names supplied by the kernel itself rather than by an
/// application command or credential descriptor.
const KERNEL_ENVIRONMENT_NAMES: [&str; 2] = ["NO_COLOR", "PATH"];

/// Immutable process inputs for one Rust agent host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiHostSpawnSpec {
    host_executable: PathBuf,
    working_directory: PathBuf,
    actor_source_fd: RawFd,
    environment: Vec<(OsString, OsString)>,
}

impl PiHostSpawnSpec {
    /// Builds the exact provider-capable Rust host launch contract.
    ///
    /// `admitted_environment` is already selected by typed application and
    /// credential policy. This constructor still rejects duplicates and the
    /// kernel-owned names, so an application cannot replace descriptor or
    /// update-check custody accidentally.
    pub fn new(
        host_executable: PathBuf,
        working_directory: PathBuf,
        actor_source_fd: RawFd,
        admitted_environment: Vec<(OsString, OsString)>,
    ) -> Result<Self, ProcessCustodyError> {
        for (field, path) in [
            ("Rust agent host executable", host_executable.as_path()),
            ("working directory", working_directory.as_path()),
        ] {
            require_absolute_path(field, path)?;
        }
        if actor_source_fd != 0 {
            return Err(ProcessCustodyError::ActorDescriptorMustBeStdin { actor_source_fd });
        }

        let mut names = BTreeSet::new();
        for name in KERNEL_ENVIRONMENT_NAMES {
            names.insert(OsString::from(name));
        }
        for (name, value) in &admitted_environment {
            validate_environment_name(name)?;
            if value.as_encoded_bytes().contains(&0) {
                return Err(ProcessCustodyError::EnvironmentValueContainsNul {
                    name: name.clone(),
                });
            }
            if !names.insert(name.clone()) {
                return Err(ProcessCustodyError::DuplicateEnvironmentName { name: name.clone() });
            }
        }

        let mut environment = Vec::with_capacity(admitted_environment.len() + 2);
        environment.push((OsString::from("NO_COLOR"), OsString::from("1")));
        environment.push((OsString::from("PATH"), OsString::from("/usr/bin:/bin")));
        environment.extend(admitted_environment);

        Ok(Self {
            host_executable,
            working_directory,
            actor_source_fd,
            environment,
        })
    }

    /// Assignment-only constructor. Runtime identity and capability policy
    /// are carried in the inherited admission packet; the process receives
    /// no filesystem cache or script path from the assignment.
    pub fn new_for_assignment(
        host_executable: PathBuf,
        working_directory: PathBuf,
        actor_source_fd: RawFd,
        admitted_environment: Vec<(OsString, OsString)>,
    ) -> Result<Self, ProcessCustodyError> {
        Self::new(
            host_executable,
            working_directory,
            actor_source_fd,
            admitted_environment,
        )
    }

    /// Replaces the conservative system-only path with the exact path derived
    /// from the installed build's qualified Cargo and Git executables. This is
    /// kernel-owned launch material, never packet or actor input.
    pub(crate) fn with_kernel_tool_path(
        mut self,
        path: OsString,
    ) -> Result<Self, ProcessCustodyError> {
        if path.is_empty() {
            return Err(ProcessCustodyError::EnvironmentValueEmpty {
                name: OsString::from("PATH"),
            });
        }
        if path.as_encoded_bytes().contains(&0) {
            return Err(ProcessCustodyError::EnvironmentValueContainsNul {
                name: OsString::from("PATH"),
            });
        }
        let (_, value) = self
            .environment
            .iter_mut()
            .find(|(name, _)| name == OsStr::new("PATH"))
            .expect("Pi host construction always installs kernel PATH");
        *value = path;
        Ok(self)
    }

    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.host_executable
    }

    /// Exact Rust host arguments. The host receives all assignment material
    /// over the inherited framed descriptor; no CLI can select a packet,
    /// session, package, or resume state.
    #[must_use]
    pub fn arguments(&self) -> Vec<OsString> {
        Vec::new()
    }

    #[must_use]
    pub fn working_directory(&self) -> &Path {
        &self.working_directory
    }

    #[must_use]
    pub const fn actor_source_fd(&self) -> RawFd {
        self.actor_source_fd
    }

    /// Complete child environment. The eventual spawner must call `env_clear`
    /// before installing these exact pairs.
    #[must_use]
    pub fn environment(&self) -> &[(OsString, OsString)] {
        &self.environment
    }
}

/// Bounded files and deadlines owned by the daemon for one child.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessSupervisionSpec {
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    stdout_byte_limit: u64,
    stderr_byte_limit: u64,
    wall_limit: Duration,
    termination_grace: Duration,
}

impl ProcessSupervisionSpec {
    pub fn new(
        stdout_path: PathBuf,
        stderr_path: PathBuf,
        stdout_byte_limit: u64,
        stderr_byte_limit: u64,
        wall_limit: Duration,
        termination_grace: Duration,
    ) -> Result<Self, ProcessCustodyError> {
        require_absolute_path("stdout capture", &stdout_path)?;
        require_absolute_path("stderr capture", &stderr_path)?;
        if stdout_path == stderr_path {
            return Err(ProcessCustodyError::CapturePathsOverlap);
        }
        if stdout_byte_limit == 0 || stderr_byte_limit == 0 {
            return Err(ProcessCustodyError::ZeroStreamLimit);
        }
        if wall_limit.is_zero() {
            return Err(ProcessCustodyError::ZeroWallLimit);
        }
        if termination_grace.is_zero() {
            return Err(ProcessCustodyError::ZeroTerminationGrace);
        }
        Ok(Self {
            stdout_path,
            stderr_path,
            stdout_byte_limit,
            stderr_byte_limit,
            wall_limit,
            termination_grace,
        })
    }

    /// Absolute daemon-owned capture destination for child stdout.
    #[must_use]
    pub fn stdout_path(&self) -> &Path {
        &self.stdout_path
    }

    /// Absolute daemon-owned capture destination for child stderr.
    #[must_use]
    pub fn stderr_path(&self) -> &Path {
        &self.stderr_path
    }
}

/// Cloneable daemon-side cancellation input. It names no process and cannot
/// signal arbitrary PIDs; only the owning [`SpawnedPiHost`] consumes it.
#[derive(Clone, Debug)]
pub struct ProcessCancellation {
    requested: Arc<AtomicBool>,
}

impl ProcessCancellation {
    pub fn request(&self) {
        self.requested.store(true, Ordering::Release);
    }
}

/// One child which the daemon spawned directly and must wait directly.
#[derive(Debug)]
pub struct SpawnedPiHost {
    child: Child,
    custody: ProcessCustodyV1,
    started: Instant,
    supervision: ProcessSupervisionSpec,
    cancellation: ProcessCancellation,
    stdout_limit_exceeded: Arc<AtomicBool>,
    stderr_limit_exceeded: Arc<AtomicBool>,
    stdout_capture: JoinHandle<Result<u64, ProcessCustodyError>>,
    stderr_capture: JoinHandle<Result<u64, ProcessCustodyError>>,
}

impl SpawnedPiHost {
    #[must_use]
    pub const fn custody(&self) -> ProcessCustodyV1 {
        self.custody
    }

    #[must_use]
    pub fn cancellation(&self) -> ProcessCancellation {
        self.cancellation.clone()
    }

    /// Waits in a blocking worker so the smol executor remains available for
    /// the actor connection and daemon shutdown path.
    pub async fn wait(self) -> Result<SupervisedProcessOutcome, ProcessCustodyError> {
        smol::unblock(move || self.wait_blocking()).await
    }

    fn wait_blocking(mut self) -> Result<SupervisedProcessOutcome, ProcessCustodyError> {
        let mut forced_reason = None;
        loop {
            if let Some(status) = self.child.try_wait().map_err(ProcessCustodyError::Wait)? {
                return self.finish(status, forced_reason, false);
            }
            if self.cancellation.requested.load(Ordering::Acquire) {
                forced_reason = Some(ProcessStopReason::Cancelled);
                break;
            }
            if self.stdout_limit_exceeded.load(Ordering::Acquire) {
                forced_reason = Some(ProcessStopReason::StdoutLimit);
                break;
            }
            if self.stderr_limit_exceeded.load(Ordering::Acquire) {
                forced_reason = Some(ProcessStopReason::StderrLimit);
                break;
            }
            if self.started.elapsed() >= self.supervision.wall_limit {
                forced_reason = Some(ProcessStopReason::Deadline);
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }

        signal_group(self.custody.pgid, Signal::TERM)?;
        let grace_started = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().map_err(ProcessCustodyError::Wait)? {
                return self.finish(status, forced_reason, false);
            }
            if grace_started.elapsed() >= self.supervision.termination_grace {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        signal_group(self.custody.pgid, Signal::KILL)?;
        let status = self.child.wait().map_err(ProcessCustodyError::Wait)?;
        self.finish(status, forced_reason, true)
    }

    fn finish(
        self,
        status: ExitStatus,
        forced_reason: Option<ProcessStopReason>,
        escalated_to_kill: bool,
    ) -> Result<SupervisedProcessOutcome, ProcessCustodyError> {
        let stdout_bytes = join_capture(self.stdout_capture, "stdout")?;
        let stderr_bytes = join_capture(self.stderr_capture, "stderr")?;
        let reason = forced_reason.unwrap_or_else(|| {
            if status.success() {
                ProcessStopReason::Exited
            } else {
                ProcessStopReason::NonZeroExit
            }
        });
        Ok(SupervisedProcessOutcome {
            custody: self.custody,
            reason,
            exit_code: status.code(),
            signal: status.signal(),
            escalated_to_kill,
            stdout_bytes,
            stderr_bytes,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessStopReason {
    Exited,
    NonZeroExit,
    Cancelled,
    Deadline,
    StdoutLimit,
    StderrLimit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SupervisedProcessOutcome {
    pub custody: ProcessCustodyV1,
    pub reason: ProcessStopReason,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub escalated_to_kill: bool,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
}

/// Spawns one exact Rust host behind its inherited connected socket. The
/// socket becomes child descriptor zero; the host reads the admission frame
/// before constructing an agent and then uses the same full-duplex descriptor
/// for narrow tools. The caller retains the socket's server end.
pub fn spawn_pi_host(
    spec: &PiHostSpawnSpec,
    actor_client: StdUnixStream,
    supervision: ProcessSupervisionSpec,
) -> Result<SpawnedPiHost, ProcessCustodyError> {
    let mut command = Command::new(spec.executable());
    command.args(spec.arguments());
    command.current_dir(spec.working_directory());
    command.env_clear();
    command.envs(spec.environment().iter().cloned());
    let actor_fd: OwnedFd = actor_client.into();
    command.stdin(Stdio::from(actor_fd));
    spawn_owned_command(command, supervision)
}

fn spawn_owned_command(
    mut command: Command,
    supervision: ProcessSupervisionSpec,
) -> Result<SpawnedPiHost, ProcessCustodyError> {
    let stdout_file = create_capture_file(&supervision.stdout_path)?;
    let stderr_file = create_capture_file(&supervision.stderr_path)?;
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.process_group(0);
    let started_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ProcessCustodyError::ClockBeforeUnixEpoch)?;
    let mut child = command.spawn().map_err(ProcessCustodyError::Spawn)?;
    let pid = child.id();
    let pgid = pid;
    let stdout = child
        .stdout
        .take()
        .ok_or(ProcessCustodyError::MissingChildStream { stream: "stdout" })?;
    let stderr = child
        .stderr
        .take()
        .ok_or(ProcessCustodyError::MissingChildStream { stream: "stderr" })?;
    let stdout_limit_exceeded = Arc::new(AtomicBool::new(false));
    let stderr_limit_exceeded = Arc::new(AtomicBool::new(false));
    let stdout_capture = capture_stream(
        stdout,
        stdout_file,
        supervision.stdout_byte_limit,
        Arc::clone(&stdout_limit_exceeded),
    );
    let stderr_capture = capture_stream(
        stderr,
        stderr_file,
        supervision.stderr_byte_limit,
        Arc::clone(&stderr_limit_exceeded),
    );
    Ok(SpawnedPiHost {
        child,
        custody: ProcessCustodyV1 {
            pid,
            pgid,
            started_at_unix_millis: u64::try_from(started_at.as_millis())
                .map_err(|_| ProcessCustodyError::TimestampOutOfRange)?,
        },
        started: Instant::now(),
        supervision,
        cancellation: ProcessCancellation {
            requested: Arc::new(AtomicBool::new(false)),
        },
        stdout_limit_exceeded,
        stderr_limit_exceeded,
        stdout_capture,
        stderr_capture,
    })
}

fn create_capture_file(path: &Path) -> Result<File, ProcessCustodyError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| ProcessCustodyError::CaptureOpen {
            path: path.to_owned(),
            source,
        })
}

fn capture_stream<R>(
    mut source: R,
    mut destination: File,
    byte_limit: u64,
    exceeded: Arc<AtomicBool>,
) -> JoinHandle<Result<u64, ProcessCustodyError>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut total = 0_u64;
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            let count = source
                .read(&mut buffer)
                .map_err(ProcessCustodyError::CaptureRead)?;
            if count == 0 {
                break;
            }
            let remaining = byte_limit.saturating_sub(total);
            let admitted = usize::try_from(remaining.min(count as u64))
                .map_err(|_| ProcessCustodyError::StreamCountOutOfRange)?;
            if admitted > 0 {
                destination
                    .write_all(&buffer[..admitted])
                    .map_err(ProcessCustodyError::CaptureWrite)?;
                total = total
                    .checked_add(admitted as u64)
                    .ok_or(ProcessCustodyError::StreamCountOutOfRange)?;
            }
            if admitted < count {
                exceeded.store(true, Ordering::Release);
            }
        }
        destination
            .sync_all()
            .map_err(ProcessCustodyError::CaptureSync)?;
        Ok(total)
    })
}

fn signal_group(pgid: u32, signal: Signal) -> Result<(), ProcessCustodyError> {
    let raw = i32::try_from(pgid).map_err(|_| ProcessCustodyError::PidOutOfRange { pid: pgid })?;
    let pid = Pid::from_raw(raw).ok_or(ProcessCustodyError::PidOutOfRange { pid: pgid })?;
    match kill_process_group(pid, signal) {
        Ok(()) => Ok(()),
        Err(source) if source == rustix::io::Errno::SRCH => Ok(()),
        Err(source) => Err(ProcessCustodyError::Signal { signal, source }),
    }
}

fn join_capture(
    handle: JoinHandle<Result<u64, ProcessCustodyError>>,
    stream: &'static str,
) -> Result<u64, ProcessCustodyError> {
    handle
        .join()
        .map_err(|_| ProcessCustodyError::CaptureThreadPanicked { stream })?
}

fn require_absolute_path(field: &'static str, path: &Path) -> Result<(), ProcessCustodyError> {
    if path.is_absolute() && !path.as_os_str().is_empty() {
        Ok(())
    } else {
        Err(ProcessCustodyError::PathNotAbsolute { field })
    }
}

fn validate_environment_name(name: &OsStr) -> Result<(), ProcessCustodyError> {
    let Some(name) = name.to_str() else {
        return Err(ProcessCustodyError::InvalidEnvironmentName);
    };
    if name.is_empty()
        || name.len() > 160
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        || name.as_bytes()[0].is_ascii_digit()
    {
        return Err(ProcessCustodyError::InvalidEnvironmentName);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ProcessCustodyError {
    #[error("{field} must be an absolute host path")]
    PathNotAbsolute { field: &'static str },

    #[error("actor source descriptor must be inherited stdin (FD 0), received {actor_source_fd}")]
    ActorDescriptorMustBeStdin { actor_source_fd: RawFd },

    #[error("child environment name is not safe uppercase ASCII")]
    InvalidEnvironmentName,

    #[error("child environment repeats or replaces {name:?}")]
    DuplicateEnvironmentName { name: OsString },

    #[error("child environment value for {name:?} contains NUL")]
    EnvironmentValueContainsNul { name: OsString },

    #[error("child environment value for {name:?} is empty")]
    EnvironmentValueEmpty { name: OsString },

    #[error("stdout and stderr capture paths must be distinct")]
    CapturePathsOverlap,

    #[error("child stream byte limits must be positive")]
    ZeroStreamLimit,

    #[error("child wall limit must be positive")]
    ZeroWallLimit,

    #[error("child termination grace must be positive")]
    ZeroTerminationGrace,

    #[error("failed to create capture file {path:?}: {source}")]
    CaptureOpen {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to spawn child: {0}")]
    Spawn(#[source] io::Error),

    #[error("spawned child did not expose its piped {stream}")]
    MissingChildStream { stream: &'static str },

    #[error("system clock predates the Unix epoch")]
    ClockBeforeUnixEpoch,

    #[error("process timestamp cannot be represented")]
    TimestampOutOfRange,

    #[error("process ID {pid} cannot be represented")]
    PidOutOfRange { pid: u32 },

    #[error("failed to wait for child: {0}")]
    Wait(#[source] io::Error),

    #[error("failed to signal process group with {signal:?}: {source}")]
    Signal {
        signal: Signal,
        #[source]
        source: rustix::io::Errno,
    },

    #[error("failed reading a child stream: {0}")]
    CaptureRead(#[source] io::Error),

    #[error("failed writing a child capture: {0}")]
    CaptureWrite(#[source] io::Error),

    #[error("failed syncing a child capture: {0}")]
    CaptureSync(#[source] io::Error),

    #[error("child stream byte count cannot be represented")]
    StreamCountOutOfRange,

    #[error("{stream} capture thread panicked")]
    CaptureThreadPanicked { stream: &'static str },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_capture_paths(label: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "factory-v3-process-{label}-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        std::fs::create_dir(&root).expect("create isolated capture root");
        (root.join("stdout"), root.join("stderr"), root)
    }

    fn supervision(label: &str, limit: u64, wall: Duration) -> (ProcessSupervisionSpec, PathBuf) {
        let (stdout, stderr, root) = temporary_capture_paths(label);
        (
            ProcessSupervisionSpec::new(
                stdout,
                stderr,
                limit,
                limit,
                wall,
                Duration::from_millis(40),
            )
            .expect("supervision"),
            root,
        )
    }

    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(script);
        command.env_clear();
        command.stdin(Stdio::null());
        command
    }

    fn spec(environment: Vec<(OsString, OsString)>) -> PiHostSpawnSpec {
        PiHostSpawnSpec::new(
            PathBuf::from("/factory/factory-pi-host"),
            PathBuf::from("/work/repository"),
            0,
            environment,
        )
        .expect("valid spawn spec")
    }

    #[test]
    fn exact_rust_launch_has_no_interpreter_install_or_resume_path() {
        let spec = spec(vec![(
            OsString::from("ANTHROPIC_API_KEY"),
            OsString::from("secret"),
        )]);
        assert_eq!(spec.executable(), Path::new("/factory/factory-pi-host"));
        assert!(spec.arguments().is_empty());
        assert_eq!(spec.working_directory(), Path::new("/work/repository"));
        assert_eq!(spec.actor_source_fd(), 0);
        assert_eq!(
            spec.environment(),
            [
                (OsString::from("NO_COLOR"), OsString::from("1")),
                (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
                (
                    OsString::from("ANTHROPIC_API_KEY"),
                    OsString::from("secret")
                ),
            ]
        );
    }

    #[test]
    fn rejects_relative_paths_non_stdin_descriptors_and_environment_ambiguity() {
        let relative = PiHostSpawnSpec::new(
            PathBuf::from("factory-pi-host"),
            PathBuf::from("/work"),
            0,
            Vec::new(),
        );
        assert!(matches!(
            relative,
            Err(ProcessCustodyError::PathNotAbsolute {
                field: "Rust agent host executable"
            })
        ));

        let invalid_fd = PiHostSpawnSpec::new(
            PathBuf::from("/factory-pi-host"),
            PathBuf::from("/work"),
            -1,
            Vec::new(),
        );
        assert!(matches!(
            invalid_fd,
            Err(ProcessCustodyError::ActorDescriptorMustBeStdin {
                actor_source_fd: -1
            })
        ));

        let non_stdin_fd = PiHostSpawnSpec::new(
            PathBuf::from("/factory-pi-host"),
            PathBuf::from("/work"),
            3,
            Vec::new(),
        );
        assert!(matches!(
            non_stdin_fd,
            Err(ProcessCustodyError::ActorDescriptorMustBeStdin { actor_source_fd: 3 })
        ));

        let replaced = PiHostSpawnSpec::new(
            PathBuf::from("/factory-pi-host"),
            PathBuf::from("/work"),
            0,
            vec![(OsString::from("NO_COLOR"), OsString::from("0"))],
        );
        assert!(matches!(
            replaced,
            Err(ProcessCustodyError::DuplicateEnvironmentName { name })
                if name == "NO_COLOR"
        ));
    }

    #[test]
    fn assignment_spawn_spec_has_no_ambient_runtime_state() {
        let spec = PiHostSpawnSpec::new_for_assignment(
            PathBuf::from("/factory/factory-pi-host"),
            PathBuf::from("/work/repository"),
            0,
            Vec::new(),
        )
        .expect("assignment spawn spec");

        assert_eq!(spec.executable(), Path::new("/factory/factory-pi-host"));
        assert!(spec.arguments().is_empty());
        assert!(
            !spec.environment().iter().any(|(name, _)| {
                name == OsStr::new("HOME") || name == OsStr::new("PI_SESSION")
            })
        );
    }

    #[test]
    fn direct_wait_preserves_nonzero_status_and_bounded_streams() {
        let (supervision, root) = supervision("nonzero", 128, Duration::from_secs(2));
        let process = spawn_owned_command(
            shell("printf 'standard'; printf 'failure' >&2; exit 7"),
            supervision,
        )
        .expect("spawn child double");
        let outcome = smol::block_on(process.wait()).expect("wait child double");
        assert_eq!(outcome.reason, ProcessStopReason::NonZeroExit);
        assert_eq!(outcome.exit_code, Some(7));
        assert_eq!(outcome.stdout_bytes, 8);
        assert_eq!(outcome.stderr_bytes, 7);
        assert_eq!(
            std::fs::read(root.join("stdout")).expect("stdout"),
            b"standard"
        );
        assert_eq!(
            std::fs::read(root.join("stderr")).expect("stderr"),
            b"failure"
        );
        std::fs::remove_dir_all(root).expect("remove isolated capture root");
    }

    #[test]
    fn output_limit_terminates_the_exact_process_group() {
        let (supervision, root) = supervision("limit", 128, Duration::from_secs(2));
        let process =
            spawn_owned_command(shell("while :; do printf '0123456789'; done"), supervision)
                .expect("spawn child double");
        let outcome = smol::block_on(process.wait()).expect("wait child double");
        assert_eq!(outcome.reason, ProcessStopReason::StdoutLimit);
        assert_eq!(outcome.stdout_bytes, 128);
        assert_eq!(
            std::fs::metadata(root.join("stdout"))
                .expect("stdout")
                .len(),
            128
        );
        std::fs::remove_dir_all(root).expect("remove isolated capture root");
    }

    #[test]
    fn deadline_escalates_from_term_to_kill_and_waits_directly() {
        // Leave ample time for the shell to install its TERM disposition;
        // this judge is about escalation, not scheduler startup latency.
        let (supervision, root) = supervision("escalation", 128, Duration::from_millis(200));
        let process = spawn_owned_command(
            shell("trap '' TERM; printf ready; while :; do :; done"),
            supervision,
        )
        .expect("spawn child double");
        let outcome = smol::block_on(process.wait()).expect("wait child double");
        assert_eq!(outcome.reason, ProcessStopReason::Deadline);
        assert!(outcome.escalated_to_kill);
        assert_eq!(outcome.signal, Some(9));
        std::fs::remove_dir_all(root).expect("remove isolated capture root");
    }

    #[test]
    fn cancellation_handle_cannot_choose_an_unowned_pid() {
        let (supervision, root) = supervision("cancel", 128, Duration::from_secs(2));
        let process =
            spawn_owned_command(shell("sleep 5"), supervision).expect("spawn child double");
        let cancellation = process.cancellation();
        cancellation.request();
        let outcome = smol::block_on(process.wait()).expect("wait child double");
        assert_eq!(outcome.reason, ProcessStopReason::Cancelled);
        std::fs::remove_dir_all(root).expect("remove isolated capture root");
    }

    #[test]
    fn child_exec_refusal_is_explicit_and_never_becomes_custody() {
        let (supervision, root) = supervision("refusal", 128, Duration::from_secs(2));
        let mut command = Command::new("/factory-v3-test/does-not-exist");
        command.env_clear();
        command.stdin(Stdio::null());
        assert!(matches!(
            spawn_owned_command(command, supervision),
            Err(ProcessCustodyError::Spawn(_))
        ));
        std::fs::remove_dir_all(root).expect("remove isolated capture root");
    }
}
