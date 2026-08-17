//! One real `pi-agent-core` run assembled from sealed V2 policy and capabilities.
//!
//! The execution layer has no filesystem or provider-discovery path.  Callers supply the
//! admitted packet, policy source bytes (normally obtained through a kernel-owned sealed-artifact
//! path), explicit Luau capability bindings, and an explicit model provider.  The packet tool
//! allowlist and policy declarations must match exactly before an [`Agent`] is built.

use crate::Admission;
use crate::agent_host::{AgentHostError, BareAgentHost};
use crate::tool_bridge::{
    CommandContext, FactoryCapability, FramedDaemon, TerminalDeferral, ToolExecutionDiagnostic,
    ToolName, bind_policy,
};
use factory_protocol::ContentDigest;
use pi_agent_core::agent::Agent;
use pi_agent_core::event::{AgentEvent, AgentEventKind};
use pi_agent_core::hooks::{
    AfterToolCall, BeforeToolCall, ContextEnvelope, HookFuture, HookSet, NextTurn,
};
use pi_agent_core::provider::openai::OpenAiContextHook;
use pi_agent_core::scheduler::ModelProvider;
use pi_agent_core::state::{RunSnapshot, StopReason};
use pi_agent_core::tool::{AgentToolResult, ToolCall, ToolRegistry};
use pi_agent_luau::tool_handler::{
    CapabilityBindings, HandlerLimits, LuaToolHandler, ToolHandlerInitError, ToolHandlerSpec,
};
use pi_agent_luau::{LuaPolicy, LuaPolicyHookSet, PolicyError, PolicyLimits};
use std::fmt;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A caller-owned sealed policy source.  The host accepts bytes, never a path, and verifies the
/// caller's expected CAS digest before evaluating Luau.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SealedPolicySource {
    /// Exact source bytes from the admitted application artifact.
    pub bytes: Vec<u8>,
    /// Expected BLAKE3 digest from the sealed application declaration.
    pub digest: ContentDigest,
}

impl SealedPolicySource {
    /// Construct one source identity without reading any host path.
    pub fn new(bytes: impl Into<Vec<u8>>, digest: ContentDigest) -> Self {
        Self {
            bytes: bytes.into(),
            digest,
        }
    }

    fn text(&self) -> Result<&str, ExecutionError> {
        if ContentDigest::of_bytes(&self.bytes) != self.digest {
            return Err(ExecutionError::PolicyDigestMismatch);
        }
        std::str::from_utf8(&self.bytes).map_err(|_| ExecutionError::PolicySourceUtf8)
    }
}

/// A provider accounting snapshot. The reported amount may be partial while a
/// run is still active; `complete` is required before treating it as the
/// terminal provider total.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CostSnapshot {
    /// Provider-reported spend observed so far, in micro-USD.
    pub reported_micro_usd: Option<u64>,
    /// Whether every completed provider turn has known accounting.
    pub complete: bool,
}

/// Optional provider accounting callback owned by the provider adapter.
pub type CostReader = Arc<dyn Fn() -> CostSnapshot + Send + Sync>;

/// Inputs needed to prepare exactly one actor run.
pub struct ExecutionInput {
    /// Verified V2 packet admitted on FD 0.
    pub admission: Admission,
    /// Explicit provider implementation; no environment lookup occurs here.
    pub provider: Arc<dyn ModelProvider>,
    /// Explicit Luau capability bindings.  An absent binding is a hard error.
    pub capabilities: CapabilityBindings,
    /// Terminal gate shared with the `FactoryCapability` binding.
    pub terminal: Arc<TerminalDeferral>,
    /// Policy VM limits.
    pub policy_limits: PolicyLimits,
    /// Per-handler VM/capability limits.
    pub handler_limits: HandlerLimits,
    /// Optional provider-owned cost snapshot.
    pub cost_reader: Option<CostReader>,
    /// Shared host state for revision identity and bounded assignment phases.
    pub command_context: CommandContext,
}

