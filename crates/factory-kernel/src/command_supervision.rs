//! Bounded, deterministic custody for application-declared local commands.
//!
//! This module is deliberately separate from `process_custody`: Pi hosts have
//! an inherited authority descriptor and a durable session lifecycle, while a
//! reproducer or validation command is an application-declared deterministic
//! child. Both paths nevertheless use the same important Unix custody shape:
//! clear environment, direct child/process-group ownership, bounded stream
//! capture, TERM/KILL escalation, and a direct wait.
//!
//! The runner accepts a closed [`CommandProfileV2`] plus typed supplemental
//! evidence. It never receives a command line string, invokes a shell, or
//! expands arguments. Artifact adoption and durable validation rows remain
//! outside this module; [`ExactBytes`] is the narrow verified-bytes seam for
//! those owners.

use std::{
    collections::BTreeSet,
    env,
    ffi::OsString,
    fs,
    io::{self, Read, Write},
    os::unix::{
        fs::MetadataExt as _,
        process::{CommandExt as _, ExitStatusExt as _},
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use factory_protocol::{
    ApprovedToolV2, CommandProfileV2, ContentDigest, ExecutableV2, RepositoryRelativePath,
};
use rustix::process::{Pid, Signal, kill_process_group};
use thiserror::Error;

/// Upper bound on one profile's declared arguments.
pub const COMMAND_ARGUMENT_LIMIT: usize = 128;
/// Upper bound on one declared argument's UTF-8 bytes.
pub const COMMAND_ARGUMENT_BYTE_LIMIT: usize = 32 * 1024;
/// Upper bound on declared environment additions.
pub const COMMAND_ENVIRONMENT_LIMIT: usize = 32;
/// Upper bound on one declared environment value's bytes.
pub const COMMAND_ENVIRONMENT_VALUE_BYTE_LIMIT: usize = 4 * 1024;
/// Upper bound on either captured stream.
pub const COMMAND_STREAM_BYTE_LIMIT: u64 = 64 * 1024 * 1024;
/// Upper bound on a single deterministic command's wall duration.
pub const COMMAND_TIMEOUT_LIMIT: Duration = Duration::from_secs(60 * 60);
/// Upper bound on bytes supplied through inline or adopted-artifact seams.
pub const COMMAND_INPUT_BYTE_LIMIT: usize = 64 * 1024 * 1024;
/// Bounded default between TERM and KILL for deterministic children.
pub const DEFAULT_TERMINATION_GRACE: Duration = Duration::from_secs(1);

/// The process environment that every command starts with.
///
/// A profile may add environment entries but cannot replace these values. The
/// explicit `PATH` permits ordinary interpreter shebangs and tool-internal
/// subcommands while avoiding an ambient operator path.
const MINIMAL_ENVIRONMENT: [(&str, &str); 4] = [
    ("LANG", "C"),
    ("LC_ALL", "C"),
    ("PATH", "/usr/bin:/bin"),
    ("TZ", "UTC"),
];
const CARGO_TOOLCHAIN_ENVIRONMENT_NAMES: [&str; 2] = ["RUSTC", "RUSTDOC"];

/// A canonical regular executable selected outside an application command
/// profile. It represents the installed location of one approved host tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactExecutable(PathBuf);

impl ExactExecutable {
    /// Resolves and verifies one absolute, executable regular file.
    pub fn discover(path: impl AsRef<Path>) -> Result<Self, CommandSupervisionError> {
        let path = path.as_ref();
        if !path.is_absolute() {
            return Err(CommandSupervisionError::ExecutableNotAbsolute(
                path.to_owned(),
            ));
        }
        let canonical = fs::canonicalize(path).map_err(|source| CommandSupervisionError::Io {
            operation: "canonicalize approved executable",
            path: path.to_owned(),
            source,
        })?;
        require_regular_executable(&canonical)?;
        Ok(Self(canonical))
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }

    fn sibling(&self, name: &'static str) -> Result<PathBuf, CommandSupervisionError> {
        let candidate = self
            .0
            .parent()
            .ok_or_else(|| CommandSupervisionError::ExecutableNotAbsolute(self.0.clone()))?
            .join(name);
        let canonical =
            fs::canonicalize(&candidate).map_err(|source| CommandSupervisionError::Io {
                operation: "canonicalize Cargo toolchain executable",
                path: candidate,
                source,
            })?;
        require_regular_executable(&canonical)?;
        Ok(canonical)
    }
}

/// The installed exact executables that implement closed application-approved
/// tool identities. An application profile chooses an identity, never a host
/// search path or arbitrary executable name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovedToolExecutables {
    cargo: ExactExecutable,
    git: ExactExecutable,
}

impl ApprovedToolExecutables {
    #[must_use]
    pub const fn new(cargo: ExactExecutable, git: ExactExecutable) -> Self {
        Self { cargo, git }
    }

    fn resolve(&self, tool: ApprovedToolV2) -> &ExactExecutable {
        match tool {
            ApprovedToolV2::Cargo => &self.cargo,
            ApprovedToolV2::Git => &self.git,
        }
    }

    fn cargo_toolchain_environment(
        &self,
    ) -> Result<Vec<(&'static str, OsString)>, CommandSupervisionError> {
        let cargo_directory = self.cargo.path().parent().ok_or_else(|| {
            CommandSupervisionError::ExecutableNotAbsolute(self.cargo.path().into())
        })?;
        // Cargo-driven integration suites can deliberately invoke `cargo`
        // again from their own test binary. That child must resolve the
        // already-qualified Cargo executable, never an ambient operator path.
        let path = env::join_paths([cargo_directory, Path::new("/usr/bin"), Path::new("/bin")])
            .map_err(|_| CommandSupervisionError::ToolPathUnrepresentable)?;
        Ok(vec![
            ("RUSTC", self.cargo.sibling("rustc")?.into_os_string()),
            ("RUSTDOC", self.cargo.sibling("rustdoc")?.into_os_string()),
            ("PATH", path),
        ])
    }
}

/// A canonical repository root that command working directories and
/// repository-local executables resolve beneath.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandWorkspace(PathBuf);

impl CommandWorkspace {
    /// Opens a real, canonical directory. The kernel creates the workspace;
    /// callers cannot use a relative cwd to escape it.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, CommandSupervisionError> {
        let requested = root.as_ref();
        let canonical =
            fs::canonicalize(requested).map_err(|source| CommandSupervisionError::Io {
                operation: "canonicalize command workspace",
                path: requested.to_owned(),
                source,
            })?;
        if !fs::metadata(&canonical)
            .map_err(|source| CommandSupervisionError::Io {
                operation: "inspect command workspace",
                path: canonical.clone(),
                source,
            })?
            .is_dir()
        {
            return Err(CommandSupervisionError::WorkspaceNotDirectory(canonical));
        }
        Ok(Self(canonical))
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.0
    }

    fn resolve_directory(
        &self,
        path: &RepositoryRelativePath,
    ) -> Result<PathBuf, CommandSupervisionError> {
        if path.as_str() == "." {
            return Ok(self.0.clone());
        }
        reject_symlink_components(&self.0, path)?;
        let candidate = self.0.join(path.as_str());
        let canonical =
            fs::canonicalize(&candidate).map_err(|source| CommandSupervisionError::Io {
                operation: "canonicalize command working directory",
                path: candidate,
                source,
            })?;
        require_canonical_child(&self.0, path, &canonical)?;
        if !fs::metadata(&canonical)
            .map_err(|source| CommandSupervisionError::Io {
                operation: "inspect command working directory",
                path: canonical.clone(),
                source,
            })?
            .is_dir()
        {
            return Err(CommandSupervisionError::WorkingDirectoryNotDirectory(
                path.clone(),
            ));
        }
        Ok(canonical)
    }

    fn resolve_executable(
        &self,
        path: &RepositoryRelativePath,
    ) -> Result<PathBuf, CommandSupervisionError> {
        if path.as_str() == "." {
            return Err(CommandSupervisionError::RepositoryExecutableIsDirectory(
                path.clone(),
            ));
        }
        reject_symlink_components(&self.0, path)?;
        let candidate = self.0.join(path.as_str());
        let canonical =
            fs::canonicalize(&candidate).map_err(|source| CommandSupervisionError::Io {
                operation: "canonicalize repository executable",
                path: candidate,
                source,
            })?;
        require_canonical_child(&self.0, path, &canonical)?;
        require_regular_executable(&canonical)?;
        Ok(canonical)
    }
}

