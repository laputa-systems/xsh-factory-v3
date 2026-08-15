//! The Rust-owned boundary between a sealed Luau policy and Factory tools.
//!
//! A Luau declaration is deliberately only a prompt-facing description.  It
//! does not grant an operation merely by naming a capability.  This module
//! validates the declaration against the packet allowlist and binds the
//! declaration to an explicit host object.  The host object is the only place
//! where a model-visible tool name becomes a framed daemon operation.
//!
//! The bridge uses [`pi_agent_protocol::JsonValue`] instead of a dynamic JSON
//! map so the surrounding host can use the same value for Luau, frame, and
//! transcript boundaries without adding a second JSON representation.

use pi_agent_core::scheduler::CancellationToken;
use pi_agent_luau::tool_handler::{
    CapabilityError, CapabilityFuture, CapabilityRequest, CapabilityResponse, LuauCapability,
};
use pi_agent_protocol::{JsonNumber, JsonValue};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// The sole capability name used by daemon-bound policy handlers.
pub const FACTORY_CAPABILITY: &str = "factory";

/// A closed set of actor tools.  Keep this list in lockstep with the packet
/// contract; parsing unknown names is an admission error, never a no-op.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ToolName {
    /// Kernel-ledger workspace read.
    WorkspaceRead,
    /// Host-local workspace write.
    WorkspaceWrite,
    /// Host-local workspace edit.
    WorkspaceEdit,
    /// Host-local workspace search.
    WorkspaceSearch,
    /// Host-local workspace list.
    WorkspaceList,
    /// Host-local shell execution.
    Shell,
    /// Bounded Forum search.
    ForumSearch,
    /// Bounded Forum topic listing.
    ForumListTopics,
    /// Bounded Forum thread listing.
    ForumListThreads,
    /// Bounded Forum thread read.
    ForumReadThread,
    /// Immutable publication creation.
    PublicationCreate,
    /// Immutable workspace artifact sealing.
    ArtifactSeal,
    /// Read one admitted evidence artifact.
    ArtifactRead,
    /// Product ticket proposal.
    ProductSubmitTicket,
    /// Engineering regression checkpoint.
    CandidateCheckpointRegression,
    /// Engineering candidate submission.
    CandidateSubmit,
    /// Quality full-suite execution.
    QualityRunFullSuite,
    /// Quality review submission.
    QualitySubmitReview,
    /// Assignment terminal operation.
    WorkComplete,
}

impl ToolName {
    /// Parse one packet/policy spelling.
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "workspace_read" => Self::WorkspaceRead,
            "workspace_write" => Self::WorkspaceWrite,
            "workspace_edit" => Self::WorkspaceEdit,
            "workspace_search" => Self::WorkspaceSearch,
            "workspace_list" => Self::WorkspaceList,
            "shell" => Self::Shell,
            "forum_search" => Self::ForumSearch,
            "forum_list_topics" => Self::ForumListTopics,
            "forum_list_threads" => Self::ForumListThreads,
            "forum_read_thread" => Self::ForumReadThread,
            "publication_create" => Self::PublicationCreate,
            "artifact_seal" => Self::ArtifactSeal,
            "artifact_read" => Self::ArtifactRead,
            "product_submit_ticket" => Self::ProductSubmitTicket,
            "candidate_checkpoint_regression" => Self::CandidateCheckpointRegression,
            "candidate_submit" => Self::CandidateSubmit,
            "quality_run_full_suite" => Self::QualityRunFullSuite,
            "quality_submit_review" => Self::QualitySubmitReview,
            "work_complete" => Self::WorkComplete,
            _ => return None,
        })
    }

    /// Return the stable model-facing spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WorkspaceRead => "workspace_read",
            Self::WorkspaceWrite => "workspace_write",
            Self::WorkspaceEdit => "workspace_edit",
            Self::WorkspaceSearch => "workspace_search",
            Self::WorkspaceList => "workspace_list",
            Self::Shell => "shell",
            Self::ForumSearch => "forum_search",
            Self::ForumListTopics => "forum_list_topics",
            Self::ForumListThreads => "forum_list_threads",
            Self::ForumReadThread => "forum_read_thread",
            Self::PublicationCreate => "publication_create",
            Self::ArtifactSeal => "artifact_seal",
            Self::ArtifactRead => "artifact_read",
            Self::ProductSubmitTicket => "product_submit_ticket",
            Self::CandidateCheckpointRegression => "candidate_checkpoint_regression",
            Self::CandidateSubmit => "candidate_submit",
            Self::QualityRunFullSuite => "quality_run_full_suite",
            Self::QualitySubmitReview => "quality_submit_review",
            Self::WorkComplete => "work_complete",
        }
    }

    /// Return the exact daemon operation, when this tool is daemon-bound.
    pub const fn daemon_operation(self) -> Option<&'static str> {
        Some(match self {
            Self::WorkspaceRead => "workspace.read",
            Self::ForumSearch => "forum.search",
            Self::ForumListTopics => "forum.list_topics",
            Self::ForumListThreads => "forum.list_threads",
            Self::ForumReadThread => "forum.read_thread",
            Self::PublicationCreate => "publication.create",
            Self::ArtifactSeal => "artifact.seal_workspace_file",
            Self::ArtifactRead => "artifact.read",
            Self::ProductSubmitTicket => "product.submit_ticket",
            Self::CandidateCheckpointRegression => "candidate.checkpoint_regression",
            Self::CandidateSubmit => "candidate.submit",
            Self::QualityRunFullSuite => "quality.run_full_suite",
            Self::QualitySubmitReview => "quality.submit_review",
            Self::WorkComplete => "work.complete",
            Self::WorkspaceWrite
            | Self::WorkspaceEdit
            | Self::WorkspaceSearch
            | Self::WorkspaceList
            | Self::Shell => return None,
        })
    }

    /// Return the capability method expected from a Luau handler.
    pub fn capability_method(self) -> &'static str {
        match self {
            Self::WorkspaceWrite => "workspace.write",
            Self::WorkspaceEdit => "workspace.edit",
            Self::WorkspaceSearch => "workspace.search",
            Self::WorkspaceList => "workspace.list",
            Self::Shell => "shell.exec",
            _ => self.daemon_operation().unwrap_or(""),
        }
    }

    /// Whether this tool is the one-shot terminal surface.
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::CandidateSubmit | Self::QualitySubmitReview | Self::WorkComplete
        )
    }

    const fn defers_without_daemon(self) -> bool {
        matches!(self, Self::WorkComplete)
    }

    /// Whether the operation receives host-owned retry identity fields.
    pub const fn is_mutating(self) -> bool {
        matches!(
            self,
            Self::ArtifactSeal
                | Self::PublicationCreate
                | Self::ProductSubmitTicket
                | Self::CandidateCheckpointRegression
                | Self::CandidateSubmit
                | Self::QualityRunFullSuite
                | Self::QualitySubmitReview
        )
    }
}

