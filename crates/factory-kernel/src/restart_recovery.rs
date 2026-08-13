//! Daemon-restart reconciliation for paid actor sessions.
//!
//! A restarted daemon does **not** possess a [`std::process::Child`] handle
//! for a process started by the dead daemon.  It therefore cannot reap that
//! process: only the process's original parent (or the platform's reaper) can
//! do so.  This module never calls a wait API and never reports that it did.
//! Its observation is limited to `kill(-PGID, 0)` before and after exact
//! `TERM`/`KILL` signals.  `Absent` means the kernel observed no signalable
//! member of that exact process group at that instant; it is not a synthetic
//! child exit status.
//!
//! Recovery only addresses the exact PID/PGID pair written by `start_session`.
//! It does not enumerate processes, inspect command names, or kill a process
//! selected by a name or a directory.  If the stored PID no longer belongs to
//! the stored process group while that group is still signalable, the custody
//! identity is ambiguous and startup fails closed instead of guessing.

use std::{
    fs::{self, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use factory_protocol::{
    ContentDigest, ExpectedRevision, ProcessCustodyV1, StopReasonV1, TerminalReportV1,
};
use rustix::{
    io::Errno,
    process::{Pid, Signal, getpgid, kill_process_group, test_kill_process_group},
};
use thiserror::Error;

use crate::{
    cas::{CasArtifact, CasStore},
    local_transport::ActorConnectionBinding,
    process::{ProcessStore, RestartRecoverySession, TerminalArtifactSeals, TerminalReceipt},
    session_runtime::{
        SESSION_PARTIAL_TRANSCRIPT_RELATIVE_PATH, SESSION_STDERR_RELATIVE_PATH,
        SESSION_STDOUT_RELATIVE_PATH,
    },
    storage::StoreError,
    workspace_read::{WorkspaceReadAuthority, WorkspaceReadError},
};

const RECOVERY_PRINCIPAL: &str = "kernel";
const RECOVERY_STAGING_DIRECTORY: &str = "restart-recovery";

/// Bounded physical observation policy for a process group inherited from a
/// dead daemon.  It is deliberately a fixed local mechanism, not application
/// policy: changing it cannot make a terminal session successful or restore
/// paid-work admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RestartRecoveryPolicy {
    termination_grace: Duration,
    poll_interval: Duration,
}

impl RestartRecoveryPolicy {
    pub fn new(
        termination_grace: Duration,
        poll_interval: Duration,
    ) -> Result<Self, RestartRecoveryError> {
        if termination_grace.is_zero() {
            return Err(RestartRecoveryError::ZeroTerminationGrace);
        }
        if poll_interval.is_zero() {
            return Err(RestartRecoveryError::ZeroPollInterval);
        }
        Ok(Self {
            termination_grace,
            poll_interval,
        })
    }

    /// A short bounded observation window.  Live process supervision owns its
    /// assignment-specific wall timeout; this is only crash cleanup before a
    /// daemon accepts a new connection.
    #[must_use]
    pub fn bounded_default() -> Self {
        Self {
            termination_grace: Duration::from_millis(250),
            poll_interval: Duration::from_millis(10),
        }
    }
}

/// What restart recovery observed about one exact persisted process group.
/// None of these values claims that the restarted daemon reaped a child.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessGroupObservation {
    /// No signalable member existed when recovery inspected the stored group.
    Absent,
    /// The group disappeared after the exact `TERM` signal.
    TerminatedAfterTerm,
    /// The group remained through grace and disappeared after exact `KILL`.
    TerminatedAfterKill,
}

/// A terminal receipt for every session made non-resumable during one daemon
/// startup.  The report is intentionally small; the permanent provenance is
/// the ordinary session/audit/CAS facts created by `terminal_session`.
#[derive(Clone, Debug)]
pub struct RestartRecoveryReport {
    pub recovered: Vec<RestartRecoveredSession>,
}

#[derive(Clone, Debug)]
pub struct RestartRecoveredSession {
    pub session_id: factory_protocol::SessionId,
    pub process_group_observation: ProcessGroupObservation,
    pub terminal: TerminalReceipt,
}