/// An immutable expected-output or stdin artifact seam. The caller may load
/// bytes from CAS, but a digest mismatch is rejected before a child starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactBytes {
    digest: ContentDigest,
    bytes: Vec<u8>,
}

impl ExactBytes {
    /// Creates an inline exact-byte value with a derived content identity.
    pub fn inline(bytes: Vec<u8>) -> Result<Self, CommandSupervisionError> {
        enforce_input_limit(bytes.len())?;
        Ok(Self {
            digest: ContentDigest::of_bytes(&bytes),
            bytes,
        })
    }

    /// Verifies bytes loaded from a separately owned sealed artifact.
    pub fn from_artifact(
        digest: ContentDigest,
        bytes: Vec<u8>,
    ) -> Result<Self, CommandSupervisionError> {
        enforce_input_limit(bytes.len())?;
        if ContentDigest::of_bytes(&bytes) != digest {
            return Err(CommandSupervisionError::ArtifactDigestMismatch);
        }
        Ok(Self { digest, bytes })
    }

    #[must_use]
    pub const fn digest(&self) -> ContentDigest {
        self.digest
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Optional standard input for one exact command.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum CommandStdin {
    /// The child receives an immediate EOF.
    #[default]
    Empty,
    /// Inline bytes, bounded by [`COMMAND_INPUT_BYTE_LIMIT`].
    Inline(ExactBytes),
    /// Bytes obtained from separately sealed artifact custody.
    Artifact(ExactBytes),
}

impl CommandStdin {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Empty => &[],
            Self::Inline(bytes) | Self::Artifact(bytes) => bytes.bytes(),
        }
    }
}

/// A versioned exact comparison rule. It is carried to every receipt so the
/// later durable owner can distinguish observations judged under different
/// application revisions without treating prose as a comparison rule.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ComparisonRevision(String);

impl ComparisonRevision {
    pub fn parse(value: impl Into<String>) -> Result<Self, CommandSupervisionError> {
        let value = value.into();
        if value.is_empty() || value.len() > 160 || value.contains('\0') {
            return Err(CommandSupervisionError::InvalidComparisonRevision);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Expected observations in addition to the profile's exact exit status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandExpectation {
    comparison_revision: ComparisonRevision,
    expected_stdout: Option<ExactBytes>,
    expected_stderr: Option<ExactBytes>,
}

impl CommandExpectation {
    #[must_use]
    pub const fn new(
        comparison_revision: ComparisonRevision,
        expected_stdout: Option<ExactBytes>,
        expected_stderr: Option<ExactBytes>,
    ) -> Self {
        Self {
            comparison_revision,
            expected_stdout,
            expected_stderr,
        }
    }

    #[must_use]
    pub fn comparison_revision(&self) -> &ComparisonRevision {
        &self.comparison_revision
    }
}

/// A bounded command profile plus exact input and comparison evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeterministicCommand {
    profile: CommandProfileV2,
    stdin: CommandStdin,
    expectation: CommandExpectation,
}

impl DeterministicCommand {
    /// Validates the profile independently at the process boundary. Bundle
    /// admission normally performed these checks earlier; repetition here
    /// prevents a malformed in-memory caller from widening host authority.
    pub fn new(
        profile: CommandProfileV2,
        stdin: CommandStdin,
        expectation: CommandExpectation,
    ) -> Result<Self, CommandSupervisionError> {
        validate_profile(&profile)?;
        if expectation
            .expected_stdout
            .as_ref()
            .is_some_and(|bytes| bytes.bytes().len() > profile.stdout_byte_limit as usize)
        {
            return Err(CommandSupervisionError::ExpectedStreamExceedsLimit { stream: "stdout" });
        }
        if expectation
            .expected_stderr
            .as_ref()
            .is_some_and(|bytes| bytes.bytes().len() > profile.stderr_byte_limit as usize)
        {
            return Err(CommandSupervisionError::ExpectedStreamExceedsLimit { stream: "stderr" });
        }
        Ok(Self {
            profile,
            stdin,
            expectation,
        })
    }

    #[must_use]
    pub fn profile(&self) -> &CommandProfileV2 {
        &self.profile
    }

    #[must_use]
    pub fn expectation(&self) -> &CommandExpectation {
        &self.expectation
    }
}

/// One direct child runner configured with exact installed approved-tool paths.
#[derive(Clone, Debug)]
pub struct CommandRunner {
    tools: ApprovedToolExecutables,
    termination_grace: Duration,
}

impl CommandRunner {
    pub fn new(
        tools: ApprovedToolExecutables,
        termination_grace: Duration,
    ) -> Result<Self, CommandSupervisionError> {
        if termination_grace.is_zero() || termination_grace > COMMAND_TIMEOUT_LIMIT {
            return Err(CommandSupervisionError::InvalidTerminationGrace);
        }
        Ok(Self {
            tools,
            termination_grace,
        })
    }

    /// Spawns one command directly. `argv` is passed to `Command::args` as
    /// opaque argument values; neither a command line nor shell is created.
    pub fn run(
        &self,
        workspace: &CommandWorkspace,
        command: &DeterministicCommand,
    ) -> Result<CommandReceipt, CommandSupervisionError> {
        let (executable, cargo_toolchain_environment) = match &command.profile.executable {
            ExecutableV2::ApprovedTool(tool) => (
                self.tools.resolve(*tool).path().to_owned(),
                if *tool == ApprovedToolV2::Cargo {
                    Some(self.tools.cargo_toolchain_environment()?)
                } else {
                    None
                },
            ),
            ExecutableV2::RepositoryPath(path) => (workspace.resolve_executable(path)?, None),
        };
        let cwd = workspace.resolve_directory(&command.profile.working_directory)?;
        let mut child_command = Command::new(&executable);
        child_command.args(command.profile.argv.iter());
        child_command.current_dir(cwd);
        child_command.env_clear();
        child_command.envs(MINIMAL_ENVIRONMENT);
        if let Some(toolchain) = &cargo_toolchain_environment {
            child_command.envs(toolchain.iter().map(|(name, value)| (name, value)));
        }
        child_command.envs(
            command
                .profile
                .environment
                .iter()
                .map(|addition| (&addition.name, &addition.value)),
        );
        child_command.stdin(if matches!(command.stdin, CommandStdin::Empty) {
            Stdio::null()
        } else {
            Stdio::piped()
        });
        child_command.stdout(Stdio::piped());
        child_command.stderr(Stdio::piped());
        child_command.process_group(0);

        let started = Instant::now();
        let mut child = child_command
            .spawn()
            .map_err(CommandSupervisionError::Spawn)?;
        let pid = child.id();
        let stdout = child
            .stdout
            .take()
            .ok_or(CommandSupervisionError::MissingChildStream { stream: "stdout" })?;
        let stderr = child
            .stderr
            .take()
            .ok_or(CommandSupervisionError::MissingChildStream { stream: "stderr" })?;
        let stdout_limit_exceeded = Arc::new(AtomicBool::new(false));
        let stderr_limit_exceeded = Arc::new(AtomicBool::new(false));
        let stdout_capture = capture_stream(
            stdout,
            command.profile.stdout_byte_limit as u64,
            Arc::clone(&stdout_limit_exceeded),
        );
        let stderr_capture = capture_stream(
            stderr,
            command.profile.stderr_byte_limit as u64,
            Arc::clone(&stderr_limit_exceeded),
        );
        let stdin_writer = child
            .stdin
            .take()
            .map(|stdin| write_stdin(stdin, command.stdin.bytes().to_vec()));

        let (terminal, exit_status) = wait_for_terminal(
            &mut child,
            pid,
            Duration::from_millis(command.profile.timeout.get()),
            self.termination_grace,
            &stdout_limit_exceeded,
            &stderr_limit_exceeded,
        )?;
        let stdout = join_capture(stdout_capture, "stdout")?;
        let stderr = join_capture(stderr_capture, "stderr")?;
        if let Some(writer) = stdin_writer {
            join_stdin_writer(writer)?;
        }

        let observed_exit_status = exit_status.and_then(|status| status.code());
        let exit_matches = matches!(terminal, CommandTerminal::Exited { .. })
            && observed_exit_status == Some(command.profile.expected_exit_status);
        let stdout_matches = command
            .expectation
            .expected_stdout
            .as_ref()
            .is_none_or(|expected| expected.bytes() == stdout.as_slice());
        let stderr_matches = command
            .expectation
            .expected_stderr
            .as_ref()
            .is_none_or(|expected| expected.bytes() == stderr.as_slice());

        Ok(CommandReceipt {
            executable,
            argv: command.profile.argv.clone(),
            working_directory: command.profile.working_directory.clone(),
            comparison_revision: command.expectation.comparison_revision.clone(),
            terminal,
            exit_status: observed_exit_status,
            signal: exit_status.and_then(|status| status.signal()),
            elapsed: started.elapsed(),
            stdout,
            stderr,
            exit_matches,
            stdout_matches,
            stderr_matches,
        })
    }