impl fmt::Display for ToolName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A future returned by an explicit framed daemon adapter.
pub type DaemonFuture<'a> =
    Pin<Box<dyn Future<Output = Result<JsonValue, DaemonError>> + Send + 'a>>;

/// The narrow transport seam owned by the host's inherited FD implementation.
pub trait FramedDaemon: Send + Sync {
    /// Send one already-normalized operation payload and validate its response
    /// in the implementation of this trait. `operation` is never model data.
    fn call<'a>(&'a self, operation: &'static str, payload: JsonValue) -> DaemonFuture<'a>;
}

/// A daemon failure that must not be passed verbatim into model context.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonError {
    /// Closed daemon error code, when one was supplied.
    pub code: Option<String>,
    /// Host-only diagnostic text.
    pub message: String,
}

impl DaemonError {
    /// Construct a host transport/service failure.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            code: None,
            message: message.into(),
        }
    }
}

impl fmt::Display for DaemonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(code) = &self.code {
            write!(formatter, "{code}: {}", self.message)
        } else {
            formatter.write_str(&self.message)
        }
    }
}

impl std::error::Error for DaemonError {}

/// Monotonic command context minted by the admitted daemon session.
#[derive(Clone, Debug)]
pub struct CommandContext {
    /// Expected aggregate revision for the current actor connection.
    session_revision: Arc<AtomicU64>,
    next_command_id: Arc<AtomicU64>,
}

impl CommandContext {
    /// Construct a context at the daemon-admitted revision.
    pub fn new(session_revision: u64) -> Self {
        Self {
            session_revision: Arc::new(AtomicU64::new(session_revision)),
            next_command_id: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Return the latest daemon aggregate revision observed by this actor.
    pub fn current_revision(&self) -> u64 {
        self.session_revision.load(Ordering::Acquire)
    }

    /// Advance the actor revision after a successful daemon mutation.
    pub fn advance_revision(&self, revision: u64) {
        self.session_revision.fetch_max(revision, Ordering::AcqRel);
    }

    fn next_command_id(&self, tool: ToolName) -> String {
        let ordinal = self.next_command_id.fetch_add(1, Ordering::Relaxed) + 1;
        format!("actor-{}-{ordinal}", tool.as_str())
    }
}

/// One terminal call retained until the agent run has settled.
#[derive(Clone, Debug, PartialEq)]
pub struct DeferredTerminal {
    /// Terminal tool selected by the actor.
    pub tool: ToolName,
    /// Exact normalized payload to submit after settlement.
    pub payload: JsonValue,
}

/// Single-terminal-operation gate. It is intentionally independent of the
/// agent scheduler so host settlement can inspect and submit it exactly once.
#[derive(Debug, Default)]
pub struct TerminalDeferral {
    legal: BTreeSet<ToolName>,
    pending: Mutex<Option<DeferredTerminal>>,
}

impl TerminalDeferral {
    /// Construct a gate from packet-admitted terminal operation names.
    pub fn new<I>(legal: I) -> Self
    where
        I: IntoIterator<Item = ToolName>,
    {
        Self {
            legal: legal.into_iter().collect(),
            pending: Mutex::new(None),
        }
    }

    /// Return whether a terminal operation is admitted by the packet.
    pub fn allows(&self, tool: ToolName) -> bool {
        tool.is_terminal() && self.legal.contains(&tool)
    }

    /// Defer a legal terminal operation, rejecting duplicate invocation.
    pub fn defer(&self, tool: ToolName, payload: JsonValue) -> Result<(), TerminalError> {
        if !self.allows(tool) {
            return Err(TerminalError::Illegal(tool));
        }
        let mut pending = self.pending.lock().map_err(|_| TerminalError::Poisoned)?;
        if pending.is_some() {
            return Err(TerminalError::Duplicate);
        }
        *pending = Some(DeferredTerminal { tool, payload });
        Ok(())
    }

    /// Take the one deferred terminal payload for kernel submission.
    pub fn take(&self) -> Option<DeferredTerminal> {
        self.pending.lock().ok()?.take()
    }

    /// Inspect the pending operation without consuming it.
    pub fn pending(&self) -> Option<DeferredTerminal> {
        self.pending.lock().ok()?.clone()
    }
}

/// Terminal deferral failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalError {
    /// The packet did not admit this terminal operation.
    Illegal(ToolName),
    /// More than one terminal operation was requested.
    Duplicate,
    /// The host mutex was poisoned after an unrecoverable panic.
    Poisoned,
}

impl fmt::Display for TerminalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Illegal(tool) => write!(formatter, "terminal operation {tool} is not admitted"),
            Self::Duplicate => formatter.write_str("duplicate terminal operation invocation"),
            Self::Poisoned => formatter.write_str("terminal state is unavailable"),
        }
    }
}