impl fmt::Debug for ExecutionInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecutionInput")
            .field("assignment_id", &self.admission.packet.assignment_id)
            .field(
                "policy_bytes",
                &self.admission.packet.policy_bytes_b64.len(),
            )
            .field("has_provider", &true)
            .field("capabilities", &self.capabilities)
            .field("terminal", &self.terminal)
            .finish_non_exhaustive()
    }
}

/// A prepared, provider-bound actor run.  Preparation performs policy compilation and tool
/// allowlist checks, but does not contact a provider or daemon and does not start a run.
pub struct PreparedExecution {
    admission: Admission,
    agent: Agent,
    terminal: Arc<TerminalDeferral>,
    cost_reader: Option<CostReader>,
    command_context: CommandContext,
    _policy: Arc<LuaPolicy>,
}

impl fmt::Debug for PreparedExecution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedExecution")
            .field("assignment_id", &self.admission.packet.assignment_id)
            .field("has_model_provider", &self.agent.has_model_provider())
            .field("tools", &self.agent.tool_definitions())
            .field("command_context", &self.command_context)
            .finish_non_exhaustive()
    }
}

/// Construct all policy-backed core tools from sealed source and an exact packet allowlist.
///
/// This helper is public for focused qualification tests and for the host process assembly.  It
/// rejects undeclared, missing, duplicate, or handler-less tools; there is no generic Rust tool
/// fallback when policy compilation fails.
pub fn build_policy_tools(
    policy: &LuaPolicy,
    packet_tools: &[ToolName],
    capabilities: &CapabilityBindings,
    limits: HandlerLimits,
) -> Result<ToolRegistry, ExecutionError> {
    let bound = bind_policy(policy.tools(), packet_tools).map_err(ExecutionError::PolicyBinding)?;
    let mut registry = ToolRegistry::default();
    for declaration in policy.tools() {
        let source = declaration
            .handler_source
            .as_deref()
            .ok_or_else(|| ExecutionError::MissingHandler(declaration.name.clone()))?;
        let spec = ToolHandlerSpec {
            name: declaration.name.clone(),
            description: declaration.description.clone(),
            schema: declaration.schema.clone(),
            capability: declaration.capability.clone(),
            execution_mode: declaration.execution_mode,
        };
        let handler = LuaToolHandler::new_with_limits(source, spec, capabilities.clone(), limits)
            .map_err(|error| ExecutionError::ToolHandler {
            name: declaration.name.clone(),
            error,
        })?;
        registry.insert(Arc::new(handler));
    }
    // `bind_policy` performs the exact set comparison. Keep the local result alive in this
    // function so a future bridge change cannot silently stop proving that every declaration was
    // represented by a core registry entry.
    if bound.len() != registry.names().count() {
        return Err(ExecutionError::PolicyBinding(
            crate::tool_bridge::PolicyBindingError(
                "policy binding and core registry counts differ".to_owned(),
            ),
        ));
    }
    Ok(registry)
}

impl ExecutionInput {
    /// Prepare one provider-bound run from immutable input values.
    pub fn prepare(self) -> Result<PreparedExecution, ExecutionError> {
        let policy = Arc::new(load_packet_policy(&self.admission, self.policy_limits)?);
        let packet_tools = packet_tool_names(&self.admission)?;
        for operation in &self.admission.packet.terminal_operations {
            let tool = ToolName::parse(operation)
                .ok_or_else(|| ExecutionError::UnknownPacketTool(operation.clone()))?;
            if !self.terminal.allows(tool) {
                return Err(ExecutionError::TerminalBindingMismatch(operation.clone()));
            }
        }
        let registry = build_policy_tools(
            &policy,
            &packet_tools,
            &self.capabilities,
            self.handler_limits,
        )?;

        let bare = BareAgentHost::new(self.admission.clone()).map_err(ExecutionError::Agent)?;
        let snapshot = bare.agent().snapshot();
        let system_prompt = if policy.system_prompt_append().is_empty() {
            snapshot.system_prompt
        } else {
            format!(
                "{}\n\n{}",
                snapshot.system_prompt,
                policy.system_prompt_append()
            )
        };
        let model = snapshot
            .model
            .ok_or(ExecutionError::Agent(AgentHostError::MissingModel))?;
        if self.admission.packet.assignment_role == "engineering" {
            self.command_context.configure_engineering();
        }
        let mut builder = Agent::builder()
            .system_prompt(system_prompt)
            .model(model)
            .thinking_level(snapshot.thinking_level)
            .tools(registry)
            .model_provider(self.provider)
            .hooks(phase_hook_set(
                factory_hook_set(Arc::clone(&policy)),
                self.command_context.clone(),
            ));
        for message in snapshot.host_messages {
            builder = builder.host_message(message);
        }
        Ok(PreparedExecution {
            admission: self.admission,
            agent: builder.build(),
            terminal: self.terminal,
            cost_reader: self.cost_reader,
            command_context: self.command_context,
            _policy: policy,
        })
    }
}