    /// Runs the discovery reproducer exactly twice. A reproducible failure
    /// requires two identical observations which both disagree with the
    /// expected observation. Product's terminal-aware comparison rule admits
    /// stable termination outcomes for cancellation tickets; other rules
    /// continue to require identical normal exits.
    pub fn run_discovery_reproducer(
        &self,
        workspace: &CommandWorkspace,
        command: &DeterministicCommand,
    ) -> Result<DiscoveryReproduction, CommandSupervisionError> {
        let first = self.run(workspace, command)?;
        let second = self.run(workspace, command)?;
        let classification = if first.matches_expectation() && second.matches_expectation() {
            DiscoveryClassification::AlreadyPasses
        } else if !first.matches_expectation()
            && !second.matches_expectation()
            && first.same_actual_observation(&second)
        {
            DiscoveryClassification::ReproducibleFailure
        } else {
            DiscoveryClassification::Divergent
        };
        Ok(DiscoveryReproduction {
            classification,
            first,
            second,
        })
    }

    /// Checks the test-first boundary using separately materialized base,
    /// regression, and candidate workspaces. The targeted command must fail
    /// against base and checkpoint, then pass against the candidate. Thus the
    /// candidate is proved to carry the checkpoint's asserted behavior; this
    /// function intentionally does not judge whether the checkpoint contains
    /// an implementation change, which is a separate changed-path/Quality
    /// gate.
    pub fn verify_regression_checkpoint(
        &self,
        base: &CommandWorkspace,
        regression: &CommandWorkspace,
        candidate: &CommandWorkspace,
        command: &DeterministicCommand,
    ) -> Result<RegressionCheckpointReceipt, CommandSupervisionError> {
        let base = self.run(base, command)?;
        let regression = self.run(regression, command)?;
        let candidate = self.run(candidate, command)?;
        let status = if base.matches_expectation() {
            RegressionCheckpointStatus::BaseAlreadyPasses
        } else if regression.matches_expectation() {
            RegressionCheckpointStatus::CheckpointAlreadyPasses
        } else if !candidate.matches_expectation() {
            RegressionCheckpointStatus::CandidateDoesNotPass
        } else {
            RegressionCheckpointStatus::Verified
        };
        Ok(RegressionCheckpointReceipt {
            status,
            base,
            regression,
            candidate,
        })
    }

    /// Runs the kernel-owned hard validation against one already-materialized
    /// pristine exact source tree. Every configured command is evidenced even
    /// when an earlier command fails; the validation passes only when all
    /// comparisons pass and the Git tracked-tree probe remains exact.
    pub fn run_candidate_validation(
        &self,
        pristine: &PristineWorkspace,
        commands: &[DeterministicCommand],
    ) -> Result<ValidationReceipt, CommandSupervisionError> {
        self.run_validation(ValidationInvocation::Candidate, pristine, commands)
    }

    /// Runs the independently requested Quality validation. The same runner
    /// may be reused, but the separate invocation is explicit in the returned
    /// receipt and requires another [`PristineWorkspace`] construction.
    pub fn run_quality_validation(
        &self,
        pristine: &PristineWorkspace,
        commands: &[DeterministicCommand],
    ) -> Result<ValidationReceipt, CommandSupervisionError> {
        self.run_validation(ValidationInvocation::Quality, pristine, commands)
    }

    fn run_validation(
        &self,
        invocation: ValidationInvocation,
        pristine: &PristineWorkspace,
        commands: &[DeterministicCommand],
    ) -> Result<ValidationReceipt, CommandSupervisionError> {
        if commands.is_empty() {
            return Err(CommandSupervisionError::EmptyValidationProfile);
        }
        let before = pristine.probe.observe(self, &pristine.workspace)?;
        if !before.matches_expected_tree() {
            return Err(CommandSupervisionError::PristineTreeChangedBeforeValidation);
        }
        let mut receipts = Vec::with_capacity(commands.len());
        for command in commands {
            receipts.push(self.run(&pristine.workspace, command)?);
        }
        let after = pristine.probe.observe(self, &pristine.workspace)?;
        let status = if !after.matches_expected_tree() {
            ValidationStatus::TreeChanged
        } else if receipts.iter().all(CommandReceipt::matches_expectation) {
            ValidationStatus::Passed
        } else {
            ValidationStatus::CommandFailed
        };
        Ok(ValidationReceipt {
            invocation,
            exact_tree: pristine.expected_tree().clone(),
            before,
            after,
            commands: receipts,
            status,
        })
    }
}

/// A process terminal state recorded independently of comparison results.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandTerminal {
    Exited { exit_code: i32 },
    Signaled { signal: i32 },
    TimedOut { escalated_to_kill: bool },
    StdoutLimit { escalated_to_kill: bool },
    StderrLimit { escalated_to_kill: bool },
}

/// Exact evidence from one command invocation. Stream bytes are bounded and
/// ready for a caller to seal; this module does not claim artifact custody.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandReceipt {
    executable: PathBuf,
    argv: Vec<String>,
    working_directory: RepositoryRelativePath,
    comparison_revision: ComparisonRevision,
    terminal: CommandTerminal,
    exit_status: Option<i32>,
    signal: Option<i32>,
    elapsed: Duration,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit_matches: bool,
    stdout_matches: bool,
    stderr_matches: bool,
}

impl CommandReceipt {
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    #[must_use]
    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    #[must_use]
    pub fn working_directory(&self) -> &RepositoryRelativePath {
        &self.working_directory
    }

    #[must_use]
    pub fn comparison_revision(&self) -> &ComparisonRevision {
        &self.comparison_revision
    }

    #[must_use]
    pub const fn terminal(&self) -> CommandTerminal {
        self.terminal
    }

    #[must_use]
    pub const fn exit_status(&self) -> Option<i32> {
        self.exit_status
    }

    #[must_use]
    pub const fn signal(&self) -> Option<i32> {
        self.signal
    }

    #[must_use]
    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }

    #[must_use]
    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    #[must_use]
    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    #[must_use]
    pub const fn matches_expectation(&self) -> bool {
        self.exit_matches && self.stdout_matches && self.stderr_matches
    }

    fn same_actual_observation(&self, other: &Self) -> bool {
        if self.comparison_revision.as_str() == "status-only-v1" {
            return product_terminal_matches(&self.terminal, &other.terminal)
                && self.exit_status == other.exit_status
                && self.signal == other.signal;
        }
        if !matches!(self.terminal, CommandTerminal::Exited { .. })
            || !matches!(other.terminal, CommandTerminal::Exited { .. })
            || self.exit_status != other.exit_status
        {
            return false;
        }
        self.stdout == other.stdout && self.stderr == other.stderr
    }
}

fn product_terminal_matches(first: &CommandTerminal, second: &CommandTerminal) -> bool {
    match (first, second) {
        (CommandTerminal::Exited { .. }, CommandTerminal::Exited { .. })
        | (CommandTerminal::Signaled { .. }, CommandTerminal::Signaled { .. })
        | (CommandTerminal::TimedOut { .. }, CommandTerminal::TimedOut { .. }) => true,
        _ => false,
    }
}

/// The three closed outcomes of a two-run discovery reproducer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscoveryClassification {
    ReproducibleFailure,
    AlreadyPasses,
    Divergent,
}

/// Evidence from both discovery runs; neither run is discarded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryReproduction {
    classification: DiscoveryClassification,
    first: CommandReceipt,
    second: CommandReceipt,
}

impl DiscoveryReproduction {
    #[must_use]
    pub const fn classification(&self) -> DiscoveryClassification {
        self.classification
    }

    #[must_use]
    pub fn first(&self) -> &CommandReceipt {
        &self.first
    }

    #[must_use]
    pub fn second(&self) -> &CommandReceipt {
        &self.second
    }
}

/// Result of proving that a candidate carries a failing regression checkpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegressionCheckpointStatus {
    Verified,
    BaseAlreadyPasses,
    CheckpointAlreadyPasses,
    CandidateDoesNotPass,
}

