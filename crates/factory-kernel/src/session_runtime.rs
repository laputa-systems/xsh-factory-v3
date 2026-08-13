//! Composition boundary for one daemon-owned paid session.
//!
//! This module is deliberately the small piece which joins the independent
//! custody seams.  It creates the actor socket before a child exists, starts
//! the exact host command behind the startup gate, commits the process facts,
//! binds the opaque actor identity, and only then releases the child with one
//! `session.admitted` line.  The child and the actor connection are raced, but
//! the child is always cancelled and directly waited when the connection
//! disappears.  No PID scan or process-name lookup is part of this path.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::OpenOptions,
    future::Future,
    io::Write as _,
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use factory_protocol::{
    ArtifactId, AssignmentId, AssignmentPacketV1, ContentDigest, ExpectedRevision, MicroUsd,
    ReadExactFileV1, RepositoryRelativePath, RuntimeRelativePath, SessionId, StopReasonV1,
    TerminalOperationV1, TerminalReportV1, UsageTotalsV1,
};
use miniserde::{Serialize, json};
use thiserror::Error;

use crate::{
    candidate_runtime::{
        ActorRequestBinding, CandidateSubmissionOutcome, QualityFullSuiteOutcome,
        RegressionCheckpoint, ResolvedEngineeringCandidateAuthority,
        ResolvedQualityCandidateAuthority, checkpoint_regression, run_quality_full_suite,
        submit_candidate, submit_quality_review,
    },
    cas::{CasArtifact, CasStore},
    command_supervision::CommandRunner,
    decision_store::DecisionStore,
    forum_store::ForumStore,
    local_transport::{
        ActorConnectionBinding, ActorDisconnect, BoundActorFrame, LocalDaemon, LocalTransportError,
    },
    process::{
        ProcessStore, ReconciledSessionCancellation, SessionReceipt, StartSession,
        TerminalArtifactSeals, TerminalReceipt,
    },
    process_custody::{
        PiHostSpawnSpec, ProcessCancellation, ProcessCustodyError, ProcessStopReason,
        ProcessSupervisionSpec, SupervisedProcessOutcome,
    },
    product_runtime::{ExecuteProductProposal, execute_product_proposal},
    storage::StoreError,
    ticket_store::TicketStore,
    workspace_read::{WorkspaceReadAuthority, WorkspaceReadError},
};

const ADMISSION_PROTOCOL_VERSION: u16 = 1;
const ADMISSION_MAX_BYTES: usize = factory_protocol::RESPONSE_FRAME_MAX_BYTES - 1;
/// The framed response includes base64, so keep raw evidence decisively below
/// the transport cap. Large evidence remains sealed/navigable but is not a
/// bulk CAS download capability for an actor.
const ARTIFACT_READ_MAX_BYTES: u64 = 2 * 1024 * 1024;
pub const SESSION_STDOUT_RELATIVE_PATH: &str = "stdout.log";
pub const SESSION_STDERR_RELATIVE_PATH: &str = "stderr.log";
pub const SESSION_PARTIAL_TRANSCRIPT_RELATIVE_PATH: &str = "session.ndjson";

/// Daemon-local registry for the one admitted paid process. A cancellation
/// selects by durable session ID, while the stored handle itself names no PID
/// and can only stop the `SpawnedPiHost` that minted it.
#[derive(Clone, Debug, Default)]
pub(crate) struct ActiveSessionCancellationRegistry {
    entries: Arc<Mutex<BTreeMap<SessionId, ActiveSessionCancellationEntry>>>,
}

#[derive(Debug)]
struct ActiveSessionCancellationEntry {
    cancellation: ProcessCancellation,
    reconciled: smol::channel::Receiver<ReconciledSessionCancellation>,
}

impl ActiveSessionCancellationRegistry {
    fn register(
        &self,
        session_id: SessionId,
        cancellation: ProcessCancellation,
    ) -> Result<ActiveSessionCancellationCompletion, SessionRuntimeError> {
        let (sender, receiver) = smol::channel::bounded(1);
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| SessionRuntimeError::CancellationRegistryPoisoned)?;
        if entries.contains_key(&session_id) {
            return Err(SessionRuntimeError::CancellationRegistryConflict { session_id });
        }
        entries.insert(
            session_id,
            ActiveSessionCancellationEntry {
                cancellation,
                reconciled: receiver,
            },
        );
        drop(entries);
        Ok(ActiveSessionCancellationCompletion {
            session_id,
            sender,
            entries: Arc::clone(&self.entries),
        })
    }

    pub(crate) async fn cancel_and_wait(
        &self,
        session_id: SessionId,
    ) -> Result<ReconciledSessionCancellation, SessionRuntimeError> {
        let reconciled = {
            let entries = self
                .entries
                .lock()
                .map_err(|_| SessionRuntimeError::CancellationRegistryPoisoned)?;
            let entry = entries
                .get(&session_id)
                .ok_or(SessionRuntimeError::ActiveSessionCancellationMissing { session_id })?;
            entry.cancellation.request();
            entry.reconciled.clone()
        };
        reconciled
            .recv()
            .await
            .map_err(|_| SessionRuntimeError::CancellationReconciliationClosed { session_id })
    }
}

struct ActiveSessionCancellationCompletion {
    session_id: SessionId,
    sender: smol::channel::Sender<ReconciledSessionCancellation>,
    entries: Arc<Mutex<BTreeMap<SessionId, ActiveSessionCancellationEntry>>>,
}

impl ActiveSessionCancellationCompletion {
    async fn finish(self, reconciliation: ReconciledSessionCancellation) {
        let _ = self.sender.send(reconciliation).await;
        if let Ok(mut entries) = self.entries.lock() {
            entries.remove(&self.session_id);
        }
    }
}

impl Drop for ActiveSessionCancellationCompletion {
    fn drop(&mut self) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.remove(&self.session_id);
        }
    }
}

/// A mandatory verifier supplied by the daemon's installed-build authority.
///
/// The runtime does not infer trust from a host path or from a packet's JSON
/// shape.  The implementation must compare the supplied canonical packet
/// bytes and every installed runtime identity (canonical executable/version,
/// source graph, `deno.json`, lockfile, Pi version, and cache) against the
/// qualified build and assignment before this method returns success.
pub trait SessionRuntimeVerifier: Send + Sync {
    fn verify_packet(
        &self,
        packet: &AssignmentPacketV1,
        canonical_packet_bytes: &[u8],
    ) -> Result<(), RuntimeVerificationError>;

    fn verify_runtime(
        &self,
        packet: &AssignmentPacketV1,
        spawn: &PiHostSpawnSpec,
    ) -> Result<(), RuntimeVerificationError>;
}

/// Closed failure values returned by the installed runtime/build verifier.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RuntimeVerificationError {
    #[error("canonical packet bytes are empty")]
    PacketBytesEmpty,

    #[error("packet contract is invalid: {0}")]
    PacketContract(String),

    #[error("packet typed seal does not match its immutable fields")]
    PacketSealMismatch,

    #[error("installed runtime identity mismatch: {0}")]
    RuntimeIdentity(String),
}

/// Runtime input assembled by typed assignment admission.
#[derive(Clone)]
pub struct SessionLaunchRequest {
    pub principal: String,
    pub command_id: String,
    pub expected_assignment_revision: ExpectedRevision,
    pub assignment_id: AssignmentId,
    pub packet_digest: ContentDigest,
    pub packet: AssignmentPacketV1,
    /// Canonical bytes generated by assignment admission.  The runtime only
    /// transports these bytes; it never serializes a second packet spelling.
    pub canonical_packet_bytes: Vec<u8>,
    /// Physical seal of the exact signed canonical packet bytes registered by
    /// assignment admission. Its BLAKE3 differs intentionally from the
    /// packet's unsigned self-seal.
    pub packet_artifact: CasArtifact,
    pub spawn: PiHostSpawnSpec,
    pub supervision: ProcessSupervisionSpec,
    pub workspace_root: std::path::PathBuf,
    pub expected_read_manifest_artifact_id: factory_protocol::ArtifactId,
    pub required_reads: Vec<ReadExactFileV1>,
    /// Candidate/Quality composition is optional because Product sessions do
    /// not need it. Engineering and Quality RPCs fail closed when no daemon
    /// resolver has supplied this trusted context.
    pub candidate_quality_runtime: Option<CandidateQualitySessionRuntime>,
}

/// Future shape for the daemon-composed Candidate/Quality context resolver.
/// The resolver's inputs are inherited session identity and immutable packet,
/// never an actor request or actor-selected Git/validation identity.
pub type CandidateQualityAuthorityFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, CandidateQualityAuthorityResolutionError>> + Send + 'a>>;

/// Resolves the additional trusted facts which assignment packets deliberately
/// do not expose: the qualified repository/worktree, claimed ticket revisions,
/// candidate packet, and kernel-selected commands.  It is a narrow
/// composition seam rather than a generic workflow service.
pub trait CandidateQualityAuthorityResolver: Send + Sync {
    fn resolve_engineering<'a>(
        &'a self,
        session_id: SessionId,
        packet: &'a AssignmentPacketV1,
    ) -> CandidateQualityAuthorityFuture<'a, ResolvedEngineeringCandidateAuthority>;

    fn resolve_quality<'a>(
        &'a self,
        session_id: SessionId,
        packet: &'a AssignmentPacketV1,
    ) -> CandidateQualityAuthorityFuture<'a, ResolvedQualityCandidateAuthority>;
}

/// Resolver failures reject before any candidate/validation/review transition
/// or Git custody operation begins.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CandidateQualityAuthorityResolutionError {
    #[error("the trusted Candidate/Quality authority resolver is not configured")]
    Unavailable,
    #[error("the requested Candidate/Quality authority is not resolvable: {message}")]
    Precondition { message: String },
}

/// Runtime services that may reach Candidate/Quality custody.  The daemon
/// creates this only after it can provide a resolver rooted in trusted ticket,
/// repository, and assignment state.
#[derive(Clone)]
pub struct CandidateQualitySessionRuntime {
    decisions: DecisionStore,
    git: Arc<crate::git::GitCustody>,
    resolver: Arc<dyn CandidateQualityAuthorityResolver>,
}

impl CandidateQualitySessionRuntime {
    #[must_use]
    pub fn new(
        decisions: DecisionStore,
        git: Arc<crate::git::GitCustody>,
        resolver: Arc<dyn CandidateQualityAuthorityResolver>,
    ) -> Self {
        Self {
            decisions,
            git,
            resolver,
        }
    }
}

impl SessionLaunchRequest {
    fn validate_identity(&self) -> Result<(), SessionRuntimeError> {
        if self.assignment_id != self.packet.assignment_id {
            return Err(SessionRuntimeError::PacketIdentityMismatch);
        }
        if self.packet_digest != self.packet.packet_digest {
            return Err(SessionRuntimeError::PacketIdentityMismatch);
        }
        if self.required_reads != self.packet.required_reads {
            return Err(SessionRuntimeError::PacketIdentityMismatch);
        }
        if self.expected_read_manifest_artifact_id != self.packet.required_read_manifest_artifact_id
            || self.workspace_root != Path::new(self.packet.workspace_root.as_str())
        {
            return Err(SessionRuntimeError::PacketIdentityMismatch);
        }
        if self.canonical_packet_bytes.is_empty() {
            return Err(SessionRuntimeError::PacketBytesEmpty);
        }
        if self.canonical_packet_bytes.len() > ADMISSION_MAX_BYTES {
            return Err(SessionRuntimeError::PacketBytesTooLarge {
                actual: self.canonical_packet_bytes.len(),
                maximum: ADMISSION_MAX_BYTES,
            });
        }
        if self.workspace_root.as_os_str().is_empty() {
            return Err(SessionRuntimeError::WorkspaceRootEmpty);
        }
        for (name, path, required_relative) in [
            (
                "stdout",
                self.supervision.stdout_path(),
                SESSION_STDOUT_RELATIVE_PATH,
            ),
            (
                "stderr",
                self.supervision.stderr_path(),
                SESSION_STDERR_RELATIVE_PATH,
            ),
        ] {
            let relative = path
                .strip_prefix(&self.staging_root())
                .map_err(|_| SessionRuntimeError::CapturePathOutsideStaging { stream: name })?;
            RuntimeRelativePath::parse(relative.to_string_lossy().to_string())
                .map_err(|_| SessionRuntimeError::CapturePathOutsideStaging { stream: name })?;
            if relative != Path::new(required_relative) {
                return Err(SessionRuntimeError::CapturePathOutsideStaging { stream: name });
            }
        }
        Ok(())
    }

    fn staging_root(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(self.packet.staging_root.as_str())
    }
}

/// Why the actor side of a running session stopped first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionTransportStop {
    ProcessExited,
    PeerDisconnected,
    TransportFailed,
}

/// The process and transport outcomes after direct child reconciliation.
#[derive(Debug)]
pub struct SessionRuntimeOutcome {
    pub session: SessionReceipt,
    pub terminal: TerminalReceipt,
    pub process: SupervisedProcessOutcome,
    pub transport: SessionTransportStop,
}

