//! Narrow Factory durability gate for Tea provider requests.
//!
//! Factory tools already cross their own typed capability RPC boundary.  This
//! gate deliberately records only provider request intent and settlement, so
//! it cannot become a second generic event log or a duplicate tool authority.

use crate::{Admission, CommandContext, DaemonError, FramedDaemon};
use factory_protocol::{
    ContentDigest, OP_SESSION_PROVIDER_REQUEST_SETTLE, OP_SESSION_PROVIDER_REQUEST_START,
};
use std::fmt::Write as _;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tea_core::effect::{
    EffectAction, EffectFuture, EffectGate, EffectGateError, EffectKind, EffectOutcome,
    EffectSubject, ProviderEffectOutcome, RunProvenance,
};
use tea_core::scheduler::ModelRequest;
use tea_core::state::{StopReason, Usage};
use tea_protocol::{JsonNumber, JsonValue};

/// Factory's durable boundary for an externally observable provider request.
///
/// The actor socket binds session and assignment identity. The gate therefore
/// derives these from the admitted packet rather than accepting a model- or
/// provider-controlled identity in a request payload.
pub struct FactoryEffectGate {
    admission: Admission,
    daemon: Arc<dyn FramedDaemon>,
    command_context: CommandContext,
    effects: Mutex<BTreeMap<(String, u64), ProviderEffectStatus>>,
}

/// Host-observed facts that were acknowledged by Factory's durable ledger.
/// These are used only in the Factory execution summary; terminal recovery
/// independently rereads the ledger from PostgreSQL.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProviderEffectDiagnostics {
    /// Number of unique provider requests whose durable start was acknowledged.
    pub provider_effect_count: u32,
    /// Number of started requests with a durably acknowledged terminal outcome.
    pub settled_provider_effect_count: u32,
    /// Whether every acknowledged settled request reported every usage category.
    pub complete_provider_usage: bool,
    /// Whether every acknowledged settled request reported an exact provider cost.
    pub complete_provider_cost: bool,
}

#[derive(Clone)]
struct ProviderEffectStatus {
    state: &'static str,
    usage: Usage,
    reported_cost_micro_usd: Option<u64>,
}

impl FactoryEffectGate {
    /// Bind one gate to exactly one admitted Factory assignment.
    #[must_use]
    pub fn new(
        admission: Admission,
        daemon: Arc<dyn FramedDaemon>,
        command_context: CommandContext,
    ) -> Self {
        Self {
            admission,
            daemon,
            command_context,
            effects: Mutex::new(BTreeMap::new()),
        }
    }

    /// Return only compact effect-ledger facts acknowledged to this host.
    #[must_use]
    pub fn diagnostics(&self) -> ProviderEffectDiagnostics {
        let Ok(effects) = self.effects.lock() else {
            return ProviderEffectDiagnostics::default();
        };
        let complete_settlement = effects.values().all(|effect| effect.state == "settled");
        let complete_provider_usage = complete_settlement
            && effects.values().all(|effect| {
                effect.usage.input_tokens.is_some()
                    && effect.usage.output_tokens.is_some()
                    && effect.usage.cache_read_tokens.is_some()
                    && effect.usage.cache_write_tokens.is_some()
                    && effect.usage.reasoning_tokens.is_some()
            });
        let complete_provider_cost = complete_settlement
            && effects
                .values()
                .all(|effect| effect.reported_cost_micro_usd.is_some());
        ProviderEffectDiagnostics {
            provider_effect_count: u32::try_from(effects.len()).unwrap_or(u32::MAX),
            settled_provider_effect_count: u32::try_from(
                effects
                    .values()
                    .filter(|effect| effect.state != "started")
                    .count(),
            )
            .unwrap_or(u32::MAX),
            complete_provider_usage,
            complete_provider_cost,
        }
    }

