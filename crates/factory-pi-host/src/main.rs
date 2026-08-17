//! Production actor-host entrypoint.

mod runtime;

use factory_pi_host::{
    Admission, AdmissionConfig, CommandContext, CostReader, ExecutionDiagnostics, FramedDaemon,
    TerminalDeferral, ToolName, build_factory_execution_input, read_admission_from_fd0,
};
use factory_protocol::{
    ArtifactReceiptResponse, OP_SESSION_SEAL_ARTIFACT, OP_SESSION_SUBMIT_TERMINAL,
    OP_SESSION_VERIFY_PACKET,
};
use pi_agent_core::provider::RetryPolicy;
use pi_agent_core::provider::openrouter::{OpenRouterConfig, OpenRouterProvider};
use pi_agent_core::scheduler::ModelProvider;
use pi_agent_core::state::StopReason;
use pi_agent_core::{AgentEvent, AgentEventKind};
use pi_agent_luau::{PolicyLimits, tool_handler::HandlerLimits};
use pi_agent_protocol::{JsonNumber, JsonValue};
use std::{env, fs, path::Path, process::ExitCode, sync::Arc, time::Duration};

const MAX_PROVIDER_REQUEST_TIMEOUT: Duration = Duration::from_mins(20);
// OpenRouter is used in finite-response mode: a partial JSON body can pause
// while the model continues generating. Keep this below the request timeout,
// but give long Product generations several minutes before declaring a stall.
const MAX_PROVIDER_STALL_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_PROVIDER_RETRIES: u32 = 2;