/// Optional host-local implementation for workspace write/edit/search/list
/// and shell. The implementation must already be rooted in the kernel-created
/// worktree; this trait contains no path or process discovery fallback.
pub trait LocalToolExecutor: Send + Sync {
    /// Execute one local tool with a host-owned cancellation scope.
    fn invoke<'a>(
        &'a self,
        tool: ToolName,
        arguments: JsonValue,
        cancellation: CancellationToken,
    ) -> DaemonFuture<'a>;
}

/// An admitted Luau tool after host binding.
#[derive(Clone, Debug, PartialEq)]
pub struct BoundTool {
    /// Model-facing name.
    pub name: ToolName,
    /// Prompt-facing description copied from sealed Luau policy.
    pub description: String,
    /// Closed JSON schema copied from sealed Luau policy.
    pub schema: JsonValue,
    /// Policy-selected execution mode (`sequential` or `parallel`).
    pub execution_mode: String,
}

/// A policy/packet admission failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicyBindingError(pub String);

impl fmt::Display for PolicyBindingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for PolicyBindingError {}

/// Validate sealed Luau declarations against an exact packet allowlist.
///
/// The slice type is intentionally generic at the host boundary: callers pass
/// `LuaPolicy::tools()` directly. No policy source is read from disk here.
pub fn bind_policy(
    policy_tools: &[pi_agent_luau::PolicyTool],
    packet_tools: &[ToolName],
) -> Result<Vec<BoundTool>, PolicyBindingError> {
    let packet: BTreeSet<_> = packet_tools.iter().copied().collect();
    if packet.len() != packet_tools.len() {
        return Err(PolicyBindingError(
            "packet tool allowlist contains duplicates".into(),
        ));
    }
    let mut seen = BTreeSet::new();
    let mut bound = Vec::with_capacity(policy_tools.len());
    for declaration in policy_tools {
        let name = ToolName::parse(&declaration.name).ok_or_else(|| {
            PolicyBindingError(format!(
                "policy declares unknown tool {:?}",
                declaration.name
            ))
        })?;
        if !packet.contains(&name) {
            return Err(PolicyBindingError(format!(
                "policy tool {name} is not present in the packet allowlist"
            )));
        }
        if !seen.insert(name) {
            return Err(PolicyBindingError(format!(
                "policy declares duplicate tool {name}"
            )));
        }
        if declaration.capability != FACTORY_CAPABILITY {
            return Err(PolicyBindingError(format!(
                "tool {name} requests unbound capability {:?}",
                declaration.capability
            )));
        }
        if declaration.description.trim().is_empty() {
            return Err(PolicyBindingError(format!(
                "tool {name} has an empty description"
            )));
        }
        validate_schema_shape(&declaration.schema).map_err(|error| {
            PolicyBindingError(format!("tool {name} has invalid schema: {error}"))
        })?;
        bound.push(BoundTool {
            name,
            description: declaration.description.clone(),
            schema: declaration.schema.clone(),
            execution_mode: format!("{:?}", declaration.execution_mode).to_lowercase(),
        });
    }
    if seen != packet {
        let missing = packet
            .difference(&seen)
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        return Err(PolicyBindingError(format!(
            "policy does not declare packet tools: {}",
            missing.join(", ")
        )));
    }
    Ok(bound)
}

/// The explicit host capability used by every bound Luau handler.
pub struct FactoryCapability<C> {
    daemon: Arc<C>,
    tools: BTreeMap<ToolName, BoundTool>,
    command_context: CommandContext,
    terminal: Arc<TerminalDeferral>,
    local: Option<Arc<dyn LocalToolExecutor>>,
}

impl<C> fmt::Debug for FactoryCapability<C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FactoryCapability")
            .field("tools", &self.tools.keys().collect::<Vec<_>>())
            .field("session_revision", &self.command_context.current_revision())
            .finish_non_exhaustive()
    }
}