/// All three exact regression receipts, retained even for a failed checkpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegressionCheckpointReceipt {
    status: RegressionCheckpointStatus,
    base: CommandReceipt,
    regression: CommandReceipt,
    candidate: CommandReceipt,
}

impl RegressionCheckpointReceipt {
    #[must_use]
    pub const fn status(&self) -> RegressionCheckpointStatus {
        self.status
    }

    #[must_use]
    pub fn base(&self) -> &CommandReceipt {
        &self.base
    }

    #[must_use]
    pub fn regression(&self) -> &CommandReceipt {
        &self.regression
    }

    #[must_use]
    pub fn candidate(&self) -> &CommandReceipt {
        &self.candidate
    }
}

/// A Git source-tree object identity captured by the candidate-tree custody
/// path. It names tracked source content, not ambient build output, so a
/// compiler's untracked target directory is neither ignored accidentally nor
/// mistaken for candidate source.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GitTreeIdentity(String);

impl GitTreeIdentity {
    /// Parses the SHA-1 or SHA-256 tree object identity emitted by Git.
    pub fn parse(value: impl Into<String>) -> Result<Self, CommandSupervisionError> {
        let value = value.into();
        let valid_length = value.len() == 40 || value.len() == 64;
        if !valid_length
            || !value.bytes().all(|byte| {
                byte.is_ascii_digit() || (byte.is_ascii_lowercase() && byte.is_ascii_hexdigit())
            })
        {
            return Err(CommandSupervisionError::InvalidGitTreeIdentity);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The explicit Git physical-boundary probe for validation source identity.
///
/// The expected object comes from candidate capture. Each observation runs
/// exact configured `git diff --quiet <tree> --` through this module's normal
/// bounded direct-child runner. Git compares the expected tree with the
/// current tracked working tree, including staged or unstaged source changes;
/// it deliberately does not treat an untracked build directory as a source
/// tree mutation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitTrackedTreeProbe {
    expected_tree: GitTreeIdentity,
    timeout: Duration,
    stdout_byte_limit: u32,
    stderr_byte_limit: u32,
    comparison_revision: ComparisonRevision,
}

impl GitTrackedTreeProbe {
    /// Creates a bounded probe for one exact candidate tree object.
    pub fn new(
        expected_tree: GitTreeIdentity,
        timeout: Duration,
        stdout_byte_limit: u32,
        stderr_byte_limit: u32,
        comparison_revision: ComparisonRevision,
    ) -> Result<Self, CommandSupervisionError> {
        if timeout.is_zero() || timeout > COMMAND_TIMEOUT_LIMIT {
            return Err(CommandSupervisionError::InvalidTimeout);
        }
        for (stream, limit) in [("stdout", stdout_byte_limit), ("stderr", stderr_byte_limit)] {
            if limit == 0 || u64::from(limit) > COMMAND_STREAM_BYTE_LIMIT {
                return Err(CommandSupervisionError::InvalidStreamLimit { stream });
            }
        }
        Ok(Self {
            expected_tree,
            timeout,
            stdout_byte_limit,
            stderr_byte_limit,
            comparison_revision,
        })
    }

    #[must_use]
    pub fn expected_tree(&self) -> &GitTreeIdentity {
        &self.expected_tree
    }

    fn observe(
        &self,
        runner: &CommandRunner,
        workspace: &CommandWorkspace,
    ) -> Result<SourceTreeObservation, CommandSupervisionError> {
        let profile = CommandProfileV2 {
            name: "tracked-source-tree-exactness".to_owned(),
            executable: ExecutableV2::ApprovedTool(ApprovedToolV2::Git),
            argv: vec![
                "diff".to_owned(),
                "--quiet".to_owned(),
                self.expected_tree.as_str().to_owned(),
                "--".to_owned(),
            ],
            working_directory: RepositoryRelativePath::parse(".")
                .expect("static root repository-relative path is valid"),
            environment: Vec::new(),
            timeout: factory_protocol::DurationMillis::new(
                u64::try_from(self.timeout.as_millis())
                    .expect("command timeout hard bound fits in u64 milliseconds"),
            ),
            stdout_byte_limit: self.stdout_byte_limit,
            stderr_byte_limit: self.stderr_byte_limit,
            expected_exit_status: 0,
        };
        let command = DeterministicCommand::new(
            profile,
            CommandStdin::Empty,
            CommandExpectation::new(self.comparison_revision.clone(), None, None),
        )?;
        Ok(SourceTreeObservation {
            expected_tree: self.expected_tree.clone(),
            receipt: runner.run(workspace, &command)?,
        })
    }
}

/// Exact Git source-tree evidence from one pre- or post-validation probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceTreeObservation {
    expected_tree: GitTreeIdentity,
    receipt: CommandReceipt,
}

impl SourceTreeObservation {
    /// `true` only when the configured Git process reached normal exit zero.
    #[must_use]
    pub const fn matches_expected_tree(&self) -> bool {
        self.receipt.matches_expectation()
    }

    #[must_use]
    pub fn expected_tree(&self) -> &GitTreeIdentity {
        &self.expected_tree
    }

    #[must_use]
    pub fn receipt(&self) -> &CommandReceipt {
        &self.receipt
    }
}

/// An already-materialized candidate workspace and its explicit tracked-source
/// identity probe. The probe runs immediately before and after validation;
/// construction alone makes no claim about the mutable filesystem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PristineWorkspace {
    workspace: CommandWorkspace,
    probe: GitTrackedTreeProbe,
}

impl PristineWorkspace {
    #[must_use]
    pub const fn new(workspace: CommandWorkspace, probe: GitTrackedTreeProbe) -> Self {
        Self { workspace, probe }
    }

    #[must_use]
    pub fn workspace(&self) -> &CommandWorkspace {
        &self.workspace
    }

    #[must_use]
    pub fn expected_tree(&self) -> &GitTreeIdentity {
        self.probe.expected_tree()
    }
}

/// The authority phase which produced an independently retainable validation
/// receipt. It is not inferred from a command name or a qualitative report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidationInvocation {
    Candidate,
    Quality,
}

/// Closed validation outcome based on command comparisons and tree custody.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidationStatus {
    Passed,
    CommandFailed,
    TreeChanged,
}

/// Evidence returned by one hard or independent Quality validation invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationReceipt {
    invocation: ValidationInvocation,
    exact_tree: GitTreeIdentity,
    before: SourceTreeObservation,
    after: SourceTreeObservation,
    commands: Vec<CommandReceipt>,
    status: ValidationStatus,
}

impl ValidationReceipt {
    #[must_use]
    pub const fn invocation(&self) -> ValidationInvocation {
        self.invocation
    }

    #[must_use]
    pub fn exact_tree(&self) -> &GitTreeIdentity {
        &self.exact_tree
    }

    #[must_use]
    pub fn before(&self) -> &SourceTreeObservation {
        &self.before
    }

    #[must_use]
    pub fn after(&self) -> &SourceTreeObservation {
        &self.after
    }

    #[must_use]
    pub fn commands(&self) -> &[CommandReceipt] {
        &self.commands
    }

    #[must_use]
    pub const fn status(&self) -> ValidationStatus {
        self.status
    }
}