fn main() -> ExitCode {
    match smol::block_on(run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("factory-pi-host failed closed: {error}");
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
            .with_max_tokens(u64::from(admission.packet.model.output_token_limit))
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
        PolicyLimits::default(),
        HandlerLimits::default(),
        Some(cost_reader),
    )
    .map_err(|e| e.to_string())?;
    let prepared = input.prepare().map_err(|e| e.to_string())?;
    let result = match prepared.drive().await {
        Ok(result) => result,
        Err(error) => {
            if let Some(report) = provider.last_error_report() {
                eprintln!("factory-pi-host provider failure: {report}");
            }
            return Err(error.to_string());
        }
    };
    eprintln!(
        "factory-pi-host execution: turns_started={} engineering_phase={} stop_reason={:?} terminal={} cost_known={}",
        result.diagnostics.turns_started,
        result.diagnostics.engineering_phase,
        result.stop_reason(),
        result.terminal.is_some(),
        result.cost_micro_usd.is_some(),
    );
    let transcript = write_transcript(&admission, &result.events, &result.diagnostics)?;
    let transcript_id =
        seal_transcript(daemon.as_ref(), &admission, transcript, &command_context).await?;
    let usage = provider.usage_snapshot();
    let completed = result.terminal.is_some();
    let terminal_operation = completed
        .then(|| {
            result
                .terminal
                .as_ref()
                .map(|terminal| terminal.tool.as_str().to_owned())
        })
        .flatten();
    let terminal_payload = result
        .terminal
        .as_ref()
        .map(|terminal| terminal.payload.to_json_string())
        .transpose()
        .map_err(|e| e.to_string())?
        .unwrap_or_else(|| "{}".to_owned());
    let stop_reason = if completed {
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
                ("input_tokens", number(usage.input_tokens.unwrap_or(0))?),
                ("output_tokens", number(usage.output_tokens.unwrap_or(0))?),
                ("cache_read_tokens", number(0)?),
                ("cache_write_tokens", number(0)?),
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

fn provider_request_timeout(wall_limit_millis: u64) -> Duration {
    Duration::from_millis((wall_limit_millis / 3).max(1)).min(MAX_PROVIDER_REQUEST_TIMEOUT)
}

fn provider_stall_timeout(wall_limit_millis: u64) -> Duration {
    Duration::from_millis((wall_limit_millis / 4).max(1)).min(MAX_PROVIDER_STALL_TIMEOUT)
}

fn provider_retry_policy() -> RetryPolicy {
    RetryPolicy::new(
        MAX_PROVIDER_RETRIES,
        Duration::from_millis(250),
        Duration::from_secs(8),
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

async fn seal_transcript(
    daemon: &runtime::InheritedDaemon,
    admission: &Admission,
    transcript: Vec<u8>,
    command_context: &CommandContext,
) -> Result<i64, String> {
    let path = Path::new(&admission.packet.staging_root).join("session.ndjson.gz");
    fs::write(path, transcript).map_err(|e| format!("write transcript: {e}"))?;
    let response = daemon
        .call(
            OP_SESSION_SEAL_ARTIFACT,
            JsonValue::object([
                (
                    "client_command_id",
                    JsonValue::String(format!("host-transcript-{}", admission.frame.session_id)),
                ),
                (
                    "expected_revision",
                    number(command_context.current_revision())?,
                ),
                (
                    "staging_relative_path",
                    JsonValue::String("session.ndjson.gz".to_owned()),
                ),
                ("role", JsonValue::String("pi_transcript_gzip".to_owned())),
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
            "daemon transcript seal rejected: {error_code}: {message}"
        ));
    }
    let text = response.to_json_string().map_err(|e| e.to_string())?;
    let receipt: ArtifactReceiptResponse = miniserde::json::from_str(&text)
        .map_err(|_| "daemon transcript receipt is invalid".to_owned())?;
    if receipt.operation != OP_SESSION_SEAL_ARTIFACT
        || receipt.byte_length as usize > admission.packet.limits.output_byte_limit as usize
    {
        return Err("daemon transcript receipt is outside packet limits".to_owned());
    }
    command_context.advance_revision(receipt.aggregate_revision);
    Ok(receipt.artifact_id)
}

fn write_transcript(
    admission: &Admission,
    events: &[AgentEvent],
    diagnostics: &ExecutionDiagnostics,
) -> Result<Vec<u8>, String> {
    let limit = admission.packet.limits.output_byte_limit as usize;
    let mut lines = Vec::new();
    for event in events {
        let line = project_event(event)?;
        if gzip_stored_len(lines.len() + line.len() + 1) > limit {
            break;
        }
        lines.extend_from_slice(line.as_bytes());
        lines.push(b'\n');
    }
    let summary = execution_summary(diagnostics)?;
    if gzip_stored_len(lines.len() + summary.len() + 1) <= limit {
        lines.extend_from_slice(summary.as_bytes());
        lines.push(b'\n');
    }
    Ok(gzip_stored(&lines))
}

fn execution_summary(diagnostics: &ExecutionDiagnostics) -> Result<String, String> {
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
        ("type", JsonValue::String("execution_summary".to_owned())),
        (
            "turns_started",
            number(u64::from(diagnostics.turns_started))?,
        ),
        (
            "engineering_phase",
            JsonValue::String(diagnostics.engineering_phase.clone()),
        ),
        ("tool_executions", JsonValue::Array(tool_executions)),
    ])
    .to_json_string()
    .map_err(|e| e.to_string())
}

fn project_event(event: &AgentEvent) -> Result<String, String> {
    let mut fields = vec![("sequence", number(event.sequence.0)?)];
    match &event.kind {
        AgentEventKind::AgentStart => {
            fields.push(("type", JsonValue::String("agent_start".to_owned())));
        }
        AgentEventKind::TurnStart { turn_id } => {
            fields.push(("type", JsonValue::String("turn_start".to_owned())));
            fields.push(("turn_id", number(turn_id.0)?));
        }
        AgentEventKind::TurnEnd { turn_id, reason } => {
            fields.push(("type", JsonValue::String("turn_end".to_owned())));
            fields.push(("turn_id", number(turn_id.0)?));
            fields.push(("reason", JsonValue::String(format!("{reason:?}"))));
        }
        AgentEventKind::MessageEnd { message } => {
            fields.push(("type", JsonValue::String("message_end".to_owned())));
            if let pi_agent_core::state::Message::Assistant { content, .. } = message {
                fields.push((
                    "assistant_text",
                    JsonValue::String(bound(content, 16 * 1024)),
                ));
            }
        }
        AgentEventKind::ToolExecutionStart {
            tool_name,
            arguments,
            ..
        } => {
            fields.push(("type", JsonValue::String("tool_start".to_owned())));
            fields.push(("tool", JsonValue::String(tool_name.clone())));
            fields.push(("arguments", redacted_json(arguments.as_str())));
        }
        AgentEventKind::ToolExecutionEnd {
            tool_name, result, ..
        } => {
            fields.push(("type", JsonValue::String("tool_end".to_owned())));
            fields.push(("tool", JsonValue::String(tool_name.clone())));
            fields.push((
                "result",
                JsonValue::String(bound(&result.content, 16 * 1024)),
            ));
            fields.push(("is_error", JsonValue::Bool(result.is_error)));
        }
        AgentEventKind::AgentEnd { .. } => {
            fields.push(("type", JsonValue::String("agent_end".to_owned())));
        }
        _ => return Ok(String::new()),
    }
    JsonValue::object(fields)
        .to_json_string()
        .map_err(|e| e.to_string())
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

fn provider_cost(provider: &OpenRouterProvider) -> Option<u64> {
    let report = provider.cost_report();
    report
        .complete
        .then_some(report.reported_total_usd_exact)
        .flatten()
        .and_then(|value| micro_usd(&value))
}

fn micro_usd(value: &str) -> Option<u64> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    let whole = whole.parse::<u64>().ok()?;
    let mut digits = fraction
        .as_bytes()
        .iter()
        .copied()
        .take(6)
        .collect::<Vec<_>>();
    if !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    while digits.len() < 6 {
        digits.push(b'0');
    }
    let fractional = std::str::from_utf8(&digits).ok()?.parse::<u64>().ok()?;
    let round_up = fraction
        .as_bytes()
        .get(6..)
        .is_some_and(|tail| tail.iter().any(|byte| *byte != b'0'));
    whole
        .checked_mul(1_000_000)?
        .checked_add(fractional)?
        .checked_add(u64::from(round_up))
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

fn gzip_stored_len(input_len: usize) -> usize {
    if input_len == 0 {
        23
    } else {
        18 + input_len + input_len.div_ceil(65_535) * 5
    }
}

fn gzip_stored(input: &[u8]) -> Vec<u8> {
    let mut output = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255];
    if input.is_empty() {
        output.extend_from_slice(&[1, 0, 0, 255, 255]);
    }
    for (index, chunk) in input.chunks(65_535).enumerate() {
        output.push(u8::from((index + 1) * 65_535 >= input.len()));
        let length = chunk.len() as u16;
        output.extend_from_slice(&length.to_le_bytes());
        output.extend_from_slice(&(!length).to_le_bytes());
        output.extend_from_slice(chunk);
    }
    output.extend_from_slice(&crc32(input).to_le_bytes());
    output.extend_from_slice(&(input.len() as u32).to_le_bytes());
    output
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0xedb8_8320
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::{
        packet_verification_error, provider_request_timeout, provider_retry_policy,
        provider_stall_timeout,
    };
    use pi_agent_protocol::JsonValue;
    use std::time::Duration;

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
    fn provider_request_timeout_is_capped_below_assignment_wall() {
        assert_eq!(
            provider_request_timeout(1_800_000),
            Duration::from_secs(600)
        );
        assert_eq!(provider_request_timeout(900_000), Duration::from_secs(300));
        assert_eq!(provider_request_timeout(120_000), Duration::from_secs(40));
        assert_eq!(
            provider_request_timeout(3_600_000),
            Duration::from_mins(20)
        );
    }

    #[test]
    fn provider_retry_policy_allows_two_replay_safe_retries() {
        assert_eq!(provider_retry_policy().max_retries(), 2);
    }

    #[test]
    fn provider_stall_timeout_is_bounded_within_assignment_wall() {
        assert_eq!(provider_stall_timeout(1_800_000), Duration::from_secs(450));
        assert_eq!(provider_stall_timeout(900_000), Duration::from_secs(225));
        assert_eq!(provider_stall_timeout(120_000), Duration::from_secs(30));
        assert_eq!(provider_stall_timeout(3_600_000), Duration::from_secs(600));
    }
}