/// Performs all daemon-crash reconciliation after both singleton locks are
/// held and before any listener accepts work. Sessions are never resumed: each
/// recovered row receives the ordinary, closed `DaemonDisconnected` terminal
/// reason with absent usage, which makes the campaign cost unknown and freezes
/// all later paid admission.
pub async fn reconcile_daemon_restart(
    process: &ProcessStore,
    cas: &CasStore,
    policy: RestartRecoveryPolicy,
) -> Result<RestartRecoveryReport, RestartRecoveryError> {
    let sessions = process.restart_recovery_sessions(cas).await?;
    let mut recovered = Vec::with_capacity(sessions.len());
    for session in sessions {
        let process_group_observation = terminate_owned_process_group(session.custody, policy)?;
        let terminal = reconcile_one_session(process, cas, &session).await?;
        recovered.push(RestartRecoveredSession {
            session_id: session.session_id,
            process_group_observation,
            terminal,
        });
    }
    Ok(RestartRecoveryReport { recovered })
}

async fn reconcile_one_session(
    process: &ProcessStore,
    cas: &CasStore,
    session: &RestartRecoverySession,
) -> Result<TerminalReceipt, RestartRecoveryError> {
    let signed_staging_root = Path::new(session.packet.staging_root.as_str());
    let recovery_staging_root = recovery_staging_root(cas, session.session_id);
    fs::create_dir_all(&recovery_staging_root).map_err(|source| {
        RestartRecoveryError::EvidenceIo {
            operation: "create daemon-restart evidence directory",
            path: recovery_staging_root.clone(),
            source,
        }
    })?;

    let stdout = match adopt_signed_staging_artifact_if_present(
        process,
        cas,
        session,
        signed_staging_root,
        SESSION_STDOUT_RELATIVE_PATH,
        "stdout",
    )
    .await?
    {
        Some(seal) => seal,
        None => {
            write_and_adopt_recovery_placeholder(
                process,
                cas,
                session,
                &recovery_staging_root,
                "stdout-unavailable.txt",
                recovery_stream_placeholder(session.session_id, "stdout"),
                "stdout-unavailable",
            )
            .await?
        }
    };
    let stderr = match adopt_signed_staging_artifact_if_present(
        process,
        cas,
        session,
        signed_staging_root,
        SESSION_STDERR_RELATIVE_PATH,
        "stderr",
    )
    .await?
    {
        Some(seal) => seal,
        None => {
            write_and_adopt_recovery_placeholder(
                process,
                cas,
                session,
                &recovery_staging_root,
                "stderr-unavailable.txt",
                recovery_stream_placeholder(session.session_id, "stderr"),
                "stderr-unavailable",
            )
            .await?
        }
    };

    let partial =
        recover_structurally_readable_partial(process, cas, session, signed_staging_root).await?;
    let (transcript, partial_transcript) = match partial {
        Some(seal) => (seal, Some(seal)),
        None => (
            write_and_adopt_recovery_placeholder(
                process,
                cas,
                session,
                &recovery_staging_root,
                "transcript-unavailable.ndjson",
                recovery_transcript_placeholder(session.session_id),
                "transcript-unavailable",
            )
            .await?,
            None,
        ),
    };

    let identity = process
        .actor_connection_identity(session.session_id, &session.packet)
        .await?;
    let assertion = WorkspaceReadAuthority::empty_after_daemon_restart(
        ActorConnectionBinding::from_identity(identity),
        session.packet.required_read_manifest_artifact_id,
        session.packet.required_reads.clone(),
    )?
    .seal_assertion_after_daemon_restart(cas, &recovery_staging_root)?;
    let assertion_path = format!("required-read-assertion-{}.json", session.session_id.get());
    let registered_assertion = register_signed_staging_artifact(
        process,
        cas,
        session,
        &recovery_staging_root,
        &assertion_path,
        "required-read-assertion",
    )
    .await?;
    if registered_assertion != assertion.artifact() {
        return Err(RestartRecoveryError::RecoveryAssertionChanged);
    }
    let evidence = process
        .verify_terminal_evidence_with_packet_bytes(
            cas,
            session.session_id,
            &session.packet,
            session.packet_artifact,
            &session.canonical_packet_bytes,
            TerminalArtifactSeals {
                transcript,
                stdout,
                stderr,
                partial_transcript,
            },
            assertion,
            None,
        )
        .await?;
    let report = TerminalReportV1 {
        packet_digest: session.packet.packet_digest,
        expected_session_revision: ExpectedRevision::new(session.expected_session_revision),
        operation: None,
        stop_reason: StopReasonV1::DaemonDisconnected,
        report_digest: restart_report_digest(session),
    };
    process
        .terminal_session(
            RECOVERY_PRINCIPAL,
            &format!("kernel-restart-session-{}", session.session_id.get()),
            session.session_id,
            &report,
            evidence,
        )
        .await
        .map_err(Into::into)
}