fn factory_hook_set(policy: Arc<LuaPolicy>) -> Arc<dyn HookSet> {
    Arc::new(LuaPolicyHookSet::new(policy, Arc::new(OpenAiContextHook)))
}

fn phase_hook_set(inner: Arc<dyn HookSet>, command_context: CommandContext) -> Arc<dyn HookSet> {
    Arc::new(PhaseHookSet {
        inner,
        command_context,
    })
}

struct PhaseHookSet {
    inner: Arc<dyn HookSet>,
    command_context: CommandContext,
}

impl HookSet for PhaseHookSet {
    fn before_tool_call(
        &self,
        call: &ToolCall,
    ) -> Result<BeforeToolCall, pi_agent_core::error::HookError> {
        self.inner.before_tool_call(call)
    }

    fn after_tool_call(
        &self,
        call: &ToolCall,
        result: &AgentToolResult,
    ) -> Result<AfterToolCall, pi_agent_core::error::HookError> {
        self.inner.after_tool_call(call, result)
    }

    fn transform_context(
        &self,
        context: ContextEnvelope,
    ) -> Result<ContextEnvelope, pi_agent_core::error::HookError> {
        self.inner.transform_context(context)
    }

    fn convert_to_llm(
        &self,
        context: ContextEnvelope,
    ) -> Result<String, pi_agent_core::error::HookError> {
        self.inner.convert_to_llm(context)
    }

    fn should_stop_after_turn(
        &self,
        context: &ContextEnvelope,
    ) -> Result<bool, pi_agent_core::error::HookError> {
        Ok(self.command_context.engineering_should_stop_after_turn()
            || self.inner.should_stop_after_turn(context)?)
    }

    fn prepare_next_turn(
        &self,
        context: ContextEnvelope,
    ) -> Result<NextTurn, pi_agent_core::error::HookError> {
        self.inner.prepare_next_turn(context)
    }

    fn before_tool_call_async<'a>(
        &'a self,
        call: &'a ToolCall,
        context: ContextEnvelope,
        cancellation: pi_agent_core::scheduler::CancellationToken,
    ) -> HookFuture<'a, BeforeToolCall> {
        self.inner
            .before_tool_call_async(call, context, cancellation)
    }

    fn after_tool_call_async<'a>(
        &'a self,
        call: &'a ToolCall,
        result: &'a AgentToolResult,
        context: ContextEnvelope,
        cancellation: pi_agent_core::scheduler::CancellationToken,
    ) -> HookFuture<'a, AfterToolCall> {
        self.inner
            .after_tool_call_async(call, result, context, cancellation)
    }

    fn transform_context_async(
        &self,
        context: ContextEnvelope,
        cancellation: pi_agent_core::scheduler::CancellationToken,
    ) -> HookFuture<'_, ContextEnvelope> {
        self.inner.transform_context_async(context, cancellation)
    }

    fn convert_to_llm_async(
        &self,
        context: ContextEnvelope,
        cancellation: pi_agent_core::scheduler::CancellationToken,
    ) -> HookFuture<'_, String> {
        self.inner.convert_to_llm_async(context, cancellation)
    }

    fn should_stop_after_turn_async<'a>(
        &'a self,
        context: &'a ContextEnvelope,
        cancellation: pi_agent_core::scheduler::CancellationToken,
    ) -> HookFuture<'a, bool> {
        let inner = self
            .inner
            .should_stop_after_turn_async(context, cancellation);
        let command_context = self.command_context.clone();
        Box::pin(async move {
            Ok(command_context.engineering_should_stop_after_turn() || inner.await?)
        })
    }

    fn prepare_next_turn_async(
        &self,
        context: ContextEnvelope,
        cancellation: pi_agent_core::scheduler::CancellationToken,
    ) -> HookFuture<'_, NextTurn> {
        self.inner.prepare_next_turn_async(context, cancellation)
    }
}