impl<C> FactoryCapability<C>
where
    C: FramedDaemon + 'static,
{
    /// Construct a capability from declarations admitted by [`bind_policy`].
    pub fn new(
        daemon: Arc<C>,
        tools: Vec<BoundTool>,
        command_context: CommandContext,
        terminal: Arc<TerminalDeferral>,
        local: Option<Arc<dyn LocalToolExecutor>>,
    ) -> Self {
        Self {
            daemon,
            tools: tools.into_iter().map(|tool| (tool.name, tool)).collect(),
            command_context,
            terminal,
            local,
        }
    }

    /// Return the terminal gate shared with host settlement.
    pub fn terminal(&self) -> &Arc<TerminalDeferral> {
        &self.terminal
    }

    /// Return the bound prompt tools in deterministic name order.
    pub fn tools(&self) -> Vec<BoundTool> {
        self.tools.values().cloned().collect()
    }

    async fn invoke_request(
        &self,
        request: CapabilityRequest,
        cancellation: CancellationToken,
    ) -> Result<CapabilityResponse, CapabilityError> {
        let name =
            ToolName::parse(&request.tool_name).ok_or_else(|| CapabilityError::MethodDenied {
                capability: request.capability.clone(),
                method: request.method.clone(),
            })?;
        let declaration = self
            .tools
            .get(&name)
            .ok_or_else(|| CapabilityError::MethodDenied {
                capability: request.capability.clone(),
                method: request.method.clone(),
            })?;
        if request.capability != FACTORY_CAPABILITY
            || request.method != name.capability_method()
            || declaration.name != name
        {
            return Err(CapabilityError::MethodDenied {
                capability: request.capability,
                method: request.method,
            });
        }
        validate_json_schema(&declaration.schema, &request.arguments).map_err(|error| {
            CapabilityError::InvalidArguments {
                message: error.to_string(),
            }
        })?;
        if cancellation.is_cancelled() {
            return Err(CapabilityError::Cancelled);
        }

        let payload = normalize_wire_input(name, request.arguments, &self.command_context)
            .map_err(|error| CapabilityError::InvalidArguments { message: error })?;
        if name.defers_without_daemon() {
            self.terminal
                .defer(name, payload)
                .map_err(|error| CapabilityError::Execution {
                    message: error.to_string(),
                })?;
            return Ok(CapabilityResponse {
                value: capability_result(
                    name,
                    JsonValue::object([("accepted", JsonValue::Bool(true))]),
                )
                .map_err(|message| CapabilityError::Execution { message })?,
            });
        }
        let value = if let Some(operation) = name.daemon_operation() {
            let value = self
                .daemon
                .call(operation, payload.clone())
                .await
                .map_err(|error| CapabilityError::Execution {
                    message: task_diagnostic(name, &error.to_string()),
                })?;
            validate_daemon_response(operation, &value).map_err(|error| {
                CapabilityError::Execution {
                    message: task_diagnostic(name, &error),
                }
            })?;
            if matches!(
                operation,
                "artifact.seal_workspace_file" | "session.seal_artifact"
            ) && let Some(revision) = value.get("aggregate_revision").and_then(JsonValue::as_u64)
            {
                self.command_context.advance_revision(revision);
            }
            if name.is_terminal() {
                self.terminal
                    .defer(name, payload)
                    .map_err(|error| CapabilityError::Execution {
                        message: error.to_string(),
                    })?;
            }
            value
        } else {
            let local = self
                .local
                .as_ref()
                .ok_or_else(|| CapabilityError::MethodDenied {
                    capability: FACTORY_CAPABILITY.into(),
                    method: request.method,
                })?;
            local
                .invoke(name, payload, cancellation)
                .await
                .map_err(|error| CapabilityError::Execution {
                    message: task_diagnostic(name, &error.to_string()),
                })?
        };
        Ok(CapabilityResponse {
            value: capability_result(name, value)
                .map_err(|message| CapabilityError::Execution { message })?,
        })
    }
}

impl<C> LuauCapability for FactoryCapability<C>
where
    C: FramedDaemon + 'static,
{
    fn invoke(
        &self,
        request: CapabilityRequest,
        cancellation: CancellationToken,
    ) -> CapabilityFuture {
        // The handler's future must own the capability object. Cloning the
        // explicit state avoids borrowing a host mutex across an await.
        let capability = Arc::new(self.clone_for_future());
        Box::pin(async move { capability.invoke_request(request, cancellation).await })
    }
}

impl<C> FactoryCapability<C>
where
    C: FramedDaemon + 'static,
{
    fn clone_for_future(&self) -> Self {
        Self {
            daemon: Arc::clone(&self.daemon),
            tools: self.tools.clone(),
            command_context: self.command_context.clone(),
            terminal: Arc::clone(&self.terminal),
            local: self.local.clone(),
        }
    }
}

fn normalize_wire_input(
    name: ToolName,
    mut input: JsonValue,
    command_context: &CommandContext,
) -> Result<JsonValue, String> {
    let object = input
        .as_object_mut()
        .ok_or_else(|| "tool arguments must be an object".to_owned())?;
    match name {
        ToolName::ForumListTopics => {
            default_string(object, "cursor");
        }
        ToolName::ForumListThreads => {
            default_string(object, "cursor");
        }
        ToolName::ForumSearch => {
            default_string(object, "cursor");
            object.insert("author_office".into(), JsonValue::Null);
            if let Some(kind) = object.get("post_kind").cloned() {
                let value = match kind.as_str() {
                    Some("Note") => 0,
                    Some("Question") => 1,
                    Some("Finding") => 2,
                    Some("Proposal") => 3,
                    Some("Challenge") => 4,
                    Some("Correction") => 5,
                    Some("DecisionLink") => 6,
                    Some(_) | None => return Err("post_kind is not a closed Forum kind".into()),
                };
                object.insert(
                    "post_kind".into(),
                    JsonValue::number(JsonNumber::Unsigned(value as u64))
                        .map_err(|e| e.to_string())?,
                );
            } else {
                object.insert("post_kind".into(), JsonValue::Null);
            }
        }
        ToolName::ForumReadThread => {
            object.entry("after_post_id".into()).or_insert_with(|| {
                JsonValue::number(JsonNumber::Unsigned(0)).expect("zero is finite")
            });
        }
        ToolName::ProductSubmitTicket => {
            let first = object
                .get("reproducer")
                .and_then(JsonValue::as_object)
                .and_then(|reproducer| reproducer.get("first_observation"))
                .cloned();
            if let Some(first) = first
                && let Some(reproducer) = object
                    .get_mut("reproducer")
                    .and_then(JsonValue::as_object_mut)
            {
                reproducer.insert("second_observation".into(), first);
            }
        }
        _ => {}
    }
    if name.is_mutating() {
        object.insert(
            "client_command_id".into(),
            JsonValue::String(command_context.next_command_id(name)),
        );
        if name != ToolName::PublicationCreate {
            object.insert(
                "expected_revision".into(),
                JsonValue::number(JsonNumber::Unsigned(command_context.current_revision()))
                    .map_err(|error| error.to_string())?,
            );
        }
    }
    Ok(input)
}