    async fn record_start(&self, action: EffectAction, request: &ModelRequest) -> Result<(), EffectGateError> {
        let provenance = provider_provenance(&action)?;
        let model = request
            .model
            .as_ref()
            .ok_or_else(|| EffectGateError::new("provider request has no selected model"))?;
        let effect_key = (provenance.core_run_id.clone(), action.id().0);
        let payload = JsonValue::object([
            (
                "client_command_id",
                JsonValue::String(provider_command_id("start", &self.admission, &provenance, action.id().0)),
            ),
            ("expected_revision", number(self.command_context.current_revision())?),
            ("core_run_id", JsonValue::String(provenance.core_run_id)),
            ("effect_id", number(action.id().0)?),
            (
                "harness_snapshot_id",
                JsonValue::String(provenance.harness_snapshot_id),
            ),
            (
                "harness_revision_id",
                JsonValue::String(provenance.harness_revision_id),
            ),
            (
                "model_harness_profile_id",
                JsonValue::String(provenance.model_harness_profile_id),
            ),
            (
                "provider_surface_digest",
                JsonValue::String(provenance.provider_surface_digest),
            ),
            ("provider", JsonValue::String(model.provider.clone())),
            ("model", JsonValue::String(model.model.clone())),
            (
                "request_fingerprint",
                JsonValue::String(provider_request_fingerprint(request).to_hex()),
            ),
        ]);
        self.call(OP_SESSION_PROVIDER_REQUEST_START, payload).await?;
        self.record_status(
            effect_key,
            ProviderEffectStatus {
                state: "started",
                usage: Usage::default(),
                reported_cost_micro_usd: None,
            },
        );
        Ok(())
    }

    async fn record_settlement(
        &self,
        action: EffectAction,
        outcome: ProviderEffectOutcome,
    ) -> Result<(), EffectGateError> {
        let provenance = provider_provenance(&action)?;
        let settlement = ProviderSettlement::from_outcome(outcome)?;
        let effect_key = (provenance.core_run_id.clone(), action.id().0);
        let payload = JsonValue::object([
            (
                "client_command_id",
                JsonValue::String(provider_command_id("settle", &self.admission, &provenance, action.id().0)),
            ),
            ("expected_revision", number(self.command_context.current_revision())?),
            ("core_run_id", JsonValue::String(provenance.core_run_id)),
            ("effect_id", number(action.id().0)?),
            ("outcome", JsonValue::String(settlement.outcome.to_owned())),
            (
                "stop_reason",
                JsonValue::String(settlement.stop_reason.to_owned()),
            ),
            ("context_overflow", JsonValue::Bool(settlement.context_overflow)),
            ("input_tokens", optional_number(settlement.usage.input_tokens)?),
            ("output_tokens", optional_number(settlement.usage.output_tokens)?),
            (
                "cache_read_tokens",
                optional_number(settlement.usage.cache_read_tokens)?,
            ),
            (
                "cache_write_tokens",
                optional_number(settlement.usage.cache_write_tokens)?,
            ),
            (
                "reasoning_tokens",
                optional_number(settlement.usage.reasoning_tokens)?,
            ),
            (
                "reported_cost_micro_usd",
                optional_number(settlement.reported_cost_micro_usd)?,
            ),
            (
                "failure_class",
                settlement
                    .failure_class
                    .map_or(JsonValue::Null, |value| JsonValue::String(value.to_owned())),
            ),
        ]);
        self.call(OP_SESSION_PROVIDER_REQUEST_SETTLE, payload).await?;
        self.record_status(
            effect_key,
            ProviderEffectStatus {
                state: settlement.outcome,
                usage: settlement.usage,
                reported_cost_micro_usd: settlement.reported_cost_micro_usd,
            },
        );
        Ok(())
    }

    async fn call(&self, operation: &'static str, payload: JsonValue) -> Result<(), EffectGateError> {
        let response = self
            .daemon
            .call(operation, payload)
            .await
            .map_err(daemon_gate_error)?;
        if let Some(code) = response.get("error_code").and_then(JsonValue::as_str) {
            let message = response
                .get("message")
                .and_then(JsonValue::as_str)
                .unwrap_or("provider-effect daemon rejection");
            return Err(EffectGateError::new(format!(
                "provider-effect daemon rejection {code}: {message}"
            )));
        }
        if response.get("operation").and_then(JsonValue::as_str) != Some(operation)
            || response.get("effect_state").and_then(JsonValue::as_str).is_none()
        {
            return Err(EffectGateError::new(
                "provider-effect daemon response is not a closed receipt",
            ));
        }
        Ok(())
    }

    fn record_status(&self, key: (String, u64), status: ProviderEffectStatus) {
        if let Ok(mut effects) = self.effects.lock() {
            effects.insert(key, status);
        }
    }
}