/// Assemble the normal Factory capability and execution input for the FD 0 host integration.
///
/// The caller supplies only daemon-bound transport and the already-rooted local executor. This
/// helper decodes the inline packet policy, proves the exact policy/packet tool set, and binds
/// the resulting declarations to one shared terminal gate before returning the provider-bound
/// [`ExecutionInput`].
pub fn build_factory_execution_input<C>(
    admission: Admission,
    provider: Arc<dyn ModelProvider>,
    daemon: Arc<C>,
    command_context: crate::tool_bridge::CommandContext,
    terminal: Arc<TerminalDeferral>,
    local: Option<Arc<dyn crate::tool_bridge::LocalToolExecutor>>,
    policy_limits: PolicyLimits,
    handler_limits: HandlerLimits,
    cost_reader: Option<CostReader>,
) -> Result<ExecutionInput, ExecutionError>
where
    C: FramedDaemon + 'static,
{
    let policy = load_packet_policy(&admission, policy_limits)?;
    let packet_tools = packet_tool_names(&admission)?;
    let bound =
        bind_policy(policy.tools(), &packet_tools).map_err(ExecutionError::PolicyBinding)?;
    let capability = Arc::new(FactoryCapability::new(
        daemon,
        bound,
        command_context.clone(),
        Arc::clone(&terminal),
        local,
    ));
    let capabilities = factory_capability_bindings(capability)
        .map_err(|error| ExecutionError::CapabilityBinding(error.to_string()))?;
    Ok(ExecutionInput {
        admission,
        provider,
        capabilities,
        terminal,
        policy_limits,
        handler_limits,
        cost_reader,
        command_context,
    })
}

impl PreparedExecution {
    /// Borrow the verified packet used to construct this run.
    #[must_use]
    pub fn admission(&self) -> &Admission {
        &self.admission
    }

    /// Borrow the provider-bound agent for qualification inspection.
    #[must_use]
    pub fn agent(&self) -> &Agent {
        &self.agent
    }

    /// Borrow the one-shot terminal deferral gate.
    #[must_use]
    pub fn terminal(&self) -> &Arc<TerminalDeferral> {
        &self.terminal
    }