/// Runtime composition errors.  A failed startup after spawn always includes
/// a direct child wait attempt before the error is returned.
#[derive(Debug, Error)]
pub enum SessionRuntimeError {
    #[error(transparent)]
    Store(#[from] StoreError),

    #[error(transparent)]
    Custody(#[from] ProcessCustodyError),

    #[error(transparent)]
    Transport(#[from] LocalTransportError),

    #[error("assignment packet identity does not match the requested session")]
    PacketIdentityMismatch,

    #[error("assignment packet bytes are empty")]
    PacketBytesEmpty,

    #[error("assignment packet bytes are {actual} bytes, exceeding {maximum}")]
    PacketBytesTooLarge { actual: usize, maximum: usize },

    #[error("workspace root is empty")]
    WorkspaceRootEmpty,

    #[error("daemon-owned {stream} capture is outside the assignment staging root")]
    CapturePathOutsideStaging { stream: &'static str },

    #[error("runtime verification failed: {0}")]
    Verification(#[from] RuntimeVerificationError),

    #[error(transparent)]
    Read(#[from] WorkspaceReadError),

    #[error("session admission failed after durable start: {source}")]
    AdmissionFailed {
        #[source]
        source: LocalTransportError,
    },

    #[error("session start failed and child cleanup also failed: start={start}; cleanup={cleanup}")]
    StartAndCleanupFailed {
        start: StoreError,
        cleanup: ProcessCustodyError,
    },

    #[error("session start failed: {0}")]
    StartFailed(StoreError),

    #[error("child cleanup failed after session admission failure: {0}")]
    AdmissionCleanupFailed(ProcessCustodyError),

    #[error("the child process result channel closed before direct wait completed")]
    ProcessResultChannelClosed,

    #[error("the actor transport result channel closed before it reported a stop")]
    TransportResultChannelClosed,

    #[error("workspace-read ledger is still held by the actor dispatcher")]
    ReadAuthorityStillInUse,

    #[error("workspace-read ledger mutex was poisoned")]
    ReadAuthorityPoisoned,

    #[error("session RPC state mutex was poisoned")]
    RpcStatePoisoned,

    #[error("active-session cancellation registry mutex was poisoned")]
    CancellationRegistryPoisoned,

    #[error("session {session_id} already has an active cancellation handle")]
    CancellationRegistryConflict { session_id: SessionId },

    #[error("session {session_id} has no daemon-owned active cancellation handle")]
    ActiveSessionCancellationMissing { session_id: SessionId },

    #[error("session {session_id} cancellation closed before durable reconciliation")]
    CancellationReconciliationClosed { session_id: SessionId },

    #[error("terminal proposal channel closed before reconciliation")]
    TerminalProposalChannelClosed,

    #[error("terminal response channel closed before durable reconciliation")]
    TerminalResponseChannelClosed,

    #[error("terminal operation or stop reason is not in the closed V1 contract")]
    InvalidTerminalContract,

    #[error("terminal transcript identity does not match the daemon-sealed artifact")]
    TranscriptIdentityMismatch,

    #[error("terminal artifact path {path} is outside the assigned staging root")]
    ArtifactPathOutsideStaging { path: std::path::PathBuf },

    #[error("I/O while preparing terminal evidence at {path}: {source}")]
    TerminalEvidenceIo {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Clone, Copy)]
struct RegisteredSessionArtifact {
    seal: CasArtifact,
    artifact_id: ArtifactId,
}

struct SessionRpcState {
    packet_verified: bool,
    transcript: Option<RegisteredSessionArtifact>,
    terminal_request: Option<factory_protocol::SessionSubmitTerminalRequest>,
    engineering: EngineeringSessionState,
    quality: QualitySessionState,
}

#[derive(Default)]
struct EngineeringSessionState {
    checkpoint_in_flight: bool,
    submission_in_flight: bool,
    submitted: bool,
    authority: Option<ResolvedEngineeringCandidateAuthority>,
    checkpoint: Option<RegressionCheckpoint>,
}

#[derive(Default)]
struct QualitySessionState {
    full_suite_in_flight: bool,
    review_in_flight: bool,
    review_submitted: bool,
    authority: Option<ResolvedQualityCandidateAuthority>,
    full_suite: Option<QualityFullSuiteOutcome>,
}

impl Default for SessionRpcState {
    fn default() -> Self {
        Self {
            packet_verified: false,
            transcript: None,
            terminal_request: None,
            engineering: EngineeringSessionState::default(),
            quality: QualitySessionState::default(),
        }
    }
}

impl EngineeringSessionState {
    fn begin_checkpoint(&mut self) -> Result<(), &'static str> {
        if self.checkpoint.is_some() || self.checkpoint_in_flight || self.submitted {
            return Err(
                "the Engineering session already has a regression checkpoint or candidate submission",
            );
        }
        self.checkpoint_in_flight = true;
        Ok(())
    }

    fn accept_checkpoint(
        &mut self,
        authority: ResolvedEngineeringCandidateAuthority,
        checkpoint: RegressionCheckpoint,
    ) {
        self.checkpoint_in_flight = false;
        self.authority = Some(authority);
        self.checkpoint = Some(checkpoint);
    }

    fn abandon_checkpoint(&mut self) {
        self.checkpoint_in_flight = false;
    }

    fn begin_submission(
        &mut self,
    ) -> Result<(ResolvedEngineeringCandidateAuthority, RegressionCheckpoint), &'static str> {
        if self.submitted || self.submission_in_flight {
            return Err("the Engineering session already submitted its candidate");
        }
        let authority = self
            .authority
            .clone()
            .ok_or("an accepted regression checkpoint is required before candidate submission")?;
        let checkpoint = self
            .checkpoint
            .clone()
            .ok_or("an accepted regression checkpoint is required before candidate submission")?;
        self.submission_in_flight = true;
        Ok((authority, checkpoint))
    }

    fn abandon_submission(&mut self) {
        self.submission_in_flight = false;
    }

    fn complete_submission(&mut self) {
        self.submission_in_flight = false;
        self.submitted = true;
    }
}

impl QualitySessionState {
    fn begin_full_suite(&mut self) -> Result<(), &'static str> {
        if self.full_suite.is_some() || self.full_suite_in_flight || self.review_submitted {
            return Err(
                "the Quality session already has a full-suite receipt or review submission",
            );
        }
        self.full_suite_in_flight = true;
        Ok(())
    }

    fn accept_full_suite(
        &mut self,
        authority: ResolvedQualityCandidateAuthority,
        full_suite: QualityFullSuiteOutcome,
    ) {
        self.full_suite_in_flight = false;
        self.authority = Some(authority);
        self.full_suite = Some(full_suite);
    }

    fn abandon_full_suite(&mut self) {
        self.full_suite_in_flight = false;
    }

    fn begin_review(
        &mut self,
        recovered: Option<ResolvedQualityCandidateAuthority>,
    ) -> Result<(ResolvedQualityCandidateAuthority, QualityFullSuiteOutcome), &'static str> {
        if self.review_submitted || self.review_in_flight {
            return Err("the Quality session already submitted its review");
        }
        if self.full_suite_in_flight {
            return Err("the Quality session full suite is still running");
        }
        let (authority, full_suite) = match (self.authority.clone(), self.full_suite.clone()) {
            (Some(authority), Some(full_suite)) => (authority, full_suite),
            (None, None) => {
                let authority = recovered.ok_or(
                    "this session must run its kernel-owned full suite before review submission",
                )?;
                let full_suite = authority.prior_full_suite.clone().ok_or(
                    "this Quality assignment has no persisted passed full-suite receipt to review",
                )?;
                // This is an explicit recovery capability, not an actor
                // result.  The resolver reread the exact candidate/tree/log
                // receipt before placing it in session-local state.
                self.authority = Some(authority.clone());
                self.full_suite = Some(full_suite.clone());
                (authority, full_suite)
            }
            _ => return Err("the Quality session has inconsistent full-suite state"),
        };
        self.review_in_flight = true;
        Ok((authority, full_suite))
    }

    fn abandon_review(&mut self) {
        self.review_in_flight = false;
    }

    fn complete_review(&mut self) {
        self.review_in_flight = false;
        self.review_submitted = true;
    }
}

struct TerminalProposal {
    request: factory_protocol::SessionSubmitTerminalRequest,
    response: smol::channel::Sender<Vec<u8>>,
}

#[derive(Clone)]
struct KernelSessionRpc {
    process: ProcessStore,
    forum: ForumStore,
    tickets: TicketStore,
    command_runner: CommandRunner,
    candidate_quality_runtime: Option<CandidateQualitySessionRuntime>,
    cas: CasStore,
    packet: AssignmentPacketV1,
    canonical_packet_bytes: Vec<u8>,
    packet_artifact: CasArtifact,
    session: SessionReceipt,
    principal: String,
    allowed_tools: BTreeSet<String>,
    /// Shared with the workspace-read dispatcher. This remains daemon-owned
    /// state; actor frames can only add observations through exact reads.
    read_authority: Arc<Mutex<Option<WorkspaceReadAuthority>>>,
    state: Arc<Mutex<SessionRpcState>>,
    terminal_sender: smol::channel::Sender<TerminalProposal>,
}

impl KernelSessionRpc {
    fn dispatch(
        &self,
        frame: BoundActorFrame,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, LocalTransportError>> + Send>> {
        let this = self.clone();
        Box::pin(async move { this.handle(frame).await })
    }

    /// A syntactically framed actor request that is rejected by a session
    /// authority must receive a typed response, just like Forum rejections.
    /// Closing the inherited socket would erase the actionable candidate or
    /// read-gate error and turn a recoverable actor mistake into an opaque
    /// process-custody failure.
    fn dispatch_response(
        &self,
        frame: BoundActorFrame,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, LocalTransportError>> + Send>> {
        let request_id = frame.envelope().request_id.clone();
        let operation = frame.envelope().operation.clone();
        let dispatch = self.dispatch(frame);
        Box::pin(async move {
            match dispatch.await {
                Ok(response) => Ok(response),
                // The actor already crossed the framed, inherited-identity
                // boundary. Only a request/domain rejection is recoverable on
                // that connection; custody, binding, storage, and I/O faults
                // remain fatal and are never disclosed as actor prose.
                Err(LocalTransportError::Frame(error)) => {
                    Ok(json::to_string(&factory_protocol::ErrorResponse {
                        protocol_version: factory_protocol::PROTOCOL_VERSION_V1,
                        request_id,
                        operation,
                        error_code: "session_rejected".to_owned(),
                        message: error.to_string(),
                    })
                    .into_bytes())
                }
                Err(error) => Err(error),
            }
        })
    }

    /// A workspace path or request rejection is an ordinary tool result, not
    /// loss of the inherited actor transport.  Keep the connection alive so
    /// the actor can correct the path and continue.  Binding mismatches remain
    /// fatal because they mean the daemon-side capability itself is wrong.
    fn workspace_read_response(
        request_id: String,
        operation: String,
        result: Result<Vec<u8>, WorkspaceReadError>,
    ) -> Result<Vec<u8>, LocalTransportError> {
        match result {
            Ok(response) => Ok(response),
            Err(WorkspaceReadError::ConnectionIdentityMismatch) => Err(
                LocalTransportError::WorkspaceRead(WorkspaceReadError::ConnectionIdentityMismatch),
            ),
            Err(error) => Ok(json::to_string(&factory_protocol::ErrorResponse {
                protocol_version: factory_protocol::PROTOCOL_VERSION_V1,
                request_id,
                operation,
                error_code: "workspace_read_rejected".to_owned(),
                message: error.to_string(),
            })
            .into_bytes()),
        }
    }

    async fn handle(&self, frame: BoundActorFrame) -> Result<Vec<u8>, LocalTransportError> {
        self.verify_binding(frame.binding())?;
        match frame.envelope().operation.as_str() {
            factory_protocol::OP_SESSION_VERIFY_PACKET => self.verify_packet(frame.frame()),
            factory_protocol::OP_SESSION_SEAL_ARTIFACT => self.seal_artifact(frame.frame()).await,
            factory_protocol::OP_ARTIFACT_SEAL_WORKSPACE_FILE => {
                self.seal_workspace_file(frame.frame()).await
            }
            factory_protocol::OP_ARTIFACT_READ => {
                self.read_assignment_artifact(frame.frame()).await
            }
            factory_protocol::OP_PRODUCT_SUBMIT_TICKET => {
                self.submit_product_ticket(frame.frame()).await
            }
            factory_protocol::OP_CANDIDATE_CHECKPOINT_REGRESSION => {
                self.checkpoint_regression(frame.frame()).await
            }
            factory_protocol::OP_CANDIDATE_SUBMIT => self.submit_candidate(frame.frame()).await,
            factory_protocol::OP_QUALITY_RUN_FULL_SUITE => {
                self.run_quality_full_suite(frame.frame()).await
            }
            factory_protocol::OP_QUALITY_SUBMIT_REVIEW => {
                self.submit_quality_review(frame.frame()).await
            }
            factory_protocol::OP_SESSION_SUBMIT_TERMINAL => {
                self.submit_terminal(frame.frame()).await
            }
            operation if forum_tool_name(operation).is_some() => {
                let tool = forum_tool_name(operation).expect("guard proved Forum operation");
                if !self.allowed_tools.contains(tool) {
                    return Err(invalid_rpc("forum", "Forum operation is not assigned"));
                }
                crate::forum_rpc::dispatch_actor_forum(&self.forum, &frame).await
            }
            _ => Err(invalid_rpc(
                "session",
                "operation is outside session authority",
            )),
        }
    }

    fn verify_binding(&self, binding: &ActorConnectionBinding) -> Result<(), LocalTransportError> {
        if binding.session_id() != self.session.session_id
            || binding.assignment_id() != self.packet.assignment_id
            || binding.application_revision_id() != self.packet.application_revision_id
            || binding.campaign_id() != self.packet.campaign_id
            || binding.office() != self.packet.office
        {
            return Err(StoreError::PacketIdentityMismatch.into());
        }
        Ok(())
    }

    fn require_required_reads_before_mutation(
        &self,
        operation: &'static str,
    ) -> Result<(), LocalTransportError> {
        let authority = self
            .read_authority
            .lock()
            .map_err(|_| invalid_rpc(operation, "required-read authority is poisoned"))?;
        authority
            .as_ref()
            .ok_or_else(|| invalid_rpc(operation, "required-read authority is unavailable"))?
            .assert_required_reads_satisfied()
            .map_err(|_| {
                invalid_rpc(
                    operation,
                    "all assigned exact reads are required before mutation",
                )
            })
    }

    fn verify_packet(&self, frame: &[u8]) -> Result<Vec<u8>, LocalTransportError> {
        let request: factory_protocol::SessionVerifyPacketRequest =
            factory_protocol::decode_operation_request(
                frame,
                factory_protocol::REQUEST_FRAME_MAX_BYTES,
                factory_protocol::OP_SESSION_VERIFY_PACKET,
            )?;
        let digest = request
            .packet_digest
            .parse::<ContentDigest>()
            .map_err(|_| invalid_rpc("session.verify_packet", "packet digest is invalid"))?;
        let bytes = decode_base64(&request.packet_bytes_b64)
            .map_err(|detail| invalid_rpc("session.verify_packet", detail))?;
        if bytes != self.canonical_packet_bytes {
            return Err(invalid_rpc(
                "session.verify_packet",
                "packet bytes differ from daemon admission",
            ));
        }
        self.process.verify_packet_bytes(
            &self.cas,
            &self.packet,
            self.packet_artifact,
            &bytes,
            digest,
        )?;
        self.state
            .lock()
            .map_err(|_| invalid_rpc("session.verify_packet", "session RPC state is poisoned"))?
            .packet_verified = true;
        Ok(
            json::to_string(&factory_protocol::SessionPacketVerificationResponse {
                protocol_version: factory_protocol::PROTOCOL_VERSION_V1,
                request_id: request.request_id,
                operation: factory_protocol::OP_SESSION_VERIFY_PACKET.to_owned(),
                packet_digest: digest.to_hex(),
                verified: true,
            })
            .into_bytes(),
        )
    }

    async fn seal_artifact(&self, frame: &[u8]) -> Result<Vec<u8>, LocalTransportError> {
        let request: factory_protocol::SessionSealArtifactRequest =
            factory_protocol::decode_operation_request(
                frame,
                factory_protocol::REQUEST_FRAME_MAX_BYTES,
                factory_protocol::OP_SESSION_SEAL_ARTIFACT,
            )?;
        self.require_packet_verified()?;
        if request.expected_revision != self.session.resulting_revision.get()
            || request.role != "pi_transcript_gzip"
            || request.byte_limit == 0
            || request.byte_limit > self.cas.maximum_object_bytes()
        {
            return Err(invalid_rpc(
                "session.seal_artifact",
                "revision, role, or byte limit is outside assignment authority",
            ));
        }
        let relative = RuntimeRelativePath::parse(request.staging_relative_path)
            .map_err(|_| invalid_rpc("session.seal_artifact", "staging path is invalid"))?;
        let (seal, receipt) = self
            .process
            .adopt_and_register_actor_artifact(
                &self.cas,
                &self.principal,
                &request.client_command_id,
                self.packet.kernel_build_id,
                Path::new(self.packet.staging_root.as_str()),
                Path::new(relative.as_str()),
                request.byte_limit,
            )
            .await?;
        let registered = RegisteredSessionArtifact {
            seal,
            artifact_id: receipt.artifact_id,
        };
        let mut state = self
            .state
            .lock()
            .map_err(|_| invalid_rpc("session.seal_artifact", "session RPC state is poisoned"))?;
        if state
            .transcript
            .is_some_and(|prior| prior.seal != seal || prior.artifact_id != receipt.artifact_id)
        {
            return Err(invalid_rpc(
                "session.seal_artifact",
                "a different transcript was already sealed",
            ));
        }
        state.transcript = Some(registered);
        drop(state);
        Ok(json::to_string(&factory_protocol::ArtifactReceiptResponse {
            protocol_version: factory_protocol::PROTOCOL_VERSION_V1,
            request_id: request.request_id,
            operation: factory_protocol::OP_SESSION_SEAL_ARTIFACT.to_owned(),
            artifact_id: receipt.artifact_id.get(),
            digest: seal.digest().to_hex(),
            byte_length: seal.byte_length(),
            aggregate_revision: self.session.resulting_revision.get(),
        })
        .into_bytes())
    }

    async fn seal_workspace_file(&self, frame: &[u8]) -> Result<Vec<u8>, LocalTransportError> {
        if !self.allowed_tools.contains("artifact_seal") {
            return Err(invalid_rpc(
                "artifact.seal_workspace_file",
                "artifact sealing is not assigned",
            ));
        }
        let request: factory_protocol::ArtifactSealWorkspaceFileRequest =
            factory_protocol::decode_operation_request(
                frame,
                factory_protocol::REQUEST_FRAME_MAX_BYTES,
                factory_protocol::OP_ARTIFACT_SEAL_WORKSPACE_FILE,
            )?;
        self.require_packet_verified()?;
        if request.expected_revision != self.session.resulting_revision.get()
            || request.byte_limit == 0
            || request.byte_limit > self.cas.maximum_object_bytes()
        {
            return Err(invalid_rpc(
                "artifact.seal_workspace_file",
                "revision or byte limit is outside assignment authority",
            ));
        }
        let relative = RepositoryRelativePath::parse(request.workspace_relative_path)
            .map_err(|_| invalid_rpc("artifact.seal_workspace_file", "workspace path is invalid"))?;
        let (seal, receipt) = self
            .process
            .adopt_and_register_actor_artifact(
                &self.cas,
                &self.principal,
                &request.client_command_id,
                self.packet.kernel_build_id,
                Path::new(self.packet.workspace_root.as_str()),
                Path::new(relative.as_str()),
                request.byte_limit,
            )
            .await?;
        Ok(json::to_string(&factory_protocol::ArtifactReceiptResponse {
            protocol_version: factory_protocol::PROTOCOL_VERSION_V1,
            request_id: request.request_id,
            operation: factory_protocol::OP_ARTIFACT_SEAL_WORKSPACE_FILE.to_owned(),
            artifact_id: receipt.artifact_id.get(),
            digest: seal.digest().to_hex(),
            byte_length: seal.byte_length(),
            aggregate_revision: self.session.resulting_revision.get(),
        })
        .into_bytes())
    }

    /// Reads only an artifact in this assignment's durable evidence closure.
    /// The requested ID is an index into that closure, never a capability to
    /// probe or retrieve arbitrary registered CAS content. This operation is
    /// read-only: no session, artifact, or audit row is created.
    async fn read_assignment_artifact(&self, frame: &[u8]) -> Result<Vec<u8>, LocalTransportError> {
        if !self.allowed_tools.contains("artifact_read") {
            return Err(invalid_rpc(
                "artifact.read",
                "artifact reading is not assigned",
            ));
        }
        self.require_packet_verified()?;
        let request: factory_protocol::ArtifactReadRequest =
            factory_protocol::decode_operation_request(
                frame,
                factory_protocol::REQUEST_FRAME_MAX_BYTES,
                factory_protocol::OP_ARTIFACT_READ,
            )?;
        let artifact_id = ArtifactId::new(request.artifact_id)
            .map_err(|_| invalid_rpc("artifact.read", "artifact ID is invalid"))?;
        let expected_digest = request
            .expected_digest
            .parse::<ContentDigest>()
            .map_err(|_| invalid_rpc("artifact.read", "expected digest is invalid"))?;
        let sealed = self
            .process
            .registered_artifact(&self.cas, artifact_id)
            .await?;
        require_packet_evidence_reference(
            &self.packet,
            artifact_id,
            expected_digest,
            sealed.digest(),
            sealed.byte_length(),
        )?;
        require_current_assignment_evidence_closure(
            &self.assignment_artifact_ids().await?,
            artifact_id,
        )?;
        if sealed.byte_length() > ARTIFACT_READ_MAX_BYTES {
            return Err(invalid_rpc(
                "artifact.read",
                "artifact exceeds the bounded assignment read limit",
            ));
        }
        let bytes = self
            .cas
            .read_verified(sealed.digest())
            .map_err(|error| artifact_read_error(format!("CAS verification failed: {error}")))?;
        Ok(json::to_string(&factory_protocol::ArtifactReadResponse {
            protocol_version: factory_protocol::PROTOCOL_VERSION_V1,
            request_id: request.request_id,
            operation: factory_protocol::OP_ARTIFACT_READ.to_owned(),
            artifact_id: artifact_id.get(),
            digest: sealed.digest().to_hex(),
            byte_length: sealed.byte_length(),
            content_base64: base64_encode(&bytes),
        })
        .into_bytes())
    }

    async fn assignment_artifact_ids(&self) -> Result<BTreeSet<ArtifactId>, LocalTransportError> {
        match self.packet.office {
            factory_protocol::Office::ProductResearch => Ok(BTreeSet::new()),
            factory_protocol::Office::Engineering => {
                let attempt = self.packet.ticket_attempt_id.ok_or_else(|| {
                    invalid_rpc("artifact.read", "Engineering packet has no attempt target")
                })?;
                let row = sqlx::query!(
                    "SELECT ta.stage, tr.proposal_artifact_id, tr.reproducer_artifact_id,
                            tr.expected_observation_artifact_id, tr.discovery_observation_artifact_id
                       FROM factory.ticket_attempts ta
                       JOIN factory.ticket_revisions tr ON tr.id = ta.ticket_revision_id
                      WHERE ta.id = $1 AND ta.campaign_id = $2
                        AND tr.application_revision_id = $3 AND ta.stage IN (0, 4)",
                    attempt.get(),
                    self.packet.campaign_id.get(),
                    self.packet.application_revision_id.get(),
                )
                .fetch_optional(&self.process.pool_for_session_runtime())
                .await
                .map_err(StoreError::from)?
                .ok_or_else(|| invalid_rpc("artifact.read", "Engineering target is not active"))?;
                let mut ids = BTreeSet::new();
                for value in [
                    row.proposal_artifact_id,
                    row.reproducer_artifact_id,
                    row.expected_observation_artifact_id,
                    row.discovery_observation_artifact_id,
                ] {
                    ids.insert(
                        ArtifactId::new(value)
                            .map_err(|error| artifact_read_error(error.to_string()))?,
                    );
                }
                self.extend_ticket_proposal_closure(
                    &mut ids,
                    ArtifactId::new(row.proposal_artifact_id)
                        .map_err(|error| artifact_read_error(error.to_string()))?,
                )
                .await?;
                if row.stage == 4 {
                    self.extend_rework_candidate_closure(&mut ids, attempt)
                        .await?;
                }
                Ok(ids)
            }
            factory_protocol::Office::Quality => {
                let attempt = self.packet.ticket_attempt_id.ok_or_else(|| {
                    invalid_rpc("artifact.read", "Quality packet has no attempt target")
                })?;
                let candidate = self.packet.candidate_id.ok_or_else(|| {
                    invalid_rpc("artifact.read", "Quality packet has no candidate target")
                })?;
                let rows = sqlx::query_scalar!(
                    "SELECT artifact_id FROM (
                         SELECT tr.proposal_artifact_id AS artifact_id
                           FROM factory.candidates c
                           JOIN factory.ticket_attempts ta ON ta.id = c.ticket_attempt_id
                           JOIN factory.ticket_revisions tr ON tr.id = ta.ticket_revision_id
                           LEFT JOIN factory.validations qv ON qv.candidate_id = c.id
                                AND qv.validation_scope = 1 AND qv.lifecycle = 1
                           LEFT JOIN factory.reviews qr ON qr.candidate_id = c.id
                          WHERE c.id = $1 AND c.ticket_attempt_id = $2
                            AND ta.campaign_id = $3 AND tr.application_revision_id = $4
                            AND c.lifecycle = 1 AND c.candidate_commit IS NOT NULL
                            AND (ta.stage IN (2, 6)
                                 OR (ta.stage = 3 AND qv.id IS NOT NULL AND qr.id IS NULL))
                         UNION
                         SELECT c.changed_paths_artifact_id FROM factory.candidates c WHERE c.id = $1
                         UNION SELECT c.patch_artifact_id FROM factory.candidates c WHERE c.id = $1
                         UNION SELECT c.engineering_report_artifact_id FROM factory.candidates c WHERE c.id = $1
                         UNION SELECT c.risks_artifact_id FROM factory.candidates c WHERE c.id = $1
                         UNION SELECT c.regression_patch_artifact_id FROM factory.candidates c WHERE c.id = $1
                         UNION SELECT c.regression_command_set_artifact_id FROM factory.candidates c WHERE c.id = $1
                         UNION SELECT c.regression_log_artifact_id FROM factory.candidates c WHERE c.id = $1
                         UNION SELECT v.command_set_artifact_id FROM factory.validations v WHERE v.candidate_id = $1
                         UNION SELECT v.log_artifact_id FROM factory.validations v WHERE v.candidate_id = $1
                         UNION SELECT r.rationale_artifact_id FROM factory.reviews r WHERE r.candidate_id = $1
                         UNION SELECT r.risks_artifact_id FROM factory.reviews r WHERE r.candidate_id = $1
                         UNION SELECT r.additional_probes_artifact_id FROM factory.reviews r
                          WHERE r.candidate_id = $1 AND r.additional_probes_artifact_id IS NOT NULL
                         UNION SELECT ad.rationale_artifact_id FROM factory.architect_decisions ad WHERE ad.candidate_id = $1
                     ) closure",
                    candidate.get(),
                    attempt.get(),
                    self.packet.campaign_id.get(),
                    self.packet.application_revision_id.get(),
                )
                .fetch_all(&self.process.pool_for_session_runtime())
                .await
                .map_err(StoreError::from)?;
                if rows.is_empty() {
                    return Err(invalid_rpc("artifact.read", "Quality target is not active"));
                }
                let mut ids = rows
                    .into_iter()
                    .map(|value| {
                        let value = value.ok_or_else(|| {
                            invalid_rpc("artifact.read", "Quality evidence closure is corrupt")
                        })?;
                        ArtifactId::new(value)
                            .map_err(|error| artifact_read_error(error.to_string()))
                    })
                    .collect::<Result<BTreeSet<_>, _>>()?;
                let proposal_id = self
                    .quality_ticket_proposal_artifact(attempt, candidate)
                    .await?;
                self.extend_ticket_proposal_closure(&mut ids, proposal_id)
                    .await?;
                Ok(ids)
            }
        }
    }

    async fn quality_ticket_proposal_artifact(
        &self,
        attempt: factory_protocol::TicketAttemptId,
        candidate: factory_protocol::CandidateId,
    ) -> Result<ArtifactId, LocalTransportError> {
        let value = sqlx::query_scalar!(
            "SELECT tr.proposal_artifact_id
               FROM factory.candidates c
               JOIN factory.ticket_attempts ta ON ta.id = c.ticket_attempt_id
               JOIN factory.ticket_revisions tr ON tr.id = ta.ticket_revision_id
               LEFT JOIN factory.validations qv ON qv.candidate_id = c.id
                    AND qv.validation_scope = 1 AND qv.lifecycle = 1
               LEFT JOIN factory.reviews qr ON qr.candidate_id = c.id
              WHERE c.id = $1 AND c.ticket_attempt_id = $2 AND ta.campaign_id = $3
                AND tr.application_revision_id = $4
                AND c.lifecycle = 1 AND c.candidate_commit IS NOT NULL
                AND (ta.stage IN (2, 6)
                     OR (ta.stage = 3 AND qv.id IS NOT NULL AND qr.id IS NULL))",
            candidate.get(),
            attempt.get(),
            self.packet.campaign_id.get(),
            self.packet.application_revision_id.get(),
        )
        .fetch_optional(&self.process.pool_for_session_runtime())
        .await
        .map_err(StoreError::from)?
        .ok_or_else(|| invalid_rpc("artifact.read", "Quality target is not active"))?;
        ArtifactId::new(value).map_err(|error| artifact_read_error(error.to_string()))
    }

    /// An Engineering rework has no candidate target in its packet, yet it
    /// must inspect the one rejected candidate's sealed Quality/review closure
    /// before it prepares a new tree.  Re-read that exact attempt-local head;
    /// do not trust an actor-selected candidate ID.
    async fn extend_rework_candidate_closure(
        &self,
        ids: &mut BTreeSet<ArtifactId>,
        attempt: factory_protocol::TicketAttemptId,
    ) -> Result<(), LocalTransportError> {
        let rows = sqlx::query_scalar!(
            "WITH rework_candidate AS (
                 SELECT c.id
                   FROM factory.candidates c
                   JOIN factory.ticket_attempts ta ON ta.id = c.ticket_attempt_id
                  WHERE c.ticket_attempt_id = $1 AND ta.campaign_id = $2
                    AND ta.stage = 4 AND c.lifecycle = 2
                  ORDER BY c.created_at DESC, c.id DESC
                  LIMIT 1
             )
             SELECT artifact_id FROM (
                 SELECT c.changed_paths_artifact_id AS artifact_id
                   FROM factory.candidates c JOIN rework_candidate rc ON rc.id = c.id
                 UNION SELECT c.patch_artifact_id FROM factory.candidates c JOIN rework_candidate rc ON rc.id = c.id
                 UNION SELECT c.engineering_report_artifact_id FROM factory.candidates c JOIN rework_candidate rc ON rc.id = c.id
                 UNION SELECT c.risks_artifact_id FROM factory.candidates c JOIN rework_candidate rc ON rc.id = c.id
                 UNION SELECT c.regression_patch_artifact_id FROM factory.candidates c JOIN rework_candidate rc ON rc.id = c.id
                 UNION SELECT c.regression_command_set_artifact_id FROM factory.candidates c JOIN rework_candidate rc ON rc.id = c.id
                 UNION SELECT c.regression_log_artifact_id FROM factory.candidates c JOIN rework_candidate rc ON rc.id = c.id
                 UNION SELECT v.command_set_artifact_id FROM factory.validations v JOIN rework_candidate rc ON rc.id = v.candidate_id
                 UNION SELECT v.log_artifact_id FROM factory.validations v JOIN rework_candidate rc ON rc.id = v.candidate_id
                 UNION SELECT r.rationale_artifact_id FROM factory.reviews r JOIN rework_candidate rc ON rc.id = r.candidate_id
                 UNION SELECT r.risks_artifact_id FROM factory.reviews r JOIN rework_candidate rc ON rc.id = r.candidate_id
                 UNION SELECT r.additional_probes_artifact_id FROM factory.reviews r
                    JOIN rework_candidate rc ON rc.id = r.candidate_id
                  WHERE r.additional_probes_artifact_id IS NOT NULL
                 UNION SELECT ad.rationale_artifact_id FROM factory.architect_decisions ad
                    JOIN rework_candidate rc ON rc.id = ad.candidate_id
             ) closure",
            attempt.get(),
            self.packet.campaign_id.get(),
        )
        .fetch_all(&self.process.pool_for_session_runtime())
        .await
        .map_err(StoreError::from)?;
        if rows.is_empty() {
            return Err(invalid_rpc(
                "artifact.read",
                "Engineering rework has no current rejected candidate evidence",
            ));
        }
        for value in rows {
            let value = value.ok_or_else(|| {
                invalid_rpc(
                    "artifact.read",
                    "Engineering rework evidence closure is corrupt",
                )
            })?;
            ids.insert(
                ArtifactId::new(value).map_err(|error| artifact_read_error(error.to_string()))?,
            );
        }
        Ok(())
    }

    async fn extend_ticket_proposal_closure(
        &self,
        ids: &mut BTreeSet<ArtifactId>,
        proposal_artifact_id: ArtifactId,
    ) -> Result<(), LocalTransportError> {
        let context = self
            .tickets
            .proposal_admission_context(self.packet.application_revision_id)
            .await?;
        let sealed = self
            .process
            .registered_artifact(&self.cas, proposal_artifact_id)
            .await?;
        let bytes = self
            .cas
            .read_verified(sealed.digest())
            .map_err(|error| artifact_read_error(format!("CAS verification failed: {error}")))?;
        let proposal =
            factory_protocol::parse_product_ticket_proposal_v1(&bytes, &context.ticket_bounds)
                .map_err(|error| {
                    artifact_read_error(format!("stored ticket proposal is invalid: {error}"))
                })?;
        for reference in proposal_artifact_references(&proposal) {
            ids.insert(reference.artifact_id);
        }
        Ok(())
    }

    async fn submit_product_ticket(&self, frame: &[u8]) -> Result<Vec<u8>, LocalTransportError> {
        if self.packet.office != factory_protocol::Office::ProductResearch
            || !self.allowed_tools.contains("product_submit_ticket")
        {
            return Err(invalid_rpc(
                "product.submit_ticket",
                "Product submission is not assigned to this office",
            ));
        }
        self.require_packet_verified()?;
        self.require_required_reads_before_mutation("product.submit_ticket")?;
        let request = factory_protocol::decode_product_submit_ticket_request_v1(frame)?;
        if request.expected_revision != self.session.resulting_revision.get() {
            return Err(invalid_rpc(
                "product.submit_ticket",
                "session revision is stale",
            ));
        }
        let receipt = execute_product_proposal(
            &self.process,
            &self.tickets,
            &self.cas,
            &self.command_runner,
            ExecuteProductProposal {
                principal: &self.principal,
                request: &request,
                application_revision_id: self.packet.application_revision_id,
                kernel_build_id: self.packet.kernel_build_id,
                workspace_root: Path::new(self.packet.workspace_root.as_str()),
            },
        )
        .await
        .map_err(|error| product_rpc_error(error.to_string()))?;
        Ok(
            json::to_string(&factory_protocol::OperationReceiptResponse {
                protocol_version: factory_protocol::PROTOCOL_VERSION_V1,
                request_id: request.request_id,
                operation: factory_protocol::OP_PRODUCT_SUBMIT_TICKET.to_owned(),
                audit_id: receipt.audit_log_id,
                aggregate_revision: receipt.resulting_revision.get(),
            })
            .into_bytes(),
        )
    }

    /// Accepts the one nonterminal Engineering checkpoint.  The trusted
    /// resolver runs before Git custody or a durable candidate/validation
    /// transition, so a scheduler that has not composed repository/ticket
    /// authority cannot cause a partial candidate write.
    async fn checkpoint_regression(&self, frame: &[u8]) -> Result<Vec<u8>, LocalTransportError> {
        if self.packet.office != factory_protocol::Office::Engineering
            || !self
                .allowed_tools
                .contains("candidate_checkpoint_regression")
        {
            return Err(invalid_rpc(
                "candidate.checkpoint_regression",
                "Engineering regression checkpoint is not assigned to this office",
            ));
        }
        self.require_packet_verified()?;
        self.require_required_reads_before_mutation("candidate.checkpoint_regression")?;
        let request: factory_protocol::CandidateCheckpointRegressionRequest =
            factory_protocol::decode_operation_request(
                frame,
                factory_protocol::REQUEST_FRAME_MAX_BYTES,
                factory_protocol::OP_CANDIDATE_CHECKPOINT_REGRESSION,
            )?;
        let runtime = require_candidate_quality_runtime(self.candidate_quality_runtime.as_ref())?;
        {
            let mut state = self.state.lock().map_err(|_| {
                invalid_rpc(
                    "candidate.checkpoint_regression",
                    "session RPC state is poisoned",
                )
            })?;
            state
                .engineering
                .begin_checkpoint()
                .map_err(|detail| invalid_rpc("candidate.checkpoint_regression", detail))?;
        }
        let resolved = match runtime
            .resolver
            .resolve_engineering(self.session.session_id, &self.packet)
            .await
        {
            Ok(resolved) => resolved,
            Err(error) => {
                self.clear_engineering_checkpoint_in_flight()?;
                return Err(candidate_rpc_error(error.to_string()));
            }
        };
        let authority = resolved.authority(self.actor_binding());
        let checkpoint = match checkpoint_regression(
            &self.process,
            &self.cas,
            &self.command_runner,
            &runtime.git,
            &authority,
            &request,
        )
        .await
        {
            Ok(checkpoint) => checkpoint,
            Err(error) => {
                self.clear_engineering_checkpoint_in_flight()?;
                return Err(candidate_rpc_error(error.to_string()));
            }
        };
        let response = factory_protocol::RegressionCheckpointReceiptResponse {
            protocol_version: factory_protocol::PROTOCOL_VERSION_V1,
            request_id: request.request_id,
            operation: factory_protocol::OP_CANDIDATE_CHECKPOINT_REGRESSION.to_owned(),
            regression_tree: checkpoint.regression_tree().as_str().to_owned(),
            regression_patch_artifact_id: checkpoint.regression_patch().artifact_id.get(),
            regression_command_set_artifact_id: checkpoint.command_set().artifact_id.get(),
            regression_log_artifact_id: checkpoint.log().artifact_id.get(),
        };
        let mut state = self.state.lock().map_err(|_| {
            invalid_rpc(
                "candidate.checkpoint_regression",
                "session RPC state is poisoned",
            )
        })?;
        state.engineering.accept_checkpoint(resolved, checkpoint);
        Ok(json::to_string(&response).into_bytes())
    }

    /// Uses only the opaque checkpoint and daemon-composed authority retained
    /// above.  The candidate request cannot carry a tree, commit, validation
    /// result, repository, or ticket identity.
    async fn submit_candidate(&self, frame: &[u8]) -> Result<Vec<u8>, LocalTransportError> {
        if self.packet.office != factory_protocol::Office::Engineering
            || !self.allowed_tools.contains("candidate_submit")
        {
            return Err(invalid_rpc(
                "candidate.submit",
                "Engineering candidate submission is not assigned to this office",
            ));
        }
        self.require_packet_verified()?;
        self.require_required_reads_before_mutation("candidate.submit")?;
        let request: factory_protocol::CandidateSubmitRequest =
            factory_protocol::decode_operation_request(
                frame,
                factory_protocol::REQUEST_FRAME_MAX_BYTES,
                factory_protocol::OP_CANDIDATE_SUBMIT,
            )?;
        let runtime = require_candidate_quality_runtime(self.candidate_quality_runtime.as_ref())?;
        let (resolved, checkpoint) = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| invalid_rpc("candidate.submit", "session RPC state is poisoned"))?;
            state
                .engineering
                .begin_submission()
                .map_err(|detail| invalid_rpc("candidate.submit", detail))?
        };
        let authority = resolved.authority(self.actor_binding());
        let outcome = match submit_candidate(
            &self.process,
            &runtime.decisions,
            &self.cas,
            &self.command_runner,
            &runtime.git,
            &authority,
            &checkpoint,
            &request,
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                self.clear_engineering_submission_in_flight()?;
                return Err(candidate_rpc_error(error.to_string()));
            }
        };
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| invalid_rpc("candidate.submit", "session RPC state is poisoned"))?;
            state.engineering.complete_submission();
        }
        match outcome {
            CandidateSubmissionOutcome::Validated {
                candidate,
                hard_validation,
                candidate_tree,
            } => Ok(
                json::to_string(&factory_protocol::CandidateReceiptResponse {
                    protocol_version: factory_protocol::PROTOCOL_VERSION_V1,
                    request_id: request.request_id,
                    operation: factory_protocol::OP_CANDIDATE_SUBMIT.to_owned(),
                    audit_id: candidate.audit_log_id,
                    aggregate_revision: candidate.resulting_revision.get(),
                    candidate_id: candidate.candidate_id.get(),
                    validation_id: hard_validation.validation_id.get(),
                    candidate_tree: candidate_tree.as_str().to_owned(),
                })
                .into_bytes(),
            ),
            CandidateSubmissionOutcome::Rejected {
                candidate,
                hard_validation,
            } => Err(candidate_rpc_error(format!(
                "hard candidate validation is {:?} (candidate {}, validation {})",
                hard_validation.state,
                candidate.candidate_id.get(),
                hard_validation.validation_id.get()
            ))),
        }
    }

    /// Runs the Quality full suite once per Quality session.  The candidate
    /// and exact tree come exclusively from the resolved authority, never
    /// from the frame.
    async fn run_quality_full_suite(&self, frame: &[u8]) -> Result<Vec<u8>, LocalTransportError> {
        if self.packet.office != factory_protocol::Office::Quality
            || !self.allowed_tools.contains("quality_run_full_suite")
        {
            return Err(invalid_rpc(
                "quality.run_full_suite",
                "Quality full-suite execution is not assigned to this office",
            ));
        }
        self.require_packet_verified()?;
        self.require_required_reads_before_mutation("quality.run_full_suite")?;
        let request: factory_protocol::QualityRunFullSuiteRequest =
            factory_protocol::decode_operation_request(
                frame,
                factory_protocol::REQUEST_FRAME_MAX_BYTES,
                factory_protocol::OP_QUALITY_RUN_FULL_SUITE,
            )?;
        let runtime = require_candidate_quality_runtime(self.candidate_quality_runtime.as_ref())?;
        {
            let mut state = self.state.lock().map_err(|_| {
                invalid_rpc("quality.run_full_suite", "session RPC state is poisoned")
            })?;
            state
                .quality
                .begin_full_suite()
                .map_err(|detail| invalid_rpc("quality.run_full_suite", detail))?;
        }
        let resolved = match runtime
            .resolver
            .resolve_quality(self.session.session_id, &self.packet)
            .await
        {
            Ok(resolved) => resolved,
            Err(error) => {
                self.clear_quality_full_suite_in_flight()?;
                return Err(candidate_rpc_error(error.to_string()));
            }
        };
        if resolved.prior_full_suite.is_some() {
            self.clear_quality_full_suite_in_flight()?;
            return Err(candidate_rpc_error(
                "this Quality assignment already has a durable passed full-suite receipt; submit its missing review instead".to_owned(),
            ));
        }
        let authority = resolved.authority(self.actor_binding());
        let full_suite = match run_quality_full_suite(
            &self.process,
            &runtime.decisions,
            &self.cas,
            &self.command_runner,
            &runtime.git,
            &authority,
            &request,
        )
        .await
        {
            Ok(full_suite) => full_suite,
            Err(error) => {
                self.clear_quality_full_suite_in_flight()?;
                return Err(candidate_rpc_error(error.to_string()));
            }
        };
        let response = factory_protocol::QualityValidationReceiptResponse {
            protocol_version: factory_protocol::PROTOCOL_VERSION_V1,
            request_id: request.request_id,
            operation: factory_protocol::OP_QUALITY_RUN_FULL_SUITE.to_owned(),
            audit_id: full_suite.audit_log_id,
            aggregate_revision: full_suite.receipt.revision.get(),
            validation_id: full_suite.receipt.validation_id.get(),
            candidate_id: full_suite.receipt.candidate_id.get(),
            candidate_tree: full_suite.receipt.candidate_tree.as_str().to_owned(),
        };
        let mut state = self
            .state
            .lock()
            .map_err(|_| invalid_rpc("quality.run_full_suite", "session RPC state is poisoned"))?;
        state.quality.accept_full_suite(resolved, full_suite);
        Ok(json::to_string(&response).into_bytes())
    }

    /// Submits Quality's one terminal review after this session's kernel-run
    /// full-suite receipt, or after a resolver-preloaded receipt from a prior
    /// interrupted Quality session. In either case the receipt is durable and
    /// exact; actor input cannot name a validation ID on its own.
    async fn submit_quality_review(&self, frame: &[u8]) -> Result<Vec<u8>, LocalTransportError> {
        if self.packet.office != factory_protocol::Office::Quality
            || !self.allowed_tools.contains("quality_submit_review")
        {
            return Err(invalid_rpc(
                "quality.submit_review",
                "Quality review submission is not assigned to this office",
            ));
        }
        self.require_packet_verified()?;
        self.require_required_reads_before_mutation("quality.submit_review")?;
        let request: factory_protocol::QualitySubmitReviewRequest =
            factory_protocol::decode_operation_request(
                frame,
                factory_protocol::REQUEST_FRAME_MAX_BYTES,
                factory_protocol::OP_QUALITY_SUBMIT_REVIEW,
            )?;
        let runtime = require_candidate_quality_runtime(self.candidate_quality_runtime.as_ref())?;
        let needs_recovered_receipt = {
            let state = self.state.lock().map_err(|_| {
                invalid_rpc("quality.submit_review", "session RPC state is poisoned")
            })?;
            state.quality.authority.is_none() && state.quality.full_suite.is_none()
        };
        let recovered = if needs_recovered_receipt {
            match runtime
                .resolver
                .resolve_quality(self.session.session_id, &self.packet)
                .await
            {
                Ok(resolved) => Some(resolved),
                Err(error) => return Err(candidate_rpc_error(error.to_string())),
            }
        } else {
            None
        };
        let (resolved, full_suite) = {
            let mut state = self.state.lock().map_err(|_| {
                invalid_rpc("quality.submit_review", "session RPC state is poisoned")
            })?;
            state
                .quality
                .begin_review(recovered)
                .map_err(|detail| invalid_rpc("quality.submit_review", detail))?
        };
        let authority = resolved.review_authority(self.actor_binding(), &full_suite);
        let review = match submit_quality_review(
            &self.process,
            &runtime.decisions,
            &self.cas,
            &authority,
            &request,
        )
        .await
        {
            Ok(review) => review,
            Err(error) => {
                self.clear_quality_review_in_flight()?;
                return Err(candidate_rpc_error(error.to_string()));
            }
        };
        let mut state = self
            .state
            .lock()
            .map_err(|_| invalid_rpc("quality.submit_review", "session RPC state is poisoned"))?;
        state.quality.complete_review();
        drop(state);
        Ok(
            json::to_string(&factory_protocol::QualityReviewReceiptResponse {
                protocol_version: factory_protocol::PROTOCOL_VERSION_V1,
                request_id: request.request_id,
                operation: factory_protocol::OP_QUALITY_SUBMIT_REVIEW.to_owned(),
                audit_id: review.audit_log_id,
                aggregate_revision: review.resulting_candidate_revision.get(),
                review_id: review.review_id.get(),
                candidate_id: review.candidate_id.get(),
                verdict: match review.verdict {
                    factory_protocol::ReviewVerdict::Accept => "accept".to_owned(),
                    factory_protocol::ReviewVerdict::Reject => "reject".to_owned(),
                },
            })
            .into_bytes(),
        )
    }

    fn actor_binding(&self) -> ActorRequestBinding<'_> {
        ActorRequestBinding {
            principal: &self.principal,
            session_id: self.session.session_id,
            session_revision: ExpectedRevision::new(self.session.resulting_revision),
            packet: &self.packet,
        }
    }

    fn clear_engineering_checkpoint_in_flight(&self) -> Result<(), LocalTransportError> {
        self.state
            .lock()
            .map_err(|_| {
                invalid_rpc(
                    "candidate.checkpoint_regression",
                    "session RPC state is poisoned",
                )
            })?
            .engineering
            .abandon_checkpoint();
        Ok(())
    }

    fn clear_engineering_submission_in_flight(&self) -> Result<(), LocalTransportError> {
        self.state
            .lock()
            .map_err(|_| invalid_rpc("candidate.submit", "session RPC state is poisoned"))?
            .engineering
            .abandon_submission();
        Ok(())
    }

    fn clear_quality_full_suite_in_flight(&self) -> Result<(), LocalTransportError> {
        self.state
            .lock()
            .map_err(|_| invalid_rpc("quality.run_full_suite", "session RPC state is poisoned"))?
            .quality
            .abandon_full_suite();
        Ok(())
    }

    fn clear_quality_review_in_flight(&self) -> Result<(), LocalTransportError> {
        self.state
            .lock()
            .map_err(|_| invalid_rpc("quality.submit_review", "session RPC state is poisoned"))?
            .quality
            .abandon_review();
        Ok(())
    }

    async fn submit_terminal(&self, frame: &[u8]) -> Result<Vec<u8>, LocalTransportError> {
        let request: factory_protocol::SessionSubmitTerminalRequest =
            factory_protocol::decode_operation_request(
                frame,
                factory_protocol::REQUEST_FRAME_MAX_BYTES,
                factory_protocol::OP_SESSION_SUBMIT_TERMINAL,
            )?;
        self.require_packet_verified()?;
        if request.expected_revision != self.session.resulting_revision.get() {
            return Err(invalid_rpc(
                "session.submit_terminal",
                "session revision is stale",
            ));
        }
        let payload = decode_base64(&request.terminal_payload_b64)
            .map_err(|detail| invalid_rpc("session.submit_terminal", detail))?;
        if payload.len() > factory_protocol::REQUEST_FRAME_MAX_BYTES {
            return Err(invalid_rpc(
                "session.submit_terminal",
                "terminal payload exceeds its bound",
            ));
        }
        let stop = parse_stop_reason(&request.stop_reason)
            .ok_or_else(|| invalid_rpc("session.submit_terminal", "stop reason is unknown"))?;
        let operation = request
            .terminal_operation
            .as_deref()
            .map(parse_terminal_operation)
            .transpose()
            .map_err(|()| invalid_rpc("session.submit_terminal", "operation is unknown"))?;
        if (stop == StopReasonV1::Completed) != operation.is_some() {
            return Err(invalid_rpc(
                "session.submit_terminal",
                "operation and stop reason disagree",
            ));
        }
        {
            let mut state = self.state.lock().map_err(|_| {
                invalid_rpc("session.submit_terminal", "session RPC state is poisoned")
            })?;
            match operation {
                Some(TerminalOperationV1::CandidateSubmit) if !state.engineering.submitted => {
                    return Err(invalid_rpc(
                        "session.submit_terminal",
                        "candidate terminal completion requires this session's candidate submission",
                    ));
                }
                Some(TerminalOperationV1::QualitySubmitReview)
                    if !state.quality.review_submitted =>
                {
                    return Err(invalid_rpc(
                        "session.submit_terminal",
                        "Quality terminal completion requires this session's review submission",
                    ));
                }
                _ => {}
            }
            let transcript = state.transcript.ok_or_else(|| {
                invalid_rpc("session.submit_terminal", "transcript is not sealed")
            })?;
            if transcript.artifact_id.get() != request.transcript_artifact_id {
                return Err(invalid_rpc(
                    "session.submit_terminal",
                    "transcript artifact identity does not match",
                ));
            }
            if state
                .terminal_request
                .as_ref()
                .is_some_and(|prior| prior != &request)
            {
                return Err(invalid_rpc(
                    "session.submit_terminal",
                    "a different terminal request already exists",
                ));
            }
            state.terminal_request = Some(request.clone());
        }
        let (response_sender, response_receiver) = smol::channel::bounded(1);
        self.terminal_sender
            .send(TerminalProposal {
                request,
                response: response_sender,
            })
            .await
            .map_err(|_| LocalTransportError::ResponseDisconnected)?;
        response_receiver
            .recv()
            .await
            .map_err(|_| LocalTransportError::ResponseDisconnected)
    }

    fn require_packet_verified(&self) -> Result<(), LocalTransportError> {
        if self
            .state
            .lock()
            .map_err(|_| invalid_rpc("session", "session RPC state is poisoned"))?
            .packet_verified
        {
            Ok(())
        } else {
            Err(invalid_rpc("session", "packet has not been verified"))
        }
    }
}

