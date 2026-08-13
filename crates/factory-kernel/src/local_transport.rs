//! Unix-only local transport for the resident kernel.
//!
//! The transport owns two distinct paths. The operator path is a `0600` Unix
//! socket below the runtime root. Actor hosts instead receive one end of a
//! daemon-created connected socket pair, while the server end retains an
//! [`ActorConnectionBinding`] that cannot occur in actor JSON. No TCP or HTTP
//! surface exists here. The actor server handles a request to completion before
//! reading another one, which makes the one-in-flight invariant structural.

use std::{
    fs::{self, File},
    io::{self, Read as _, Write as _},
    os::{
        fd::{AsRawFd, RawFd},
        unix::{
            fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _},
            net::UnixStream as StdUnixStream,
        },
    },
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use factory_protocol::{
    ApplicationRevisionId, ApplicationRevisionReceiptResponse, ApplicationShowResponse,
    ArchitectDecideCandidateRequest, ArchitectDecisionReceiptResponse,
    ArchitectReleaseTicketAttemptRequest, ArchitectSponsorTicketRevisionRequest, AssignmentId,
    AuditShowResponse, CampaignId, CampaignReceiptResponse, CampaignStatusResponse,
    CandidateShowResponse, ConflictResponse, ErrorResponse, FRAME_PREFIX_BYTES,
    ForumCreateThreadRequestV1, ForumCreateTopicRequestV1, ForumListThreadsRequestV1,
    ForumListTopicsRequestV1, ForumPostRequestV1, ForumPostsResponseV1, ForumReadThreadRequestV1,
    ForumSearchRequestV1, ForumSearchResponseV1, ForumThreadsResponseV1, ForumTopicsResponseV1,
    FrameError, Office, OperationReceiptResponse, OperatorApplicationActivateRequest,
    OperatorApplicationRegisterRequest, OperatorApplicationShowRequest,
    OperatorArtifactSealReceiptResponse, OperatorArtifactSealRequest, OperatorAuditShowRequest,
    OperatorCampaignStatusRequest, OperatorCancelCampaignRequest, OperatorCandidateShowRequest,
    OperatorStartCampaignRequest, OperatorStatusRequest, OperatorStatusResponse,
    OperatorTicketListRequest, OperatorTicketShowRequest, PROTOCOL_VERSION_V1,
    REQUEST_FRAME_MAX_BYTES, RESPONSE_FRAME_MAX_BYTES, RoutingEnvelope, SessionId,
    TicketListResponse, TicketShowResponse, decode_frame, decode_json_frame,
    decode_routing_envelope, encode_frame, encode_json_frame,
};
use miniserde::{Serialize, json};
use rustix::{
    fs::{CWD, FlockOperation, Mode, OFlags, flock, openat},
    io::{FdFlags, fcntl_getfd, fcntl_setfd},
};
use smol::{
    Timer, future,
    net::unix::{UnixListener, UnixStream},
    prelude::*,
};
use thiserror::Error;

use crate::{
    application_rpc::{
        ApplicationOperatorRpc, ApplicationOperatorRpcError, OperatorApplicationCapability,
    },
    cas::CasStore,
    decision_store::DecisionStore,
    forum_store::ForumStore,
    operator_artifact_rpc::{
        OperatorArtifactCapability, OperatorArtifactRpc, OperatorArtifactRpcError,
    },
    operator_forum_rpc::{OperatorForumRpc, OperatorForumRpcError},
    operator_navigation::{
        OperatorNavigationCapability, OperatorNavigationRpc, OperatorNavigationRpcError,
    },
    operator_rpc::{
        ArchitectTransitionResolver, CampaignOperatorRpc, CampaignOperatorRpcError,
        OperatorArchitectCapability, OperatorCampaignCapability, OperatorRpc, OperatorRpcError,
    },
    process::ProcessStore,
    session_runtime::ActiveSessionCancellationRegistry,
    storage::{DaemonLock, KernelStore, StoreError},
    ticket_store::TicketStore,
};

/// Runtime-root filename for the advisory filesystem singleton.
pub const RUNTIME_LOCK_FILENAME: &str = "factoryd.lock";
/// Runtime-root filename for the mode-`0600` operator socket.
pub const OPERATOR_SOCKET_FILENAME: &str = "factoryd.operator.sock";
/// A read-only transport status operation independent from Architect commands.
pub const OPERATOR_STATUS_OPERATION: &str = factory_protocol::OP_FACTORYD_STATUS;

const DEFAULT_READ_DEADLINE: Duration = Duration::from_secs(5);
const DEFAULT_OPERATION_DEADLINE: Duration = Duration::from_secs(30);
const DEFAULT_WRITE_DEADLINE: Duration = Duration::from_secs(5);
const MAX_OPERATOR_REQUEST_ID_BYTES: usize = 160;

/// A validated local transport configuration. The database URL deliberately
/// does not belong here: only [`KernelStore::connect`] sees that secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalTransportConfig {
    runtime_root: PathBuf,
    read_deadline: Duration,
    operation_deadline: Duration,
    write_deadline: Duration,
}

impl LocalTransportConfig {
    /// Uses the bounded default read and operation deadlines.
    #[must_use]
    pub fn new(runtime_root: PathBuf) -> Self {
        Self {
            runtime_root,
            read_deadline: DEFAULT_READ_DEADLINE,
            operation_deadline: DEFAULT_OPERATION_DEADLINE,
            write_deadline: DEFAULT_WRITE_DEADLINE,
        }
    }

    /// Replaces both deadlines. Zero duration is rejected by [`Self::validate`]
    /// before a daemon starts serving.
    #[must_use]
    pub fn with_deadlines(mut self, read_deadline: Duration, operation_deadline: Duration) -> Self {
        self.read_deadline = read_deadline;
        self.operation_deadline = operation_deadline;
        self
    }

    /// Sets the bounded response/output write deadline independently from a
    /// command's execution deadline.
    #[must_use]
    pub fn with_write_deadline(mut self, write_deadline: Duration) -> Self {
        self.write_deadline = write_deadline;
        self
    }

    #[must_use]
    pub fn runtime_root(&self) -> &Path {
        &self.runtime_root
    }

    #[must_use]
    pub fn operator_socket_path(&self) -> PathBuf {
        self.runtime_root.join(OPERATOR_SOCKET_FILENAME)
    }

    #[must_use]
    pub const fn read_deadline(&self) -> Duration {
        self.read_deadline
    }

    #[must_use]
    pub const fn operation_deadline(&self) -> Duration {
        self.operation_deadline
    }

    #[must_use]
    pub const fn write_deadline(&self) -> Duration {
        self.write_deadline
    }

    fn validate(&self) -> Result<(), LocalTransportError> {
        if self.runtime_root.as_os_str().is_empty() {
            return Err(LocalTransportError::EmptyRuntimeRoot);
        }
        if self.read_deadline.is_zero() {
            return Err(LocalTransportError::ZeroReadDeadline);
        }
        if self.operation_deadline.is_zero() {
            return Err(LocalTransportError::ZeroOperationDeadline);
        }
        if self.write_deadline.is_zero() {
            return Err(LocalTransportError::ZeroWriteDeadline);
        }
        Ok(())
    }
}

/// Exact durable identity selected by the kernel before it creates an actor
/// descriptor. It is input to daemon-owned socket creation, never an actor
/// payload field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActorConnectionIdentity {
    session_id: SessionId,
    assignment_id: AssignmentId,
    application_revision_id: ApplicationRevisionId,
    campaign_id: CampaignId,
    office: Office,
}

impl ActorConnectionIdentity {
    /// Assignment admission constructs this identity inside the kernel before
    /// any actor descriptor exists. It is deliberately not an application or
    /// actor-facing constructor.
    #[allow(
        dead_code,
        reason = "T5 assignment admission is the first production caller; T3 defines the non-public custody seam before that transition exists"
    )]
    pub(crate) const fn from_admitted_assignment(
        session_id: SessionId,
        assignment_id: AssignmentId,
        application_revision_id: ApplicationRevisionId,
        campaign_id: CampaignId,
        office: Office,
    ) -> Self {
        Self {
            session_id,
            assignment_id,
            application_revision_id,
            campaign_id,
            office,
        }
    }
}

/// Authority capability held only by a daemon-side actor connection.
///
/// Its fields and constructor are private. An actor can receive a connected
/// descriptor but cannot construct this type or replace it by adding JSON
/// fields to a request. The kernel exposes getters only to typed command
/// adapters such as the Forum store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActorConnectionBinding {
    session_id: SessionId,
    assignment_id: AssignmentId,
    application_revision_id: ApplicationRevisionId,
    campaign_id: CampaignId,
    office: Office,
}

impl ActorConnectionBinding {
    pub(crate) fn from_identity(identity: ActorConnectionIdentity) -> Self {
        Self {
            session_id: identity.session_id,
            assignment_id: identity.assignment_id,
            application_revision_id: identity.application_revision_id,
            campaign_id: identity.campaign_id,
            office: identity.office,
        }
    }

    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    #[must_use]
    pub const fn assignment_id(&self) -> AssignmentId {
        self.assignment_id
    }

    #[must_use]
    pub const fn application_revision_id(&self) -> ApplicationRevisionId {
        self.application_revision_id
    }

    #[must_use]
    pub const fn campaign_id(&self) -> CampaignId {
        self.campaign_id
    }

    #[must_use]
    pub const fn office(&self) -> Office {
        self.office
    }
}

/// One actor message accepted from the server end of a socket pair. The raw
/// complete frame remains available for the operation-specific parser, while
/// the authoritative binding remains outside its JSON bytes.
#[derive(Clone, Debug)]
pub struct BoundActorFrame {
    binding: ActorConnectionBinding,
    envelope: RoutingEnvelope,
    frame: Vec<u8>,
}

impl BoundActorFrame {
    #[must_use]
    pub const fn binding(&self) -> &ActorConnectionBinding {
        &self.binding
    }

    #[must_use]
    pub fn envelope(&self) -> &RoutingEnvelope {
        &self.envelope
    }

    /// Returns the exact one-frame bytes for the closed operation parser.
    #[must_use]
    pub fn frame(&self) -> &[u8] {
        &self.frame
    }
}

/// The actor end of a daemon-created connected descriptor. A later process
/// custody tranche passes this already-connected file descriptor to Deno; it
/// never gives an actor a listener path or database URL.
#[derive(Debug)]
pub struct ActorClientDescriptor {
    stream: StdUnixStream,
}

impl ActorClientDescriptor {
    /// The descriptor number to inherit into the one assigned actor host.
    #[must_use]
    pub fn as_raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    /// Transfers the connected descriptor to the daemon-owned child launcher.
    #[must_use]
    pub fn into_std_stream(self) -> StdUnixStream {
        self.stream
    }
}

/// The daemon end of one actor socketpair. It reports disconnects to the
/// process-custody layer instead of treating a dropped socket as success.
#[derive(Debug)]
pub struct ActorServerConnection {
    stream: UnixStream,
    binding: ActorConnectionBinding,
    config: LocalTransportConfig,
}

/// The daemon end of a socketpair before session admission.  It has no actor
/// identity yet; the runtime binds the identity only after the durable
/// `StartSession` transition commits.
#[derive(Debug)]
pub(crate) struct UnboundActorServerConnection {
    stream: UnixStream,
    config: LocalTransportConfig,
}

impl UnboundActorServerConnection {
    pub(crate) fn bind(self, identity: ActorConnectionIdentity) -> ActorServerConnection {
        ActorServerConnection {
            stream: self.stream,
            binding: ActorConnectionBinding::from_identity(identity),
            config: self.config,
        }
    }
}