    /// Drive the single assignment from the caller-owned executor.
    pub async fn drive(&self) -> Result<ExecutionResult, ExecutionError> {
        let prompt = decode_prompt(&self.admission.packet.assignment_prompt_bytes_b64)?;
        let handle = self
            .agent
            .start_prompt(prompt)
            .map_err(ExecutionError::Core)?;
        let cancellation = handle.cancellation();
        let cost_limit_reached = Arc::new(AtomicBool::new(false));
        let cost_monitor = self.cost_reader.as_ref().map(|reader| {
            let reader = Arc::clone(reader);
            let cancellation = cancellation.clone();
            let reached = Arc::clone(&cost_limit_reached);
            let allowance = self
                .admission
                .packet
                .remaining_campaign_allowance_micro_usd;
            smol::spawn(async move {
                loop {
                    if cancellation.is_cancelled() {
                        break;
                    }
                    let snapshot = reader();
                    if snapshot
                        .reported_micro_usd
                        .is_some_and(|cost| cost >= allowance)
                    {
                        reached.store(true, Ordering::Release);
                        cancellation.cancel();
                        break;
                    }
                    smol::Timer::after(factory_settings::PROVIDER_COST_POLL_INTERVAL).await;
                }
            })
        });
        let drive_result = handle.drive().await;
        if let Some(monitor) = cost_monitor {
            monitor.cancel().await;
        }
        let snapshot = handle.snapshot();
        let events = handle.events();
        if let Err(error) = drive_result {
            eprintln!(
                "factory-pi-host execution failure: engineering_phase={:?} core_phase={:?} event_count={} error={error}",
                self.command_context.engineering_diagnostics(),
                snapshot.phase,
                events.len(),
            );
            if !matches!(error, pi_agent_core::CoreError::Cancelled) {
                return Err(ExecutionError::Core(error));
            }
        }
        Ok(ExecutionResult::from_run(
            snapshot,
            events,
            self.terminal.pending(),
            self.cost_reader
                .as_ref()
                .and_then(|reader| {
                    let snapshot = reader();
                    snapshot
                        .complete
                        .then_some(snapshot.reported_micro_usd)
                        .flatten()
                }),
            cost_limit_reached.load(Ordering::Acquire),
            &self.command_context,
        ))
    }
}

/// Observable result of one settled agent run.
#[derive(Clone, Debug, PartialEq)]
pub struct ExecutionResult {
    /// Agent lifecycle events in core sequence order.
    pub events: Vec<AgentEvent>,
    /// Final core run snapshot.
    pub run: RunSnapshot,
    /// Deferred terminal operation, if policy/model selected one.
    pub terminal: Option<crate::tool_bridge::DeferredTerminal>,
    /// Aggregate usage attached by explicit tool capabilities. Model usage remains absent until
    /// the provider/core observer surface reports it; absence is not interpreted as zero.
    pub usage: UsageSummary,
    /// Provider-reported cost, if the explicit accounting callback had a known value.
    pub cost_micro_usd: Option<u64>,
    /// Whether the host cancelled this run after live provider spend reached its allowance.
    pub cost_limit_reached: bool,
    /// Bounded run diagnostics retained for operator and transcript inspection.
    pub diagnostics: ExecutionDiagnostics,
}

/// Observable limits and progress for one prepared actor run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionDiagnostics {
    /// Number of core turn-start events emitted by the run.
    pub turns_started: u32,
    /// Controller-owned Engineering phase at terminal reconciliation.
    pub engineering_phase: String,
    /// Host-boundary timing and failure counts for completed tool executions.
    pub tool_executions: Vec<ToolExecutionDiagnostic>,
}

impl ExecutionResult {
    fn from_run(
        run: RunSnapshot,
        events: Vec<AgentEvent>,
        terminal: Option<crate::tool_bridge::DeferredTerminal>,
        cost_micro_usd: Option<u64>,
        cost_limit_reached: bool,
        command_context: &CommandContext,
    ) -> Self {
        let usage = UsageSummary::from_events(&events);
        let turns_started = events
            .iter()
            .filter(|event| matches!(event.kind, AgentEventKind::TurnStart { .. }))
            .count() as u32;
        let diagnostics = ExecutionDiagnostics {
            turns_started,
            engineering_phase: command_context.engineering_diagnostics().name().to_owned(),
            tool_executions: command_context.tool_execution_diagnostics(),
        };
        Self {
            events,
            run,
            terminal,
            usage,
            cost_micro_usd,
            cost_limit_reached,
            diagnostics,
        }
    }

    /// Whether the core reached a normal terminal run state.
    #[must_use]
    pub fn settled(&self) -> bool {
        self.run.phase.is_terminal()
    }

    /// Return the final stop reason, if the core supplied one.
    #[must_use]
    pub fn stop_reason(&self) -> Option<StopReason> {
        self.run.stop_reason
    }
}

/// Usage counters collected from explicit tool results.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UsageSummary {
    /// Sum of reported input tokens, when present.
    pub input_tokens: Option<u64>,
    /// Sum of reported output tokens, when present.
    pub output_tokens: Option<u64>,
    /// Sum of reported reasoning tokens, when present.
    pub reasoning_tokens: Option<u64>,
}