/// Launches one actor host, supervises it, and returns only after its session
/// is durably terminal. The child is spawned before `StartSession` so the
/// exact PID/PGID can commit, but it receives no admission bytes before that
/// transaction succeeds. A terminal RPC is a proposal: the daemon stops and
/// directly waits the child, seals final streams and its read ledger, commits
/// the terminal transition, and only then attempts to return the real receipt.
pub async fn launch_session<V>(
    process: &ProcessStore,
    forum: &ForumStore,
    tickets: &TicketStore,
    command_runner: &CommandRunner,
    daemon: &LocalDaemon,
    cas: &CasStore,
    request: SessionLaunchRequest,
    verifier: &V,
) -> Result<SessionRuntimeOutcome, SessionRuntimeError>
where
    V: SessionRuntimeVerifier,
{
    request.validate_identity()?;
    verifier.verify_packet(&request.packet, &request.canonical_packet_bytes)?;
    verifier.verify_runtime(&request.packet, &request.spawn)?;

    process.verify_packet_bytes(
        cas,
        &request.packet,
        request.packet_artifact,
        &request.canonical_packet_bytes,
        request.packet_digest,
    )?;
    let admitted_wire = factory_protocol::verify_assignment_packet_v1(
        &request.canonical_packet_bytes,
        &request.packet_digest.to_hex(),
    )
    .map_err(|_| SessionRuntimeError::Store(StoreError::InvalidPacketDigest))?;

    let (actor_client, unbound_server) = daemon.create_unbound_actor_socketpair()?;
    let supervision = request.supervision.clone();
    let spawned = crate::process_custody::spawn_pi_host(
        &request.spawn,
        actor_client.into_std_stream(),
        request.supervision.clone(),
    )?;
    let custody = spawned.custody();
    let cancellation = spawned.cancellation();
    let start = StartSession {
        principal: request.principal.clone(),
        command_id: request.command_id.clone(),
        expected_assignment_revision: request.expected_assignment_revision,
        assignment_id: request.assignment_id,
        packet_digest: request.packet_digest,
        custody,
    };
    let session = match process.start_session(&start).await {
        Ok(receipt) => receipt,
        Err(start_error) => {
            cancellation.request();
            return match spawned.wait().await {
                Ok(_) => Err(SessionRuntimeError::StartFailed(start_error)),
                Err(cleanup) => Err(SessionRuntimeError::StartAndCleanupFailed {
                    start: start_error,
                    cleanup,
                }),
            };
        }
    };
    let cancellation_completion = daemon
        .active_session_cancellations()
        .register(session.session_id, cancellation.clone())?;

    let identity = match process
        .actor_connection_identity(session.session_id, &request.packet)
        .await
    {
        Ok(identity) => identity,
        Err(error) => {
            cancellation.request();
            let _ = spawned.wait().await;
            return Err(SessionRuntimeError::Store(error));
        }
    };
    let mut server = unbound_server
        .bind(identity)
        .with_assignment_read_deadline(Duration::from_millis(
            request.packet.limits.wall_limit.get(),
        ))?;
    let admission = admission_line(
        request.packet.assignment_id,
        session.session_id,
        session.resulting_revision,
        request.packet.packet_digest,
        &request.canonical_packet_bytes,
    )
    .map_err(SessionRuntimeError::Transport)?;
    if let Err(error) = server.send_session_admission_line(&admission).await {
        cancellation.request();
        return match spawned.wait().await {
            Ok(_) => Err(SessionRuntimeError::AdmissionFailed { source: error }),
            Err(cleanup) => Err(SessionRuntimeError::AdmissionCleanupFailed(cleanup)),
        };
    }

    let read_authority = server
        .workspace_read_authority(
            Path::new(request.packet.workspace_root.as_str()),
            request.expected_read_manifest_artifact_id,
            request.required_reads.clone(),
        )
        .map_err(LocalTransportError::from)?;
    let process_cancellation = cancellation.clone();
    let read_authority = Arc::new(Mutex::new(Some(read_authority)));
    let read_authority_for_server = Arc::clone(&read_authority);
    let rpc_state = Arc::new(Mutex::new(SessionRpcState::default()));
    let (terminal_sender, terminal_receiver) = smol::channel::bounded(1);
    let rpc = KernelSessionRpc {
        process: process.clone(),
        forum: forum.clone(),
        tickets: tickets.clone(),
        command_runner: command_runner.clone(),
        candidate_quality_runtime: request.candidate_quality_runtime.clone(),
        cas: cas.clone(),
        packet: request.packet.clone(),
        canonical_packet_bytes: request.canonical_packet_bytes.clone(),
        packet_artifact: request.packet_artifact,
        session: session.clone(),
        // Actor-local command IDs restart at one for every fresh host. Bind
        // their durable idempotency namespace to the admitted session instead
        // of the operator principal that requested the launch.
        principal: actor_session_principal(&session, &request.packet),
        allowed_tools: admitted_wire.tools.into_iter().collect(),
        read_authority: Arc::clone(&read_authority),
        state: Arc::clone(&rpc_state),
        terminal_sender,
    };
    let (process_sender, process_receiver) = smol::channel::bounded(1);
    let process_task = smol::spawn(async move {
        let result = spawned.wait().await;
        let _ = process_sender.send(result).await;
    });
    let (server_sender, server_receiver) = smol::channel::bounded(1);
    let mut server_task = Some(smol::spawn(async move {
        let result = server
            .serve(move |frame| {
                if frame.envelope().operation == factory_protocol::OP_WORKSPACE_READ {
                    let request_id = frame.envelope().request_id.clone();
                    let operation = frame.envelope().operation.clone();
                    let result = match read_authority_for_server.lock() {
                        Ok(mut authority) => match authority.as_mut() {
                            Some(authority) => authority.handle_frame(&frame),
                            None => Err(WorkspaceReadError::ConnectionIdentityMismatch),
                        },
                        Err(_) => Err(WorkspaceReadError::ConnectionIdentityMismatch),
                    };
                    Box::pin(async move {
                        KernelSessionRpc::workspace_read_response(request_id, operation, result)
                    })
                } else {
                    rpc.dispatch_response(frame)
                }
            })
            .await;
        let _ = server_sender.send(result).await;
    }));
    enum FirstStop {
        Process(Result<SupervisedProcessOutcome, ProcessCustodyError>),
        Transport(Result<ActorDisconnect, LocalTransportError>),
        Terminal(TerminalProposal),
    }
    let first: Result<FirstStop, SessionRuntimeError> = smol::future::or(
        async {
            process_receiver
                .recv()
                .await
                .map(FirstStop::Process)
                .map_err(|_| SessionRuntimeError::ProcessResultChannelClosed)
        },
        smol::future::or(
            async {
                server_receiver
                    .recv()
                    .await
                    .map(FirstStop::Transport)
                    .map_err(|_| SessionRuntimeError::TransportResultChannelClosed)
            },
            async {
                match terminal_receiver.recv().await {
                    Ok(proposal) => Ok(FirstStop::Terminal(proposal)),
                    // The terminal proposal is optional. Its sender is owned
                    // by the transport task, so closure means the transport
                    // or child outcome is the authoritative stop signal. Let
                    // one of those sibling futures win instead of turning an
                    // ordinary actor exit into an infrastructure failure.
                    Err(_) => {
                        std::future::pending::<Result<FirstStop, SessionRuntimeError>>().await
                    }
                }
            },
        ),
    )
    .await;
    let (transport, process_outcome, terminal_proposal) = match first {
        Ok(FirstStop::Process(process_result)) => {
            let terminal = terminal_receiver.try_recv().ok();
            if let Some(task) = server_task.take() {
                let _ = task.cancel().await;
            }
            (
                SessionTransportStop::ProcessExited,
                process_result?,
                terminal,
            )
        }
        Ok(FirstStop::Transport(server_result)) => {
            match &server_result {
                Ok(disconnect) => tracing::warn!(
                    session_id = session.session_id.get(),
                    ?disconnect,
                    "actor transport ended before process or terminal evidence"
                ),
                Err(error) => tracing::warn!(
                    session_id = session.session_id.get(),
                    %error,
                    "actor transport failed before process or terminal evidence"
                ),
            }
            process_cancellation.request();
            let process_outcome = process_receiver
                .recv()
                .await
                .map_err(|_| SessionRuntimeError::ProcessResultChannelClosed)??;
            // `submit_terminal` deliberately waits for the durable receipt.
            // A peer may close immediately after sending it, making both the
            // proposal and disconnect ready in the same scheduler turn. Keep
            // the already-received proposal instead of downgrading truthful
            // actor evidence to an infrastructure terminal solely due to that
            // race.
            let terminal = terminal_receiver.try_recv().ok();
            let transport = match server_result {
                Ok(ActorDisconnect::PeerClosed) => SessionTransportStop::PeerDisconnected,
                Err(_) => SessionTransportStop::TransportFailed,
            };
            (transport, process_outcome, terminal)
        }
        Ok(FirstStop::Terminal(proposal)) => {
            process_cancellation.request();
            let process_outcome = process_receiver
                .recv()
                .await
                .map_err(|_| SessionRuntimeError::ProcessResultChannelClosed)??;
            (
                SessionTransportStop::ProcessExited,
                process_outcome,
                Some(proposal),
            )
        }
        Err(error) => {
            process_cancellation.request();
            let _ = process_receiver.recv().await;
            if let Some(task) = server_task.take() {
                let _ = task.cancel().await;
            }
            return Err(error);
        }
    };
    let read_authority = read_authority
        .lock()
        .map_err(|_| SessionRuntimeError::ReadAuthorityPoisoned)?
        .take()
        .ok_or(SessionRuntimeError::ReadAuthorityStillInUse)?;
    let terminal_request = terminal_proposal.as_ref().map(|proposal| &proposal.request);
    let terminal = reconcile_terminal(
        process,
        cas,
        &request,
        &session,
        &supervision,
        read_authority,
        &rpc_state,
        terminal_request,
        process_outcome,
        transport,
    )
    .await?;
    cancellation_completion
        .finish(ReconciledSessionCancellation {
            campaign_id: request.packet.campaign_id,
            session_id: session.session_id,
            campaign_revision: terminal.campaign_revision,
        })
        .await;
    if let Some(proposal) = terminal_proposal {
        let response = json::to_string(&factory_protocol::OperationReceiptResponse {
            protocol_version: factory_protocol::PROTOCOL_VERSION_V1,
            request_id: proposal.request.request_id,
            operation: factory_protocol::OP_SESSION_SUBMIT_TERMINAL.to_owned(),
            audit_id: terminal.audit_log_id,
            aggregate_revision: terminal.resulting_revision.get(),
        })
        .into_bytes();
        let _ = proposal.response.send(response).await;
    }
    if let Some(task) = server_task.take() {
        let _ = task.cancel().await;
    }
    drop(process_task);
    Ok(SessionRuntimeOutcome {
        session,
        terminal,
        process: process_outcome,
        transport,
    })
}