impl ActorServerConnection {
    /// Replaces the short operator/frame idle bound with the admitted
    /// assignment's wall limit. Actor connections are intentionally idle
    /// while Deno starts and while a model reasons; process custody, not the
    /// operator socket timeout, owns that full-session wall bound.
    pub(crate) fn with_assignment_read_deadline(
        mut self,
        read_deadline: Duration,
    ) -> Result<Self, LocalTransportError> {
        if read_deadline.is_zero() {
            return Err(LocalTransportError::ZeroReadDeadline);
        }
        self.config.read_deadline = read_deadline;
        Ok(self)
    }

    /// Replaces the generic per-operation transport bound with the admitted
    /// assignment's wall limit. Some actor operations deliberately run a
    /// controller-custodied command (for example a candidate checkpoint),
    /// whose narrower command profile enforces its own execution limit. The
    /// assignment wall limit remains the outer deadline for that RPC and for
    /// the supervised actor process, so the daemon's short operator default
    /// cannot abort an otherwise admitted command.
    pub(crate) fn with_assignment_operation_deadline(
        mut self,
        operation_deadline: Duration,
    ) -> Result<Self, LocalTransportError> {
        if operation_deadline.is_zero() {
            return Err(LocalTransportError::ZeroOperationDeadline);
        }
        self.config.operation_deadline = operation_deadline;
        Ok(self)
    }

    /// Returns the opaque capability retained by this daemon-side connection.
    /// A caller can obtain it only after [`LocalDaemon`] has created the
    /// connected descriptor and bound the identity to its server end.
    #[must_use]
    pub const fn binding(&self) -> ActorConnectionBinding {
        self.binding
    }

    /// Writes the one newline-delimited startup attestation consumed by the
    /// inherited actor descriptor.  It is intentionally separate from the
    /// framed request loop: the child must receive this gate before it can
    /// construct a model session, while later actor RPCs use framed JSON.
    pub(crate) async fn send_session_admission_line(
        &mut self,
        line: &[u8],
    ) -> Result<(), LocalTransportError> {
        const MAX_LINE_BYTES: usize = RESPONSE_FRAME_MAX_BYTES - 1;
        if line.is_empty() || line.len() > MAX_LINE_BYTES || line.last() != Some(&b'\n') {
            return Err(LocalTransportError::Frame(FrameError::Oversized {
                actual: line.len(),
                maximum: MAX_LINE_BYTES,
            }));
        }
        with_write_deadline(self.config.write_deadline, self.stream.write_all(line)).await
    }

    /// Creates the one required-read ledger bound to this exact actor
    /// connection. The actor receives only the peer descriptor and cannot
    /// call this constructor or substitute another session identity in JSON.
    pub fn workspace_read_authority(
        &self,
        workspace_root: &Path,
        expected_manifest_artifact_id: factory_protocol::ArtifactId,
        required: Vec<factory_protocol::ReadExactFileV1>,
    ) -> Result<
        crate::workspace_read::WorkspaceReadAuthority,
        crate::workspace_read::WorkspaceReadError,
    > {
        crate::workspace_read::WorkspaceReadAuthority::from_admitted_assignment(
            self.binding,
            workspace_root,
            expected_manifest_artifact_id,
            required,
        )
    }

    /// Serves one actor connection sequentially. The dispatcher returns the
    /// operation-specific, already-serialized UTF-8 JSON response; it is
    /// awaited before the next request read, so exactly one request can be in
    /// flight. Frame trailing-byte rejection applies to complete in-memory
    /// frames; on a stream a following frame is the next sequential request,
    /// never suffix data for the prior frame.
    pub async fn serve<F, Fut>(
        mut self,
        mut dispatch: F,
    ) -> Result<ActorDisconnect, LocalTransportError>
    where
        F: FnMut(BoundActorFrame) -> Fut,
        Fut: core::future::Future<Output = Result<Vec<u8>, LocalTransportError>>,
    {
        loop {
            let Some(frame) = read_stream_frame(
                &mut self.stream,
                REQUEST_FRAME_MAX_BYTES,
                self.config.read_deadline,
            )
            .await?
            else {
                return Ok(ActorDisconnect::PeerClosed);
            };
            let envelope = decode_routing_envelope(&frame, REQUEST_FRAME_MAX_BYTES)?;
            let request = BoundActorFrame {
                binding: self.binding,
                envelope,
                frame,
            };
            let response =
                with_operation_deadline(self.config.operation_deadline, dispatch(request)).await?;
            validate_response_json(&response)?;
            write_stream_frame(
                &mut self.stream,
                &response,
                RESPONSE_FRAME_MAX_BYTES,
                self.config.write_deadline,
            )
            .await?;
        }
    }
}

/// A signal to T5 process supervision that an actor-host liveness channel has
/// ended. A disconnect is never an implicit command acknowledgement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActorDisconnect {
    PeerClosed,
}

/// Operator client. It receives only an explicit socket path and cannot open a
/// database connection. Architect mutation methods are typed separately from
/// actor protocol calls and can therefore be constructed only by the local
/// Grand Architect client, never by a Pi host SDK facade.
#[derive(Clone, Debug)]
pub struct OperatorClient {
    socket_path: PathBuf,
    read_deadline: Duration,
    write_deadline: Duration,
}