async fn recover_structurally_readable_partial(
    process: &ProcessStore,
    cas: &CasStore,
    session: &RestartRecoverySession,
    signed_staging_root: &Path,
) -> Result<Option<CasArtifact>, RestartRecoveryError> {
    let candidate = signed_staging_root.join(SESSION_PARTIAL_TRANSCRIPT_RELATIVE_PATH);
    if !exists_or_is_missing(&candidate)? {
        return Ok(None);
    }
    // The first adoption gives structural validation a safe, immutable view
    // without ever following a staging-root symlink. It is deliberately left
    // unregistered if parsing rejects it; only structurally readable partial
    // transcript bytes become session provenance.
    let physical = cas.adopt(
        signed_staging_root,
        SESSION_PARTIAL_TRANSCRIPT_RELATIVE_PATH,
    )?;
    let bytes = cas.read(physical.digest())?;
    match validate_partial_ndjson(&bytes) {
        Ok(()) => {
            let registered = register_signed_staging_artifact(
                process,
                cas,
                session,
                signed_staging_root,
                SESSION_PARTIAL_TRANSCRIPT_RELATIVE_PATH,
                "partial-transcript",
            )
            .await?;
            if registered != physical {
                return Err(RestartRecoveryError::RecoveredArtifactChanged { path: candidate });
            }
            Ok(Some(registered))
        }
        Err(
            RestartRecoveryError::PartialTranscriptUtf8
            | RestartRecoveryError::PartialTranscriptTruncated
            | RestartRecoveryError::PartialTranscriptBlankRecord
            | RestartRecoveryError::PartialTranscriptInvalidJson,
        ) => {
            tracing::warn!(
                session_id = session.session_id.get(),
                "discarding structurally unreadable partial actor transcript during daemon restart"
            );
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

async fn adopt_signed_staging_artifact_if_present(
    process: &ProcessStore,
    cas: &CasStore,
    session: &RestartRecoverySession,
    signed_staging_root: &Path,
    relative_path: &str,
    role: &str,
) -> Result<Option<CasArtifact>, RestartRecoveryError> {
    let candidate = signed_staging_root.join(relative_path);
    if !exists_or_is_missing(&candidate)? {
        return Ok(None);
    }
    register_signed_staging_artifact(
        process,
        cas,
        session,
        signed_staging_root,
        relative_path,
        role,
    )
    .await
    .map(Some)
}

async fn register_signed_staging_artifact(
    process: &ProcessStore,
    cas: &CasStore,
    session: &RestartRecoverySession,
    staging_root: &Path,
    relative_path: &str,
    role: &str,
) -> Result<CasArtifact, RestartRecoveryError> {
    let (seal, _) = process
        .adopt_and_register_staged_artifact(
            cas,
            RECOVERY_PRINCIPAL,
            &format!("kernel-restart-session-{}-{role}", session.session_id.get()),
            session.packet.kernel_build_id,
            staging_root,
            Path::new(relative_path),
            cas.maximum_object_bytes(),
        )
        .await?;
    Ok(seal)
}

async fn write_and_adopt_recovery_placeholder(
    process: &ProcessStore,
    cas: &CasStore,
    session: &RestartRecoverySession,
    recovery_staging_root: &Path,
    filename: &str,
    bytes: Vec<u8>,
    role: &str,
) -> Result<CasArtifact, RestartRecoveryError> {
    let path = recovery_staging_root.join(filename);
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => file
            .write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|source| RestartRecoveryError::EvidenceIo {
                operation: "write daemon-restart placeholder evidence",
                path: path.clone(),
                source,
            })?,
        Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(source) => {
            return Err(RestartRecoveryError::EvidenceIo {
                operation: "create daemon-restart placeholder evidence",
                path,
                source,
            });
        }
    }
    let seal = register_signed_staging_artifact(
        process,
        cas,
        session,
        recovery_staging_root,
        filename,
        role,
    )
    .await?;
    if cas.read(seal.digest())? != bytes {
        return Err(RestartRecoveryError::RecoveredArtifactChanged { path });
    }
    Ok(seal)
}

fn exists_or_is_missing(path: &Path) -> Result<bool, RestartRecoveryError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(RestartRecoveryError::EvidenceIo {
            operation: "inspect daemon-restart evidence",
            path: path.to_owned(),
            source,
        }),
    }
}