#[allow(clippy::too_many_arguments)]
async fn reconcile_terminal(
    process: &ProcessStore,
    cas: &CasStore,
    request: &SessionLaunchRequest,
    session: &SessionReceipt,
    supervision: &ProcessSupervisionSpec,
    read_authority: WorkspaceReadAuthority,
    rpc_state: &Arc<Mutex<SessionRpcState>>,
    terminal_request: Option<&factory_protocol::SessionSubmitTerminalRequest>,
    process_outcome: SupervisedProcessOutcome,
    transport: SessionTransportStop,
) -> Result<TerminalReceipt, SessionRuntimeError> {
    let staging_root = request.staging_root();
    let stdout = adopt_runtime_artifact(
        process,
        cas,
        request,
        session.session_id,
        "stdout",
        supervision.stdout_path(),
        cas.maximum_object_bytes(),
    )
    .await?;
    let stderr = adopt_runtime_artifact(
        process,
        cas,
        request,
        session.session_id,
        "stderr",
        supervision.stderr_path(),
        cas.maximum_object_bytes(),
    )
    .await?;

    let registered_transcript = rpc_state
        .lock()
        .map_err(|_| SessionRuntimeError::RpcStatePoisoned)?
        .transcript;
    let (transcript, partial_transcript) = if let Some(registered) = registered_transcript {
        (registered.seal, None)
    } else {
        let partial_path = staging_root.join(SESSION_PARTIAL_TRANSCRIPT_RELATIVE_PATH);
        ensure_partial_transcript(&partial_path)?;
        let partial = adopt_runtime_artifact(
            process,
            cas,
            request,
            session.session_id,
            "partial-transcript",
            &partial_path,
            cas.maximum_object_bytes(),
        )
        .await?;
        (partial, Some(partial))
    };

    let assertion = read_authority.seal_assertion(cas, &staging_root)?;
    let assertion_path = staging_root.join(format!(
        "required-read-assertion-{}.json",
        session.session_id.get()
    ));
    let registered_assertion = adopt_runtime_artifact(
        process,
        cas,
        request,
        session.session_id,
        "read-assertion",
        &assertion_path,
        cas.maximum_object_bytes(),
    )
    .await?;
    if registered_assertion != assertion.artifact() {
        return Err(SessionRuntimeError::Read(
            WorkspaceReadError::AssertionChanged,
        ));
    }

    let usage = terminal_request.map(|terminal| UsageTotalsV1 {
        input_tokens: terminal.input_tokens,
        output_tokens: terminal.output_tokens,
        cache_read_tokens: terminal.cache_read_tokens,
        cache_write_tokens: terminal.cache_write_tokens,
        reasoning_tokens: terminal.reasoning_tokens,
        reported_cost_micro_usd: terminal.reported_cost_micro_usd.map(MicroUsd::new),
    });
    let evidence = process
        .verify_terminal_evidence_with_packet_bytes(
            cas,
            session.session_id,
            &request.packet,
            request.packet_artifact,
            &request.canonical_packet_bytes,
            TerminalArtifactSeals {
                transcript,
                stdout,
                stderr,
                partial_transcript,
            },
            assertion,
            usage,
        )
        .await?;

    if let Some(actor) = terminal_request {
        let operation = actor
            .terminal_operation
            .as_deref()
            .map(parse_terminal_operation)
            .transpose()
            .map_err(|()| SessionRuntimeError::InvalidTerminalContract)?;
        let stop_reason = parse_stop_reason(&actor.stop_reason)
            .ok_or(SessionRuntimeError::InvalidTerminalContract)?;
        let report = TerminalReportV1 {
            packet_digest: request.packet_digest,
            expected_session_revision: ExpectedRevision::new(session.resulting_revision),
            operation,
            stop_reason,
            report_digest: ContentDigest::of_bytes(
                &decode_base64(&actor.terminal_payload_b64)
                    .map_err(|_| SessionRuntimeError::InvalidTerminalContract)?,
            ),
        };
        match process
            .terminal_session(
                &actor_session_principal(session, &request.packet),
                &actor.client_command_id,
                session.session_id,
                &report,
                evidence.clone(),
            )
            .await
        {
            Ok(receipt) => return Ok(receipt),
            Err(error) => {
                tracing::debug!(session_id = session.session_id.get(), %error, "actor terminal proposal rejected; recording infrastructure terminal state");
            }
        }
    }

    let stop_reason = infrastructure_stop_reason(process_outcome.reason, transport);
    let report = TerminalReportV1 {
        packet_digest: request.packet_digest,
        expected_session_revision: ExpectedRevision::new(session.resulting_revision),
        operation: None,
        stop_reason,
        report_digest: infrastructure_report_digest(process_outcome, transport),
    };
    process
        .terminal_session(
            "kernel",
            &format!("kernel-reconcile-session-{}", session.session_id.get()),
            session.session_id,
            &report,
            evidence,
        )
        .await
        .map_err(SessionRuntimeError::from)
}