impl OperatorClient {
    #[must_use]
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            read_deadline: DEFAULT_READ_DEADLINE,
            write_deadline: DEFAULT_WRITE_DEADLINE,
        }
    }

    #[must_use]
    pub fn with_read_deadline(mut self, read_deadline: Duration) -> Self {
        self.read_deadline = read_deadline;
        self
    }

    #[must_use]
    pub fn with_write_deadline(mut self, write_deadline: Duration) -> Self {
        self.write_deadline = write_deadline;
        self
    }

    /// Performs one framed, typed, read-only daemon status exchange.
    pub async fn probe(
        &self,
        request_id: String,
    ) -> Result<OperatorStatusResponse, LocalTransportError> {
        validate_request_id(&request_id)?;
        if self.read_deadline.is_zero() {
            return Err(LocalTransportError::ZeroReadDeadline);
        }
        if self.write_deadline.is_zero() {
            return Err(LocalTransportError::ZeroWriteDeadline);
        }
        let mut stream = UnixStream::connect(&self.socket_path).await?;
        let request = OperatorStatusRequest {
            protocol_version: PROTOCOL_VERSION_V1,
            request_id,
            operation: OPERATOR_STATUS_OPERATION.to_owned(),
        };
        let expected_request_id = request.request_id.clone();
        let frame = encode_json_frame(&request, REQUEST_FRAME_MAX_BYTES)?;
        write_frame_bytes(&mut stream, &frame, self.write_deadline).await?;
        let response = read_stream_frame(&mut stream, RESPONSE_FRAME_MAX_BYTES, self.read_deadline)
            .await?
            .ok_or(LocalTransportError::ResponseDisconnected)?;
        let response: OperatorStatusResponse = decode_json_frame(
            &response,
            RESPONSE_FRAME_MAX_BYTES,
            OPERATOR_STATUS_OPERATION,
        )?;
        if response.protocol_version != PROTOCOL_VERSION_V1 {
            return Err(LocalTransportError::UnsupportedOperatorProtocol(
                response.protocol_version,
            ));
        }
        if response.operation != OPERATOR_STATUS_OPERATION {
            return Err(LocalTransportError::UnexpectedOperatorOperation {
                actual: response.operation,
            });
        }
        if response.request_id != expected_request_id {
            return Err(LocalTransportError::OperatorRequestIdMismatch);
        }
        if response.state != "ready" {
            return Err(LocalTransportError::UnexpectedOperatorState {
                actual: response.state,
            });
        }
        Ok(response)
    }

    /// Sends one external sponsorship command over the authenticated operator
    /// socket. Durable idempotency and the ticket revision guard remain in
    /// `DecisionStore`; this client has no PostgreSQL access.
    pub async fn sponsor_ticket_revision(
        &self,
        request: ArchitectSponsorTicketRevisionRequest,
    ) -> Result<ArchitectDecisionReceiptResponse, LocalTransportError> {
        self.architect_exchange(
            &request,
            factory_protocol::OP_ARCHITECT_SPONSOR_TICKET_REVISION,
        )
        .await
    }

    /// Sends an explicit release request. A daemon without a trusted
    /// current-head resolver returns a typed `architect_transition_unavailable`
    /// rejection before it can reach durable mutation authority.
    pub async fn release_ticket_attempt(
        &self,
        request: ArchitectReleaseTicketAttemptRequest,
    ) -> Result<ArchitectDecisionReceiptResponse, LocalTransportError> {
        self.architect_exchange(
            &request,
            factory_protocol::OP_ARCHITECT_RELEASE_TICKET_ATTEMPT,
        )
        .await
    }

    /// Sends one final Architect candidate decision. Hard validations remain
    /// non-overridable kernel checks even when this request links a rejected
    /// Quality review as its only allowed qualitative override.
    pub async fn decide_candidate(
        &self,
        request: ArchitectDecideCandidateRequest,
    ) -> Result<ArchitectDecisionReceiptResponse, LocalTransportError> {
        self.architect_exchange(&request, factory_protocol::OP_ARCHITECT_DECIDE_CANDIDATE)
            .await
    }

    /// Starts one campaign through the authenticated local socket. The request
    /// intentionally has no build or repository field: the daemon resolves
    /// and returns those immutable pins under trusted database authority.
    pub async fn start_campaign(
        &self,
        request: OperatorStartCampaignRequest,
    ) -> Result<CampaignReceiptResponse, LocalTransportError> {
        self.campaign_mutation_exchange(&request, factory_protocol::OP_OPERATOR_START_CAMPAIGN)
            .await
    }

    /// Reads one bounded campaign/buffer/cost projection without a durable
    /// status receipt or any PostgreSQL write.
    pub async fn campaign_status(
        &self,
        request: OperatorCampaignStatusRequest,
    ) -> Result<CampaignStatusResponse, LocalTransportError> {
        self.campaign_status_exchange(&request).await
    }

    /// Cancels one campaign under its observed campaign revision. A retried
    /// command returns the original pinned identity receipt.
    pub async fn cancel_campaign(
        &self,
        request: OperatorCancelCampaignRequest,
    ) -> Result<CampaignReceiptResponse, LocalTransportError> {
        self.campaign_mutation_exchange(&request, factory_protocol::OP_OPERATOR_CANCEL_CAMPAIGN)
            .await
    }

    /// Reads one admitted generic application revision. This operation is
    /// deliberately status-only: it does not create a polling or audit row.
    pub async fn show_application(
        &self,
        request: OperatorApplicationShowRequest,
    ) -> Result<ApplicationShowResponse, LocalTransportError> {
        self.application_exchange(&request, factory_protocol::OP_OPERATOR_SHOW_APPLICATION)
            .await
    }

    /// Requests daemon/CAS admission of a compiled bundle from a local source
    /// root. The client sends paths and closed expected identities, never
    /// bundle or template bytes.
    pub async fn register_application(
        &self,
        request: OperatorApplicationRegisterRequest,
    ) -> Result<ApplicationRevisionReceiptResponse, LocalTransportError> {
        self.application_exchange(&request, factory_protocol::OP_OPERATOR_REGISTER_APPLICATION)
            .await
    }

    /// Makes one already-admitted application revision active between
    /// campaigns under the Grand Architect's local socket capability.
    pub async fn activate_application(
        &self,
        request: OperatorApplicationActivateRequest,
    ) -> Result<ApplicationRevisionReceiptResponse, LocalTransportError> {
        self.application_exchange(&request, factory_protocol::OP_OPERATOR_ACTIVATE_APPLICATION)
            .await
    }

    /// Adopts exactly one regular evidence file through daemon-owned CAS and
    /// returns the ordinary immutable artifact-registration audit receipt.
    pub async fn seal_operator_artifact(
        &self,
        request: OperatorArtifactSealRequest,
    ) -> Result<OperatorArtifactSealReceiptResponse, LocalTransportError> {
        self.application_exchange(&request, factory_protocol::OP_OPERATOR_SEAL_ARTIFACT)
            .await
    }

    /// Lists no more than twenty current ticket revisions through the local
    /// socket. State filtering is a closed lifecycle spelling, not SQL.
    pub async fn list_tickets(
        &self,
        request: OperatorTicketListRequest,
    ) -> Result<TicketListResponse, LocalTransportError> {
        self.application_exchange(&request, factory_protocol::OP_OPERATOR_LIST_TICKETS)
            .await
    }

    pub async fn show_ticket(
        &self,
        request: OperatorTicketShowRequest,
    ) -> Result<TicketShowResponse, LocalTransportError> {
        self.application_exchange(&request, factory_protocol::OP_OPERATOR_SHOW_TICKET)
            .await
    }

    pub async fn show_candidate(
        &self,
        request: OperatorCandidateShowRequest,
    ) -> Result<CandidateShowResponse, LocalTransportError> {
        self.application_exchange(&request, factory_protocol::OP_OPERATOR_SHOW_CANDIDATE)
            .await
    }

    pub async fn show_audit(
        &self,
        request: OperatorAuditShowRequest,
    ) -> Result<AuditShowResponse, LocalTransportError> {
        self.application_exchange(&request, factory_protocol::OP_OPERATOR_SHOW_AUDIT)
            .await
    }

    /// Browses permanent Forum state through the same local operator socket.
    /// Read methods never create a receipt; mutation attribution is supplied
    /// solely by daemon-side operator capability.
    pub async fn forum_list_topics(
        &self,
        request: ForumListTopicsRequestV1,
    ) -> Result<ForumTopicsResponseV1, LocalTransportError> {
        self.application_exchange(&request, factory_protocol::OP_FORUM_LIST_TOPICS)
            .await
    }
    pub async fn forum_list_threads(
        &self,
        request: ForumListThreadsRequestV1,
    ) -> Result<ForumThreadsResponseV1, LocalTransportError> {
        self.application_exchange(&request, factory_protocol::OP_FORUM_LIST_THREADS)
            .await
    }
    pub async fn forum_read_thread(
        &self,
        request: ForumReadThreadRequestV1,
    ) -> Result<ForumPostsResponseV1, LocalTransportError> {
        self.application_exchange(&request, factory_protocol::OP_FORUM_READ_THREAD)
            .await
    }
    pub async fn forum_search(
        &self,
        request: ForumSearchRequestV1,
    ) -> Result<ForumSearchResponseV1, LocalTransportError> {
        self.application_exchange(&request, factory_protocol::OP_FORUM_SEARCH)
            .await
    }
    pub async fn forum_create_topic(
        &self,
        request: ForumCreateTopicRequestV1,
    ) -> Result<OperationReceiptResponse, LocalTransportError> {
        self.application_exchange(&request, factory_protocol::OP_FORUM_CREATE_TOPIC)
            .await
    }
    pub async fn forum_create_thread(
        &self,
        request: ForumCreateThreadRequestV1,
    ) -> Result<OperationReceiptResponse, LocalTransportError> {
        self.application_exchange(&request, factory_protocol::OP_FORUM_CREATE_THREAD)
            .await
    }
    pub async fn forum_post(
        &self,
        request: ForumPostRequestV1,
    ) -> Result<OperationReceiptResponse, LocalTransportError> {
        self.application_exchange(&request, factory_protocol::OP_FORUM_POST)
            .await
    }

    async fn architect_exchange<T: Serialize>(
        &self,
        request: &T,
        expected_operation: &'static str,
    ) -> Result<ArchitectDecisionReceiptResponse, LocalTransportError> {
        if self.read_deadline.is_zero() {
            return Err(LocalTransportError::ZeroReadDeadline);
        }
        if self.write_deadline.is_zero() {
            return Err(LocalTransportError::ZeroWriteDeadline);
        }
        let request_json = json::to_string(request);
        let envelope: RoutingEnvelope = json::from_str(&request_json).map_err(|error| {
            LocalTransportError::Frame(FrameError::InvalidJson {
                operation: expected_operation,
                detail: format!("{error:?}"),
            })
        })?;
        validate_request_id(&envelope.request_id)?;
        if envelope.protocol_version != PROTOCOL_VERSION_V1 {
            return Err(LocalTransportError::UnsupportedOperatorProtocol(
                envelope.protocol_version,
            ));
        }
        if envelope.operation != expected_operation {
            return Err(LocalTransportError::UnexpectedOperatorOperation {
                actual: envelope.operation,
            });
        }
        let mut stream = UnixStream::connect(&self.socket_path).await?;
        let frame = encode_json_frame(request, REQUEST_FRAME_MAX_BYTES)?;
        write_frame_bytes(&mut stream, &frame, self.write_deadline).await?;
        let response = read_stream_frame(&mut stream, RESPONSE_FRAME_MAX_BYTES, self.read_deadline)
            .await?
            .ok_or(LocalTransportError::ResponseDisconnected)?;
        match decode_json_frame::<ArchitectDecisionReceiptResponse>(
            &response,
            RESPONSE_FRAME_MAX_BYTES,
            expected_operation,
        ) {
            Ok(success) => {
                validate_architect_response_identity(
                    success.protocol_version,
                    &success.request_id,
                    &success.operation,
                    &envelope.request_id,
                    expected_operation,
                )?;
                Ok(success)
            }
            Err(success_parse_error) => {
                if let Ok(conflict) = decode_json_frame::<ConflictResponse>(
                    &response,
                    RESPONSE_FRAME_MAX_BYTES,
                    expected_operation,
                ) {
                    validate_architect_response_identity(
                        conflict.protocol_version,
                        &conflict.request_id,
                        &conflict.operation,
                        &envelope.request_id,
                        expected_operation,
                    )?;
                    return Err(LocalTransportError::OperatorCommandRejected {
                        operation: conflict.operation,
                        error_code: conflict.error_code,
                        message: conflict.message,
                    });
                }
                if let Ok(error) = decode_json_frame::<ErrorResponse>(
                    &response,
                    RESPONSE_FRAME_MAX_BYTES,
                    expected_operation,
                ) {
                    validate_architect_response_identity(
                        error.protocol_version,
                        &error.request_id,
                        &error.operation,
                        &envelope.request_id,
                        expected_operation,
                    )?;
                    return Err(LocalTransportError::OperatorCommandRejected {
                        operation: error.operation,
                        error_code: error.error_code,
                        message: error.message,
                    });
                }
                Err(LocalTransportError::Frame(success_parse_error))
            }
        }
    }

    async fn campaign_mutation_exchange<T: Serialize>(
        &self,
        request: &T,
        expected_operation: &'static str,
    ) -> Result<CampaignReceiptResponse, LocalTransportError> {
        self.campaign_exchange(request, expected_operation).await
    }

    async fn campaign_status_exchange(
        &self,
        request: &OperatorCampaignStatusRequest,
    ) -> Result<CampaignStatusResponse, LocalTransportError> {
        self.campaign_exchange(request, factory_protocol::OP_OPERATOR_CAMPAIGN_STATUS)
            .await
    }

    async fn campaign_exchange<T, Response>(
        &self,
        request: &T,
        expected_operation: &'static str,
    ) -> Result<Response, LocalTransportError>
    where
        T: Serialize,
        Response: miniserde::Deserialize + miniserde::Serialize,
    {
        if self.read_deadline.is_zero() {
            return Err(LocalTransportError::ZeroReadDeadline);
        }
        if self.write_deadline.is_zero() {
            return Err(LocalTransportError::ZeroWriteDeadline);
        }
        let request_json = json::to_string(request);
        let envelope: RoutingEnvelope = json::from_str(&request_json).map_err(|error| {
            LocalTransportError::Frame(FrameError::InvalidJson {
                operation: expected_operation,
                detail: format!("{error:?}"),
            })
        })?;
        validate_request_id(&envelope.request_id)?;
        if envelope.protocol_version != PROTOCOL_VERSION_V1 {
            return Err(LocalTransportError::UnsupportedOperatorProtocol(
                envelope.protocol_version,
            ));
        }
        if envelope.operation != expected_operation {
            return Err(LocalTransportError::UnexpectedOperatorOperation {
                actual: envelope.operation,
            });
        }
        let mut stream = UnixStream::connect(&self.socket_path).await?;
        let frame = encode_json_frame(request, REQUEST_FRAME_MAX_BYTES)?;
        write_frame_bytes(&mut stream, &frame, self.write_deadline).await?;
        let response = read_stream_frame(&mut stream, RESPONSE_FRAME_MAX_BYTES, self.read_deadline)
            .await?
            .ok_or(LocalTransportError::ResponseDisconnected)?;
        match decode_json_frame::<Response>(&response, RESPONSE_FRAME_MAX_BYTES, expected_operation)
        {
            Ok(success) => {
                let response_json = json::to_string(&success);
                let routing: RoutingEnvelope = json::from_str(&response_json).map_err(|error| {
                    LocalTransportError::Frame(FrameError::InvalidJson {
                        operation: expected_operation,
                        detail: format!("{error:?}"),
                    })
                })?;
                validate_architect_response_identity(
                    routing.protocol_version,
                    &routing.request_id,
                    &routing.operation,
                    &envelope.request_id,
                    expected_operation,
                )?;
                Ok(success)
            }
            Err(success_parse_error) => {
                if let Ok(conflict) = decode_json_frame::<ConflictResponse>(
                    &response,
                    RESPONSE_FRAME_MAX_BYTES,
                    expected_operation,
                ) {
                    validate_architect_response_identity(
                        conflict.protocol_version,
                        &conflict.request_id,
                        &conflict.operation,
                        &envelope.request_id,
                        expected_operation,
                    )?;
                    return Err(LocalTransportError::OperatorCommandRejected {
                        operation: conflict.operation,
                        error_code: conflict.error_code,
                        message: conflict.message,
                    });
                }
                if let Ok(error) = decode_json_frame::<ErrorResponse>(
                    &response,
                    RESPONSE_FRAME_MAX_BYTES,
                    expected_operation,
                ) {
                    validate_architect_response_identity(
                        error.protocol_version,
                        &error.request_id,
                        &error.operation,
                        &envelope.request_id,
                        expected_operation,
                    )?;
                    return Err(LocalTransportError::OperatorCommandRejected {
                        operation: error.operation,
                        error_code: error.error_code,
                        message: error.message,
                    });
                }
                Err(LocalTransportError::Frame(success_parse_error))
            }
        }
    }

    async fn application_exchange<T, Response>(
        &self,
        request: &T,
        expected_operation: &'static str,
    ) -> Result<Response, LocalTransportError>
    where
        T: Serialize,
        Response: miniserde::Deserialize + miniserde::Serialize,
    {
        if self.read_deadline.is_zero() {
            return Err(LocalTransportError::ZeroReadDeadline);
        }
        if self.write_deadline.is_zero() {
            return Err(LocalTransportError::ZeroWriteDeadline);
        }
        let request_json = json::to_string(request);
        let envelope: RoutingEnvelope = json::from_str(&request_json).map_err(|error| {
            LocalTransportError::Frame(FrameError::InvalidJson {
                operation: expected_operation,
                detail: format!("{error:?}"),
            })
        })?;
        validate_request_id(&envelope.request_id)?;
        if envelope.protocol_version != PROTOCOL_VERSION_V1 {
            return Err(LocalTransportError::UnsupportedOperatorProtocol(
                envelope.protocol_version,
            ));
        }
        if envelope.operation != expected_operation {
            return Err(LocalTransportError::UnexpectedOperatorOperation {
                actual: envelope.operation,
            });
        }
        let mut stream = UnixStream::connect(&self.socket_path).await?;
        let frame = encode_json_frame(request, REQUEST_FRAME_MAX_BYTES)?;
        write_frame_bytes(&mut stream, &frame, self.write_deadline).await?;
        let response = read_stream_frame(&mut stream, RESPONSE_FRAME_MAX_BYTES, self.read_deadline)
            .await?
            .ok_or(LocalTransportError::ResponseDisconnected)?;
        match decode_json_frame::<Response>(&response, RESPONSE_FRAME_MAX_BYTES, expected_operation)
        {
            Ok(success) => {
                let response_json = json::to_string(&success);
                let routing: RoutingEnvelope = json::from_str(&response_json).map_err(|error| {
                    LocalTransportError::Frame(FrameError::InvalidJson {
                        operation: expected_operation,
                        detail: format!("{error:?}"),
                    })
                })?;
                validate_architect_response_identity(
                    routing.protocol_version,
                    &routing.request_id,
                    &routing.operation,
                    &envelope.request_id,
                    expected_operation,
                )?;
                Ok(success)
            }
            Err(success_parse_error) => {
                if let Ok(conflict) = decode_json_frame::<ConflictResponse>(
                    &response,
                    RESPONSE_FRAME_MAX_BYTES,
                    expected_operation,
                ) {
                    validate_architect_response_identity(
                        conflict.protocol_version,
                        &conflict.request_id,
                        &conflict.operation,
                        &envelope.request_id,
                        expected_operation,
                    )?;
                    return Err(LocalTransportError::OperatorCommandRejected {
                        operation: conflict.operation,
                        error_code: conflict.error_code,
                        message: conflict.message,
                    });
                }
                if let Ok(error) = decode_json_frame::<ErrorResponse>(
                    &response,
                    RESPONSE_FRAME_MAX_BYTES,
                    expected_operation,
                ) {
                    validate_architect_response_identity(
                        error.protocol_version,
                        &error.request_id,
                        &error.operation,
                        &envelope.request_id,
                        expected_operation,
                    )?;
                    return Err(LocalTransportError::OperatorCommandRejected {
                        operation: error.operation,
                        error_code: error.error_code,
                        message: error.message,
                    });
                }
                Err(LocalTransportError::Frame(success_parse_error))
            }
        }
    }
}