fn validate_profile(profile: &CommandProfileV2) -> Result<(), CommandSupervisionError> {
    if profile.name.is_empty() || profile.name.len() > 160 || profile.name.contains('\0') {
        return Err(CommandSupervisionError::InvalidProfileName);
    }
    if profile.argv.len() > COMMAND_ARGUMENT_LIMIT {
        return Err(CommandSupervisionError::TooManyArguments);
    }
    if profile
        .argv
        .iter()
        .any(|argument| argument.contains('\0') || argument.len() > COMMAND_ARGUMENT_BYTE_LIMIT)
    {
        return Err(CommandSupervisionError::InvalidArgument);
    }
    if profile.expected_exit_status < 0 || profile.expected_exit_status > 255 {
        return Err(CommandSupervisionError::InvalidExpectedExitStatus);
    }
    let timeout = Duration::from_millis(profile.timeout.get());
    if timeout.is_zero() || timeout > COMMAND_TIMEOUT_LIMIT {
        return Err(CommandSupervisionError::InvalidTimeout);
    }
    for (stream, limit) in [
        ("stdout", profile.stdout_byte_limit),
        ("stderr", profile.stderr_byte_limit),
    ] {
        if limit == 0 || u64::from(limit) > COMMAND_STREAM_BYTE_LIMIT {
            return Err(CommandSupervisionError::InvalidStreamLimit { stream });
        }
    }
    if profile.environment.len() > COMMAND_ENVIRONMENT_LIMIT {
        return Err(CommandSupervisionError::TooManyEnvironmentAdditions);
    }
    let mut names = BTreeSet::new();
    for addition in &profile.environment {
        let name = addition.name.as_str();
        let valid_name = !name.is_empty()
            && name.len() <= 160
            && name
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
            && !name.as_bytes()[0].is_ascii_digit();
        if !valid_name || !names.insert(name) {
            return Err(CommandSupervisionError::InvalidEnvironmentName);
        }
        if MINIMAL_ENVIRONMENT
            .iter()
            .any(|(baseline, _)| *baseline == name)
            || CARGO_TOOLCHAIN_ENVIRONMENT_NAMES.contains(&name)
        {
            return Err(CommandSupervisionError::EnvironmentReplacesBaseline(
                addition.name.clone(),
            ));
        }
        if addition.value.contains('\0')
            || addition.value.len() > COMMAND_ENVIRONMENT_VALUE_BYTE_LIMIT
        {
            return Err(CommandSupervisionError::InvalidEnvironmentValue(
                addition.name.clone(),
            ));
        }
    }
    Ok(())
}

fn enforce_input_limit(length: usize) -> Result<(), CommandSupervisionError> {
    if length > COMMAND_INPUT_BYTE_LIMIT {
        Err(CommandSupervisionError::InputTooLarge { observed: length })
    } else {
        Ok(())
    }
}

fn require_regular_executable(path: &Path) -> Result<(), CommandSupervisionError> {
    let metadata = fs::metadata(path).map_err(|source| CommandSupervisionError::Io {
        operation: "inspect executable",
        path: path.to_owned(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(CommandSupervisionError::ExecutableNotRegularFile(
            path.to_owned(),
        ));
    }
    if metadata.mode() & 0o111 == 0 {
        return Err(CommandSupervisionError::ExecutableNotExecutable(
            path.to_owned(),
        ));
    }
    Ok(())
}

fn reject_symlink_components(
    root: &Path,
    relative: &RepositoryRelativePath,
) -> Result<(), CommandSupervisionError> {
    let mut current = root.to_owned();
    for component in relative.as_str().split('/') {
        if component == "." {
            continue;
        }
        current.push(component);
        let metadata =
            fs::symlink_metadata(&current).map_err(|source| CommandSupervisionError::Io {
                operation: "inspect repository-relative command path",
                path: current.clone(),
                source,
            })?;
        if metadata.file_type().is_symlink() {
            return Err(CommandSupervisionError::SymlinkPathRejected(
                relative.clone(),
            ));
        }
    }
    Ok(())
}

fn require_canonical_child(
    root: &Path,
    requested: &RepositoryRelativePath,
    canonical: &Path,
) -> Result<(), CommandSupervisionError> {
    let actual_relative = canonical
        .strip_prefix(root)
        .map_err(|_| CommandSupervisionError::PathEscapesWorkspace(requested.clone()))?;
    if actual_relative != Path::new(requested.as_str()) {
        return Err(CommandSupervisionError::NonCanonicalRepositoryPath(
            requested.clone(),
        ));
    }
    Ok(())
}

fn capture_stream<R>(
    mut source: R,
    byte_limit: u64,
    exceeded: Arc<AtomicBool>,
) -> JoinHandle<Result<Vec<u8>, CommandSupervisionError>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let capacity = usize::try_from(byte_limit.min(64 * 1024))
            .map_err(|_| CommandSupervisionError::StreamCountOutOfRange)?;
        let mut captured = Vec::with_capacity(capacity);
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            let count = source
                .read(&mut buffer)
                .map_err(CommandSupervisionError::CaptureRead)?;
            if count == 0 {
                break;
            }
            let remaining = byte_limit.saturating_sub(captured.len() as u64);
            let admitted = usize::try_from(remaining.min(count as u64))
                .map_err(|_| CommandSupervisionError::StreamCountOutOfRange)?;
            captured.extend_from_slice(&buffer[..admitted]);
            if admitted < count {
                exceeded.store(true, Ordering::Release);
            }
        }
        Ok(captured)
    })
}

fn write_stdin(
    mut stdin: std::process::ChildStdin,
    bytes: Vec<u8>,
) -> JoinHandle<Result<(), CommandSupervisionError>> {
    thread::spawn(move || match stdin.write_all(&bytes) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(source) => Err(CommandSupervisionError::StdinWrite(source)),
    })
}

