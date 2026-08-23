//! One real Tea hosted epoch assembled from sealed V2 policy and capabilities.
//!
//! Factory retains process, session, daemon, terminal, and cost custody. Tea
//! owns extension resolution and construction of the provider-bound [`Agent`].

use crate::Admission;
use crate::tool_bridge::{
    CommandContext, FactoryCapability, FramedDaemon, TerminalDeferral, ToolExecutionDiagnostic,
    ToolName,
};
use tea_core::agent::Agent;
use tea_core::effect::NoopEffectGate;
use tea_core::error::CoreError;
use tea_core::event::{AgentEvent, AgentEventKind, EventObserver, ObserverFuture};
use tea_core::hooks::{
    AfterToolCall, AgentLoopTurnUpdate, BeforeToolCall, ContextEnvelope, HookFuture, HookSet,
};
use tea_core::harness::extension::ExtensionCapability;
use tea_providers::openai::OpenAiContextHook;
use tea_core::scheduler::{CancellationToken, ModelProvider};
use tea_core::state::{RunSnapshot, StopReason};
use tea_core::tool::{AgentToolResult, ToolCall};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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
    /// Provider-independent extension source and language-neutral tool surface.
    verified: crate::tea_harness::VerifiedExtension,
    /// Exact effect authority bound to the extension's requested capability.
    capability: Arc<dyn ExtensionCapability>,
    /// Terminal gate shared with the `FactoryCapability` binding.
    pub terminal: Arc<TerminalDeferral>,
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
            .field("verified_tools", &self.verified.tools)
            .field("terminal", &self.terminal)
            .finish_non_exhaustive()
    }
}

/// A prepared, provider-bound actor run.  Preparation performs policy compilation and tool
/// allowlist checks, but does not contact a provider or daemon and does not start a run.
pub struct PreparedExecution {
    admission: Admission,
    hosted: tea_core::runtime::HostedEpoch,
    terminal: Arc<TerminalDeferral>,
    cost_reader: Option<CostReader>,
    command_context: CommandContext,
}

/// Enforce the admitted campaign allowance at provider-turn boundaries.
///
/// Provider accounting is refreshed when a provider turn settles, so a timer would only reread
/// the same snapshot between turns. The core's awaited lifecycle observer gives the host a
/// precise boundary immediately after each completed model usage event (and its corresponding
/// turn end), preserving cancellation without a scheduler poll loop.
struct TurnCostObserver {
    reader: CostReader,
    cancellation: CancellationToken,
    allowance: u64,
    reached: Arc<AtomicBool>,
}

impl EventObserver for TurnCostObserver {
    fn observe<'a>(
        &'a self,
        event: &'a AgentEvent,
        _event_cancellation: CancellationToken,
    ) -> ObserverFuture<'a> {
        Box::pin(async move {
            if matches!(
                &event.kind,
                AgentEventKind::ModelTurnUsage { .. } | AgentEventKind::TurnEnd { .. }
            ) {
                let snapshot = (self.reader)();
                if snapshot
                    .reported_micro_usd
                    .is_some_and(|cost| cost >= self.allowance)
                {
                    self.reached.store(true, Ordering::Release);
                    self.cancellation.cancel();
                }
            }
            Ok(())
        })
    }
}

impl fmt::Debug for PreparedExecution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedExecution")
            .field("assignment_id", &self.admission.packet.assignment_id)
            .field("has_model_provider", &self.hosted.agent().has_model_provider())
            .field("tools", &self.hosted.agent().tool_definitions())
            .field("command_context", &self.command_context)
            .finish_non_exhaustive()
    }
}