impl UsageSummary {
    fn from_events(events: &[AgentEvent]) -> Self {
        let mut summary = Self::default();
        for event in events {
            if let AgentEventKind::ToolExecutionEnd { result, .. } = &event.kind
                && let Some(usage) = result.usage.as_ref()
            {
                add(&mut summary.input_tokens, usage.input_tokens);
                add(&mut summary.output_tokens, usage.output_tokens);
                add(&mut summary.reasoning_tokens, usage.reasoning_tokens);
            }
        }
        summary
    }
}

fn add(target: &mut Option<u64>, value: Option<u64>) {
    if let Some(value) = value {
        *target = Some(target.unwrap_or(0).saturating_add(value));
    }
}

fn decode_prompt(value: &str) -> Result<String, ExecutionError> {
    let bytes = decode_base64(value).ok_or(ExecutionError::PromptBase64Invalid)?;
    String::from_utf8(bytes).map_err(|_| ExecutionError::PromptUtf8)
}

fn load_packet_policy(
    admission: &Admission,
    limits: PolicyLimits,
) -> Result<LuaPolicy, ExecutionError> {
    if admission.packet.policy_entrypoint != factory_protocol::PolicyEntrypointV2::FACTORY_POLICY {
        return Err(ExecutionError::PolicyEntrypointInvalid);
    }
    let policy_bytes = decode_base64(&admission.packet.policy_bytes_b64)
        .ok_or(ExecutionError::PolicySourceBase64Invalid)?;
    let policy_digest = ContentDigest::from_str(&admission.packet.policy_digest)
        .map_err(|_| ExecutionError::PolicyDigestMismatch)?;
    let source = SealedPolicySource::new(policy_bytes, policy_digest);
    LuaPolicy::load_with_limits(source.text()?, limits).map_err(ExecutionError::Policy)
}

fn packet_tool_names(admission: &Admission) -> Result<Vec<ToolName>, ExecutionError> {
    admission
        .packet
        .tools
        .iter()
        .map(|name| {
            ToolName::parse(name).ok_or_else(|| ExecutionError::UnknownPacketTool(name.clone()))
        })
        .collect()
}

fn decode_base64(value: &str) -> Option<Vec<u8>> {
    if value.is_empty() || !value.len().is_multiple_of(4) {
        return None;
    }
    let mut output = Vec::with_capacity(value.len() / 4 * 3);
    let (chunks, remainder) = value.as_bytes().as_chunks::<4>();
    debug_assert!(remainder.is_empty(), "base64 length was validated");
    for chunk in chunks {
        let a = b64(chunk[0])?;
        let b = b64(chunk[1])?;
        let c = if chunk[2] == b'=' { 0 } else { b64(chunk[2])? };
        let d = if chunk[3] == b'=' { 0 } else { b64(chunk[3])? };
        if chunk[2] == b'=' && chunk[3] != b'=' {
            return None;
        }
        output.push((a << 2) | (b >> 4));
        if chunk[2] != b'=' {
            output.push((b << 4) | (c >> 2));
        }
        if chunk[3] != b'=' {
            output.push((c << 6) | d);
        }
    }
    Some(output)
}