/// A fully locked resident daemon. Construction acquires the runtime-root
/// filesystem singleton first, then the PostgreSQL advisory singleton; only a
/// value holding both can accept operator or create actor connections.
#[derive(Debug)]
pub struct LocalDaemon {
    runtime: RuntimeSocket,
    daemon_lock: Option<DaemonLock>,
    status_store: KernelStore,
    operator_rpc: Option<OperatorRpc>,
    campaign_rpc: Option<CampaignOperatorRpc>,
    application_rpc: Option<ApplicationOperatorRpc>,
    navigation_rpc: Option<OperatorNavigationRpc>,
    artifact_rpc: Option<OperatorArtifactRpc>,
    forum_rpc: Option<OperatorForumRpc>,
    active_sessions: ActiveSessionCancellationRegistry,
}

impl LocalDaemon {
    /// Binds the Unix-only runtime and PostgreSQL singleton before exposing a
    /// serving value. On any PostgreSQL failure the filesystem listener and
    /// lock are dropped without deleting an unowned path.
    pub async fn bind(
        config: LocalTransportConfig,
        store: &KernelStore,
    ) -> Result<Self, LocalTransportError> {
        let runtime = RuntimeSocket::bind(config)?;
        let daemon_lock = store.acquire_daemon_lock().await?;
        Ok(Self {
            runtime,
            daemon_lock: Some(daemon_lock),
            status_store: store.clone(),
            operator_rpc: None,
            campaign_rpc: None,
            application_rpc: None,
            navigation_rpc: None,
            artifact_rpc: None,
            forum_rpc: None,
            active_sessions: ActiveSessionCancellationRegistry::default(),
        })
    }

    #[must_use]
    pub fn operator_socket_path(&self) -> &Path {
        self.runtime.socket_path()
    }

    /// Enables only the Grand Architect decision family on this daemon's
    /// already-bound mode-`0600` operator socket. The caller supplies the
    /// narrow durable store, never a raw SQL pool; this method mints the
    /// unconstructible socket capability inside the kernel transport.
    #[must_use]
    pub fn with_architect_control(mut self, decisions: DecisionStore) -> Self {
        self.operator_rpc = Some(OperatorRpc::from_operator_transport(
            OperatorArchitectCapability::from_operator_transport(),
            decisions,
        ));
        self
    }

    /// Enables the deliberately narrow campaign start/status/cancel family on
    /// the already-bound local operator socket. The stores encapsulate their
    /// pools; neither this transport nor factoryctl receives a database URL.
    #[must_use]
    pub fn with_campaign_control(mut self, process: ProcessStore, tickets: TicketStore) -> Self {
        self.campaign_rpc = Some(CampaignOperatorRpc::from_operator_transport(
            OperatorCampaignCapability::from_operator_transport(),
            process,
            tickets,
            self.active_sessions.clone(),
        ));
        self
    }

    pub(crate) fn active_session_cancellations(&self) -> ActiveSessionCancellationRegistry {
        self.active_sessions.clone()
    }

    /// Enables generic application inspection, registration, and activation
    /// on the existing mode-`0600` operator socket. The daemon keeps the CAS
    /// custody object, so source bytes never cross factoryctl's boundary.
    #[must_use]
    pub fn with_application_control(mut self, store: KernelStore, cas: Arc<CasStore>) -> Self {
        self.application_rpc = Some(ApplicationOperatorRpc::from_operator_transport(
            OperatorApplicationCapability::from_operator_transport(),
            store,
            cas,
        ));
        self
    }

    /// Enables only the named read-only ticket/candidate/audit projections.
    /// The daemon retains the concrete PostgreSQL owner; callers receive no
    /// pool, database URL, or arbitrary query capability.
    #[must_use]
    pub fn with_navigation_control(mut self, store: KernelStore) -> Self {
        self.navigation_rpc = Some(OperatorNavigationRpc::from_operator_transport(
            OperatorNavigationCapability::from_operator_transport(),
            store,
        ));
        self
    }

    /// Enables one bounded operator evidence-file adoption operation on the
    /// existing mode-`0600` socket. The daemon alone keeps CAS custody and
    /// turns the sealed object into an ordinary immutable artifact receipt.
    #[must_use]
    pub fn with_operator_artifact_control(
        mut self,
        store: KernelStore,
        cas: Arc<CasStore>,
    ) -> Self {
        self.artifact_rpc = Some(OperatorArtifactRpc::from_operator_transport(
            OperatorArtifactCapability::from_operator_transport(),
            store,
            cas,
        ));
        self
    }

    /// Enables the seven fixed Forum operations under the kernel-minted Grand
    /// Architect capability. The operator wire contains no office/session
    /// identity and therefore cannot forge actor attribution.
    #[must_use]
    pub fn with_forum_control(mut self, forum: ForumStore) -> Self {
        self.forum_rpc = Some(OperatorForumRpc::from_operator_transport(forum));
        self
    }

    /// Composes trusted resolver inputs for release/final decisions. The
    /// resolver must use kernel-owned reads and command runners, never actor
    /// payload fields. Sponsorship does not need this seam.
    #[must_use]
    pub fn with_architect_transition_resolver(
        mut self,
        resolver: Arc<dyn ArchitectTransitionResolver>,
    ) -> Self {
        if let Some(router) = self.operator_rpc.take() {
            self.operator_rpc = Some(router.with_transition_resolver(resolver));
        }
        self
    }

    /// Creates one daemon-bound actor socketpair. T5 passes the returned client
    /// descriptor to its one owned Deno process and serves the returned server
    /// connection under process custody.
    pub fn create_actor_socketpair(
        &self,
        identity: ActorConnectionIdentity,
    ) -> Result<(ActorClientDescriptor, ActorServerConnection), LocalTransportError> {
        let (client, server) = StdUnixStream::pair()?;
        // `UnixStream::pair` is commonly created close-on-exec. The exact
        // client descriptor must survive the daemon's owned child exec so the
        // Deno host receives the already-connected liveness channel, never a
        // path it can reconnect to with another identity.
        let client_flags = fcntl_getfd(&client).map_err(io::Error::other)?;
        fcntl_setfd(&client, client_flags & !FdFlags::CLOEXEC).map_err(io::Error::other)?;
        let server = UnixStream::try_from(server)?;
        Ok((
            ActorClientDescriptor { stream: client },
            ActorServerConnection {
                stream: server,
                binding: ActorConnectionBinding::from_identity(identity),
                config: self.runtime.config.clone(),
            },
        ))
    }

    /// Creates the connected descriptors before an actor identity exists.
    /// This is crate-private so callers cannot fabricate a public binding;
    /// `session_runtime` binds the returned server end only after PostgreSQL
    /// accepts the process custody transition.
    pub(crate) fn create_unbound_actor_socketpair(
        &self,
    ) -> Result<(ActorClientDescriptor, UnboundActorServerConnection), LocalTransportError> {
        let (client, server) = StdUnixStream::pair()?;
        let client_flags = fcntl_getfd(&client).map_err(io::Error::other)?;
        fcntl_setfd(&client, client_flags & !FdFlags::CLOEXEC).map_err(io::Error::other)?;
        let server = UnixStream::try_from(server)?;
        Ok((
            ActorClientDescriptor { stream: client },
            UnboundActorServerConnection {
                stream: server,
                config: self.runtime.config.clone(),
            },
        ))
    }

    /// Reconstitutes an actor binding from a running session rather than from
    /// caller-provided identity fields, then creates its connected pair.
    pub async fn create_admitted_actor_socketpair(
        &self,
        process: &crate::process::ProcessStore,
        session_id: SessionId,
        packet: &factory_protocol::AssignmentPacketV1,
    ) -> Result<(ActorClientDescriptor, ActorServerConnection), LocalTransportError> {
        let identity = process
            .actor_connection_identity(session_id, packet)
            .await?;
        self.create_actor_socketpair(identity)
    }

    /// Runs the resident operator listener forever. Each connection performs
    /// one bounded status or explicit Architect exchange; actor connections
    /// are supplied only as daemon-created socketpairs instead.
    pub async fn serve(&self) -> Result<(), LocalTransportError> {
        loop {
            let (stream, _) = self.runtime.listener.accept().await?;
            let deadline = self.runtime.config.read_deadline;
            let operation_deadline = self.runtime.config.operation_deadline;
            let write_deadline = self.runtime.config.write_deadline;
            let router = self.operator_rpc.clone();
            let campaign_router = self.campaign_rpc.clone();
            let application_router = self.application_rpc.clone();
            let navigation_router = self.navigation_rpc.clone();
            let artifact_router = self.artifact_rpc.clone();
            let forum_router = self.forum_rpc.clone();
            let status_store = self.status_store.clone();
            smol::spawn(async move {
                if let Err(error) = serve_operator_connection(
                    stream,
                    deadline,
                    operation_deadline,
                    write_deadline,
                    Some(status_store),
                    router,
                    campaign_router,
                    application_router,
                    navigation_router,
                    artifact_router,
                    forum_router,
                )
                .await
                {
                    tracing::debug!(%error, "operator socket request rejected");
                }
            })
            .detach();
        }
    }