impl ExecutionInput {
    /// Prepare one provider-bound run from immutable input values.
    pub fn prepare(self) -> Result<PreparedExecution, ExecutionError> {
        for operation in &self.admission.packet.terminal_operations {
            let tool = ToolName::parse(operation)
                .ok_or_else(|| ExecutionError::UnknownPacketTool(operation.clone()))?;
            if !self.terminal.allows(tool) {
                return Err(ExecutionError::TerminalBindingMismatch(operation.clone()));
            }
        }
        if self.admission.packet.assignment_role == "engineering" {
            self.command_context.configure_engineering();
        }
        let hosted = crate::tea_harness::prepare_hosted_epoch(
            &self.admission,
            self.provider,
            self.verified,
            self.capability,
            Arc::new(NoopEffectGate),
            phase_hook_set(Arc::new(OpenAiContextHook), self.command_context.clone()),
        )
        .map_err(ExecutionError::Harness)?;
        Ok(PreparedExecution {
            admission: self.admission,
            hosted,
            terminal: self.terminal,
            cost_reader: self.cost_reader,
            command_context: self.command_context,
        })
    }
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
    ) -> Result<BeforeToolCall, tea_core::error::HookError> {
        self.inner.before_tool_call(call)
    }

    fn after_tool_call(
        &self,
        call: &ToolCall,
        result: &AgentToolResult,
    ) -> Result<AfterToolCall, tea_core::error::HookError> {
        self.inner.after_tool_call(call, result)
    }

    fn transform_context(
        &self,
        context: ContextEnvelope,
    ) -> Result<ContextEnvelope, tea_core::error::HookError> {
        self.inner.transform_context(context)
    }

    fn convert_to_llm(
        &self,
        context: ContextEnvelope,
    ) -> Result<String, tea_core::error::HookError> {
        self.inner.convert_to_llm(context)
    }

    fn should_stop_after_turn(
        &self,
        context: &ContextEnvelope,
    ) -> Result<bool, tea_core::error::HookError> {
        Ok(self.command_context.engineering_should_stop_after_turn()
            || self.inner.should_stop_after_turn(context)?)
    }

    fn prepare_next_turn(
        &self,
        context: ContextEnvelope,
    ) -> Result<AgentLoopTurnUpdate, tea_core::error::HookError> {
        self.inner.prepare_next_turn(context)
    }

    fn before_tool_call_async<'a>(
        &'a self,
        call: &'a ToolCall,
        context: ContextEnvelope,
        cancellation: tea_core::scheduler::CancellationToken,
    ) -> HookFuture<'a, BeforeToolCall> {
        self.inner
            .before_tool_call_async(call, context, cancellation)
    }

    fn after_tool_call_async<'a>(
        &'a self,
        call: &'a ToolCall,
        result: &'a AgentToolResult,
        context: ContextEnvelope,
        cancellation: tea_core::scheduler::CancellationToken,
    ) -> HookFuture<'a, AfterToolCall> {
        self.inner
            .after_tool_call_async(call, result, context, cancellation)
    }

    fn transform_context_async(
        &self,
        context: ContextEnvelope,
        cancellation: tea_core::scheduler::CancellationToken,
    ) -> HookFuture<'_, ContextEnvelope> {
        self.inner.transform_context_async(context, cancellation)
    }

    fn convert_to_llm_async(
        &self,
        context: ContextEnvelope,
        cancellation: tea_core::scheduler::CancellationToken,
    ) -> HookFuture<'_, String> {
        self.inner.convert_to_llm_async(context, cancellation)
    }

    fn should_stop_after_turn_async<'a>(
        &'a self,
        context: &'a ContextEnvelope,
        cancellation: tea_core::scheduler::CancellationToken,
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
        cancellation: tea_core::scheduler::CancellationToken,
    ) -> HookFuture<'_, AgentLoopTurnUpdate> {
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
    cost_reader: Option<CostReader>,
) -> Result<ExecutionInput, ExecutionError>
where
    C: FramedDaemon + 'static,
{
    let verified = crate::tea_harness::verify_extension(&admission)
        .map_err(ExecutionError::Harness)?;
    let capability: Arc<dyn ExtensionCapability> = Arc::new(FactoryCapability::new(
        daemon,
        verified.tools.clone(),
        command_context.clone(),
        Arc::clone(&terminal),
        local,
    ));
    Ok(ExecutionInput {
        admission,
        provider,
        verified,
        capability,
        terminal,
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
        self.hosted.agent()
    }

    /// Borrow the normalized Tea provenance for trace attribution.
    #[must_use]
    pub fn provenance(&self) -> &tea_core::effect::RunProvenance {
        self.hosted.provenance()
    }

    /// Borrow Tea's normalized provider and policy surface fingerprints.
    #[must_use]
    pub fn surface_fingerprints(&self) -> &tea_core::harness::HarnessSurfaceFingerprints {
        self.hosted.surface_fingerprints()
    }

    /// Borrow the one-shot terminal deferral gate.
    #[must_use]
    pub fn terminal(&self) -> &Arc<TerminalDeferral> {
        &self.terminal
    }

    /// Drive the single assignment from the caller-owned executor.
    pub async fn drive(&self) -> Result<ExecutionResult, ExecutionError> {
        let prompt = crate::tea_harness::decode_assignment_prompt(&self.admission)
            .map_err(ExecutionError::Harness)?;
        let handle = self
            .hosted
            .agent()
            .start_prompt(prompt)
            .map_err(ExecutionError::Core)?;
        let cancellation = handle.cancellation();
        let cost_limit_reached = Arc::new(AtomicBool::new(false));
        let cost_observer = self.cost_reader.as_ref().map(|reader| {
            Arc::new(TurnCostObserver {
                reader: Arc::clone(reader),
                cancellation: cancellation.clone(),
                allowance: self
                    .admission
                    .packet
                    .remaining_campaign_allowance_micro_usd,
                reached: Arc::clone(&cost_limit_reached),
            }) as Arc<dyn EventObserver>
        });
        let _cost_subscription = cost_observer.map(|observer| self.hosted.agent().subscribe(observer));
        let drive_result = handle.drive().await;
        let snapshot = handle.snapshot();
        let events = handle.events();
        if let Err(error) = drive_result {
            eprintln!(
                "factory-tea-host execution failure: engineering_phase={:?} core_phase={:?} event_count={} error={error}",
                self.command_context.engineering_diagnostics(),
                snapshot.phase,
                events.len(),
            );
            if !matches!(error, CoreError::Cancelled) {
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

/// Failures before a terminal submission can be considered.
#[derive(Debug)]
pub enum ExecutionError {
    /// Tea rejected extension verification, resolution, or hosted preparation.
    Harness(tea_core::harness::HarnessError),
    /// Packet included an unknown model-facing tool name.
    UnknownPacketTool(String),
    /// The caller's terminal gate does not admit a packet terminal operation.
    TerminalBindingMismatch(String),
    /// Core refused to start or settle the run.
    Core(CoreError),
}

impl fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Harness(error) => write!(f, "Tea hosted harness rejected admission: {error}"),
            Self::UnknownPacketTool(name) => write!(f, "packet contains unknown tool {name:?}"),
            Self::TerminalBindingMismatch(name) => {
                write!(f, "terminal gate does not admit packet operation {name:?}")
            }
            Self::Core(error) => write!(f, "agent core run failed: {error}"),
        }
    }
}

impl std::error::Error for ExecutionError {}

#[cfg(test)]
mod tests {
    use super::*;
    use tea_core::hooks::ContextEnvelope;
    use tea_core::scheduler::CancellationToken;
    use tea_core::state::{RunId, StopReason, TurnId};

    #[test]
    fn provider_cost_is_checked_at_turn_end_without_a_timer() {
        let cancellation = CancellationToken::new();
        let observer = TurnCostObserver {
            reader: Arc::new(|| CostSnapshot {
                reported_micro_usd: Some(101),
                complete: false,
            }),
            cancellation: cancellation.clone(),
            allowance: 100,
            reached: Arc::new(AtomicBool::new(false)),
        };
        let event = AgentEvent {
            run_id: RunId(1),
            sequence: tea_core::event::EventSequence(1),
            kind: AgentEventKind::TurnEnd {
                turn_id: TurnId(1),
                reason: StopReason::Stop,
            },
        };
        smol::block_on(observer.observe(&event, cancellation.clone()))
            .expect("turn-boundary observer succeeds");
        assert!(cancellation.is_cancelled());
        assert!(observer.reached.load(Ordering::Acquire));
    }

    #[test]
    fn phase_hooks_do_not_stop_for_turn_count() {
        let command_context = CommandContext::new(1);
        command_context.configure_engineering();
        command_context
            .record_engineering_checkpoint()
            .expect("checkpoint advances the phase");
        let hooks = phase_hook_set(
            Arc::new(tea_core::hooks::NoHooks),
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