fn actor_session_principal(session: &SessionReceipt, packet: &AssignmentPacketV1) -> String {
    format!(
        "actor-session-{}-assignment-{}-application-{}-campaign-{}",
        session.session_id.get(),
        packet.assignment_id.get(),
        packet.application_revision_id.get(),
        packet.campaign_id.get(),
    )
}

async fn adopt_runtime_artifact(
    process: &ProcessStore,
    cas: &CasStore,
    request: &SessionLaunchRequest,
    session_id: SessionId,
    role: &str,
    absolute_path: &Path,
    byte_limit: u64,
) -> Result<CasArtifact, SessionRuntimeError> {
    let staging_root = request.staging_root();
    let relative = absolute_path.strip_prefix(&staging_root).map_err(|_| {
        SessionRuntimeError::ArtifactPathOutsideStaging {
            path: absolute_path.to_owned(),
        }
    })?;
    let relative =
        RuntimeRelativePath::parse(relative.to_string_lossy().to_string()).map_err(|_| {
            SessionRuntimeError::ArtifactPathOutsideStaging {
                path: absolute_path.to_owned(),
            }
        })?;
    let (seal, _) = process
        .adopt_and_register_actor_artifact(
            cas,
            &request.principal,
            &format!("kernel-session-{}-{role}", session_id.get()),
            request.packet.kernel_build_id,
            &staging_root,
            Path::new(relative.as_str()),
            byte_limit,
        )
        .await?;
    Ok(seal)
}