fn recovery_staging_root(cas: &CasStore, session_id: factory_protocol::SessionId) -> PathBuf {
    cas.runtime_root()
        .join(RECOVERY_STAGING_DIRECTORY)
        .join(session_id.get().to_string())
}

fn recovery_stream_placeholder(session_id: factory_protocol::SessionId, stream: &str) -> Vec<u8> {
    format!(
        "factory-restart-stream-unavailable-v1\nsession_id={}\nstream={stream}\n",
        session_id.get()
    )
    .into_bytes()
}

fn recovery_transcript_placeholder(session_id: factory_protocol::SessionId) -> Vec<u8> {
    format!(
        "{{\"kind\":\"factory.restart.transcript_unavailable.v1\",\"session_id\":{}}}\n",
        session_id.get()
    )
    .into_bytes()
}

fn restart_report_digest(session: &RestartRecoverySession) -> ContentDigest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"factory-daemon-restart-terminal-v1\0");
    hasher.update(&session.session_id.get().to_be_bytes());
    hasher.update(&session.packet.packet_digest.as_bytes());
    hasher.update(&session.custody.pid.to_be_bytes());
    hasher.update(&session.custody.pgid.to_be_bytes());
    hasher.update(&session.custody.started_at_unix_millis.to_be_bytes());
    ContentDigest::from_bytes(*hasher.finalize().as_bytes())
}

/// Terminates an exact persisted actor process group before a restarted daemon
/// writes terminal facts.  `custody.pid == custody.pgid` is a mandatory
/// invariant because the daemon creates a fresh group for each direct child.
/// The current leader's PGID is checked before any signal, which catches a
/// reused PID bound to another group without process-name scanning.
pub fn terminate_owned_process_group(
    custody: ProcessCustodyV1,
    policy: RestartRecoveryPolicy,
) -> Result<ProcessGroupObservation, RestartRecoveryError> {
    if custody.pid == 0 || custody.pgid == 0 || custody.pid != custody.pgid {
        return Err(RestartRecoveryError::InvalidCustody { custody });
    }
    let pid = pid_from_u32(custody.pid)?;
    let group = pid_from_u32(custody.pgid)?;

    match getpgid(Some(pid)) {
        Ok(observed) if observed == group => {}
        Ok(observed) => {
            return Err(RestartRecoveryError::PidGroupMismatch {
                pid: custody.pid,
                expected_pgid: custody.pgid,
                observed_pgid: raw_pid(observed),
            });
        }
        Err(Errno::SRCH) => {
            return match group_exists(group)? {
                false => Ok(ProcessGroupObservation::Absent),
                // The leader has gone away while a group bearing its ID still
                // answers signals.  It may be original descendants, but this
                // restart has no generation-safe handle that proves that fact.
                // Refuse to serve rather than signal a possibly reused group.
                true => Err(RestartRecoveryError::LeaderMissingGroupSurvives {
                    pid: custody.pid,
                    pgid: custody.pgid,
                }),
            };
        }
        Err(source) => return Err(RestartRecoveryError::InspectProcessGroup { custody, source }),
    }

    if !group_exists(group)? {
        return Ok(ProcessGroupObservation::Absent);
    }
    signal_exact_group(group, Signal::TERM, custody)?;
    if wait_for_group_absence(group, policy)? {
        return Ok(ProcessGroupObservation::TerminatedAfterTerm);
    }
    signal_exact_group(group, Signal::KILL, custody)?;
    if wait_for_group_absence(group, policy)? {
        return Ok(ProcessGroupObservation::TerminatedAfterKill);
    }
    Err(RestartRecoveryError::GroupSurvivedKill {
        pid: custody.pid,
        pgid: custody.pgid,
    })
}