fn b64(value: u8) -> Option<u8> {
    match value {
        b'A'..=b'Z' => Some(value - b'A'),
        b'a'..=b'z' => Some(value - b'a' + 26),
        b'0'..=b'9' => Some(value - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Failures before a terminal submission can be considered.
#[derive(Debug)]
pub enum ExecutionError {
    /// The packet's sealed Luau policy bytes were not canonical base64.
    PolicySourceBase64Invalid,
    /// The packet named an entrypoint outside the closed V2 policy ABI.
    PolicyEntrypointInvalid,
    /// Packet prompt bytes were not valid canonical base64.
    PromptBase64Invalid,
    /// Packet prompt bytes were not UTF-8.
    PromptUtf8,
    /// Policy source bytes were not UTF-8.
    PolicySourceUtf8,
    /// Policy bytes did not match their supplied digest.
    PolicyDigestMismatch,
    /// Policy VM rejected source or declarations.
    Policy(PolicyError),
    /// Packet included an unknown model-facing tool name.
    UnknownPacketTool(String),
    /// The caller's terminal gate does not admit a packet terminal operation.
    TerminalBindingMismatch(String),
    /// Policy and packet tool sets did not match.
    PolicyBinding(crate::tool_bridge::PolicyBindingError),
    /// The sole factory capability could not be installed.
    CapabilityBinding(String),
    /// A policy declaration omitted executable handler source.
    MissingHandler(String),
    /// Luau handler construction failed.
    ToolHandler {
        /// Tool whose handler was rejected.
        name: String,
        /// Handler construction error.
        error: ToolHandlerInitError,
    },
    /// Agent construction/admission failed.
    Agent(AgentHostError),
    /// Core refused to start or settle the run.
    Core(pi_agent_core::CoreError),
}

impl fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PolicySourceBase64Invalid => {
                f.write_str("packet policy bytes are not canonical base64")
            }
            Self::PolicyEntrypointInvalid => {
                f.write_str("packet policy entrypoint is not factory_policy")
            }
            Self::PromptBase64Invalid => f.write_str("assignment prompt is not canonical base64"),
            Self::PromptUtf8 => f.write_str("assignment prompt is not UTF-8"),
            Self::PolicySourceUtf8 => f.write_str("sealed Luau policy is not UTF-8"),
            Self::PolicyDigestMismatch => f.write_str("sealed Luau policy digest mismatches"),
            Self::Policy(error) => write!(f, "Luau policy rejected: {error}"),
            Self::UnknownPacketTool(name) => write!(f, "packet contains unknown tool {name:?}"),
            Self::TerminalBindingMismatch(name) => {
                write!(f, "terminal gate does not admit packet operation {name:?}")
            }
            Self::PolicyBinding(error) => write!(f, "policy binding rejected: {error}"),
            Self::CapabilityBinding(error) => {
                write!(f, "factory capability binding rejected: {error}")
            }
            Self::MissingHandler(name) => write!(f, "policy tool {name:?} has no handler source"),
            Self::ToolHandler { name, error } => {
                write!(f, "policy handler {name:?} rejected: {error}")
            }
            Self::Agent(error) => write!(f, "agent host rejected admission: {error}"),
            Self::Core(error) => write!(f, "agent core run failed: {error}"),
        }
    }
}

impl std::error::Error for ExecutionError {}

