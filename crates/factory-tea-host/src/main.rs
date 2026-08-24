//! Production actor-host entrypoint.

mod runtime;

use factory_protocol::{
    ArtifactReceiptResponse, MicroUsd, OP_SESSION_SEAL_ARTIFACT, OP_SESSION_SUBMIT_TERMINAL,
    OP_SESSION_VERIFY_PACKET,
};
use factory_settings::{
    MAX_PROVIDER_RETRIES, PROVIDER_RETRY_INITIAL_BACKOFF, PROVIDER_RETRY_MAX_BACKOFF,
};
use factory_tea_host::{
    Admission, AdmissionConfig, CommandContext, CostReader, CostSnapshot, ExecutionDiagnostics,
    FramedDaemon, ProviderEffectDiagnostics, TerminalDeferral, ToolName, build_factory_execution_input,
    read_admission_from_fd0,
};
use flate2::{Compression, write::GzEncoder};
use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::Path,
    process::ExitCode,
    sync::{Arc, Mutex},
    time::Duration,
};
use tea_core::event::EventObserver;
use tea_core::scheduler::ModelProvider;
use tea_core::state::StopReason;
use tea_core::trace::TraceObserver;
use tea_core::{effect::RunProvenance, harness::HarnessSurfaceFingerprints};
use tea_protocol::{JsonNumber, JsonValue};
use tea_providers::RetryPolicy;
use tea_providers::openrouter::{OpenRouterConfig, OpenRouterProvider};
use tea_trace::{JsonLinesSink, RedactingSink, Redactor, TraceEvent, TraceSink};