impl EffectGate for FactoryEffectGate {
    fn before<'a>(&'a self, action: EffectAction) -> EffectFuture<'a> {
        Box::pin(async move {
            if action.kind() != EffectKind::ProviderRequest {
                // Tool and other non-provider effects retain their existing
                // typed Factory capability/custody boundaries. Recording them
                // here would duplicate authority in a generic side channel.
                return Ok(());
            }
            let EffectSubject::ProviderRequest { request } = action.subject() else {
                return Err(EffectGateError::new("provider effect action has an invalid subject"));
            };
            let request = request.clone();
            self.record_start(action, &request).await
        })
    }

    fn after<'a>(&'a self, action: EffectAction, outcome: EffectOutcome) -> EffectFuture<'a> {
        Box::pin(async move {
            if action.kind() != EffectKind::ProviderRequest {
                return Ok(());
            }
            let EffectOutcome::ProviderRequest(outcome) = outcome else {
                return Err(EffectGateError::new("provider effect outcome has an invalid category"));
            };
            self.record_settlement(action, outcome).await
        })
    }
}

struct ProviderProvenance {
    core_run_id: String,
    harness_snapshot_id: String,
    harness_revision_id: String,
    model_harness_profile_id: String,
    provider_surface_digest: String,
}

fn provider_provenance(action: &EffectAction) -> Result<ProviderProvenance, EffectGateError> {
    let source = action.provenance();
    let core_run_id = required_provenance(source, source.core_run_id.as_deref(), "core run")?;
    if core_run_id.len() > 60 {
        return Err(EffectGateError::new(
            "provider effect core run provenance exceeds command identity bound",
        ));
    }
    Ok(ProviderProvenance {
        core_run_id,
        harness_snapshot_id: required_provenance(
            source,
            source.harness_snapshot_id.as_deref(),
            "harness snapshot",
        )?,
        harness_revision_id: required_provenance(
            source,
            source.harness_revision_id.as_deref(),
            "harness revision",
        )?,
        model_harness_profile_id: required_provenance(
            source,
            source.model_harness_profile_id.as_deref(),
            "model harness profile",
        )?,
        provider_surface_digest: required_provenance(
            source,
            source.provider_surface_digest.as_deref(),
            "provider surface",
        )?,
    })
}

fn required_provenance(
    _source: &RunProvenance,
    value: Option<&str>,
    field: &str,
) -> Result<String, EffectGateError> {
    let value = value.ok_or_else(|| EffectGateError::new(format!("provider effect lacks {field} provenance")))?;
    if value.is_empty() || value.len() > 240 || value.contains('\0') {
        return Err(EffectGateError::new(format!("provider effect has invalid {field} provenance")));
    }
    Ok(value.to_owned())
}

struct ProviderSettlement {
    outcome: &'static str,
    stop_reason: &'static str,
    context_overflow: bool,
    usage: Usage,
    reported_cost_micro_usd: Option<u64>,
    failure_class: Option<&'static str>,
}

impl ProviderSettlement {
    fn from_outcome(outcome: ProviderEffectOutcome) -> Result<Self, EffectGateError> {
        match outcome {
            ProviderEffectOutcome::Settled(response) => {
                let (outcome, failure_class) = match response.stop_reason {
                    StopReason::Cancelled | StopReason::Aborted => ("cancelled", None),
                    StopReason::Error => ("failed", Some("provider_response_error")),
                    StopReason::Stop | StopReason::ToolUse | StopReason::Length => ("settled", None),
                };
                let usage = response.usage.unwrap_or_default();
                let reported_cost_micro_usd = usage
                    .cost
                    .as_deref()
                    .map(factory_protocol::MicroUsd::parse_decimal_usd)
                    .transpose()
                    .map_err(|_| EffectGateError::new("provider reported an invalid exact cost"))?
                    .map(factory_protocol::MicroUsd::get);
                Ok(Self {
                    outcome,
                    stop_reason: stop_reason_name(response.stop_reason),
                    context_overflow: response.context_overflow,
                    usage,
                    reported_cost_micro_usd,
                    failure_class,
                })
            }
            ProviderEffectOutcome::Failed { .. } => Ok(Self {
                outcome: "failed",
                stop_reason: "error",
                context_overflow: false,
                usage: Usage::default(),
                reported_cost_micro_usd: None,
                failure_class: Some("provider_transport_error"),
            }),
        }
    }
}