fn default_string(object: &mut BTreeMap<String, JsonValue>, key: &str) {
    object
        .entry(key.to_owned())
        .or_insert_with(|| JsonValue::String(String::new()));
}

/// Remove protocol and organizational metadata before a result enters model
/// context. This is a diagnostic projection, not a replacement for sealed
/// kernel evidence.
pub fn model_visible_tool_result(name: ToolName, value: JsonValue) -> JsonValue {
    let visible = strip_hidden_fields(value);
    if matches!(name, ToolName::ProductSubmitTicket | ToolName::WorkComplete)
        && matches!(&visible, JsonValue::Object(map) if map.is_empty())
    {
        return JsonValue::object([(String::from("accepted"), JsonValue::Bool(true))]);
    }
    if matches!(name, ToolName::ForumSearch | ToolName::ForumReadThread) {
        return translate_forum_kinds(visible);
    }
    visible
}

fn capability_result(name: ToolName, value: JsonValue) -> Result<JsonValue, String> {
    let visible = model_visible_tool_result(name, value);
    let details_json = visible
        .to_json_string()
        .map_err(|error| error.to_string())?;
    let content = model_result_content(name, &visible);
    Ok(JsonValue::object([
        ("content", JsonValue::String(content)),
        ("details_json", JsonValue::String(details_json)),
        ("is_error", JsonValue::Bool(false)),
        ("terminate", JsonValue::Bool(name.is_terminal())),
    ]))
}

fn model_result_content(name: ToolName, value: &JsonValue) -> String {
    if matches!(name, ToolName::WorkspaceRead | ToolName::ArtifactRead)
        && let Some(encoded) = value.get("content_base64").and_then(JsonValue::as_str)
        && let Some(bytes) = crate::admission::decode_base64(encoded)
        && let Ok(content) = String::from_utf8(bytes)
    {
        return content;
    }
    value.to_json_string().unwrap_or_else(|_| "{}".to_owned())
}

fn strip_hidden_fields(value: JsonValue) -> JsonValue {
    match value {
        JsonValue::Array(values) => {
            JsonValue::Array(values.into_iter().map(strip_hidden_fields).collect())
        }
        JsonValue::Object(values) => JsonValue::Object(
            values
                .into_iter()
                .filter(|(key, _)| !hidden_result_field(key))
                .map(|(key, value)| (key, strip_hidden_fields(value)))
                .collect(),
        ),
        value => value,
    }
}

fn hidden_result_field(key: &str) -> bool {
    matches!(
        key,
        "protocol_version"
            | "request_id"
            | "operation"
            | "audit_id"
            | "aggregate_revision"
            | "author_kind"
            | "author_office"
    ) || key.split('_').any(|part| {
        matches!(
            part,
            "architect"
                | "campaign"
                | "company"
                | "control"
                | "daemon"
                | "director"
                | "factory"
                | "kernel"
                | "office"
                | "sponsor"
        )
    })
}

fn translate_forum_kinds(value: JsonValue) -> JsonValue {
    match value {
        JsonValue::Object(mut object) => {
            if let Some(JsonValue::Array(items)) = object.remove("items") {
                let items = items
                    .into_iter()
                    .map(|item| match item {
                        JsonValue::Object(mut item) => {
                            if let Some(JsonValue::Number(number)) = item.get("kind") {
                                let label = match number {
                                    JsonNumber::Unsigned(0) | JsonNumber::Signed(0) => Some("Note"),
                                    JsonNumber::Unsigned(1) | JsonNumber::Signed(1) => {
                                        Some("Question")
                                    }
                                    JsonNumber::Unsigned(2) | JsonNumber::Signed(2) => {
                                        Some("Finding")
                                    }
                                    JsonNumber::Unsigned(3) | JsonNumber::Signed(3) => {
                                        Some("Proposal")
                                    }
                                    JsonNumber::Unsigned(4) | JsonNumber::Signed(4) => {
                                        Some("Challenge")
                                    }
                                    JsonNumber::Unsigned(5) | JsonNumber::Signed(5) => {
                                        Some("Correction")
                                    }
                                    JsonNumber::Unsigned(6) | JsonNumber::Signed(6) => {
                                        Some("DecisionLink")
                                    }
                                    _ => None,
                                };
                                if let Some(label) = label {
                                    item.insert("kind".into(), JsonValue::String(label.into()));
                                }
                            }
                            JsonValue::Object(item)
                        }
                        item => item,
                    })
                    .collect();
                object.insert("items".into(), JsonValue::Array(items));
            }
            JsonValue::Object(object)
        }
        value => value,
    }
}