/// Validates a recovered transcript before the caller links its CAS object to
/// a terminal session.  A partial stream is readable only when every nonempty
/// newline-delimited record is independently valid JSON and the final record
/// is newline-terminated.  A truncated final JSON object is evidence of an
/// interrupted write, not a structurally readable record.
pub fn validate_partial_ndjson(bytes: &[u8]) -> Result<(), RestartRecoveryError> {
    if bytes.is_empty() {
        return Ok(());
    }
    let text =
        std::str::from_utf8(bytes).map_err(|_| RestartRecoveryError::PartialTranscriptUtf8)?;
    if !text.ends_with('\n') {
        return Err(RestartRecoveryError::PartialTranscriptTruncated);
    }
    for line in text.lines() {
        if line.is_empty() {
            return Err(RestartRecoveryError::PartialTranscriptBlankRecord);
        }
        let _: miniserde::json::Value = miniserde::json::from_str(line)
            .map_err(|_| RestartRecoveryError::PartialTranscriptInvalidJson)?;
    }
    Ok(())
}

fn pid_from_u32(value: u32) -> Result<Pid, RestartRecoveryError> {
    let value = i32::try_from(value).map_err(|_| RestartRecoveryError::PidOutOfRange { value })?;
    Pid::from_raw(value).ok_or(RestartRecoveryError::PidOutOfRange {
        value: u32::try_from(value).unwrap_or_default(),
    })
}

fn raw_pid(pid: Pid) -> u32 {
    u32::try_from(pid.as_raw_pid()).expect("rustix PID is positive")
}

fn group_exists(group: Pid) -> Result<bool, RestartRecoveryError> {
    match test_kill_process_group(group) {
        Ok(()) => Ok(true),
        Err(Errno::SRCH) => Ok(false),
        Err(source) => Err(RestartRecoveryError::ObserveProcessGroup { source }),
    }
}

fn signal_exact_group(
    group: Pid,
    signal: Signal,
    custody: ProcessCustodyV1,
) -> Result<(), RestartRecoveryError> {
    match kill_process_group(group, signal) {
        Ok(()) | Err(Errno::SRCH) => Ok(()),
        Err(source) => Err(RestartRecoveryError::SignalProcessGroup {
            pid: custody.pid,
            pgid: custody.pgid,
            signal,
            source,
        }),
    }
}