const fn stop_reason_name(reason: StopReason) -> &'static str {
    match reason {
        StopReason::Stop => "stop",
        StopReason::ToolUse => "tool_use",
        StopReason::Length => "length",
        StopReason::Aborted => "aborted",
        StopReason::Cancelled => "cancelled",
        StopReason::Error => "error",
    }
}

fn provider_command_id(
    phase: &str,
    admission: &Admission,
    provenance: &ProviderProvenance,
    effect_id: u64,
) -> String {
    // `core_run_id` is a Factory-minted closed value, and the whole string is
    // bounded well below the protocol command-ID ceiling by provenance checks.
    format!(
        "provider-effect-{phase}-session-{}-assignment-{}-run-{}-effect-{effect_id}",
        admission.frame.session_id, admission.packet.assignment_id, provenance.core_run_id
    )
}

fn provider_request_fingerprint(request: &ModelRequest) -> ContentDigest {
    // Hash content into component digests before forming the persisted
    // identity. The resulting request fingerprint distinguishes the exact
    // request shape without retaining prompts, context, tool arguments, or a
    // provider request body in Factory state.
    let mut canonical = String::new();
    append_component_digest(&mut canonical, "system_prompt", &request.system_prompt);
    append_component_digest(&mut canonical, "context", &request.context);
    if let Some(model) = &request.model {
        append_component(&mut canonical, "provider", &model.provider);
        append_component(&mut canonical, "model", &model.model);
        append_component(&mut canonical, "revision", model.revision.as_deref().unwrap_or(""));
    } else {
        append_component(&mut canonical, "model", "");
    }
    append_component(&mut canonical, "thinking", thinking_level_name(request.thinking_level));
    for tool in &request.tools {
        append_component(&mut canonical, "tool_name", &tool.name);
        append_component_digest(&mut canonical, "tool_description", &tool.description);
        let schema = tool.schema.to_json_string().unwrap_or_default();
        append_component_digest(&mut canonical, "tool_schema", &schema);
    }
    ContentDigest::of_bytes(canonical.as_bytes())
}

fn append_component_digest(output: &mut String, label: &str, value: &str) {
    append_component(output, label, &ContentDigest::of_bytes(value.as_bytes()).to_hex());
}

fn append_component(output: &mut String, label: &str, value: &str) {
    let _ = write!(output, "{label}:{}:{value}\n", value.len());
}

const fn thinking_level_name(value: tea_core::state::ThinkingLevel) -> &'static str {
    match value {
        tea_core::state::ThinkingLevel::Off => "off",
        tea_core::state::ThinkingLevel::Minimal => "minimal",
        tea_core::state::ThinkingLevel::Low => "low",
        tea_core::state::ThinkingLevel::Medium => "medium",
        tea_core::state::ThinkingLevel::High => "high",
        tea_core::state::ThinkingLevel::XHigh => "xhigh",
        tea_core::state::ThinkingLevel::Max => "max",
    }
}

fn number(value: u64) -> Result<JsonValue, EffectGateError> {
    JsonValue::number(JsonNumber::Unsigned(value))
        .map_err(|_| EffectGateError::new("provider-effect number is invalid"))
}

fn optional_number(value: Option<u64>) -> Result<JsonValue, EffectGateError> {
    value.map_or(Ok(JsonValue::Null), number)
}

fn daemon_gate_error(error: DaemonError) -> EffectGateError {
    EffectGateError::new(format!("provider-effect daemon transport failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::provider_request_fingerprint;
    use tea_core::scheduler::ModelRequest;

    #[test]
    fn provider_request_fingerprint_does_not_retain_request_content() {
        let request = ModelRequest {
            system_prompt: "secret system text".to_owned(),
            context: "secret model context".to_owned(),
            ..ModelRequest::default()
        };
        let fingerprint = provider_request_fingerprint(&request).to_hex();
        assert_eq!(fingerprint.len(), 64);
        assert!(!fingerprint.contains("secret"));
        assert_ne!(
            fingerprint,
            provider_request_fingerprint(&ModelRequest::default()).to_hex()
        );
    }
}