/// Produce the bounded correction shown to a model after a daemon rejection.
/// Transport names, credentials, and authority identities never escape this
/// function.
pub fn task_diagnostic(tool: ToolName, detail: &str) -> String {
    let mut sanitized = detail
        .split_whitespace()
        .filter(|word| {
            !word.contains("postgres")
                && !word.contains("Bearer")
                && !word.contains("OPENROUTER_API_KEY")
                && !word.contains("/Users/")
                && !word.contains("/home/")
        })
        .collect::<Vec<_>>()
        .join(" ");
    sanitized = sanitized
        .replace("daemon", "assigned service")
        .replace("kernel", "assigned service");
    sanitized.truncate(480);
    if tool == ToolName::CandidateCheckpointRegression
        && sanitized.contains("all assigned exact reads are required before mutation")
    {
        return "Before retrying the checkpoint, use workspace_read (not shell) on every path listed in the assignment exact-read section, then retry the same checkpoint before editing.".into();
    }
    if tool == ToolName::ProductSubmitTicket {
        if sanitized.contains(
            "Product named a reproducer profile that is not in the admitted application revision",
        ) {
            return "The admitted reproducer profile name is `reproducer`. Set `reproducer_profile` to `reproducer`; it names the profile, not the command bytes. Keep `reproducer.command` as the exact canonical JSON profile supplied in the assignment, then submit again.".into();
        }
        if sanitized.contains("command bytes are not canonical V2 JSON") {
            return "The sealed command artifact must be the canonical JSON profile, not a shell command string. Use the exact profile JSON supplied in the assignment as the command artifact; put the behavioral program only in the sealed stdin artifact.".into();
        }
        if sanitized.contains("sealed reproducer command differs from its named admitted profile") {
            return "The sealed reproducer command must exactly match the assigned `reproducer` profile. Use the exact canonical JSON profile supplied in the assignment as the command artifact; keep the target executable only inside the sealed stdin program, then submit again.".into();
        }
        if sanitized.contains("ticket contract reads paths must be unique") {
            return "Use one `contract_reads` entry per repository path. Combine relevant constraints from the same file in that entry's reason, then submit again.".into();
        }
        if sanitized.contains("ticket contract read reason") {
            return "Each `contract_reads` reason must be nonempty, contain no NUL, and fit within 240 UTF-8 bytes. Tighten the reason without changing its path, then submit again.".into();
        }
    }
    if sanitized.is_empty() {
        format!("The assigned {tool} operation failed.")
    } else {
        match tool {
            ToolName::CandidateCheckpointRegression => format!(
                "The regression checkpoint was rejected: {sanitized}. Do not edit until the checkpoint succeeds."
            ),
            ToolName::CandidateSubmit => format!("Candidate submission did not pass: {sanitized}."),
            ToolName::QualityRunFullSuite => {
                format!("Quality full-suite execution did not pass: {sanitized}.")
            }
            ToolName::ProductSubmitTicket => format!(
                "Ticket submission did not pass: {sanitized}. Correct the stated field and submit again."
            ),
            _ => format!("The assigned {tool} operation failed: {sanitized}."),
        }
    }
}

/// Validate the response shape for one exact framed operation.  The framed
/// transport implementation is still responsible for identity, frame size,
/// and error-envelope checks; this second check prevents a malformed success
/// from becoming model-visible even when a transport adapter is replaced in a
/// test or by a future local implementation.
pub fn validate_daemon_response(operation: &str, value: &JsonValue) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{operation} response must be an object"))?;
    let require = |field: &str| {
        if object.contains_key(field) {
            Ok(())
        } else {
            Err(format!("{operation} response is missing {field}"))
        }
    };
    let string = |field: &str| {
        require(field)?;
        if object.get(field).and_then(JsonValue::as_str).is_some() {
            Ok(())
        } else {
            Err(format!("{operation} response requires string {field}"))
        }
    };
    let integer = |field: &str| {
        require(field)?;
        if matches!(
            object.get(field),
            Some(JsonValue::Number(JsonNumber::Signed(value))) if *value >= 0
        ) || matches!(
            object.get(field),
            Some(JsonValue::Number(JsonNumber::Unsigned(_)))
        ) {
            Ok(())
        } else {
            Err(format!(
                "{operation} response requires nonnegative integer {field}"
            ))
        }
    };
    match operation {
        "workspace.read" => {
            string("canonical_path")?;
            string("blake3")?;
            integer("byte_length")?;
            string("content_base64")?;
        }
        "session.verify_packet" => {
            string("packet_digest")?;
            require("verified")?;
            if object.get("verified").and_then(JsonValue::as_bool) != Some(true) {
                return Err("session.verify_packet response was not verified".into());
            }
        }
        "artifact.seal_workspace_file" | "session.seal_artifact" => {
            integer("artifact_id")?;
            string("digest")?;
            integer("byte_length")?;
            integer("aggregate_revision")?;
        }
        "artifact.read" => {
            integer("artifact_id")?;
            string("digest")?;
            integer("byte_length")?;
            string("content_base64")?;
        }
        "session.submit_terminal" => {
            integer("audit_id")?;
            integer("aggregate_revision")?;
        }
        "candidate.checkpoint_regression" => {
            string("regression_tree")?;
            integer("regression_patch_artifact_id")?;
            integer("regression_command_set_artifact_id")?;
            integer("regression_log_artifact_id")?;
        }
        "candidate.submit" => {
            integer("audit_id")?;
            integer("aggregate_revision")?;
            integer("candidate_id")?;
            integer("validation_id")?;
            string("candidate_tree")?;
        }
        "quality.run_full_suite" => {
            integer("audit_id")?;
            integer("aggregate_revision")?;
            integer("validation_id")?;
            integer("candidate_id")?;
            string("candidate_tree")?;
        }
        "quality.submit_review" => {
            integer("audit_id")?;
            integer("aggregate_revision")?;
            integer("review_id")?;
            integer("candidate_id")?;
            require("verdict")?;
            if !matches!(
                object.get("verdict").and_then(JsonValue::as_str),
                Some("accept" | "reject")
            ) {
                return Err("quality.submit_review response has invalid verdict".into());
            }
        }
        "publication.create" => {
            integer("audit_id")?;
            integer("aggregate_revision")?;
            integer("publication_id")?;
            require("was_idempotent_retry")?;
            if object
                .get("was_idempotent_retry")
                .and_then(JsonValue::as_bool)
                .is_none()
            {
                return Err(
                    "publication.create response requires an idempotency replay flag".into(),
                );
            }
        }
        operation if operation.starts_with("forum.") => {
            string("next_cursor")?;
            require("items")?;
            if !matches!(object.get("items"), Some(JsonValue::Array(_))) {
                return Err(format!("{operation} response requires array items"));
            }
        }
        "product.submit_ticket" | "work.complete" => {}
        _ => return Err(format!("unknown framed operation {operation:?}")),
    }
    Ok(())
}