fn main() -> ExitCode {
    match smol::block_on(run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("factory-tea-host failed closed: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let admission =
        read_admission_from_fd0(AdmissionConfig::default()).map_err(|e| e.to_string())?;
    let daemon = Arc::new(runtime::InheritedDaemon::from_fd0().map_err(|e| e.to_string())?);
    verify_packet(daemon.as_ref(), &admission).await?;
    if admission.packet.model.provider != "openrouter" {
        return Err("packet provider is not supported by this host".to_owned());
    }
    let api_key = env::var(&admission.packet.runtime.credential_env)
        .map_err(|_| "selected provider credential is unavailable".to_owned())?;
    let provider = Arc::new(OpenRouterProvider::new(
        OpenRouterConfig::try_new(api_key, admission.packet.model.model_id.clone())
            .map_err(|e| e.to_string())?
            .with_request_timeout(provider_request_timeout(
                admission.packet.limits.wall_limit_millis,
            ))
            .with_stall_timeout(provider_stall_timeout(
                admission.packet.limits.wall_limit_millis,
            ))
            .with_retry_policy(provider_retry_policy()),
    ));
    let accounting_provider = Arc::clone(&provider);
    let cost_reader: CostReader = Arc::new(move || provider_cost(&accounting_provider));
    let terminal_tools = admission
        .packet
        .terminal_operations
        .iter()
        .map(|name| ToolName::parse(name).ok_or_else(|| format!("unknown terminal tool {name}")))
        .collect::<Result<Vec<_>, _>>()?;
    let terminal = Arc::new(TerminalDeferral::new(terminal_tools));
    let workspace = Arc::new(
        runtime::RootedWorkspace::new(&admission.packet.workspace_root)
            .map_err(|e| e.to_string())?,
    );
    let command_context = CommandContext::new(admission.frame.session_revision);
    let input = build_factory_execution_input(
        admission.clone(),
        Arc::clone(&provider) as Arc<dyn ModelProvider>,
        Arc::clone(&daemon),
        command_context.clone(),
        terminal,
        Some(workspace),
        Some(cost_reader),
    )
    .map_err(|e| e.to_string())?;
    let prepared = input.prepare().map_err(|e| e.to_string())?;
    let trace_sink = FactoryTraceSink::new(&admission)?;
    let trace_observer = Arc::new(TraceObserver::new_with_provenance(
        format!("factory-assignment-{}", admission.packet.assignment_id),
        prepared.provenance().clone(),
        RedactingSink::new(trace_sink.clone(), FactoryTraceRedactor),
    ));
    let _trace_subscription = prepared
        .agent()
        .subscribe(Arc::clone(&trace_observer) as Arc<dyn EventObserver>);
    let result = match prepared.drive().await {
        Ok(result) => result,
        Err(error) => {
            if let Some(report) = provider.last_error_report() {
                eprintln!("factory-tea-host provider failure: {report}");
            }
            return Err(error.to_string());
        }
    };
    eprintln!(
        "factory-tea-host execution: turns_started={} engineering_phase={} stop_reason={:?} terminal={} cost_known={}",
        result.diagnostics.turns_started,
        result.diagnostics.engineering_phase,
        result.stop_reason(),
        result.terminal.is_some(),
        result.cost_micro_usd.is_some(),
    );
    if trace_observer.failed_events() != 0 {
        return Err("Tea trace persistence failed during assignment execution".to_owned());
    }
    let summary = execution_summary(
        &admission,
        prepared.provenance(),
        prepared.surface_fingerprints(),
        &result.diagnostics,
        prepared.provider_effect_diagnostics(),
        result.cost_limit_reached,
        result.terminal.as_ref().map(|terminal| terminal.tool),
        trace_sink.truncated()?,
    )?;
    let transcript = gzip_transcript(&trace_sink.bytes()?)?;
    let transcript_id =
        seal_session_artifact(
            daemon.as_ref(),
            &admission,
            "tea-trace.jsonl.gz",
            "tea_trace_jsonl_gzip",
            transcript,
            &command_context,
        )
        .await?;
    let summary_id = seal_session_artifact(
        daemon.as_ref(),
        &admission,
        "factory-execution-summary.json",
        "factory_execution_summary_json",
        summary.into_bytes(),
        &command_context,
    )
    .await?;
    let usage = provider.usage_snapshot();
    // A live cost cancellation is a failed economic stop, even if a terminal tool
    // raced with the monitor.  Never submit the deferred operation in that case.
    let completed = !result.cost_limit_reached && result.terminal.is_some();
    let terminal_operation = completed
        .then(|| {
            result
                .terminal
                .as_ref()
                .map(|terminal| terminal.tool.as_str().to_owned())
        })
        .flatten();
    let terminal_payload = if completed {
        result
            .terminal
            .as_ref()
            .map(|terminal| terminal.payload.to_json_string())
            .transpose()
            .map_err(|e| e.to_string())?
            .unwrap_or_else(|| "{}".to_owned())
    } else {
        "{}".to_owned()
    };
    let stop_reason = if result.cost_limit_reached {
        "cost_limit"
    } else if completed {
        "completed"
    } else if result.cost_micro_usd.is_none() {
        "unknown_cost"
    } else {
        stop_reason(result.stop_reason())
    };
    daemon
        .call(
            OP_SESSION_SUBMIT_TERMINAL,
            JsonValue::object([
                (
                    "client_command_id",
                    JsonValue::String(format!("host-terminal-{}", admission.frame.session_id)),
                ),
                (
                    "expected_revision",
                    number(command_context.current_revision())?,
                ),
                ("terminal_operation", optional_string(terminal_operation)),
                (
                    "terminal_payload_b64",
                    JsonValue::String(base64(terminal_payload.as_bytes())),
                ),
                ("transcript_artifact_id", number(transcript_id as u64)?),
                (
                    "execution_summary_artifact_id",
                    number(summary_id as u64)?,
                ),
                ("input_tokens", optional_number(usage.input_tokens)?),
                ("output_tokens", optional_number(usage.output_tokens)?),
                ("cache_read_tokens", optional_number(usage.cache_read_tokens)?),
                ("cache_write_tokens", optional_number(usage.cache_write_tokens)?),
                ("reasoning_tokens", optional_number(usage.reasoning_tokens)?),
                (
                    "reported_cost_micro_usd",
                    optional_number(result.cost_micro_usd)?,
                ),
                ("stop_reason", JsonValue::String(stop_reason.to_owned())),
            ]),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

const TRACE_END_RESERVE: usize = 2 * 1024;

struct FactoryTraceState {
    file: File,
    bytes: Vec<u8>,
    truncated: bool,
    trace_limit: usize,
    end_limit: usize,
}

/// Factory-owned bounded persistence for already-redacted Tea trace records.
#[derive(Clone)]
struct FactoryTraceSink {
    state: Arc<Mutex<FactoryTraceState>>,
}

impl FactoryTraceSink {
    fn new(admission: &Admission) -> Result<Self, String> {
        let path = Path::new(&admission.packet.staging_root).join("session.ndjson");
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .map_err(|error| format!("create live transcript {}: {error}", path.display()))?;
        let raw_limit = max_gzip_input(admission.packet.limits.output_byte_limit as usize);
        if raw_limit < TRACE_END_RESERVE + 1024 {
            return Err(
                "assignment output limit is too small for required Tea trace evidence".to_owned(),
            );
        }
        Ok(Self {
            state: Arc::new(Mutex::new(FactoryTraceState {
                file,
                bytes: Vec::new(),
                truncated: false,
                trace_limit: raw_limit - TRACE_END_RESERVE,
                end_limit: raw_limit,
            })),
        })
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        self.state
            .lock()
            .map(|state| state.bytes.clone())
            .map_err(|_| "Tea trace mutex poisoned".to_owned())
    }

    fn truncated(&self) -> Result<bool, String> {
        self.state
            .lock()
            .map(|state| state.truncated)
            .map_err(|_| "Tea trace mutex poisoned".to_owned())
    }
}

impl TraceSink for FactoryTraceSink {
    type Error = io::Error;

    fn append(&mut self, event: TraceEvent) -> Result<(), Self::Error> {
        let is_terminal = matches!(event, TraceEvent::EpisodeEnd(_));
        let mut encoder = JsonLinesSink::new(Vec::new());
        encoder.append(event)?;
        let line = encoder.into_inner();
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("Tea trace mutex poisoned"))?;
        let limit = if is_terminal {
            state.end_limit
        } else {
            state.trace_limit
        };
        if state.bytes.len().saturating_add(line.len()) > limit {
            state.truncated = true;
            return Ok(());
        }
        append_trace_bytes(&mut state, &line)
    }
}

fn append_trace_bytes(state: &mut FactoryTraceState, bytes: &[u8]) -> io::Result<()> {
    state.file.write_all(bytes)?;
    state.file.flush()?;
    state.bytes.extend_from_slice(bytes);
    Ok(())
}

/// Factory's trace boundary removes prompts and credentials before persistence.
#[derive(Clone, Copy)]
struct FactoryTraceRedactor;

impl Redactor for FactoryTraceRedactor {
    fn redact(&self, event: TraceEvent) -> TraceEvent {
        match event {
            TraceEvent::Turn(mut turn) => {
                turn.input = "[redacted model input]".to_owned();
                turn.output = turn.output.map(|output| bound(&output, 16 * 1024));
                TraceEvent::Turn(turn)
            }
            TraceEvent::Tool(mut tool) => {
                tool.input = redact_tool_input(&tool.input);
                tool.output = tool.output.map(|output| bound(&output, 16 * 1024));
                tool.error = tool.error.map(|error| bound(&error, 4 * 1024));
                TraceEvent::Tool(tool)
            }
            TraceEvent::EpisodeEnd(mut end) => {
                end.error = end.error.map(|error| bound(&error, 1024));
                TraceEvent::EpisodeEnd(end)
            }
            event @ (TraceEvent::EpisodeHeader(_) | TraceEvent::Compaction(_)) => event,
        }
    }
}

fn provider_request_timeout(wall_limit_millis: u64) -> Duration {
    Duration::from_millis(wall_limit_millis.max(1))
}

fn provider_stall_timeout(wall_limit_millis: u64) -> Duration {
    Duration::from_millis(wall_limit_millis.max(1))
}

fn provider_retry_policy() -> RetryPolicy {
    RetryPolicy::new(
        MAX_PROVIDER_RETRIES,
        PROVIDER_RETRY_INITIAL_BACKOFF,
        PROVIDER_RETRY_MAX_BACKOFF,
    )
}

async fn verify_packet(
    daemon: &runtime::InheritedDaemon,
    admission: &Admission,
) -> Result<(), String> {
    let response = daemon
        .call(
            OP_SESSION_VERIFY_PACKET,
            JsonValue::object([
                (
                    "packet_digest",
                    JsonValue::String(admission.frame.packet_digest.clone()),
                ),
                (
                    "packet_bytes_b64",
                    JsonValue::String(base64(&admission.packet_bytes)),
                ),
            ]),
        )
        .await
        .map_err(|e| e.to_string())?;
    if let Some(error) = packet_verification_error(&response) {
        return Err(error);
    }
    if response.get("verified").and_then(JsonValue::as_bool) != Some(true) {
        return Err("daemon did not verify the admitted assignment packet".to_owned());
    }
    Ok(())
}

fn packet_verification_error(response: &JsonValue) -> Option<String> {
    let error_code = response.get("error_code")?.as_str()?;
    let message = response
        .get("message")
        .and_then(JsonValue::as_str)
        .unwrap_or("no rejection detail provided");
    Some(format!(
        "daemon rejected the admitted assignment packet: {error_code}: {message}"
    ))
}

async fn seal_session_artifact(
    daemon: &runtime::InheritedDaemon,
    admission: &Admission,
    staging_file_name: &'static str,
    role: &'static str,
    bytes: Vec<u8>,
    command_context: &CommandContext,
) -> Result<i64, String> {
    if bytes.len() > admission.packet.limits.output_byte_limit as usize {
        return Err(format!("{role} exceeds packet evidence authority"));
    }
    let path = Path::new(&admission.packet.staging_root).join(staging_file_name);
    fs::write(path, bytes).map_err(|e| format!("write {role}: {e}"))?;
    let response = daemon
        .call(
            OP_SESSION_SEAL_ARTIFACT,
            JsonValue::object([
                (
                    "client_command_id",
                    JsonValue::String(format!("host-{role}-{}", admission.frame.session_id)),
                ),
                (
                    "expected_revision",
                    number(command_context.current_revision())?,
                ),
                (
                    "staging_relative_path",
                    JsonValue::String(staging_file_name.to_owned()),
                ),
                ("role", JsonValue::String(role.to_owned())),
                (
                    "byte_limit",
                    number(u64::from(admission.packet.limits.output_byte_limit))?,
                ),
            ]),
        )
        .await
        .map_err(|e| e.to_string())?;
    if let Some(error_code) = response.get("error_code").and_then(JsonValue::as_str) {
        let message = response
            .get("message")
            .and_then(JsonValue::as_str)
            .unwrap_or("no rejection detail provided");
        return Err(format!(
            "daemon {role} seal rejected: {error_code}: {message}"
        ));
    }
    let text = response.to_json_string().map_err(|e| e.to_string())?;
    let receipt: ArtifactReceiptResponse = miniserde::json::from_str(&text)
        .map_err(|_| format!("daemon {role} receipt is invalid"))?;
    if receipt.operation != OP_SESSION_SEAL_ARTIFACT
        || receipt.byte_length as usize > admission.packet.limits.output_byte_limit as usize
    {
        return Err(format!("daemon {role} receipt is outside packet limits"));
    }
    command_context.advance_revision(receipt.aggregate_revision);
    Ok(receipt.artifact_id)
}

fn execution_summary(
    admission: &Admission,
    provenance: &RunProvenance,
    surfaces: &HarnessSurfaceFingerprints,
    diagnostics: &ExecutionDiagnostics,
    provider_effects: ProviderEffectDiagnostics,
    cost_limit_reached: bool,
    terminal: Option<ToolName>,
    trace_truncated: bool,
) -> Result<String, String> {
    let tool_executions = diagnostics
        .tool_executions
        .iter()
        .map(|tool| {
            Ok(JsonValue::object([
                ("tool", JsonValue::String(tool.tool.clone())),
                ("calls", number(u64::from(tool.calls))?),
                ("failures", number(u64::from(tool.failures))?),
                ("total_millis", number(tool.total_millis)?),
                ("maximum_millis", number(tool.maximum_millis)?),
            ]))
        })
        .collect::<Result<Vec<_>, String>>()?;
    JsonValue::object([
        ("schema_version", JsonValue::Number(JsonNumber::Unsigned(1))),
        (
            "type",
            JsonValue::String("factory.execution_summary.v1".to_owned()),
        ),
        (
            "factory",
            JsonValue::object([
                (
                    "application_revision_id",
                    JsonValue::String(admission.packet.application_revision_id.to_string()),
                ),
                (
                    "assignment_id",
                    JsonValue::String(admission.packet.assignment_id.to_string()),
                ),
                (
                    "kernel_build_id",
                    JsonValue::String(admission.packet.kernel_build_id.clone()),
                ),
                (
                    "rust_host_identity",
                    JsonValue::String(factory_settings::RUST_HOST_IDENTITY.to_owned()),
                ),
                (
                    "packet_digest",
                    JsonValue::String(admission.frame.packet_digest.clone()),
                ),
                (
                    "policy_digest",
                    JsonValue::String(admission.packet.policy_digest.clone()),
                ),
            ]),
        ),
        (
            "model",
            JsonValue::object([
                (
                    "provider",
                    JsonValue::String(admission.packet.model.provider.clone()),
                ),
                (
                    "model_id",
                    JsonValue::String(admission.packet.model.model_id.clone()),
                ),
                (
                    "thinking_level",
                    JsonValue::String(admission.packet.model.thinking_level.clone()),
                ),
            ]),
        ),
        (
            "tea_provenance",
            JsonValue::object([
                (
                    "session_id",
                    optional_borrowed_string(provenance.session_id.as_deref()),
                ),
                (
                    "operation_id",
                    optional_borrowed_string(provenance.operation_id.as_deref()),
                ),
                (
                    "epoch_id",
                    optional_borrowed_string(provenance.epoch_id.as_deref()),
                ),
                (
                    "core_run_id",
                    optional_borrowed_string(provenance.core_run_id.as_deref()),
                ),
                (
                    "harness_snapshot_id",
                    optional_borrowed_string(provenance.harness_snapshot_id.as_deref()),
                ),
                (
                    "harness_revision_id",
                    optional_borrowed_string(provenance.harness_revision_id.as_deref()),
                ),
                (
                    "model_harness_profile_id",
                    optional_borrowed_string(provenance.model_harness_profile_id.as_deref()),
                ),
                (
                    "provider_surface_digest",
                    optional_borrowed_string(provenance.provider_surface_digest.as_deref()),
                ),
            ]),
        ),
        (
            "tea_surfaces",
            JsonValue::object([
                (
                    "system_prompt_digest",
                    JsonValue::String(surfaces.system_prompt_digest.to_hex()),
                ),
                (
                    "ordered_tool_definitions_digest",
                    JsonValue::String(surfaces.ordered_tool_definitions_digest.to_hex()),
                ),
                (
                    "tool_execution_policy_digest",
                    JsonValue::String(surfaces.tool_execution_policy_digest.to_hex()),
                ),
                (
                    "hook_bundle_digest",
                    JsonValue::String(surfaces.hook_bundle_digest.to_hex()),
                ),
                (
                    "capability_bindings_digest",
                    JsonValue::String(surfaces.capability_bindings_digest.to_hex()),
                ),
                (
                    "compaction_policy_digest",
                    JsonValue::String(surfaces.compaction_policy_digest.to_hex()),
                ),
                (
                    "provider_surface_digest",
                    JsonValue::String(surfaces.provider_surface_digest.to_hex()),
                ),
            ]),
        ),
        (
            "turns_started",
            number(u64::from(diagnostics.turns_started))?,
        ),
        (
            "engineering_phase",
            JsonValue::String(diagnostics.engineering_phase.clone()),
        ),
        (
            "provider_effects",
            JsonValue::object([
                (
                    "count",
                    number(u64::from(provider_effects.provider_effect_count))?,
                ),
                (
                    "settled_count",
                    number(u64::from(provider_effects.settled_provider_effect_count))?,
                ),
                (
                    "complete_usage",
                    JsonValue::Bool(provider_effects.complete_provider_usage),
                ),
                (
                    "complete_cost",
                    JsonValue::Bool(provider_effects.complete_provider_cost),
                ),
                // The summary is sealed before terminal reconciliation. A
                // recovery can only occur after a host has disappeared, so
                // this successful host path is explicitly not recovered.
                ("recovered_from_provider_ledger", JsonValue::Bool(false)),
            ]),
        ),
        ("cost_limit_reached", JsonValue::Bool(cost_limit_reached)),
        (
            "terminal_operation",
            terminal.map_or(JsonValue::Null, |tool| {
                JsonValue::String(tool.as_str().to_owned())
            }),
        ),
        ("trace_truncated", JsonValue::Bool(trace_truncated)),
        ("tool_executions", JsonValue::Array(tool_executions)),
    ])
    .to_json_string()
    .map_err(|e| e.to_string())
}

fn redact_tool_input(text: &str) -> String {
    let value = redacted_json(text);
    value
        .to_json_string().map_or_else(|_| "\"invalid tool JSON\"".to_owned(), |value| bound(&value, 16 * 1024))
}

fn redacted_json(text: &str) -> JsonValue {
    match JsonValue::parse(text) {
        Ok(value) => redact(value),
        Err(_) => JsonValue::String("invalid tool JSON".to_owned()),
    }
}

fn redact(value: JsonValue) -> JsonValue {
    match value {
        JsonValue::Array(values) => JsonValue::Array(values.into_iter().map(redact).collect()),
        JsonValue::Object(values) => JsonValue::Object(
            values
                .into_iter()
                .map(|(key, value)| {
                    let lowered = key.to_ascii_lowercase();
                    let value = if lowered.contains("secret")
                        || lowered.contains("token")
                        || lowered.contains("authorization")
                        || lowered.contains("api_key")
                    {
                        JsonValue::String("[redacted]".to_owned())
                    } else {
                        redact(value)
                    };
                    (key, value)
                })
                .collect(),
        ),
        JsonValue::String(value) => JsonValue::String(bound(&value, 16 * 1024)),
        value => value,
    }
}

fn provider_cost(provider: &OpenRouterProvider) -> CostSnapshot {
    let report = provider.cost_report();
    CostSnapshot {
        reported_micro_usd: report
            .reported_total_usd_exact
            .as_deref()
            .and_then(|value| MicroUsd::parse_decimal_usd(value).ok())
            .map(MicroUsd::get),
        complete: report.complete,
    }
}

fn stop_reason(reason: Option<StopReason>) -> &'static str {
    match reason {
        Some(StopReason::Cancelled) => "cancelled",
        Some(StopReason::Length) => "output_limit",
        _ => "protocol_error",
    }
}

fn optional_string(value: Option<String>) -> JsonValue {
    value.map_or(JsonValue::Null, JsonValue::String)
}

fn optional_borrowed_string(value: Option<&str>) -> JsonValue {
    value.map_or(JsonValue::Null, |value| JsonValue::String(value.to_owned()))
}

fn optional_number(value: Option<u64>) -> Result<JsonValue, String> {
    value
        .map(number)
        .transpose()?
        .map_or(Ok(JsonValue::Null), Ok)
}

fn number(value: u64) -> Result<JsonValue, String> {
    JsonValue::number(JsonNumber::Unsigned(value)).map_err(|e| e.to_string())
}

fn bound(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_owned();
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        output.push(TABLE[((value >> 18) & 63) as usize] as char);
        output.push(TABLE[((value >> 12) & 63) as usize] as char);
        output.push(if chunk.len() > 1 {
            TABLE[((value >> 6) & 63) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            TABLE[(value & 63) as usize] as char
        } else {
            '='
        });
    }
    output
}

/// Returns a conservative size bound for the stored-block fallback. The
/// `miniz_oxide` backend may split uncompressed data below the DEFLATE maximum
/// block size, so this deliberately assumes 32 KiB blocks.
fn gzip_upper_bound_len(input_len: usize) -> usize {
    if input_len == 0 {
        23
    } else {
        18 + input_len + input_len.div_ceil(32_768) * 5
    }
}

fn max_gzip_input(output_limit: usize) -> usize {
    let mut low = 0;
    let mut high = output_limit;
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if gzip_upper_bound_len(middle) <= output_limit {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    low
}

fn gzip_transcript(input: &[u8]) -> Result<Vec<u8>, String> {
    let compressed = gzip_with_level(input, Compression::default())?;
    if compressed.len() < gzip_upper_bound_len(input.len()) {
        return Ok(compressed);
    }
    gzip_with_level(input, Compression::none())
}

fn gzip_with_level(input: &[u8], level: Compression) -> Result<Vec<u8>, String> {
    let mut encoder = GzEncoder::new(Vec::new(), level);
    encoder
        .write_all(input)
        .map_err(|error| format!("write gzip transcript: {error}"))?;
    encoder
        .finish()
        .map_err(|error| format!("finish gzip transcript: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{
        CostSnapshot, FactoryTraceRedactor, execution_summary, gzip_transcript,
        gzip_upper_bound_len, max_gzip_input, packet_verification_error,
        provider_request_timeout, provider_retry_policy, provider_stall_timeout,
    };
    use factory_protocol::{AssignmentPacketWireV2, SessionAdmissionFrameV2};
    use factory_tea_host::{Admission, ExecutionDiagnostics, ProviderEffectDiagnostics, ToolName};
    use std::fs;
    use std::process::Command;
    use std::time::Duration;
    use tea_core::effect::RunProvenance;
    use tea_core::harness::HarnessSurfaceFingerprints;
    use tea_protocol::JsonValue;
    use tea_session::Digest;
    use tea_trace::{EpisodeEnd, EpisodeHeader, Redactor, Tool, TraceEvent, TraceSink, Turn, decode_jsonl};

    #[test]
    fn packet_verification_error_preserves_daemon_rejection() {
        let response = JsonValue::object([
            (
                "error_code",
                JsonValue::String("session_rejected".to_owned()),
            ),
            (
                "message",
                JsonValue::String("packet bytes differ from daemon admission".to_owned()),
            ),
        ]);

        assert_eq!(
            packet_verification_error(&response).as_deref(),
            Some(
                "daemon rejected the admitted assignment packet: session_rejected: packet bytes differ from daemon admission"
            )
        );
    }

    #[test]
    fn provider_request_timeout_matches_assignment_wall() {
        assert_eq!(provider_request_timeout(1_800_000), Duration::from_mins(30));
        assert_eq!(provider_request_timeout(900_000), Duration::from_mins(15));
        assert_eq!(provider_request_timeout(120_000), Duration::from_secs(120));
        assert_eq!(
            provider_request_timeout(3_600_000),
            Duration::from_secs(3_600)
        );
    }

    #[test]
    fn provider_retry_policy_allows_two_replay_safe_retries() {
        assert_eq!(provider_retry_policy().max_retries(), 2);
    }

    #[test]
    fn provider_stall_timeout_matches_assignment_wall() {
        assert_eq!(provider_stall_timeout(1_800_000), Duration::from_mins(30));
        assert_eq!(provider_stall_timeout(900_000), Duration::from_mins(15));
        assert_eq!(provider_stall_timeout(120_000), Duration::from_secs(120));
        assert_eq!(
            provider_stall_timeout(3_600_000),
            Duration::from_secs(3_600)
        );
    }

    #[test]
    fn partial_cost_snapshots_are_not_terminal_costs() {
        let snapshot = CostSnapshot {
            reported_micro_usd: Some(25),
            complete: false,
        };
        assert_eq!(snapshot.reported_micro_usd, Some(25));
        assert!(!snapshot.complete);
    }

    #[test]
    fn trace_redaction_happens_before_persistence_shape() {
        let redactor = FactoryTraceRedactor;
        let TraceEvent::Turn(turn) = redactor.redact(TraceEvent::Turn(
            Turn::new(0, "sealed assignment prompt").with_output("bounded answer"),
        )) else {
            panic!("turn kind is preserved");
        };
        assert_eq!(turn.input, "[redacted model input]");
        assert!(!turn.input.contains("sealed assignment"));

        let TraceEvent::Tool(tool) = redactor.redact(TraceEvent::Tool(
            Tool::new(
                0,
                "call-1",
                "fixture",
                r#"{"api_key":"private","nested":{"authorization":"bearer"}}"#,
            )
            .with_output("ok"),
        )) else {
            panic!("tool kind is preserved");
        };
        assert!(!tool.input.contains("private"));
        assert!(!tool.input.contains("bearer"));
        assert!(tool.input.contains("[redacted]"));
    }

    #[test]
    fn factory_trace_sink_seals_only_a_complete_tea_episode() {
        let mut packet: AssignmentPacketWireV2 = miniserde::json::from_str(include_str!(
            "../../../tests/protocol-fixtures/assignment-packet-v2.json"
        ))
        .expect("generic packet fixture parses");
        let staging = std::env::temp_dir().join(format!(
            "factory-tea-trace-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock is after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&staging).expect("create trace staging directory");
        packet.staging_root = staging.display().to_string();
        let admission = Admission {
            frame: SessionAdmissionFrameV2 {
                r#type: "session.admitted".to_owned(),
                protocol_version: factory_protocol::PROTOCOL_VERSION_V2,
                assignment_id: packet.assignment_id.to_string(),
                session_id: 9,
                session_revision: 7,
                packet_digest: packet.packet_digest.clone(),
                packet_b64: "AA==".to_owned(),
            },
            packet_bytes: Vec::new(),
            packet,
        };
        let mut sink = super::FactoryTraceSink::new(&admission).expect("trace sink");
        sink.append(TraceEvent::from(EpisodeHeader::new("episode")))
            .expect("header");
        sink.append(TraceEvent::from(EpisodeEnd::completed()))
            .expect("terminal event");
        let trace = String::from_utf8(sink.bytes().expect("trace bytes")).expect("UTF-8 trace");
        let events = decode_jsonl(&trace).expect("every record is a Tea trace event");
        assert!(matches!(events.first(), Some(TraceEvent::EpisodeHeader(_))));
        assert!(matches!(events.last(), Some(TraceEvent::EpisodeEnd(_))));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, TraceEvent::EpisodeHeader(_)))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, TraceEvent::EpisodeEnd(_)))
                .count(),
            1
        );
        assert!(!trace.contains("factory.execution_summary"));
        fs::remove_dir_all(staging).expect("remove trace staging directory");
    }

    #[test]
    fn execution_summary_retains_factory_and_tea_epoch_identities() {
        let packet: AssignmentPacketWireV2 = miniserde::json::from_str(include_str!(
            "../../../tests/protocol-fixtures/assignment-packet-v2.json"
        ))
        .expect("generic packet fixture parses");
        let admission = Admission {
            frame: SessionAdmissionFrameV2 {
                r#type: "session.admitted".to_owned(),
                protocol_version: factory_protocol::PROTOCOL_VERSION_V2,
                assignment_id: packet.assignment_id.to_string(),
                session_id: 9,
                session_revision: 7,
                packet_digest: packet.packet_digest.clone(),
                packet_b64: "AA==".to_owned(),
            },
            packet_bytes: Vec::new(),
            packet,
        };
        let provenance = RunProvenance {
            harness_snapshot_id: Some("snapshot-id".to_owned()),
            harness_revision_id: Some("revision-id".to_owned()),
            model_harness_profile_id: Some("profile-id".to_owned()),
            provider_surface_digest: Some("provider-surface".to_owned()),
            ..RunProvenance::default()
        };
        let surfaces = HarnessSurfaceFingerprints {
            system_prompt_digest: Digest::from_bytes("system"),
            ordered_tool_definitions_digest: Digest::from_bytes("tools"),
            tool_execution_policy_digest: Digest::from_bytes("tool-execution-policy"),
            hook_bundle_digest: Digest::from_bytes("hooks"),
            capability_bindings_digest: Digest::from_bytes("bindings"),
            compaction_policy_digest: Digest::from_bytes("compaction"),
            provider_surface_digest: Digest::from_bytes("provider"),
        };

        let summary = execution_summary(
            &admission,
            &provenance,
            &surfaces,
            &ExecutionDiagnostics {
                turns_started: 1,
                engineering_phase: "submitted".to_owned(),
                tool_executions: Vec::new(),
            },
            ProviderEffectDiagnostics::default(),
            false,
            Some(ToolName::WorkComplete),
            false,
        )
        .expect("summary is canonical JSON");
        let summary = JsonValue::parse(&summary).expect("summary parses");
        assert_eq!(
            summary.to_json_string().expect("summary re-encodes"),
            execution_summary(
                &admission,
                &provenance,
                &surfaces,
                &ExecutionDiagnostics {
                    turns_started: 1,
                    engineering_phase: "submitted".to_owned(),
                    tool_executions: Vec::new(),
                },
                ProviderEffectDiagnostics::default(),
                false,
                Some(ToolName::WorkComplete),
                false,
            )
            .expect("canonical summary")
        );
        let factory = summary.get("factory").expect("Factory identities exist");
        assert_eq!(
            factory
                .get("application_revision_id")
                .and_then(JsonValue::as_str),
            Some("33"),
        );
        assert_eq!(
            factory
                .get("rust_host_identity")
                .and_then(JsonValue::as_str),
            Some(factory_settings::RUST_HOST_IDENTITY),
        );
        let tea = summary.get("tea_provenance").expect("Tea identities exist");
        assert_eq!(
            tea.get("harness_snapshot_id").and_then(JsonValue::as_str),
            Some("snapshot-id"),
        );
        assert_eq!(
            tea.get("harness_revision_id").and_then(JsonValue::as_str),
            Some("revision-id"),
        );
        assert_eq!(
            tea.get("model_harness_profile_id")
                .and_then(JsonValue::as_str),
            Some("profile-id"),
        );
        assert_eq!(
            summary
                .get("tea_surfaces")
                .and_then(|value| value.get("provider_surface_digest"))
                .and_then(JsonValue::as_str),
            Some(surfaces.provider_surface_digest.to_hex().as_str()),
        );
    }

    #[test]
    fn trace_raw_limit_has_a_conservative_gzip_bound() {
        let output_limit = 1_000_000;
        let raw_limit = max_gzip_input(output_limit);
        assert!(gzip_upper_bound_len(raw_limit) <= output_limit);
        assert!(gzip_upper_bound_len(raw_limit + 1) > output_limit);
    }

    #[test]
    fn transcript_gzip_is_valid_and_compresses_repeated_records() {
        let mut input = Vec::new();
        for sequence in 0..256 {
            input.extend_from_slice(
                format!(
                    "{{\"sequence\":{sequence},\"type\":\"tool_end\",\"tool\":\"workspace_read\",\"result\":\"unchanged\"}}\n"
                )
                .as_bytes(),
            );
        }
        let archive = gzip_transcript(&input).expect("compress transcript");
        assert!(archive.len() < gzip_upper_bound_len(input.len()));
        assert!(archive.len() * 2 < input.len());

        let path = std::env::temp_dir().join(format!(
            "factory-tea-host-gzip-test-{}.gz",
            std::process::id()
        ));
        fs::write(&path, &archive).expect("write gzip test member");
        let decoded = Command::new("gzip")
            .args(["-dc", path.to_str().expect("gzip test path")])
            .output()
            .expect("run gzip decoder");
        fs::remove_file(&path).expect("remove gzip test member");
        assert!(decoded.status.success());
        assert_eq!(decoded.stdout, input);
    }

    #[test]
    fn transcript_gzip_fallback_stays_within_the_stored_bound() {
        let mut input = Vec::with_capacity(100_000);
        let mut state = 0x9e37_79b9_u32;
        for _ in 0..100_000 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            input.push((state >> 24) as u8);
        }
        let archive = gzip_transcript(&input).expect("compress incompressible transcript");
        assert!(
            archive.len() <= gzip_upper_bound_len(input.len()),
            "archive={} bound={}",
            archive.len(),
            gzip_upper_bound_len(input.len())
        );
    }
}