    /// Accepts and serves one operator connection. This makes the daemon path
    /// provider-free and integration-testable without an unbounded task.
    pub async fn serve_one_operator(&self) -> Result<(), LocalTransportError> {
        let (stream, _) = self.runtime.listener.accept().await?;
        serve_operator_connection(
            stream,
            self.runtime.config.read_deadline,
            self.runtime.config.operation_deadline,
            self.runtime.config.write_deadline,
            Some(self.status_store.clone()),
            self.operator_rpc.clone(),
            self.campaign_rpc.clone(),
            self.application_rpc.clone(),
            self.navigation_rpc.clone(),
            self.artifact_rpc.clone(),
            self.forum_rpc.clone(),
        )
        .await
    }

    /// Releases the PostgreSQL lock on the orderly shutdown path. Dropping the
    /// returned value then removes only the socket inode this daemon created.
    pub async fn shutdown(mut self) -> Result<(), LocalTransportError> {
        let daemon_lock = self
            .daemon_lock
            .take()
            .ok_or(LocalTransportError::DaemonLockAlreadyReleased)?;
        daemon_lock.release().await?;
        Ok(())
    }
}

/// Errors at the physical Unix-socket authority boundary.
#[derive(Debug, Error)]
pub enum LocalTransportError {
    #[error(transparent)]
    Io(#[from] io::Error),

    #[error(transparent)]
    Frame(#[from] FrameError),

    #[error(transparent)]
    WorkspaceRead(#[from] crate::workspace_read::WorkspaceReadError),

    #[error(transparent)]
    Store(#[from] StoreError),

    #[error("Architect operator router rejected the frame: {message}")]
    OperatorRpc { message: String },

    #[error("runtime root path is empty")]
    EmptyRuntimeRoot,

    #[error("runtime-root filesystem singleton is already held")]
    RuntimeAlreadyLocked,

    #[error("runtime root {path} is not a directory")]
    RuntimeRootNotDirectory { path: PathBuf },

    #[error("refusing to remove non-socket operator path {path}")]
    UnsafeOperatorSocketPath { path: PathBuf },

    #[error("refusing to use non-regular runtime lock path {path}")]
    UnsafeRuntimeLockPath { path: PathBuf },

    #[error("operator socket {path} was not created with mode 0600")]
    OperatorSocketPermissions { path: PathBuf },

    #[error("read deadline must be greater than zero")]
    ZeroReadDeadline,

    #[error("operation deadline must be greater than zero")]
    ZeroOperationDeadline,

    #[error("socket write deadline must be greater than zero")]
    ZeroWriteDeadline,

    #[error("socket read exceeded its bounded deadline")]
    ReadDeadlineExceeded,

    #[error("operation exceeded its bounded deadline")]
    OperationDeadlineExceeded,

    #[error("socket write exceeded its bounded deadline")]
    WriteDeadlineExceeded,

    #[error("peer closed before a response frame")]
    ResponseDisconnected,

    #[error(
        "operator request ID must be 1 through {MAX_OPERATOR_REQUEST_ID_BYTES} printable ASCII bytes"
    )]
    InvalidOperatorRequestId,

    #[error("unsupported operator protocol version {0}")]
    UnsupportedOperatorProtocol(u16),

    #[error("unexpected operator operation {actual:?}")]
    UnexpectedOperatorOperation { actual: String },

    #[error("operator status request used operation {actual:?}")]
    UnknownOperatorOperation { actual: String },

    #[error("Architect control was not configured for this daemon")]
    ArchitectControlUnavailable,

    #[error("campaign control was not configured for this daemon")]
    CampaignControlUnavailable,

    #[error("application control was not configured for this daemon")]
    ApplicationControlUnavailable,

    #[error("operator artifact control was not configured for this daemon")]
    OperatorArtifactControlUnavailable,

    #[error("read-only navigation control was not configured for this daemon")]
    NavigationControlUnavailable,

    #[error("Forum control was not configured for this daemon")]
    ForumControlUnavailable,

    #[error("operator status response did not preserve the request ID")]
    OperatorRequestIdMismatch,

    #[error("unexpected operator daemon state {actual:?}")]
    UnexpectedOperatorState { actual: String },

    #[error("operator command {operation:?} was rejected as {error_code}: {message}")]
    OperatorCommandRejected {
        operation: String,
        error_code: String,
        message: String,
    },

    #[error("daemon advisory lock was already released")]
    DaemonLockAlreadyReleased,
}

impl From<OperatorRpcError> for LocalTransportError {
    fn from(error: OperatorRpcError) -> Self {
        Self::OperatorRpc {
            message: error.to_string(),
        }
    }
}

impl From<CampaignOperatorRpcError> for LocalTransportError {
    fn from(error: CampaignOperatorRpcError) -> Self {
        Self::OperatorRpc {
            message: error.to_string(),
        }
    }
}

impl From<ApplicationOperatorRpcError> for LocalTransportError {
    fn from(error: ApplicationOperatorRpcError) -> Self {
        Self::OperatorRpc {
            message: error.to_string(),
        }
    }
}

impl From<OperatorArtifactRpcError> for LocalTransportError {
    fn from(error: OperatorArtifactRpcError) -> Self {
        Self::OperatorRpc {
            message: error.to_string(),
        }
    }
}

impl From<OperatorNavigationRpcError> for LocalTransportError {
    fn from(error: OperatorNavigationRpcError) -> Self {
        Self::OperatorRpc {
            message: error.to_string(),
        }
    }
}

impl From<OperatorForumRpcError> for LocalTransportError {
    fn from(error: OperatorForumRpcError) -> Self {
        Self::OperatorRpc {
            message: error.to_string(),
        }
    }
}

#[derive(Debug)]
struct RuntimeSocket {
    config: LocalTransportConfig,
    lease: RuntimeLease,
    listener: UnixListener,
    socket_identity: SocketIdentity,
}

impl RuntimeSocket {
    fn bind(config: LocalTransportConfig) -> Result<Self, LocalTransportError> {
        config.validate()?;
        let lease = RuntimeLease::acquire(config.runtime_root())?;
        let socket_path = config.operator_socket_path();
        remove_stale_socket(&lease, &socket_path)?;
        let listener = UnixListener::bind(&socket_path)?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;
        sync_runtime_directory(config.runtime_root())?;
        let metadata = fs::symlink_metadata(&socket_path)?;
        let mode = metadata.permissions().mode() & 0o777;
        if !metadata.file_type().is_socket() || mode != 0o600 {
            return Err(LocalTransportError::OperatorSocketPermissions { path: socket_path });
        }
        Ok(Self {
            config,
            lease,
            listener,
            socket_identity: SocketIdentity::from_metadata(&metadata),
        })
    }

    fn socket_path(&self) -> &Path {
        &self.lease.socket_path
    }
}

impl Drop for RuntimeSocket {
    fn drop(&mut self) {
        // The lock token plus device/inode comparison prevent an orderly stop
        // from deleting a replacement path that this daemon did not bind.
        if self.lease.still_owned()
            && fs::symlink_metadata(&self.lease.socket_path)
                .ok()
                .is_some_and(|metadata| {
                    metadata.file_type().is_socket()
                        && SocketIdentity::from_metadata(&metadata) == self.socket_identity
                })
        {
            let _ = fs::remove_file(&self.lease.socket_path);
            let _ = sync_runtime_directory(&self.lease.configured_root());
        }
    }
}

#[derive(Debug)]
struct RuntimeLease {
    lock_file: File,
    lock_path: PathBuf,
    socket_path: PathBuf,
    token: String,
    lock_identity: SocketIdentity,
}

impl RuntimeLease {
    fn acquire(runtime_root: &Path) -> Result<Self, LocalTransportError> {
        fs::create_dir_all(runtime_root)?;
        if !fs::symlink_metadata(runtime_root)?.file_type().is_dir() {
            return Err(LocalTransportError::RuntimeRootNotDirectory {
                path: runtime_root.to_path_buf(),
            });
        }
        let lock_path = runtime_root.join(RUNTIME_LOCK_FILENAME);
        let lock_file = openat(
            CWD,
            &lock_path,
            OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(io::Error::other)?;
        let mut lock_file = File::from(lock_file);
        let lock_metadata = lock_file.metadata()?;
        if !lock_metadata.file_type().is_file() {
            return Err(LocalTransportError::UnsafeRuntimeLockPath { path: lock_path });
        }
        match flock(&lock_file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return Err(LocalTransportError::RuntimeAlreadyLocked);
            }
            Err(error) => return Err(io::Error::other(error).into()),
        }
        lock_file.set_len(0)?;
        let token = runtime_lock_token(runtime_root);
        lock_file.write_all(token.as_bytes())?;
        lock_file.sync_all()?;
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600))?;
        sync_runtime_directory(runtime_root)?;
        Ok(Self {
            lock_file,
            lock_path,
            socket_path: runtime_root.join(OPERATOR_SOCKET_FILENAME),
            token,
            lock_identity: SocketIdentity::from_metadata(&lock_metadata),
        })
    }

    fn still_owned(&self) -> bool {
        let Ok(mut file) = File::open(&self.lock_path) else {
            return false;
        };
        let mut token = String::new();
        file.metadata().is_ok_and(|metadata| {
            metadata.file_type().is_file()
                && SocketIdentity::from_metadata(&metadata) == self.lock_identity
        }) && file
            .read_to_string(&mut token)
            .is_ok_and(|_| token == self.token)
    }

    fn configured_root(&self) -> &Path {
        self.lock_path
            .parent()
            .expect("runtime lock always has parent")
    }
}