fn validate_schema_shape(schema: &JsonValue) -> Result<(), SchemaError> {
    let object = schema
        .as_object()
        .ok_or_else(|| SchemaError("schema must be an object".into()))?;
    if let Some(type_value) = object.get("type") {
        let valid = match type_value {
            JsonValue::String(kind) => matches!(
                kind.as_str(),
                "null" | "boolean" | "object" | "array" | "string" | "number" | "integer"
            ),
            JsonValue::Array(types) => types.iter().all(|kind| {
                matches!(
                    kind.as_str(),
                    Some("null" | "boolean" | "object" | "array" | "string" | "number" | "integer")
                )
            }),
            _ => false,
        };
        if !valid {
            return Err(SchemaError("schema has an unknown type".into()));
        }
    }
    if let Some(properties) = object.get("properties") {
        let properties = properties
            .as_object()
            .ok_or_else(|| SchemaError("schema properties must be an object".into()))?;
        for schema in properties.values() {
            validate_schema_shape(schema)?;
        }
    }
    if let Some(items) = object.get("items") {
        validate_schema_shape(items)?;
    }
    Ok(())
}

/// Validate the JSON Schema subset used by sealed Factory policies.
pub fn validate_json_schema(schema: &JsonValue, value: &JsonValue) -> Result<(), SchemaError> {
    let object = schema
        .as_object()
        .ok_or_else(|| SchemaError("schema must be an object".into()))?;
    if let Some(enum_values) = object.get("enum").and_then(JsonValue::as_array)
        && !enum_values.iter().any(|candidate| candidate == value)
    {
        return Err(SchemaError("value is not in enum".into()));
    }
    if let Some(type_value) = object.get("type") {
        let valid = match type_value {
            JsonValue::String(kind) => json_type_matches(kind, value),
            JsonValue::Array(types) => types
                .iter()
                .filter_map(JsonValue::as_str)
                .any(|kind| json_type_matches(kind, value)),
            _ => false,
        };
        if !valid {
            return Err(SchemaError("value has the wrong JSON type".into()));
        }
    }
    if let JsonValue::String(string) = value {
        if let Some(minimum) = object.get("minLength").and_then(JsonValue::as_u64)
            && string.chars().count() < minimum as usize
        {
            return Err(SchemaError("string is shorter than minLength".into()));
        }
        if let Some(maximum) = object.get("maxLength").and_then(JsonValue::as_u64)
            && string.chars().count() > maximum as usize
        {
            return Err(SchemaError("string exceeds maxLength".into()));
        }
        if let Some(pattern) = object.get("pattern").and_then(JsonValue::as_str)
            && pattern == "^[a-f0-9]{64}$"
            && (string.len() != 64
                || !string
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()))
        {
            return Err(SchemaError(
                "string does not match the lowercase digest pattern".into(),
            ));
        }
    }
    if let JsonValue::Number(number) = value {
        let numeric = match number {
            JsonNumber::Signed(value) => *value as f64,
            JsonNumber::Unsigned(value) => *value as f64,
            JsonNumber::Float(value) => *value,
        };
        if let Some(minimum) = object.get("minimum").and_then(JsonValue::as_f64)
            && numeric < minimum
        {
            return Err(SchemaError("number is below minimum".into()));
        }
        if let Some(maximum) = object.get("maximum").and_then(JsonValue::as_f64)
            && numeric > maximum
        {
            return Err(SchemaError("number exceeds maximum".into()));
        }
        if object.get("type").and_then(JsonValue::as_str) == Some("integer")
            && matches!(number, JsonNumber::Float(value) if value.fract() != 0.0)
        {
            return Err(SchemaError("number is not an integer".into()));
        }
    }
    if let JsonValue::Object(properties) = value {
        if object
            .get("additionalProperties")
            .and_then(JsonValue::as_bool)
            == Some(false)
            && let Some(allowed) = object.get("properties").and_then(JsonValue::as_object)
            && let Some(unknown) = properties.keys().find(|key| !allowed.contains_key(*key))
        {
            return Err(SchemaError(format!("unknown property {unknown:?}")));
        }
        if let Some(required) = object.get("required").and_then(JsonValue::as_array) {
            for field in required.iter().filter_map(JsonValue::as_str) {
                if !properties.contains_key(field) {
                    return Err(SchemaError(format!("missing required property {field:?}")));
                }
            }
        }
        if let Some(property_schemas) = object.get("properties").and_then(JsonValue::as_object) {
            for (key, value) in properties {
                if let Some(property_schema) = property_schemas.get(key) {
                    validate_json_schema(property_schema, value)?;
                }
            }
        }
    }
    if let JsonValue::Array(values) = value {
        if let Some(maximum) = object.get("maxItems").and_then(JsonValue::as_u64)
            && values.len() > maximum as usize
        {
            return Err(SchemaError("array exceeds maxItems".into()));
        }
        if let Some(minimum) = object.get("minItems").and_then(JsonValue::as_u64)
            && values.len() < minimum as usize
        {
            return Err(SchemaError("array is shorter than minItems".into()));
        }
        if let Some(item_schema) = object.get("items") {
            for value in values {
                validate_json_schema(item_schema, value)?;
            }
        }
    }
    Ok(())
}