fn ensure_partial_transcript(path: &Path) -> Result<(), SessionRuntimeError> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|source| SessionRuntimeError::TerminalEvidenceIo {
            path: path.to_owned(),
            source,
        })?;
    file.flush()
        .and_then(|()| file.sync_all())
        .map_err(|source| SessionRuntimeError::TerminalEvidenceIo {
            path: path.to_owned(),
            source,
        })
}

fn infrastructure_stop_reason(
    process: ProcessStopReason,
    transport: SessionTransportStop,
) -> StopReasonV1 {
    // An operator cancellation closes the actor descriptor as a consequence
    // of terminating the exact owned group. Preserve the initiating custody
    // reason instead of misclassifying that expected peer close as a daemon
    // disconnect.
    if transport == SessionTransportStop::TransportFailed {
        return StopReasonV1::ProtocolError;
    }
    if process == ProcessStopReason::Cancelled {
        return StopReasonV1::Cancelled;
    }
    if transport == SessionTransportStop::PeerDisconnected {
        return StopReasonV1::DaemonDisconnected;
    }
    match process {
        ProcessStopReason::Exited => StopReasonV1::ProtocolError,
        ProcessStopReason::NonZeroExit => StopReasonV1::NonZeroExit,
        ProcessStopReason::Cancelled => StopReasonV1::Cancelled,
        ProcessStopReason::Deadline => StopReasonV1::Deadline,
        ProcessStopReason::StdoutLimit | ProcessStopReason::StderrLimit => {
            StopReasonV1::OutputLimit
        }
    }
}