impl Drop for RuntimeLease {
    fn drop(&mut self) {
        let _ = flock(&self.lock_file, FlockOperation::Unlock);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SocketIdentity {
    device: u64,
    inode: u64,
}

impl SocketIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

fn remove_stale_socket(
    lease: &RuntimeLease,
    socket_path: &Path,
) -> Result<(), LocalTransportError> {
    let metadata = match fs::symlink_metadata(socket_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !lease.still_owned() || !metadata.file_type().is_socket() {
        return Err(LocalTransportError::UnsafeOperatorSocketPath {
            path: socket_path.to_path_buf(),
        });
    }
    fs::remove_file(socket_path)?;
    sync_runtime_directory(
        socket_path
            .parent()
            .ok_or_else(|| io::Error::other("operator socket has no parent"))?,
    )?;
    Ok(())
}

async fn serve_operator_connection(
    mut stream: UnixStream,
    read_deadline: Duration,
    operation_deadline: Duration,
    write_deadline: Duration,
    status_store: Option<KernelStore>,
    operator_rpc: Option<OperatorRpc>,
    campaign_rpc: Option<CampaignOperatorRpc>,
    application_rpc: Option<ApplicationOperatorRpc>,
    navigation_rpc: Option<OperatorNavigationRpc>,
    artifact_rpc: Option<OperatorArtifactRpc>,
    forum_rpc: Option<OperatorForumRpc>,
) -> Result<(), LocalTransportError> {
    let request = read_stream_frame(&mut stream, REQUEST_FRAME_MAX_BYTES, read_deadline)
        .await?
        .ok_or(LocalTransportError::ResponseDisconnected)?;
    // Status is transport-owned rather than an actor protocol operation, so
    // it intentionally is not in the actor routing allowlist.
    let envelope: RoutingEnvelope = decode_json_frame(
        &request,
        REQUEST_FRAME_MAX_BYTES,
        "operator routing envelope",
    )?;
    validate_request_id(&envelope.request_id)?;
    if envelope.protocol_version != PROTOCOL_VERSION_V1 {
        return Err(LocalTransportError::UnsupportedOperatorProtocol(
            envelope.protocol_version,
        ));
    }
    match envelope.operation.as_str() {
        OPERATOR_STATUS_OPERATION => {
            let request: OperatorStatusRequest =
                decode_json_frame(&request, REQUEST_FRAME_MAX_BYTES, OPERATOR_STATUS_OPERATION)?;
            let build_status = match status_store {
                Some(store) => store.kernel_build_status().await?,
                None => crate::storage::KernelBuildStatus {
                    current_kernel_build_id: None,
                    aggregate_revision: factory_protocol::AggregateRevision::initial(),
                },
            };
            let response = OperatorStatusResponse {
                protocol_version: PROTOCOL_VERSION_V1,
                request_id: request.request_id,
                operation: OPERATOR_STATUS_OPERATION.to_owned(),
                state: "ready".to_owned(),
                current_kernel_build_id: build_status
                    .current_kernel_build_id
                    .map(|build| build.digest().to_hex()),
                aggregate_revision: build_status.aggregate_revision.get(),
            };
            write_stream_frame_json(
                &mut stream,
                &response,
                RESPONSE_FRAME_MAX_BYTES,
                write_deadline,
            )
            .await
        }
        factory_protocol::OP_ARCHITECT_SPONSOR_TICKET_REVISION
        | factory_protocol::OP_ARCHITECT_RELEASE_TICKET_ATTEMPT
        | factory_protocol::OP_ARCHITECT_DECIDE_CANDIDATE => {
            let router = operator_rpc.ok_or(LocalTransportError::ArchitectControlUnavailable)?;
            let response = with_operation_deadline(operation_deadline, async move {
                router
                    .dispatch(&request)
                    .await
                    .map_err(LocalTransportError::from)
            })
            .await?;
            validate_response_json(&response)?;
            write_stream_frame(
                &mut stream,
                &response,
                RESPONSE_FRAME_MAX_BYTES,
                write_deadline,
            )
            .await
        }
        factory_protocol::OP_OPERATOR_START_CAMPAIGN
        | factory_protocol::OP_OPERATOR_CAMPAIGN_STATUS
        | factory_protocol::OP_OPERATOR_CANCEL_CAMPAIGN => {
            let router = campaign_rpc.ok_or(LocalTransportError::CampaignControlUnavailable)?;
            let response = with_operation_deadline(operation_deadline, async move {
                router
                    .dispatch(&request)
                    .await
                    .map_err(LocalTransportError::from)
            })
            .await?;
            validate_response_json(&response)?;
            write_stream_frame(
                &mut stream,
                &response,
                RESPONSE_FRAME_MAX_BYTES,
                write_deadline,
            )
            .await
        }
        factory_protocol::OP_OPERATOR_SHOW_APPLICATION
        | factory_protocol::OP_OPERATOR_REGISTER_APPLICATION
        | factory_protocol::OP_OPERATOR_ACTIVATE_APPLICATION => {
            let router =
                application_rpc.ok_or(LocalTransportError::ApplicationControlUnavailable)?;
            let response = with_operation_deadline(operation_deadline, async move {
                router
                    .dispatch(&request)
                    .await
                    .map_err(LocalTransportError::from)
            })
            .await?;
            validate_response_json(&response)?;
            write_stream_frame(
                &mut stream,
                &response,
                RESPONSE_FRAME_MAX_BYTES,
                write_deadline,
            )
            .await
        }
        factory_protocol::OP_OPERATOR_LIST_TICKETS
        | factory_protocol::OP_OPERATOR_SHOW_TICKET
        | factory_protocol::OP_OPERATOR_SHOW_CANDIDATE
        | factory_protocol::OP_OPERATOR_SHOW_AUDIT => {
            let router = navigation_rpc.ok_or(LocalTransportError::NavigationControlUnavailable)?;
            let response = with_operation_deadline(operation_deadline, async move {
                router
                    .dispatch(&request)
                    .await
                    .map_err(LocalTransportError::from)
            })
            .await?;
            validate_response_json(&response)?;
            write_stream_frame(
                &mut stream,
                &response,
                RESPONSE_FRAME_MAX_BYTES,
                write_deadline,
            )
            .await
        }
        factory_protocol::OP_OPERATOR_SEAL_ARTIFACT => {
            let router =
                artifact_rpc.ok_or(LocalTransportError::OperatorArtifactControlUnavailable)?;
            let response = with_operation_deadline(operation_deadline, async move {
                router
                    .dispatch(&request)
                    .await
                    .map_err(LocalTransportError::from)
            })
            .await?;
            validate_response_json(&response)?;
            write_stream_frame(
                &mut stream,
                &response,
                RESPONSE_FRAME_MAX_BYTES,
                write_deadline,
            )
            .await
        }
        factory_protocol::OP_FORUM_LIST_TOPICS
        | factory_protocol::OP_FORUM_LIST_THREADS
        | factory_protocol::OP_FORUM_SEARCH
        | factory_protocol::OP_FORUM_READ_THREAD
        | factory_protocol::OP_FORUM_CREATE_TOPIC
        | factory_protocol::OP_FORUM_CREATE_THREAD
        | factory_protocol::OP_FORUM_POST => {
            let router = forum_rpc.ok_or(LocalTransportError::ForumControlUnavailable)?;
            let response = with_operation_deadline(operation_deadline, async move {
                router
                    .dispatch(&request)
                    .await
                    .map_err(LocalTransportError::from)
            })
            .await?;
            validate_response_json(&response)?;
            write_stream_frame(
                &mut stream,
                &response,
                RESPONSE_FRAME_MAX_BYTES,
                write_deadline,
            )
            .await
        }
        _ => Err(LocalTransportError::UnknownOperatorOperation {
            actual: envelope.operation,
        }),
    }
}

async fn read_stream_frame(
    stream: &mut UnixStream,
    maximum: usize,
    read_deadline: Duration,
) -> Result<Option<Vec<u8>>, LocalTransportError> {
    with_read_deadline(read_deadline, async {
        let mut prefix = [0_u8; FRAME_PREFIX_BYTES];
        let prefix_read = read_until_eof(stream, &mut prefix).await?;
        if prefix_read == 0 {
            return Ok(None);
        }
        if prefix_read != FRAME_PREFIX_BYTES {
            return Err(FrameError::Truncated {
                expected: FRAME_PREFIX_BYTES,
                received: prefix_read,
            }
            .into());
        }
        let payload_length = u32::from_be_bytes(prefix) as usize;
        if payload_length > maximum {
            return Err(FrameError::Oversized {
                actual: payload_length,
                maximum,
            }
            .into());
        }
        let mut frame = Vec::with_capacity(FRAME_PREFIX_BYTES + payload_length);
        frame.extend_from_slice(&prefix);
        let mut payload = vec![0_u8; payload_length];
        let payload_read = read_until_eof(stream, &mut payload).await?;
        if payload_read != payload_length {
            return Err(FrameError::Truncated {
                expected: FRAME_PREFIX_BYTES + payload_length,
                received: FRAME_PREFIX_BYTES + payload_read,
            }
            .into());
        }
        frame.extend_from_slice(&payload);
        // `read_until_eof` consumed exactly the declared length. The shared
        // decoder still checks the complete-frame invariant in one place.
        let _ = decode_frame(&frame, maximum)?;
        Ok(Some(frame))
    })
    .await
}

async fn read_until_eof(stream: &mut UnixStream, buffer: &mut [u8]) -> io::Result<usize> {
    let mut offset = 0;
    while offset < buffer.len() {
        let count = stream.read(&mut buffer[offset..]).await?;
        if count == 0 {
            break;
        }
        offset += count;
    }
    Ok(offset)
}

async fn write_stream_frame(
    stream: &mut UnixStream,
    payload: &[u8],
    maximum: usize,
    write_deadline: Duration,
) -> Result<(), LocalTransportError> {
    let frame = encode_frame(payload, maximum)?;
    write_frame_bytes(stream, &frame, write_deadline).await
}

async fn write_stream_frame_json<T: Serialize>(
    stream: &mut UnixStream,
    value: &T,
    maximum: usize,
    write_deadline: Duration,
) -> Result<(), LocalTransportError> {
    let frame = encode_json_frame(value, maximum)?;
    write_frame_bytes(stream, &frame, write_deadline).await
}

async fn write_frame_bytes(
    stream: &mut UnixStream,
    frame: &[u8],
    write_deadline: Duration,
) -> Result<(), LocalTransportError> {
    with_write_deadline(write_deadline, stream.write_all(frame)).await
}

fn validate_response_json(response: &[u8]) -> Result<(), LocalTransportError> {
    let response = std::str::from_utf8(response).map_err(|_| FrameError::InvalidUtf8)?;
    // This is a syntax-only guard at the generic transport boundary. The
    // operation dispatcher still parses/produces its own closed response
    // struct; the discarded Value is never an authority or durable command.
    let _: miniserde::json::Value =
        json::from_str(response).map_err(|error| FrameError::InvalidJson {
            operation: "operation response",
            detail: format!("{error:?}"),
        })?;
    Ok(())
}

async fn with_read_deadline<T>(
    deadline: Duration,
    operation: impl core::future::Future<Output = Result<T, LocalTransportError>>,
) -> Result<T, LocalTransportError> {
    future::or(operation, async move {
        Timer::after(deadline).await;
        Err(LocalTransportError::ReadDeadlineExceeded)
    })
    .await
}

async fn with_operation_deadline<T>(
    deadline: Duration,
    operation: impl core::future::Future<Output = Result<T, LocalTransportError>>,
) -> Result<T, LocalTransportError> {
    future::or(operation, async move {
        Timer::after(deadline).await;
        Err(LocalTransportError::OperationDeadlineExceeded)
    })
    .await
}

async fn with_write_deadline<T>(
    deadline: Duration,
    operation: impl core::future::Future<Output = io::Result<T>>,
) -> Result<T, LocalTransportError> {
    future::or(
        async move { operation.await.map_err(LocalTransportError::from) },
        async move {
            Timer::after(deadline).await;
            Err(LocalTransportError::WriteDeadlineExceeded)
        },
    )
    .await
}

fn sync_runtime_directory(path: &Path) -> Result<(), LocalTransportError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn validate_request_id(request_id: &str) -> Result<(), LocalTransportError> {
    if request_id.is_empty()
        || request_id.len() > MAX_OPERATOR_REQUEST_ID_BYTES
        || !request_id.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(LocalTransportError::InvalidOperatorRequestId);
    }
    Ok(())
}

fn validate_architect_response_identity(
    protocol_version: u16,
    request_id: &str,
    operation: &str,
    expected_request_id: &str,
    expected_operation: &'static str,
) -> Result<(), LocalTransportError> {
    if protocol_version != PROTOCOL_VERSION_V1 {
        return Err(LocalTransportError::UnsupportedOperatorProtocol(
            protocol_version,
        ));
    }
    if operation != expected_operation {
        return Err(LocalTransportError::UnexpectedOperatorOperation {
            actual: operation.to_owned(),
        });
    }
    if request_id != expected_request_id {
        return Err(LocalTransportError::OperatorRequestIdMismatch);
    }
    Ok(())
}

fn runtime_lock_token(runtime_root: &Path) -> String {
    let started_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let mut hasher = blake3::Hasher::new();
    hasher.update(runtime_root.as_os_str().as_encoded_bytes());
    hasher.update(&std::process::id().to_be_bytes());
    hasher.update(&started_nanos.to_be_bytes());
    format!("factoryd-runtime-lock:{}\n", hasher.finalize().to_hex())
}

#[cfg(test)]
mod tests {
    use std::{
        os::unix::net::UnixListener as StdUnixListener,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use factory_protocol::{OP_ARTIFACT_READ, Office};

    use super::*;

    #[test]
    fn runtime_socket_is_0600_and_safe_restart_and_stale_cleanup_work() {
        let root = test_runtime_root("runtime");
        let config = LocalTransportConfig::new(root.clone());
        let first = RuntimeSocket::bind(config.clone()).expect("first runtime socket");
        let mode = fs::metadata(first.socket_path())
            .expect("socket metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        assert!(matches!(
            RuntimeSocket::bind(config.clone()),
            Err(LocalTransportError::RuntimeAlreadyLocked)
        ));
        drop(first);
        let second = RuntimeSocket::bind(config.clone()).expect("orderly restart");
        drop(second);

        let stale_path = config.operator_socket_path();
        let listener = StdUnixListener::bind(&stale_path).expect("create stale socket");
        drop(listener);
        let recovered = RuntimeSocket::bind(config.clone()).expect("clean stale socket after lock");
        drop(recovered);

        fs::write(&stale_path, b"not a socket").expect("regular file");
        assert!(matches!(
            RuntimeSocket::bind(config),
            Err(LocalTransportError::UnsafeOperatorSocketPath { .. })
        ));
        fs::remove_file(&stale_path).expect("remove test file");
        fs::remove_file(root.join(RUNTIME_LOCK_FILENAME)).expect("remove test lock");
        fs::remove_dir(root).expect("remove test root");
    }

    #[test]
    fn operator_status_is_a_typed_framed_exchange() {
        smol::block_on(async {
            let root = test_runtime_root("operator");
            let runtime = RuntimeSocket::bind(LocalTransportConfig::new(root.clone()))
                .expect("runtime socket");
            let path = runtime.socket_path().to_path_buf();
            let server = smol::spawn(async move {
                let (stream, _) = runtime.listener.accept().await?;
                serve_operator_connection(
                    stream,
                    runtime.config.read_deadline,
                    runtime.config.operation_deadline,
                    runtime.config.write_deadline,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .await
            });
            let status = OperatorClient::new(path)
                .probe("status-1".to_owned())
                .await
                .expect("status response");
            assert_eq!(status.state, "ready");
            assert_eq!(status.request_id, "status-1");
            assert_eq!(status.current_kernel_build_id, None);
            assert_eq!(status.aggregate_revision, 0);
            server.await.expect("server response");
            fs::remove_file(root.join(RUNTIME_LOCK_FILENAME)).expect("remove test lock");
            fs::remove_dir(root).expect("remove test root");
        });
    }

    #[test]
    fn operator_client_rejects_a_response_for_another_request_id() {
        smol::block_on(async {
            let root = test_runtime_root("operator-request-id");
            fs::create_dir_all(&root).expect("test root");
            let path = root.join("operator.sock");
            let listener = UnixListener::bind(&path).expect("listener");
            let server = smol::spawn(async move {
                let (mut stream, _) = listener.accept().await.expect("accept client");
                let _ =
                    read_stream_frame(&mut stream, REQUEST_FRAME_MAX_BYTES, Duration::from_secs(1))
                        .await
                        .expect("request frame");
                write_stream_frame_json(
                    &mut stream,
                    &OperatorStatusResponse {
                        protocol_version: PROTOCOL_VERSION_V1,
                        request_id: "another-request".to_owned(),
                        operation: OPERATOR_STATUS_OPERATION.to_owned(),
                        state: "ready".to_owned(),
                        current_kernel_build_id: Some("a".repeat(64)),
                        aggregate_revision: 1,
                    },
                    RESPONSE_FRAME_MAX_BYTES,
                    Duration::from_secs(1),
                )
                .await
            });
            assert!(matches!(
                OperatorClient::new(path.clone())
                    .probe("expected-request".to_owned())
                    .await,
                Err(LocalTransportError::OperatorRequestIdMismatch)
            ));
            server.await.expect("server");
            fs::remove_file(path).expect("remove socket");
            fs::remove_dir(root).expect("remove test root");
        });
    }

    #[test]
    fn architect_client_surfaces_a_typed_operator_rejection() {
        smol::block_on(async {
            let root = test_runtime_root("architect-rejection");
            fs::create_dir_all(&root).expect("test root");
            let path = root.join("operator.sock");
            let listener = UnixListener::bind(&path).expect("listener");
            let server = smol::spawn(async move {
                let (mut stream, _) = listener.accept().await.expect("accept client");
                let request =
                    read_stream_frame(&mut stream, REQUEST_FRAME_MAX_BYTES, Duration::from_secs(1))
                        .await
                        .expect("request frame")
                        .expect("request exists");
                let request: ArchitectReleaseTicketAttemptRequest = decode_json_frame(
                    &request,
                    REQUEST_FRAME_MAX_BYTES,
                    factory_protocol::OP_ARCHITECT_RELEASE_TICKET_ATTEMPT,
                )
                .expect("typed request");
                write_stream_frame_json(
                    &mut stream,
                    &ErrorResponse {
                        protocol_version: PROTOCOL_VERSION_V1,
                        request_id: request.request_id,
                        operation: factory_protocol::OP_ARCHITECT_RELEASE_TICKET_ATTEMPT.to_owned(),
                        error_code: "architect_transition_unavailable".to_owned(),
                        message: "no trusted resolver".to_owned(),
                    },
                    RESPONSE_FRAME_MAX_BYTES,
                    Duration::from_secs(1),
                )
                .await
            });
            let error = OperatorClient::new(path.clone())
                .release_ticket_attempt(ArchitectReleaseTicketAttemptRequest {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: "release-1".to_owned(),
                    operation: factory_protocol::OP_ARCHITECT_RELEASE_TICKET_ATTEMPT.to_owned(),
                    client_command_id: "release-command".to_owned(),
                    expected_revision: 4,
                    ticket_attempt_id: 7,
                    rationale: factory_protocol::SealedArtifactReferenceWireV1 {
                        artifact_id: 1,
                        digest: "a".repeat(64),
                        byte_length: 12,
                    },
                    principal: "grand-architect".to_owned(),
                })
                .await
                .expect_err("typed rejection");
            assert!(matches!(
                error,
                LocalTransportError::OperatorCommandRejected { error_code, .. }
                    if error_code == "architect_transition_unavailable"
            ));
            server.await.expect("server");
            fs::remove_file(path).expect("remove socket");
            fs::remove_dir(root).expect("remove test root");
        });
    }

    #[test]
    fn campaign_client_uses_only_typed_socket_frames() {
        smol::block_on(async {
            let root = test_runtime_root("campaign-client");
            fs::create_dir_all(&root).expect("test root");
            let path = root.join("operator.sock");
            let listener = UnixListener::bind(&path).expect("listener");
            let server = smol::spawn(async move {
                let (mut stream, _) = listener.accept().await.expect("accept client");
                let request =
                    read_stream_frame(&mut stream, REQUEST_FRAME_MAX_BYTES, Duration::from_secs(1))
                        .await
                        .expect("request frame")
                        .expect("request exists");
                let request: OperatorCampaignStatusRequest = decode_json_frame(
                    &request,
                    REQUEST_FRAME_MAX_BYTES,
                    factory_protocol::OP_OPERATOR_CAMPAIGN_STATUS,
                )
                .expect("typed campaign request");
                assert_eq!(request.campaign_id, 9);
                write_stream_frame_json(
                    &mut stream,
                    &CampaignStatusResponse {
                        protocol_version: PROTOCOL_VERSION_V1,
                        request_id: request.request_id,
                        operation: factory_protocol::OP_OPERATOR_CAMPAIGN_STATUS.to_owned(),
                        campaign_id: 9,
                        state: "running".to_owned(),
                        aggregate_revision: 2,
                        kernel_build_id: "a".repeat(64),
                        application_revision_id: 3,
                        repository_id: 4,
                        aggregate_budget_micro_usd: 100,
                        measured_cost_state: "known".to_owned(),
                        measured_cost_micro_usd: Some(7),
                        remaining_budget_micro_usd: Some(93),
                        deadline_unix_millis: 4_000_000_000_000,
                        delivery_target: 2,
                        failure_reason: None,
                        base_commit: None,
                        candidate_tree: None,
                        candidate_commit: None,
                        delivered_commit: None,
                        delivered_attempt_count: 0,
                        ready_ticket_count: 1,
                        proposed_ticket_count: 0,
                        in_flight_ticket_count: 1,
                        downstream_ticket_attempt_count: 0,
                        downstream_action_stage: None,
                        downstream_ticket_attempt_id: None,
                        downstream_ticket_attempt_revision: None,
                        downstream_candidate_id: None,
                        downstream_candidate_revision: None,
                        downstream_evidence: None,
                        ready_low_water: 1,
                        ready_target: 2,
                        ready_maximum: 3,
                        proposal_maximum: 2,
                        oldest_sponsored_ticket_revision_id: Some(11),
                        oldest_sponsored_ticket_revision: Some(5),
                        scheduler_next_action: "blocked".to_owned(),
                        scheduler_constraint: Some("in_flight_ticket_limit_reached".to_owned()),
                        session_costs: vec![],
                        session_cost_aggregates: vec![],
                    },
                    RESPONSE_FRAME_MAX_BYTES,
                    Duration::from_secs(1),
                )
                .await
            });
            let status = OperatorClient::new(path.clone())
                .campaign_status(OperatorCampaignStatusRequest {
                    protocol_version: PROTOCOL_VERSION_V1,
                    request_id: "campaign-status-1".to_owned(),
                    operation: factory_protocol::OP_OPERATOR_CAMPAIGN_STATUS.to_owned(),
                    campaign_id: 9,
                })
                .await
                .expect("typed campaign status");
            assert_eq!(
                status.scheduler_constraint.as_deref(),
                Some("in_flight_ticket_limit_reached")
            );
            assert_eq!(status.remaining_budget_micro_usd, Some(93));
            server.await.expect("server");
            fs::remove_file(path).expect("remove socket");
            fs::remove_dir(root).expect("remove test root");
        });
    }

    #[test]
    fn actor_binding_cannot_be_replaced_by_payload_fields() {
        smol::block_on(async {
            let (mut client, server) =
                actor_pair(LocalTransportConfig::new(test_runtime_root("binding")));
            let expected = server.binding;
            let task = smol::spawn(async move {
                server
                    .serve(|request| async move {
                        assert_eq!(request.binding().session_id(), expected.session_id());
                        assert_eq!(request.binding().assignment_id(), expected.assignment_id());
                        assert_eq!(request.binding().office(), expected.office());
                        Ok(br#"{"accepted":true}"#.to_vec())
                    })
                    .await
            });
            let payload = br#"{"protocol_version":1,"request_id":"r","operation":"artifact.read","session_id":999,"assignment_id":999,"office":"quality"}"#;
            write_stream_frame(
                &mut client,
                payload,
                REQUEST_FRAME_MAX_BYTES,
                Duration::from_secs(1),
            )
            .await
            .expect("write request");
            let response = read_stream_frame(
                &mut client,
                RESPONSE_FRAME_MAX_BYTES,
                Duration::from_secs(1),
            )
            .await
            .expect("read response")
            .expect("response");
            assert_eq!(
                decode_frame(&response, RESPONSE_FRAME_MAX_BYTES).unwrap(),
                br#"{"accepted":true}"#
            );
            drop(client);
            assert_eq!(task.await.expect("server"), ActorDisconnect::PeerClosed);
        });
    }

    #[test]
    fn malformed_truncated_and_oversize_actor_frames_reject() {
        smol::block_on(async {
            for frame in [
                vec![0, 0, 0],
                vec![0, 16, 0, 1],
                vec![0, 0, 0, 2, b'{'],
                vec![0, 0, 0, 1, b'{'],
                vec![0, 0, 0, 1, 0xff],
            ] {
                let (mut client, server) =
                    actor_pair(LocalTransportConfig::new(test_runtime_root("bad")));
                let task =
                    smol::spawn(async move { server.serve(|_| async { Ok(Vec::new()) }).await });
                client.write_all(&frame).await.expect("malformed write");
                drop(client);
                assert!(matches!(task.await, Err(LocalTransportError::Frame(_))));
            }
            let oversize = u32::try_from(REQUEST_FRAME_MAX_BYTES + 1)
                .expect("request limit fits the wire length")
                .to_be_bytes();
            let (mut client, server) =
                actor_pair(LocalTransportConfig::new(test_runtime_root("oversize")));
            let task = smol::spawn(async move { server.serve(|_| async { Ok(Vec::new()) }).await });
            client.write_all(&oversize).await.expect("oversize prefix");
            assert!(matches!(
                task.await,
                Err(LocalTransportError::Frame(FrameError::Oversized { .. }))
            ));
        });
    }

    #[test]
    fn complete_frame_suffixes_and_unknown_operations_are_rejected() {
        let trailing = [0, 0, 0, 2, b'{', b'}', b'x'];
        assert!(matches!(
            decode_frame(&trailing, REQUEST_FRAME_MAX_BYTES),
            Err(FrameError::TrailingBytes { .. })
        ));
        smol::block_on(async {
            let (mut client, server) = actor_pair(LocalTransportConfig::new(test_runtime_root(
                "unknown-operation",
            )));
            let task =
                smol::spawn(async move { server.serve(|_| async { Ok(br#"{}"#.to_vec()) }).await });
            write_stream_frame(
                &mut client,
                br#"{"protocol_version":1,"request_id":"unknown","operation":"no.such.operation"}"#,
                REQUEST_FRAME_MAX_BYTES,
                Duration::from_secs(1),
            )
            .await
            .expect("unknown operation frame");
            assert!(matches!(
                task.await,
                Err(LocalTransportError::Frame(FrameError::UnknownOperation(_)))
            ));
        });
    }

    #[test]
    fn actor_response_must_be_utf8_json() {
        smol::block_on(async {
            let (mut client, server) =
                actor_pair(LocalTransportConfig::new(test_runtime_root("bad-response")));
            let task = smol::spawn(async move { server.serve(|_| async { Ok(vec![0xff]) }).await });
            write_known_request(&mut client, "bad-response").await;
            assert!(matches!(
                task.await,
                Err(LocalTransportError::Frame(FrameError::InvalidUtf8))
            ));
        });
    }

    #[test]
    fn actor_read_and_operation_deadlines_are_bounded() {
        smol::block_on(async {
            let config = LocalTransportConfig::new(test_runtime_root("read-deadline"))
                .with_deadlines(Duration::from_millis(10), Duration::from_secs(1));
            let (_client, server) = actor_pair(config);
            assert!(matches!(
                server.serve(|_| async { Ok(Vec::new()) }).await,
                Err(LocalTransportError::ReadDeadlineExceeded)
            ));

            let config = LocalTransportConfig::new(test_runtime_root("assignment-read-deadline"))
                .with_deadlines(Duration::from_millis(10), Duration::from_secs(1));
            let (mut client, server) = actor_pair(config);
            let server = server
                .with_assignment_read_deadline(Duration::from_millis(100))
                .expect("assignment wall limit is a valid actor read deadline");
            let task =
                smol::spawn(async move { server.serve(|_| async { Ok(br#"{}"#.to_vec()) }).await });
            Timer::after(Duration::from_millis(30)).await;
            write_known_request(&mut client, "assignment-read-deadline").await;
            read_stream_frame(
                &mut client,
                RESPONSE_FRAME_MAX_BYTES,
                Duration::from_secs(1),
            )
            .await
            .expect("read actor response")
            .expect("actor response exists");
            drop(client);
            assert_eq!(
                task.await.expect("actor server"),
                ActorDisconnect::PeerClosed
            );

            let config = LocalTransportConfig::new(test_runtime_root("operation-deadline"))
                .with_deadlines(Duration::from_secs(1), Duration::from_millis(10));
            let (mut client, server) = actor_pair(config);
            let task = smol::spawn(async move {
                server
                    .serve(|_| async {
                        Timer::after(Duration::from_millis(50)).await;
                        Ok(Vec::new())
                    })
                    .await
            });
            write_known_request(&mut client, "operation-deadline").await;
            assert!(matches!(
                task.await,
                Err(LocalTransportError::OperationDeadlineExceeded)
            ));

            let config =
                LocalTransportConfig::new(test_runtime_root("assignment-operation-deadline"))
                    .with_deadlines(Duration::from_secs(1), Duration::from_millis(10));
            let (mut client, server) = actor_pair(config);
            let server = server
                .with_assignment_operation_deadline(Duration::from_millis(100))
                .expect("assignment wall limit is a valid actor operation deadline");
            let task = smol::spawn(async move {
                server
                    .serve(|_| async {
                        Timer::after(Duration::from_millis(30)).await;
                        Ok(br#"{}"#.to_vec())
                    })
                    .await
            });
            write_known_request(&mut client, "assignment-operation-deadline").await;
            read_stream_frame(
                &mut client,
                RESPONSE_FRAME_MAX_BYTES,
                Duration::from_secs(1),
            )
            .await
            .expect("read actor response")
            .expect("actor response exists");
            drop(client);
            assert_eq!(
                task.await.expect("actor server"),
                ActorDisconnect::PeerClosed
            );

            assert!(matches!(
                with_write_deadline(Duration::from_millis(10), async {
                    Timer::after(Duration::from_millis(50)).await;
                    Ok(())
                })
                .await,
                Err(LocalTransportError::WriteDeadlineExceeded)
            ));
        });
    }

    #[test]
    fn actor_connection_never_dispatches_more_than_one_request_at_once_and_reports_disconnect() {
        smol::block_on(async {
            let (mut client, server) =
                actor_pair(LocalTransportConfig::new(test_runtime_root("inflight")));
            let active = Arc::new(AtomicUsize::new(0));
            let maximum = Arc::new(AtomicUsize::new(0));
            let task = smol::spawn({
                let active = Arc::clone(&active);
                let maximum = Arc::clone(&maximum);
                async move {
                    server
                        .serve(move |_| {
                            let active = Arc::clone(&active);
                            let maximum = Arc::clone(&maximum);
                            async move {
                                let observed = active.fetch_add(1, Ordering::SeqCst) + 1;
                                maximum.fetch_max(observed, Ordering::SeqCst);
                                Timer::after(Duration::from_millis(10)).await;
                                active.fetch_sub(1, Ordering::SeqCst);
                                Ok(br#"{}"#.to_vec())
                            }
                        })
                        .await
                }
            });
            write_known_request(&mut client, "first").await;
            write_known_request(&mut client, "second").await;
            let _ = read_stream_frame(
                &mut client,
                RESPONSE_FRAME_MAX_BYTES,
                Duration::from_secs(1),
            )
            .await
            .expect("first response");
            let _ = read_stream_frame(
                &mut client,
                RESPONSE_FRAME_MAX_BYTES,
                Duration::from_secs(1),
            )
            .await
            .expect("second response");
            drop(client);
            assert_eq!(task.await.expect("server"), ActorDisconnect::PeerClosed);
            assert_eq!(maximum.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    #[ignore = "requires FACTORY_TEST_DATABASE_URL for a disposable PostgreSQL 18 database"]
    fn daemon_composes_runtime_and_postgresql_singletons_before_a_restart() {
        smol::block_on(async {
            let database_url = test_database_url();
            let first_store = KernelStore::connect(&database_url)
                .await
                .expect("first store");
            first_store.migrate_and_verify().await.expect("migrate");
            let second_store = KernelStore::connect(&database_url)
                .await
                .expect("second store");
            let first_root = test_runtime_root("daemon-first");
            let second_root = test_runtime_root("daemon-second");
            let first =
                LocalDaemon::bind(LocalTransportConfig::new(first_root.clone()), &first_store)
                    .await
                    .expect("first daemon");
            assert!(matches!(
                LocalDaemon::bind(
                    LocalTransportConfig::new(second_root.clone()),
                    &second_store
                )
                .await,
                Err(LocalTransportError::Store(StoreError::DaemonAlreadyRunning))
            ));
            first.shutdown().await.expect("first shutdown");
            let restarted = LocalDaemon::bind(
                LocalTransportConfig::new(second_root.clone()),
                &second_store,
            )
            .await
            .expect("restart after both locks release");
            restarted.shutdown().await.expect("restart shutdown");
            first_store.close().await;
            second_store.close().await;
            for root in [first_root, second_root] {
                let _ = fs::remove_file(root.join(RUNTIME_LOCK_FILENAME));
                let _ = fs::remove_dir(root);
            }
        });
    }

    fn actor_pair(config: LocalTransportConfig) -> (UnixStream, ActorServerConnection) {
        let (client, server) = UnixStream::pair().expect("socketpair");
        let binding = ActorConnectionBinding::from_identity(
            ActorConnectionIdentity::from_admitted_assignment(
                SessionId::new(1).unwrap(),
                AssignmentId::new(2).unwrap(),
                ApplicationRevisionId::new(3).unwrap(),
                CampaignId::new(4).unwrap(),
                Office::Engineering,
            ),
        );
        (
            client,
            ActorServerConnection {
                stream: server,
                binding,
                config,
            },
        )
    }

    async fn write_known_request(stream: &mut UnixStream, request_id: &str) {
        let payload = format!(
            "{{\"protocol_version\":1,\"request_id\":\"{request_id}\",\"operation\":\"{OP_ARTIFACT_READ}\"}}"
        );
        write_stream_frame(
            stream,
            payload.as_bytes(),
            REQUEST_FRAME_MAX_BYTES,
            Duration::from_secs(1),
        )
        .await
        .expect("known request");
    }

    fn test_runtime_root(name: &str) -> PathBuf {
        // macOS gives `$TMPDIR` a long per-user prefix; Unix socket paths are
        // bounded by `sockaddr_un`, so use the standard short local root for
        // these provider-free socket fixtures.
        PathBuf::from("/tmp").join(format!(
            "f3t-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ))
    }

    fn test_database_url() -> String {
        let url = std::env::var("FACTORY_TEST_DATABASE_URL")
            .expect("FACTORY_TEST_DATABASE_URL must name a disposable PostgreSQL 18 database");
        let database_name = url
            .rsplit('/')
            .next()
            .and_then(|part| part.split('?').next())
            .expect("database URL has a final path component");
        assert!(
            database_name.strip_prefix("factory_test_v3_").is_some_and(
                |suffix| !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
            ),
            "FACTORY_TEST_DATABASE_URL must name exactly factory_test_v3_<digits>"
        );
        url
    }
}