/// A bounded schema failure suitable for tests and host logs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchemaError(pub String);

impl fmt::Display for SchemaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SchemaError {}

fn json_type_matches(kind: &str, value: &JsonValue) -> bool {
    match kind {
        "null" => value.is_null(),
        "boolean" => matches!(value, JsonValue::Bool(_)),
        "object" => value.is_object(),
        "array" => matches!(value, JsonValue::Array(_)),
        "string" => matches!(value, JsonValue::String(_)),
        "number" => matches!(value, JsonValue::Number(_)),
        "integer" => matches!(
            value,
            JsonValue::Number(JsonNumber::Signed(_) | JsonNumber::Unsigned(_))
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(values: impl IntoIterator<Item = (&'static str, JsonValue)>) -> JsonValue {
        JsonValue::Object(
            values
                .into_iter()
                .map(|(key, value)| (key.into(), value))
                .collect(),
        )
    }

    #[test]
    fn schema_rejects_unknown_properties_and_bad_digest() {
        let schema = object([
            ("type", JsonValue::String("object".into())),
            ("additionalProperties", JsonValue::Bool(false)),
            (
                "required",
                JsonValue::Array(vec![JsonValue::String("digest".into())]),
            ),
            (
                "properties",
                object([(
                    "digest",
                    object([
                        ("type", JsonValue::String("string".into())),
                        ("pattern", JsonValue::String("^[a-f0-9]{64}$".into())),
                    ]),
                )]),
            ),
        ]);
        let bad = object([
            ("digest", JsonValue::String("A".repeat(64))),
            ("extra", JsonValue::Bool(true)),
        ]);
        assert!(validate_json_schema(&schema, &bad).is_err());
    }

    #[test]
    fn forum_result_strips_attribution_and_translates_kind() {
        let value = object([
            ("operation", JsonValue::String("forum.search".into())),
            ("author_office", JsonValue::String("quality".into())),
            (
                "items",
                JsonValue::Array(vec![object([
                    ("kind", JsonValue::Number(JsonNumber::Unsigned(5))),
                    ("body", JsonValue::String("useful".into())),
                ])]),
            ),
        ]);
        let visible = model_visible_tool_result(ToolName::ForumSearch, value);
        assert_eq!(visible.get("author_office"), None);
        assert_eq!(
            visible.get("items").and_then(JsonValue::as_array).unwrap()[0]
                .get("kind")
                .and_then(JsonValue::as_str),
            Some("Correction")
        );
    }

    #[test]
    fn capability_result_matches_luau_handler_contract() {
        let result = capability_result(
            ToolName::WorkspaceRead,
            object([
                ("canonical_path", JsonValue::String("AGENTS.md".into())),
                ("content_base64", JsonValue::String("aGVsbG8=".into())),
            ]),
        )
        .expect("tool result should be serializable");

        assert_eq!(
            result.get("content").and_then(JsonValue::as_str),
            Some("hello")
        );
        assert!(
            result
                .get("details_json")
                .and_then(JsonValue::as_str)
                .is_some_and(|value| value.contains("content_base64"))
        );
        assert_eq!(
            result.get("is_error").and_then(JsonValue::as_bool),
            Some(false)
        );
        assert_eq!(
            result.get("terminate").and_then(JsonValue::as_bool),
            Some(false)
        );
    }

    #[test]
    fn terminal_capability_result_requests_core_termination() {
        let result = capability_result(
            ToolName::WorkComplete,
            object([("accepted", JsonValue::Bool(true))]),
        )
        .expect("terminal result should be serializable");

        assert_eq!(
            result.get("terminate").and_then(JsonValue::as_bool),
            Some(true)
        );
        assert_eq!(
            result.get("content").and_then(JsonValue::as_str),
            Some("{\"accepted\":true}")
        );
    }

    #[test]
    fn terminal_gate_accepts_one_and_rejects_second() {
        let gate = TerminalDeferral::new([ToolName::CandidateSubmit]);
        gate.defer(ToolName::CandidateSubmit, JsonValue::Null)
            .unwrap();
        assert_eq!(
            gate.defer(ToolName::CandidateSubmit, JsonValue::Null),
            Err(TerminalError::Duplicate)
        );
        assert_eq!(gate.take().unwrap().tool, ToolName::CandidateSubmit);
    }

    #[test]
    fn diagnostics_are_bounded_and_do_not_leak_host_identity() {
        let diagnostic = task_diagnostic(
            ToolName::CandidateSubmit,
            "/Users/josh postgres kernel failed",
        );
        assert!(!diagnostic.contains("/Users/josh"));
        assert!(!diagnostic.contains("postgres"));
        assert!(diagnostic.len() <= 512);
    }
}