fn wait_for_group_absence(
    group: Pid,
    policy: RestartRecoveryPolicy,
) -> Result<bool, RestartRecoveryError> {
    let deadline = Instant::now() + policy.termination_grace;
    loop {
        if !group_exists(group)? {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(
            policy
                .poll_interval
                .min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

#[derive(Debug, Error)]
pub enum RestartRecoveryError {
    #[error(transparent)]
    Store(#[from] StoreError),

    #[error(transparent)]
    WorkspaceRead(#[from] WorkspaceReadError),

    #[error(transparent)]
    Cas(#[from] crate::cas::CasError),

    #[error("restart recovery termination grace must be positive")]
    ZeroTerminationGrace,

    #[error("restart recovery process-group poll interval must be positive")]
    ZeroPollInterval,

    #[error("persisted process custody is not a direct-child process group: {custody:?}")]
    InvalidCustody { custody: ProcessCustodyV1 },

    #[error("persisted process ID {value} cannot be represented on this host")]
    PidOutOfRange { value: u32 },

    #[error(
        "persisted PID {pid} is now in PGID {observed_pgid}, not recorded PGID {expected_pgid}"
    )]
    PidGroupMismatch {
        pid: u32,
        expected_pgid: u32,
        observed_pgid: u32,
    },

    #[error(
        "persisted PID {pid} is absent but recorded PGID {pgid} remains signalable; custody is ambiguous"
    )]
    LeaderMissingGroupSurvives { pid: u32, pgid: u32 },

    #[error("could not inspect persisted process custody {custody:?}: {source}")]
    InspectProcessGroup {
        custody: ProcessCustodyV1,
        source: Errno,
    },

    #[error("could not observe exact persisted process group: {source}")]
    ObserveProcessGroup { source: Errno },

    #[error("could not signal persisted PID {pid}/PGID {pgid} with {signal:?}: {source}")]
    SignalProcessGroup {
        pid: u32,
        pgid: u32,
        signal: Signal,
        source: Errno,
    },

    #[error(
        "persisted PID {pid}/PGID {pgid} remained signalable after TERM and KILL observation windows"
    )]
    GroupSurvivedKill { pid: u32, pgid: u32 },

    #[error("recovered artifact bytes changed before they could be linked: {path}")]
    RecoveredArtifactChanged { path: PathBuf },

    #[error("recovery required-read assertion differs from the registered CAS artifact")]
    RecoveryAssertionChanged,

    #[error("I/O while {operation} at {path}: {source}")]
    EvidenceIo {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("partial transcript is not valid UTF-8")]
    PartialTranscriptUtf8,

    #[error("partial transcript ends with a truncated NDJSON record")]
    PartialTranscriptTruncated,

    #[error("partial transcript contains a blank NDJSON record")]
    PartialTranscriptBlankRecord,

    #[error("partial transcript contains an invalid NDJSON record")]
    PartialTranscriptInvalidJson,
}

#[cfg(test)]
mod tests {
    use std::{process::Command, sync::mpsc, thread, time::Duration};

    use super::*;

    #[test]
    fn partial_ndjson_requires_complete_valid_records() {
        assert!(validate_partial_ndjson(b"").is_ok());
        assert!(validate_partial_ndjson(b"{\"type\":\"event\"}\n{\"n\":2}\n").is_ok());
        assert!(matches!(
            validate_partial_ndjson(b"{\"type\":\"event\"}"),
            Err(RestartRecoveryError::PartialTranscriptTruncated)
        ));
        assert!(matches!(
            validate_partial_ndjson(b"{\"type\":\n"),
            Err(RestartRecoveryError::PartialTranscriptInvalidJson)
        ));
    }

    #[test]
    fn restart_recovery_signals_only_its_exact_direct_child_group() {
        use std::os::unix::process::CommandExt as _;

        // A real restart has no child handle: the dead daemon's process has
        // already been reparented and will be reaped by the platform.  Keep a
        // waiter active here so this test models that condition instead of
        // leaving a zombie whose process group remains observable on macOS.
        let (pid_tx, pid_rx) = mpsc::sync_channel(1);
        let waiter = thread::spawn(move || {
            let mut child = Command::new("/bin/sh")
                .args(["-c", "sleep 30"])
                .process_group(0)
                .spawn()
                .expect("spawn owned process group");
            pid_tx.send(child.id()).expect("publish child PID");
            child.wait().expect("reap test child")
        });
        let pid = pid_rx.recv().expect("receive child PID");
        // Give the shell a moment to establish the group before probing it.
        thread::sleep(Duration::from_millis(10));
        let observation = terminate_owned_process_group(
            ProcessCustodyV1 {
                pid,
                pgid: pid,
                started_at_unix_millis: 1,
            },
            RestartRecoveryPolicy::new(Duration::from_secs(1), Duration::from_millis(10)).unwrap(),
        )
        .expect("exact owned group should terminate");
        assert!(matches!(
            observation,
            ProcessGroupObservation::TerminatedAfterTerm
                | ProcessGroupObservation::TerminatedAfterKill
        ));
        let _ = waiter.join().expect("join child waiter");
    }
}