fn wait_for_terminal(
    child: &mut Child,
    pgid: u32,
    wall_limit: Duration,
    termination_grace: Duration,
    stdout_limit_exceeded: &AtomicBool,
    stderr_limit_exceeded: &AtomicBool,
) -> Result<(CommandTerminal, Option<ExitStatus>), CommandSupervisionError> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(CommandSupervisionError::Wait)? {
            return Ok((terminal_from_status(status), Some(status)));
        }
        let forced = if stdout_limit_exceeded.load(Ordering::Acquire) {
            Some(ForcedStop::StdoutLimit)
        } else if stderr_limit_exceeded.load(Ordering::Acquire) {
            Some(ForcedStop::StderrLimit)
        } else if started.elapsed() >= wall_limit {
            Some(ForcedStop::Timeout)
        } else {
            None
        };
        if let Some(forced) = forced {
            return terminate_group(child, pgid, forced, termination_grace);
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[derive(Clone, Copy)]
enum ForcedStop {
    Timeout,
    StdoutLimit,
    StderrLimit,
}

fn terminate_group(
    child: &mut Child,
    pgid: u32,
    forced: ForcedStop,
    termination_grace: Duration,
) -> Result<(CommandTerminal, Option<ExitStatus>), CommandSupervisionError> {
    signal_group(pgid, Signal::TERM)?;
    let grace_started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(CommandSupervisionError::Wait)? {
            return Ok((forced_terminal(forced, false), Some(status)));
        }
        if grace_started.elapsed() >= termination_grace {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    signal_group(pgid, Signal::KILL)?;
    let status = child.wait().map_err(CommandSupervisionError::Wait)?;
    Ok((forced_terminal(forced, true), Some(status)))
}

fn terminal_from_status(status: ExitStatus) -> CommandTerminal {
    match (status.code(), status.signal()) {
        (Some(exit_code), _) => CommandTerminal::Exited { exit_code },
        (None, Some(signal)) => CommandTerminal::Signaled { signal },
        (None, None) => CommandTerminal::Signaled { signal: 0 },
    }
}

const fn forced_terminal(forced: ForcedStop, escalated_to_kill: bool) -> CommandTerminal {
    match forced {
        ForcedStop::Timeout => CommandTerminal::TimedOut { escalated_to_kill },
        ForcedStop::StdoutLimit => CommandTerminal::StdoutLimit { escalated_to_kill },
        ForcedStop::StderrLimit => CommandTerminal::StderrLimit { escalated_to_kill },
    }
}

fn signal_group(pgid: u32, signal: Signal) -> Result<(), CommandSupervisionError> {
    let raw =
        i32::try_from(pgid).map_err(|_| CommandSupervisionError::PidOutOfRange { pid: pgid })?;
    let pid = Pid::from_raw(raw).ok_or(CommandSupervisionError::PidOutOfRange { pid: pgid })?;
    match kill_process_group(pid, signal) {
        Ok(()) => Ok(()),
        Err(source) if source == rustix::io::Errno::SRCH => Ok(()),
        Err(source) => Err(CommandSupervisionError::Signal { signal, source }),
    }
}

fn join_capture(
    handle: JoinHandle<Result<Vec<u8>, CommandSupervisionError>>,
    stream: &'static str,
) -> Result<Vec<u8>, CommandSupervisionError> {
    handle
        .join()
        .map_err(|_| CommandSupervisionError::CaptureThreadPanicked { stream })?
}

fn join_stdin_writer(
    handle: JoinHandle<Result<(), CommandSupervisionError>>,
) -> Result<(), CommandSupervisionError> {
    handle
        .join()
        .map_err(|_| CommandSupervisionError::StdinWriterPanicked)?
}

/// Closed errors at the deterministic-command boundary.
#[derive(Debug, Error)]
pub enum CommandSupervisionError {
    #[error("approved executable must be an absolute path: {0:?}")]
    ExecutableNotAbsolute(PathBuf),

    #[error("command executable is not a regular file: {0:?}")]
    ExecutableNotRegularFile(PathBuf),

    #[error("command executable is not executable: {0:?}")]
    ExecutableNotExecutable(PathBuf),

    #[error("command workspace is not a directory: {0:?}")]
    WorkspaceNotDirectory(PathBuf),

    #[error("command working directory is not a directory: {0:?}")]
    WorkingDirectoryNotDirectory(RepositoryRelativePath),

    #[error("repository executable cannot be the workspace directory: {0:?}")]
    RepositoryExecutableIsDirectory(RepositoryRelativePath),

    #[error("repository-relative command path contains a symlink: {0:?}")]
    SymlinkPathRejected(RepositoryRelativePath),

    #[error("repository-relative command path escapes the assigned workspace: {0:?}")]
    PathEscapesWorkspace(RepositoryRelativePath),

    #[error("repository-relative command path is not canonical: {0:?}")]
    NonCanonicalRepositoryPath(RepositoryRelativePath),

    #[error("comparison revision must be 1 through 160 bytes without NUL")]
    InvalidComparisonRevision,

    #[error("Git tree identity must be a 40- or 64-byte lower-case hexadecimal object name")]
    InvalidGitTreeIdentity,

    #[error("comparison artifact digest does not match supplied bytes")]
    ArtifactDigestMismatch,

    #[error(
        "command inline/artifact bytes exceed the {COMMAND_INPUT_BYTE_LIMIT}-byte limit: {observed}"
    )]
    InputTooLarge { observed: usize },

    #[error("command profile name is invalid")]
    InvalidProfileName,

    #[error("command profile has too many argv values")]
    TooManyArguments,

    #[error("command profile contains a NUL or oversized argv value")]
    InvalidArgument,

    #[error("expected command exit status must be from 0 through 255")]
    InvalidExpectedExitStatus,

    #[error("command timeout must be positive and no more than {COMMAND_TIMEOUT_LIMIT:?}")]
    InvalidTimeout,

    #[error(
        "{stream} stream limit must be positive and no more than {COMMAND_STREAM_BYTE_LIMIT} bytes"
    )]
    InvalidStreamLimit { stream: &'static str },

    #[error("command profile has too many declared environment additions")]
    TooManyEnvironmentAdditions,

    #[error("command environment name is invalid or duplicated")]
    InvalidEnvironmentName,

    #[error("command environment replaces the minimal baseline: {0}")]
    EnvironmentReplacesBaseline(String),

    #[error("command environment value is invalid or oversized: {0}")]
    InvalidEnvironmentValue(String),

    #[error("expected {stream} comparison bytes exceed the configured stream limit")]
    ExpectedStreamExceedsLimit { stream: &'static str },

    #[error("termination grace must be positive and bounded")]
    InvalidTerminationGrace,

    #[error("failed to spawn deterministic command: {0}")]
    Spawn(#[source] io::Error),

    #[error("the kernel-owned approved-tool PATH cannot be represented")]
    ToolPathUnrepresentable,

    #[error("spawned command did not expose piped {stream}")]
    MissingChildStream { stream: &'static str },

    #[error("failed to wait for deterministic command: {0}")]
    Wait(#[source] io::Error),

    #[error("process ID {pid} cannot be represented")]
    PidOutOfRange { pid: u32 },

    #[error("failed to signal deterministic command process group with {signal:?}: {source}")]
    Signal {
        signal: Signal,
        #[source]
        source: rustix::io::Errno,
    },

    #[error("failed reading command stream: {0}")]
    CaptureRead(#[source] io::Error),

    #[error("command stream byte count cannot be represented")]
    StreamCountOutOfRange,

    #[error("{stream} command capture thread panicked")]
    CaptureThreadPanicked { stream: &'static str },

    #[error("failed writing command stdin: {0}")]
    StdinWrite(#[source] io::Error),

    #[error("command stdin writer thread panicked")]
    StdinWriterPanicked,

    #[error("validation profile must contain at least one command")]
    EmptyValidationProfile,

    #[error("pristine source tree changed before validation began")]
    PristineTreeChangedBeforeValidation,

    #[error("failed to {operation} at {path:?}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::PermissionsExt as _,
        path::{Path, PathBuf},
        time::Duration,
    };

    use factory_protocol::{
        ApprovedToolV2, CommandProfileV2, DurationMillis, EnvironmentAdditionV2, ExecutableV2,
        RepositoryRelativePath,
    };

    use super::*;

    fn temporary_repository(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "factory-v3-command-supervision-{label}-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        fs::create_dir_all(root.join("tools")).expect("create synthetic repository");
        git(&root, ["init", "--quiet"]);
        root
    }

    fn git<const N: usize>(root: &Path, args: [&str; N]) -> String {
        let output = std::process::Command::new("/usr/bin/git")
            .args(args)
            .current_dir(root)
            .env_clear()
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("launch synthetic Git repository command");
        assert!(
            output.status.success(),
            "synthetic Git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("synthetic Git output is UTF-8")
            .trim()
            .to_owned()
    }

    fn commit_exact_tree(root: &Path) -> GitTreeIdentity {
        git(root, ["add", "."]);
        git(
            root,
            [
                "-c",
                "user.name=Factory Test",
                "-c",
                "user.email=factory-test@example.invalid",
                "commit",
                "--quiet",
                "--no-gpg-sign",
                "-m",
                "synthetic source",
            ],
        );
        GitTreeIdentity::parse(git(root, ["rev-parse", "HEAD^{tree}"])).expect("Git tree identity")
    }

    fn write_script(root: &Path, name: &str, body: &str) {
        let path = root.join("tools").join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write synthetic script");
        let mut permissions = fs::metadata(&path)
            .expect("inspect synthetic script")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("make synthetic script executable");
    }

    fn profile(name: &str, executable: ExecutableV2, argv: &[&str]) -> CommandProfileV2 {
        CommandProfileV2 {
            name: name.to_owned(),
            executable,
            argv: argv.iter().map(|value| (*value).to_owned()).collect(),
            working_directory: RepositoryRelativePath::parse(".").expect("root cwd"),
            environment: Vec::new(),
            timeout: DurationMillis::new(500),
            stdout_byte_limit: 4 * 1024,
            stderr_byte_limit: 4 * 1024,
            expected_exit_status: 0,
        }
    }

    fn repository_command(name: &str, argv: &[&str]) -> CommandProfileV2 {
        profile(
            name,
            ExecutableV2::RepositoryPath(
                RepositoryRelativePath::parse(format!("tools/{name}")).expect("script path"),
            ),
            argv,
        )
    }

    fn expectation(stdout: Option<&[u8]>, stderr: Option<&[u8]>) -> CommandExpectation {
        CommandExpectation::new(
            ComparisonRevision::parse("comparison-v1").expect("comparison revision"),
            stdout.map(|bytes| ExactBytes::inline(bytes.to_vec()).expect("stdout bytes")),
            stderr.map(|bytes| ExactBytes::inline(bytes.to_vec()).expect("stderr bytes")),
        )
    }

    fn command(
        profile: CommandProfileV2,
        stdin: CommandStdin,
        stdout: Option<&[u8]>,
        stderr: Option<&[u8]>,
    ) -> DeterministicCommand {
        DeterministicCommand::new(profile, stdin, expectation(stdout, stderr))
            .expect("valid deterministic command")
    }

    fn runner() -> CommandRunner {
        let executable = ExactExecutable::discover("/bin/echo").expect("installed echo");
        CommandRunner::new(
            ApprovedToolExecutables::new(executable.clone(), executable.clone()),
            Duration::from_millis(40),
        )
        .expect("runner")
    }

    fn runner_with_git() -> CommandRunner {
        let echo = ExactExecutable::discover("/bin/echo").expect("installed echo");
        let git = ExactExecutable::discover("/usr/bin/git").expect("installed Git");
        CommandRunner::new(
            ApprovedToolExecutables::new(echo, git),
            Duration::from_millis(40),
        )
        .expect("runner with exact Git")
    }

    fn workspace(root: &Path) -> CommandWorkspace {
        CommandWorkspace::open(root).expect("open synthetic workspace")
    }

    #[test]
    fn direct_argv_execution_uses_minimal_environment_and_never_interpolates_shell_text() {
        let root = temporary_repository("exact-argv");
        write_script(
            &root,
            "exact-argv",
            r#"
[ "$LANG" = C ] || exit 31
[ "$LC_ALL" = C ] || exit 32
[ "$PATH" = /usr/bin:/bin ] || exit 33
[ "$TZ" = UTC ] || exit 34
[ "$DECLARED" = allowed ] || exit 35
[ -z "$HOME" ] || exit 36
printf '%s' "$1"
"#,
        );
        let marker = root.join("interpolated-marker");
        let mut profile = repository_command("exact-argv", &["$(touch interpolated-marker)"]);
        profile.environment.push(EnvironmentAdditionV2 {
            name: "DECLARED".to_owned(),
            value: "allowed".to_owned(),
        });
        let exact = command(
            profile,
            CommandStdin::Empty,
            Some(b"$(touch interpolated-marker)"),
            None,
        );

        let receipt = runner()
            .run(&workspace(&root), &exact)
            .expect("direct command invocation");
        assert!(receipt.matches_expectation());
        assert!(
            !marker.exists(),
            "runner must never create a shell command line"
        );
        fs::remove_dir_all(root).expect("remove synthetic repository");
    }

    #[test]
    fn approved_tool_identity_resolves_to_the_exact_installed_executable() {
        let root = temporary_repository("approved-tool");
        let executable = ExactExecutable::discover("/bin/echo").expect("installed echo");
        let tools = ApprovedToolExecutables::new(executable.clone(), executable.clone());
        let command = command(
            profile(
                "approved",
                ExecutableV2::ApprovedTool(ApprovedToolV2::Git),
                &["approved-tool"],
            ),
            CommandStdin::Empty,
            Some(b"approved-tool\n"),
            None,
        );
        let receipt = CommandRunner::new(tools, Duration::from_millis(40))
            .expect("runner")
            .run(&workspace(&root), &command)
            .expect("approved tool invocation");

        assert_eq!(receipt.executable(), executable.path());
        assert!(receipt.matches_expectation());
        fs::remove_dir_all(root).expect("remove synthetic repository");
    }

    #[test]
    fn approved_cargo_can_resolve_its_exact_toolchain_and_nested_cargo_with_kernel_environment() {
        let root = temporary_repository("approved-cargo-toolchain");
        fs::create_dir(root.join("src")).expect("create synthetic Rust source directory");
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"command-supervision-smoke\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
        )
        .expect("write synthetic Cargo manifest");
        fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("write synthetic Rust source");
        fs::write(
            root.join("build.rs"),
            "fn main() {\n    assert!(std::process::Command::new(\"cargo\").arg(\"--version\").status().expect(\"nested cargo\").success());\n}\n",
        )
        .expect("write nested Cargo build script");
        let cargo = ExactExecutable::discover(
            std::env::var_os("CARGO")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/opt/homebrew/opt/rustup/bin/cargo")),
        )
        .expect("exact Cargo");
        let echo = ExactExecutable::discover("/bin/echo").expect("installed echo");
        let mut cargo_profile = profile(
            "cargo-check",
            ExecutableV2::ApprovedTool(ApprovedToolV2::Cargo),
            &["check", "--offline", "--quiet"],
        );
        cargo_profile.timeout = DurationMillis::new(5_000);
        let exact = command(cargo_profile, CommandStdin::Empty, None, None);

        let receipt = CommandRunner::new(
            ApprovedToolExecutables::new(cargo, echo),
            Duration::from_secs(1),
        )
        .expect("runner")
        .run(&workspace(&root), &exact)
        .expect("Cargo process custody");

        assert!(
            receipt.matches_expectation(),
            "exact Cargo must be able to invoke its exact toolchain and nested Cargo: {}",
            String::from_utf8_lossy(receipt.stderr())
        );
        fs::remove_dir_all(root).expect("remove synthetic repository");
    }

    #[test]
    fn stdin_artifact_bytes_are_verified_and_passed_without_a_shell() {
        let root = temporary_repository("stdin-artifact");
        write_script(&root, "stdin-artifact", "cat");
        let bytes = b"sealed stdin\n".to_vec();
        let digest = ContentDigest::of_bytes(&bytes);
        let artifact = ExactBytes::from_artifact(digest, bytes.clone()).expect("verified artifact");
        let stdin_command = command(
            repository_command("stdin-artifact", &[]),
            CommandStdin::Artifact(artifact),
            Some(&bytes),
            None,
        );

        let receipt = runner()
            .run(&workspace(&root), &stdin_command)
            .expect("stdin command");
        assert!(receipt.matches_expectation());
        assert!(matches!(
            ExactBytes::from_artifact(ContentDigest::of_bytes(b"other"), bytes),
            Err(CommandSupervisionError::ArtifactDigestMismatch)
        ));
        fs::remove_dir_all(root).expect("remove synthetic repository");
    }

    #[test]
    fn stream_limit_terminates_the_owned_process_group_and_retains_only_bounded_bytes() {
        let root = temporary_repository("stdout-limit");
        write_script(&root, "stdout-limit", "while :; do printf 0123456789; done");
        let mut profile = repository_command("stdout-limit", &[]);
        profile.stdout_byte_limit = 64;
        profile.timeout = DurationMillis::new(2_000);
        let command = command(profile, CommandStdin::Empty, None, None);

        let receipt = runner()
            .run(&workspace(&root), &command)
            .expect("bounded command invocation");
        assert!(matches!(
            receipt.terminal(),
            CommandTerminal::StdoutLimit { .. }
        ));
        assert_eq!(receipt.stdout().len(), 64);
        assert!(!receipt.matches_expectation());
        fs::remove_dir_all(root).expect("remove synthetic repository");
    }

    #[test]
    fn timeout_escalates_from_term_to_kill_and_waits_for_the_direct_child() {
        let root = temporary_repository("timeout-escalation");
        write_script(
            &root,
            "timeout-escalation",
            "trap '' TERM; while :; do :; done",
        );
        let mut profile = repository_command("timeout-escalation", &[]);
        profile.timeout = DurationMillis::new(100);
        let command = command(profile, CommandStdin::Empty, None, None);

        let receipt = runner()
            .run(&workspace(&root), &command)
            .expect("timed command invocation");
        assert_eq!(
            receipt.terminal(),
            CommandTerminal::TimedOut {
                escalated_to_kill: true
            }
        );
        assert_eq!(receipt.signal(), Some(9));
        fs::remove_dir_all(root).expect("remove synthetic repository");
    }

    #[test]
    fn two_run_discovery_classifies_reproducible_failure_already_passes_and_divergence() {
        let root = temporary_repository("discovery");
        write_script(
            &root,
            "discovery",
            r#"
case "$(cat behavior)" in
  broken) printf actual; exit 17 ;;
  fixed) printf expected; exit 0 ;;
  flip)
    if [ -e flip-state ]; then rm flip-state; printf first; exit 17; fi
    : > flip-state; printf second; exit 18
    ;;
esac
exit 99
"#,
        );
        fs::write(root.join("behavior"), "broken\n").expect("write broken behavior");
        let command = command(
            repository_command("discovery", &[]),
            CommandStdin::Empty,
            Some(b"expected"),
            None,
        );
        let runner = runner();
        let workspace = workspace(&root);

        assert_eq!(
            runner
                .run_discovery_reproducer(&workspace, &command)
                .expect("reproducible failure")
                .classification(),
            DiscoveryClassification::ReproducibleFailure
        );
        fs::write(root.join("behavior"), "fixed\n").expect("write fixed behavior");
        assert_eq!(
            runner
                .run_discovery_reproducer(&workspace, &command)
                .expect("already passes")
                .classification(),
            DiscoveryClassification::AlreadyPasses
        );
        fs::write(root.join("behavior"), "flip\n").expect("write divergent behavior");
        assert_eq!(
            runner
                .run_discovery_reproducer(&workspace, &command)
                .expect("divergent reproducer")
                .classification(),
            DiscoveryClassification::Divergent
        );
        fs::remove_dir_all(root).expect("remove synthetic repository");
    }

    #[test]
    fn status_only_discovery_accepts_host_diagnostic_variation_with_the_same_failure_status() {
        let root = temporary_repository("status-only-discovery");
        write_script(
            &root,
            "status-only-discovery",
            r#"
if [ -e flip-state ]; then rm flip-state; printf second-diagnostic; exit 17; fi
: > flip-state
printf first-diagnostic
exit 17
"#,
        );
        let profile = repository_command("status-only-discovery", &[]);
        let command = DeterministicCommand::new(
            profile,
            CommandStdin::Empty,
            CommandExpectation::new(
                ComparisonRevision::parse("status-only-v1").expect("status-only revision"),
                None,
                None,
            ),
        )
        .expect("status-only command");

        assert_eq!(
            runner()
                .run_discovery_reproducer(&workspace(&root), &command)
                .expect("two normal failures")
                .classification(),
            DiscoveryClassification::ReproducibleFailure
        );
        fs::remove_dir_all(root).expect("remove synthetic repository");
    }

    #[test]
    fn status_only_discovery_accepts_a_stable_timeout_for_cancellation_evidence() {
        let root = temporary_repository("status-only-timeout");
        write_script(
            &root,
            "status-only-timeout",
            "trap '' TERM; while :; do :; done",
        );
        let mut profile = repository_command("status-only-timeout", &[]);
        profile.timeout = DurationMillis::new(100);
        let command = DeterministicCommand::new(
            profile,
            CommandStdin::Empty,
            CommandExpectation::new(
                ComparisonRevision::parse("status-only-v1").expect("status-only revision"),
                None,
                None,
            ),
        )
        .expect("status-only timeout command");

        assert_eq!(
            runner()
                .run_discovery_reproducer(&workspace(&root), &command)
                .expect("two stable cancellation observations")
                .classification(),
            DiscoveryClassification::ReproducibleFailure
        );
        fs::remove_dir_all(root).expect("remove synthetic repository");
    }

    #[test]
    fn regression_checkpoint_fails_on_base_and_checkpoint_then_passes_on_candidate() {
        let base_root = temporary_repository("regression-base");
        let checkpoint_root = temporary_repository("regression-checkpoint");
        let candidate_root = temporary_repository("regression-candidate");
        for root in [&base_root, &checkpoint_root, &candidate_root] {
            write_script(
                root,
                "regression",
                r#"
if [ "$(cat behavior)" = fixed ]; then printf fixed; exit 0; fi
printf broken; exit 23
"#,
            );
        }
        fs::write(base_root.join("behavior"), "broken\n").expect("write base behavior");
        fs::write(checkpoint_root.join("behavior"), "broken\n").expect("write checkpoint behavior");
        fs::write(candidate_root.join("behavior"), "fixed\n").expect("write candidate behavior");
        let command = command(
            repository_command("regression", &[]),
            CommandStdin::Empty,
            Some(b"fixed"),
            None,
        );

        let receipt = runner()
            .verify_regression_checkpoint(
                &workspace(&base_root),
                &workspace(&checkpoint_root),
                &workspace(&candidate_root),
                &command,
            )
            .expect("regression checkpoint evidence");
        assert_eq!(receipt.status(), RegressionCheckpointStatus::Verified);
        assert!(!receipt.base().matches_expectation());
        assert!(!receipt.regression().matches_expectation());
        assert!(receipt.candidate().matches_expectation());
        for root in [base_root, checkpoint_root, candidate_root] {
            fs::remove_dir_all(root).expect("remove synthetic repository");
        }
    }

    #[test]
    fn candidate_and_quality_validation_have_separate_receipts_and_require_an_unchanged_pristine_tree()
     {
        let candidate_root = temporary_repository("candidate-validation");
        let quality_root = temporary_repository("quality-validation");
        for root in [&candidate_root, &quality_root] {
            write_script(root, "validation", "printf valid");
            fs::write(root.join("source"), "candidate source\n").expect("write source tree");
        }
        let expected_tree = commit_exact_tree(&candidate_root);
        let quality_tree = commit_exact_tree(&quality_root);
        assert_eq!(
            expected_tree, quality_tree,
            "matching source has one Git tree"
        );
        let validation_command = command(
            repository_command("validation", &[]),
            CommandStdin::Empty,
            Some(b"valid"),
            None,
        );
        let probe = |tree: GitTreeIdentity| {
            GitTrackedTreeProbe::new(
                tree,
                Duration::from_secs(1),
                4 * 1024,
                4 * 1024,
                ComparisonRevision::parse("git-tree-v1").expect("Git tree comparison revision"),
            )
            .expect("Git tree probe")
        };
        let hard = runner_with_git()
            .run_candidate_validation(
                &PristineWorkspace::new(workspace(&candidate_root), probe(expected_tree.clone())),
                std::slice::from_ref(&validation_command),
            )
            .expect("candidate validation");
        let quality = runner_with_git()
            .run_quality_validation(
                &PristineWorkspace::new(workspace(&quality_root), probe(expected_tree.clone())),
                &[validation_command],
            )
            .expect("quality validation");
        assert_eq!(hard.invocation(), ValidationInvocation::Candidate);
        assert_eq!(quality.invocation(), ValidationInvocation::Quality);
        assert_eq!(hard.status(), ValidationStatus::Passed);
        assert_eq!(quality.status(), ValidationStatus::Passed);
        assert_eq!(hard.exact_tree(), quality.exact_tree());

        write_script(
            &candidate_root,
            "build-output-validation",
            "mkdir -p target; printf generated > target/output; printf valid",
        );
        git(&candidate_root, ["add", "tools/build-output-validation"]);
        git(
            &candidate_root,
            [
                "-c",
                "user.name=Factory Test",
                "-c",
                "user.email=factory-test@example.invalid",
                "commit",
                "--quiet",
                "--no-gpg-sign",
                "-m",
                "add validation script",
            ],
        );
        let mutating_tree =
            GitTreeIdentity::parse(git(&candidate_root, ["rev-parse", "HEAD^{tree}"]))
                .expect("updated candidate tree identity");
        let build_output_command = command(
            repository_command("build-output-validation", &[]),
            CommandStdin::Empty,
            Some(b"valid"),
            None,
        );
        let build_output = runner_with_git()
            .run_candidate_validation(
                &PristineWorkspace::new(workspace(&candidate_root), probe(mutating_tree.clone())),
                &[build_output_command],
            )
            .expect("build-output validation receipt");
        assert_eq!(build_output.status(), ValidationStatus::Passed);
        assert!(candidate_root.join("target/output").exists());

        write_script(
            &candidate_root,
            "tracked-mutation-validation",
            "printf changed > source; printf valid",
        );
        git(
            &candidate_root,
            ["add", "tools/tracked-mutation-validation"],
        );
        git(
            &candidate_root,
            [
                "-c",
                "user.name=Factory Test",
                "-c",
                "user.email=factory-test@example.invalid",
                "commit",
                "--quiet",
                "--no-gpg-sign",
                "-m",
                "add tracked mutation script",
            ],
        );
        let tracked_tree =
            GitTreeIdentity::parse(git(&candidate_root, ["rev-parse", "HEAD^{tree}"]))
                .expect("tracked mutation candidate tree identity");
        let tracked_mutation = command(
            repository_command("tracked-mutation-validation", &[]),
            CommandStdin::Empty,
            Some(b"valid"),
            None,
        );
        let mutated = runner_with_git()
            .run_candidate_validation(
                &PristineWorkspace::new(workspace(&candidate_root), probe(tracked_tree)),
                &[tracked_mutation],
            )
            .expect("tracked mutation validation receipt");
        assert_eq!(mutated.status(), ValidationStatus::TreeChanged);
        for root in [candidate_root, quality_root] {
            fs::remove_dir_all(root).expect("remove synthetic repository");
        }
    }

    #[test]
    fn rejects_baseline_replacement_and_repository_symlink_execution() {
        let root = temporary_repository("rejections");
        write_script(&root, "rejections", "exit 0");
        let mut replacement = repository_command("rejections", &[]);
        replacement.environment.push(EnvironmentAdditionV2 {
            name: "PATH".to_owned(),
            value: "/unsafe".to_owned(),
        });
        assert!(matches!(
            DeterministicCommand::new(replacement, CommandStdin::Empty, expectation(None, None)),
            Err(CommandSupervisionError::EnvironmentReplacesBaseline(name)) if name == "PATH"
        ));

        std::os::unix::fs::symlink(root.join("tools/rejections"), root.join("tools/symlink"))
            .expect("create synthetic repository symlink");
        let profile = profile(
            "symlink",
            ExecutableV2::RepositoryPath(
                RepositoryRelativePath::parse("tools/symlink").expect("symlink path"),
            ),
            &[],
        );
        let command = command(profile, CommandStdin::Empty, None, None);
        assert!(matches!(
            runner().run(&workspace(&root), &command),
            Err(CommandSupervisionError::SymlinkPathRejected(path)) if path.as_str() == "tools/symlink"
        ));
        fs::remove_dir_all(root).expect("remove synthetic repository");
    }
}