/// Build a single factory capability binding for a policy handler set.
pub fn factory_capability_bindings<C>(
    capability: Arc<FactoryCapability<C>>,
) -> Result<CapabilityBindings, pi_agent_luau::tool_handler::BindingError>
where
    C: FramedDaemon + 'static,
{
    let mut bindings = CapabilityBindings::new();
    bindings.insert(crate::tool_bridge::FACTORY_CAPABILITY, capability)?;
    Ok(bindings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_agent_core::hooks::ContextEnvelope;
    use pi_agent_core::scheduler::CancellationToken;
    use pi_agent_core::state::{Message, MessageId};
    use pi_agent_luau::tool_handler::{
        CapabilityFuture, CapabilityRequest, CapabilityResponse, LuauCapability,
    };
    use pi_agent_protocol::JsonValue;

    struct NoopCapability;

    impl LuauCapability for NoopCapability {
        fn invoke(
            &self,
            _request: CapabilityRequest,
            _cancellation: CancellationToken,
        ) -> CapabilityFuture {
            Box::pin(std::future::ready(Ok(CapabilityResponse {
                value: JsonValue::Null,
            })))
        }
    }

    fn bindings() -> CapabilityBindings {
        let mut bindings = CapabilityBindings::new();
        bindings
            .insert("factory", Arc::new(NoopCapability))
            .expect("factory binding is unique");
        bindings
    }

    fn policy(with_handler: bool) -> LuaPolicy {
        let handler = if with_handler {
            r#", handler_source = "return function(call) return call.arguments_json end""#
        } else {
            ""
        };
        let source = format!(
            r#"
                return {{
                    system_prompt_append = "",
                    tools = {{ {{
                        name = "work_complete",
                        description = "Finish the assignment.",
                        capability = "factory",
                        execution_mode = "sequential",
                        schema_json = "{{}}"{handler}
                    }} }}
                }}
            "#
        );
        LuaPolicy::load(&source).expect("policy fixture is valid")
    }

    #[test]
    fn policy_tools_become_real_core_tools_only_with_exact_allowlist() {
        let registry = build_policy_tools(
            &policy(true),
            &[ToolName::WorkComplete],
            &bindings(),
            HandlerLimits::default(),
        )
        .expect("handler is explicitly bound");
        assert_eq!(registry.names().collect::<Vec<_>>(), vec!["work_complete"]);
    }

    #[test]
    fn factory_hooks_convert_context_to_provider_json() {
        let converted = factory_hook_set(Arc::new(policy(true)))
            .convert_to_llm(ContextEnvelope {
                version: 1,
                messages: vec![Message::User {
                    id: MessageId(1),
                    content: "hello".to_owned(),
                }],
                host_messages: Vec::new(),
            })
            .expect("factory provider hook converts retained messages");

        let messages = pi_agent_protocol::JsonValue::parse(&converted)
            .expect("factory provider context is valid JSON");
        let message = messages
            .as_array()
            .and_then(|messages| messages.first())
            .expect("factory provider context contains one message");
        assert_eq!(
            message.get("role").and_then(JsonValue::as_str),
            Some("user")
        );
        assert_eq!(
            message.get("content").and_then(JsonValue::as_str),
            Some("hello")
        );
    }

    #[test]
    fn policy_tool_without_handler_is_rejected_without_fallback() {
        let error = build_policy_tools(
            &policy(false),
            &[ToolName::WorkComplete],
            &bindings(),
            HandlerLimits::default(),
        )
        .expect_err("a declaration without executable source cannot become a tool");
        assert!(matches!(error, ExecutionError::MissingHandler(name) if name == "work_complete"));
    }

    #[test]
    fn policy_allowlist_must_equal_packet_tools() {
        let error = build_policy_tools(
            &policy(true),
            &[ToolName::WorkspaceRead],
            &bindings(),
            HandlerLimits::default(),
        )
        .expect_err("policy cannot grant a tool outside packet authority");
        assert!(matches!(error, ExecutionError::PolicyBinding(_)));
    }

    #[test]
    fn sealed_policy_source_checks_digest_before_luau() {
        let source = b"return {}".to_vec();
        let source_with_wrong_digest =
            SealedPolicySource::new(source, ContentDigest::of_bytes(b"different policy"));
        assert!(matches!(
            source_with_wrong_digest.text(),
            Err(ExecutionError::PolicyDigestMismatch)
        ));
    }

    #[test]
    fn phase_hooks_do_not_stop_for_turn_count() {
        let command_context = CommandContext::new(1);
        command_context.configure_engineering();
        command_context
            .record_engineering_checkpoint()
            .expect("checkpoint advances the phase");
        let hooks = phase_hook_set(
            Arc::new(pi_agent_core::hooks::NoHooks),
            command_context.clone(),
        );
        let context = ContextEnvelope {
            version: 1,
            messages: Vec::new(),
            host_messages: Vec::new(),
        };

        for _ in 0..64 {
            assert!(
                !smol::block_on(
                    hooks.should_stop_after_turn_async(&context, CancellationToken::new(),)
                )
                .expect("turn count does not stop an active engineering phase")
            );
        }
        command_context
            .record_engineering_submission()
            .expect("submission advances the phase");
        assert!(
            smol::block_on(hooks.should_stop_after_turn_async(&context, CancellationToken::new(),))
                .expect("submission stops the phase hook")
        );
    }
}