fn infrastructure_report_digest(
    process: SupervisedProcessOutcome,
    transport: SessionTransportStop,
) -> ContentDigest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"factory-session-infrastructure-terminal-v1\0");
    hasher.update(&(process.reason as u8).to_be_bytes());
    hasher.update(&(transport as u8).to_be_bytes());
    hasher.update(&process.exit_code.unwrap_or(i32::MIN).to_be_bytes());
    hasher.update(&process.signal.unwrap_or(i32::MIN).to_be_bytes());
    hasher.update(&process.stdout_bytes.to_be_bytes());
    hasher.update(&process.stderr_bytes.to_be_bytes());
    ContentDigest::from_bytes(*hasher.finalize().as_bytes())
}

fn parse_terminal_operation(value: &str) -> Result<TerminalOperationV1, ()> {
    Ok(match value {
        "work_complete" => TerminalOperationV1::WorkComplete,
        "candidate_submit" => TerminalOperationV1::CandidateSubmit,
        "quality_submit_review" => TerminalOperationV1::QualitySubmitReview,
        _ => return Err(()),
    })
}

fn forum_tool_name(operation: &str) -> Option<&'static str> {
    Some(match operation {
        factory_protocol::OP_FORUM_SEARCH => "forum_search",
        factory_protocol::OP_FORUM_LIST_TOPICS => "forum_list_topics",
        factory_protocol::OP_FORUM_LIST_THREADS => "forum_list_threads",
        factory_protocol::OP_FORUM_READ_THREAD => "forum_read_thread",
        factory_protocol::OP_FORUM_CREATE_TOPIC => "forum_create_topic",
        factory_protocol::OP_FORUM_CREATE_THREAD => "forum_create_thread",
        factory_protocol::OP_FORUM_POST => "forum_post",
        _ => return None,
    })
}

fn parse_stop_reason(value: &str) -> Option<StopReasonV1> {
    Some(match value {
        "completed" => StopReasonV1::Completed,
        "cancelled" => StopReasonV1::Cancelled,
        "deadline" => StopReasonV1::Deadline,
        "daemon_disconnected" => StopReasonV1::DaemonDisconnected,
        "nonzero_exit" => StopReasonV1::NonZeroExit,
        "output_limit" => StopReasonV1::OutputLimit,
        "protocol_error" => StopReasonV1::ProtocolError,
        "unknown_cost" => StopReasonV1::UnknownCost,
        _ => return None,
    })
}

fn invalid_rpc(operation: &'static str, detail: &'static str) -> LocalTransportError {
    LocalTransportError::Frame(factory_protocol::FrameError::InvalidJson {
        operation,
        detail: detail.to_owned(),
    })
}

fn product_rpc_error(detail: String) -> LocalTransportError {
    LocalTransportError::Frame(factory_protocol::FrameError::InvalidJson {
        operation: "product.submit_ticket",
        detail,
    })
}

fn candidate_rpc_error(detail: String) -> LocalTransportError {
    LocalTransportError::Frame(factory_protocol::FrameError::InvalidJson {
        operation: "candidate_quality",
        detail,
    })
}

/// Refuse an uncomposed Candidate/Quality operation before a dispatcher can
/// mark an in-memory session transition in flight or invoke custody.
fn require_candidate_quality_runtime(
    runtime: Option<&CandidateQualitySessionRuntime>,
) -> Result<&CandidateQualitySessionRuntime, LocalTransportError> {
    runtime.ok_or_else(|| {
        candidate_rpc_error(CandidateQualityAuthorityResolutionError::Unavailable.to_string())
    })
}

fn artifact_read_error(detail: impl Into<String>) -> LocalTransportError {
    LocalTransportError::Frame(factory_protocol::FrameError::InvalidJson {
        operation: "artifact.read",
        detail: detail.into(),
    })
}

/// The signed packet is the actor's complete named evidence capability.  A
/// durable closure check alone would let an actor guess a currently related
/// artifact ID; packet membership alone would leave a stale assignment able
/// to read evidence no longer valid for its current target stage.  Both gates
/// therefore compare the same full sealed identity before any CAS bytes move.
fn require_packet_evidence_reference(
    packet: &AssignmentPacketV1,
    artifact_id: ArtifactId,
    requested_digest: ContentDigest,
    registered_digest: ContentDigest,
    registered_byte_length: u64,
) -> Result<(), LocalTransportError> {
    let reference = packet
        .assignment_evidence
        .iter()
        .find(|reference| reference.artifact_id == artifact_id)
        .ok_or_else(|| {
            invalid_rpc(
                "artifact.read",
                "artifact is not named by this assignment packet evidence",
            )
        })?;
    if reference.digest != requested_digest {
        return Err(invalid_rpc(
            "artifact.read",
            "requested digest differs from the assignment packet evidence reference",
        ));
    }
    if reference.digest != registered_digest {
        return Err(invalid_rpc(
            "artifact.read",
            "registered artifact digest differs from the assignment packet evidence reference",
        ));
    }
    if reference.byte_length != registered_byte_length {
        return Err(invalid_rpc(
            "artifact.read",
            "registered artifact length differs from the assignment packet evidence reference",
        ));
    }
    Ok(())
}

fn require_current_assignment_evidence_closure(
    closure: &BTreeSet<ArtifactId>,
    artifact_id: ArtifactId,
) -> Result<(), LocalTransportError> {
    if closure.contains(&artifact_id) {
        Ok(())
    } else {
        Err(invalid_rpc(
            "artifact.read",
            "artifact is not in this assignment's current durable evidence closure",
        ))
    }
}

fn proposal_artifact_references(
    proposal: &factory_protocol::ProductTicketProposalV1,
) -> Vec<&factory_protocol::SealedArtifactReferenceV1> {
    let mut references = vec![
        &proposal.narrative,
        &proposal.evidence,
        &proposal.reproducer.command,
        &proposal.reproducer.expected_observation.stdout,
        &proposal.reproducer.expected_observation.stderr,
        &proposal.reproducer.first_observation.stdout,
        &proposal.reproducer.first_observation.stderr,
        &proposal.reproducer.second_observation.stdout,
        &proposal.reproducer.second_observation.stderr,
    ];
    if let Some(stdin) = &proposal.reproducer.stdin {
        references.push(stdin);
    }
    references
}

fn decode_base64(value: &str) -> Result<Vec<u8>, &'static str> {
    if value.is_empty() || value.len() % 4 != 0 {
        return Err("base64 value is empty or has invalid length");
    }
    let mut output = Vec::with_capacity(value.len() / 4 * 3);
    for (chunk_index, chunk) in value.as_bytes().chunks_exact(4).enumerate() {
        let final_chunk = chunk_index + 1 == value.len() / 4;
        let padding = chunk.iter().rev().take_while(|byte| **byte == b'=').count();
        if padding > 2 || (!final_chunk && padding != 0) || chunk[..4 - padding].contains(&b'=') {
            return Err("base64 padding is invalid");
        }
        let a = base64_value(chunk[0]).ok_or("base64 alphabet is invalid")?;
        let b = base64_value(chunk[1]).ok_or("base64 alphabet is invalid")?;
        let c = if chunk[2] == b'=' {
            0
        } else {
            base64_value(chunk[2]).ok_or("base64 alphabet is invalid")?
        };
        let d = if chunk[3] == b'=' {
            0
        } else {
            base64_value(chunk[3]).ok_or("base64 alphabet is invalid")?
        };
        if (padding == 2 && b & 0x0f != 0) || (padding == 1 && c & 0x03 != 0) {
            return Err("base64 has non-canonical trailing bits");
        }
        output.push((a << 2) | (b >> 4));
        if padding < 2 {
            output.push((b << 4) | (c >> 2));
        }
        if padding == 0 {
            output.push((c << 6) | d);
        }
    }
    Ok(output)
}

fn base64_value(value: u8) -> Option<u8> {
    match value {
        b'A'..=b'Z' => Some(value - b'A'),
        b'a'..=b'z' => Some(value - b'a' + 26),
        b'0'..=b'9' => Some(value - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

#[derive(Serialize)]
struct SessionAdmissionLine<'a> {
    assignment_id: String,
    packet_b64: String,
    packet_digest: String,
    protocol_version: u16,
    session_id: i64,
    session_revision: u64,
    r#type: &'a str,
}

fn admission_line(
    assignment_id: AssignmentId,
    session_id: SessionId,
    session_revision: factory_protocol::AggregateRevision,
    packet_digest: ContentDigest,
    packet_bytes: &[u8],
) -> Result<Vec<u8>, LocalTransportError> {
    let value = SessionAdmissionLine {
        assignment_id: assignment_id.get().to_string(),
        packet_b64: base64_encode(packet_bytes),
        packet_digest: packet_digest.to_hex(),
        protocol_version: ADMISSION_PROTOCOL_VERSION,
        session_id: session_id.get(),
        session_revision: session_revision.get(),
        r#type: "session.admitted",
    };
    let mut line = miniserde::json::to_string(&value).into_bytes();
    line.push(b'\n');
    if line.len() > ADMISSION_MAX_BYTES {
        return Err(LocalTransportError::Frame(
            factory_protocol::FrameError::Oversized {
                actual: line.len(),
                maximum: ADMISSION_MAX_BYTES,
            },
        ));
    }
    Ok(line)
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(TABLE[(first >> 2) as usize] as char);
        output.push(TABLE[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        output.push(if chunk.len() > 1 {
            TABLE[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            TABLE[(third & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use factory_protocol::{
        AbsoluteHostPath, AggregateRevision, CredentialDescriptorV1, DurationMillis, MicroUsd,
        ModelProfileV1, Office, RepositoryRelativePath, RuntimeIdentityV1, SessionLimitsV1,
        ThinkingLevelV1,
    };

    fn packet() -> AssignmentPacketV1 {
        AssignmentPacketV1 {
            format_version: factory_protocol::ASSIGNMENT_PACKET_V1_FORMAT,
            campaign_id: factory_protocol::CampaignId::new(1).unwrap(),
            assignment_id: AssignmentId::new(2).unwrap(),
            kernel_build_id: factory_protocol::KernelBuildId::new(ContentDigest::of_bytes(
                b"build",
            )),
            application_revision_id: factory_protocol::ApplicationRevisionId::new(3).unwrap(),
            office: Office::Engineering,
            target: "test".to_owned(),
            ticket_attempt_id: Some(factory_protocol::TicketAttemptId::new(1).unwrap()),
            candidate_id: None,
            system_prompt_artifact_id: factory_protocol::ArtifactId::new(4).unwrap(),
            assignment_prompt_artifact_id: factory_protocol::ArtifactId::new(5).unwrap(),
            required_read_manifest_artifact_id: factory_protocol::ArtifactId::new(6).unwrap(),
            workspace_root: AbsoluteHostPath::parse("/tmp/workspace").unwrap(),
            staging_root: AbsoluteHostPath::parse("/tmp/staging").unwrap(),
            model: ModelProfileV1 {
                provider: "test".to_owned(),
                model_id: "test-model".to_owned(),
                thinking_level: ThinkingLevelV1::None,
                context_token_limit: 1,
                output_token_limit: 1,
                price_input_micro_usd_per_million_tokens: MicroUsd::new(1),
                price_output_micro_usd_per_million_tokens: MicroUsd::new(1),
                price_cache_read_micro_usd_per_million_tokens: MicroUsd::new(1),
                price_cache_write_micro_usd_per_million_tokens: MicroUsd::new(1),
                capability_flags: Vec::new(),
            },
            limits: SessionLimitsV1 {
                turn_limit: 1,
                wall_limit: DurationMillis::new(1),
                output_byte_limit: 1,
            },
            runtime: RuntimeIdentityV1 {
                deno_executable: AbsoluteHostPath::parse("/opt/deno").unwrap(),
                deno_version: "test".to_owned(),
                source_graph_digest: ContentDigest::of_bytes(b"graph"),
                resolved_dependency_graph_digest: ContentDigest::of_bytes(b"dependencies"),
                deno_json_digest: ContentDigest::of_bytes(b"json"),
                deno_lock_digest: ContentDigest::of_bytes(b"lock"),
                pi_version: "test".to_owned(),
                credential: CredentialDescriptorV1::PiAuthStore {
                    path: factory_protocol::RuntimeRelativePath::parse("credentials/test").unwrap(),
                },
            },
            required_reads: vec![factory_protocol::ReadExactFileV1 {
                path: RepositoryRelativePath::parse("AGENTS.md").unwrap(),
                digest: ContentDigest::of_bytes(b"read"),
                reason: "test".to_owned(),
            }],
            assignment_evidence: vec![factory_protocol::AssignmentEvidenceV1 {
                role: factory_protocol::AssignmentEvidenceRoleV1::TicketProposal,
                artifact_id: factory_protocol::ArtifactId::new(7).unwrap(),
                digest: ContentDigest::of_bytes(b"proposal"),
                byte_length: 8,
            }],
            terminal_operations: vec![factory_protocol::TerminalOperationV1::WorkComplete],
            remaining_campaign_allowance: MicroUsd::new(1),
            revision: AggregateRevision::initial(),
            packet_digest: ContentDigest::of_bytes(b"fixture packet"),
        }
    }

    #[test]
    fn startup_line_is_exact_and_canonical_base64() {
        let packet = packet();
        let line = admission_line(
            packet.assignment_id,
            SessionId::new(7).unwrap(),
            AggregateRevision::initial(),
            packet.packet_digest,
            b"{}",
        )
        .unwrap();
        assert_eq!(line.last(), Some(&b'\n'));
        let text = std::str::from_utf8(&line).unwrap();
        assert!(text.contains("\"type\":\"session.admitted\""));
        assert!(text.contains("\"assignment_id\":\"2\""));
        assert!(text.contains("\"packet_b64\":\"e30=\""));
        assert!(text.contains("\"session_id\":7"));
    }

    #[test]
    fn base64_encoder_handles_utf8_and_padding() {
        assert_eq!(base64_encode("✓".as_bytes()), "4pyT");
        assert_eq!(base64_encode(b"a"), "YQ==");
        assert_eq!(base64_encode(b"ab"), "YWI=");
        assert_eq!(base64_encode(b"abc"), "YWJj");
        assert_eq!(decode_base64("YQ==").unwrap(), b"a");
        assert_eq!(decode_base64("YWI=").unwrap(), b"ab");
        assert!(decode_base64("YR==").is_err());
        assert!(decode_base64("YWJ=").is_err());
        assert!(decode_base64("Y=Jj").is_err());
    }

    #[test]
    fn engineering_submission_requires_the_session_owned_checkpoint() {
        let mut state = EngineeringSessionState::default();
        assert!(matches!(
            state.begin_submission(),
            Err("an accepted regression checkpoint is required before candidate submission")
        ));
        assert!(!state.submission_in_flight);

        state.begin_checkpoint().expect("first checkpoint begins");
        assert_eq!(
            state.begin_checkpoint(),
            Err(
                "the Engineering session already has a regression checkpoint or candidate submission"
            )
        );
        state.abandon_checkpoint();
        assert!(!state.checkpoint_in_flight);
    }

    #[test]
    fn quality_review_requires_the_session_owned_full_suite() {
        let mut state = QualitySessionState::default();
        assert!(matches!(
            state.begin_review(None),
            Err("this session must run its kernel-owned full suite before review submission")
        ));
        assert!(!state.review_in_flight);

        state.begin_full_suite().expect("first full suite begins");
        assert_eq!(
            state.begin_full_suite(),
            Err("the Quality session already has a full-suite receipt or review submission")
        );
        state.abandon_full_suite();
        assert!(!state.full_suite_in_flight);
    }

    #[test]
    fn unavailable_candidate_quality_authority_rejects_before_session_mutation() {
        let state = SessionRpcState::default();
        assert!(require_candidate_quality_runtime(None).is_err());
        assert!(!state.engineering.checkpoint_in_flight);
        assert!(!state.engineering.submission_in_flight);
        assert!(!state.quality.full_suite_in_flight);
        assert!(!state.quality.review_in_flight);
    }

    #[test]
    fn artifact_read_requires_packet_identity_and_current_durable_closure() {
        let packet = packet();
        let reference = &packet.assignment_evidence[0];
        let expected_id = reference.artifact_id;
        let expected_digest = reference.digest;

        require_packet_evidence_reference(
            &packet,
            expected_id,
            expected_digest,
            expected_digest,
            reference.byte_length,
        )
        .expect("exact packet reference is accepted");
        assert!(
            require_packet_evidence_reference(
                &packet,
                factory_protocol::ArtifactId::new(8).unwrap(),
                expected_digest,
                expected_digest,
                reference.byte_length,
            )
            .is_err()
        );
        assert!(
            require_packet_evidence_reference(
                &packet,
                expected_id,
                ContentDigest::of_bytes(b"wrong digest"),
                expected_digest,
                reference.byte_length,
            )
            .is_err()
        );
        assert!(
            require_packet_evidence_reference(
                &packet,
                expected_id,
                expected_digest,
                expected_digest,
                reference.byte_length + 1,
            )
            .is_err()
        );

        let mut closure = BTreeSet::new();
        closure.insert(expected_id);
        require_current_assignment_evidence_closure(&closure, expected_id)
            .expect("current stage retains named reference");
        assert!(
            require_current_assignment_evidence_closure(&BTreeSet::new(), expected_id,).is_err()
        );
    }

    #[test]
    fn rejected_workspace_read_is_a_response_and_later_reads_can_continue() {
        let rejected = KernelSessionRpc::workspace_read_response(
            "missing-read".to_owned(),
            factory_protocol::OP_WORKSPACE_READ.to_owned(),
            Err(WorkspaceReadError::Io {
                operation: "canonicalize workspace file",
                path: std::path::PathBuf::from("missing.xsh"),
                source: std::io::Error::from(std::io::ErrorKind::NotFound),
            }),
        )
        .expect("a missing path is a framed actor response");
        let error: factory_protocol::ErrorResponse =
            json::from_str(std::str::from_utf8(&rejected).unwrap()).unwrap();
        assert_eq!(error.request_id, "missing-read");
        assert_eq!(error.operation, factory_protocol::OP_WORKSPACE_READ);
        assert_eq!(error.error_code, "workspace_read_rejected");

        let successful = br#"{"operation":"workspace.read","request_id":"next"}"#.to_vec();
        assert_eq!(
            KernelSessionRpc::workspace_read_response(
                "next".to_owned(),
                factory_protocol::OP_WORKSPACE_READ.to_owned(),
                Ok(successful.clone()),
            )
            .expect("the same dispatcher remains usable"),
            successful
        );
    }

    #[test]
    fn initiating_transport_failure_is_not_mislabeled_as_actor_cancellation() {
        assert_eq!(
            infrastructure_stop_reason(
                ProcessStopReason::Cancelled,
                SessionTransportStop::TransportFailed,
            ),
            StopReasonV1::ProtocolError,
        );
        assert_eq!(
            infrastructure_stop_reason(
                ProcessStopReason::Cancelled,
                SessionTransportStop::PeerDisconnected,
            ),
            StopReasonV1::Cancelled,
        );
    }
}
