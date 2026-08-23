//! Translation from one sealed Factory assignment into one hosted Tea epoch.
//!
//! Factory owns packet admission, provider selection, outer session custody,
//! and every effect. Tea receives exact sealed policy bytes plus explicit host
//! bindings and returns the immutable model-facing harness identity used by
//! the caller-driven assignment process.

use crate::Admission;
use crate::tool_bridge::{BoundTool, FACTORY_CAPABILITY};
use factory_protocol::ContentDigest;
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::sync::Arc;
use tea_core::effect::{EffectGate, RunProvenance};
use tea_core::harness::extension::{
    ExtensionCapability, ExtensionEngine, ExtensionLimits, ExtensionSourceTree, ExtensionToolLimits,
};
use tea_core::harness::{
    CapabilityBindingRef, HarnessActor, HarnessError, HarnessResolver, HarnessResourceLimits,
    HarnessRuntimePolicyDescriptors, HarnessSeedBuilder, HarnessSeedExtension,
    HarnessSeedExtensionScope, ModelHarnessProfile, PluginCapabilityBinding,
    PluginCapabilityCatalog, SelfExtensionMode, ToolPresentationDescriptor,
};
use tea_core::hooks::HookSet;
use tea_core::runtime::{HostedEpoch, HostedEpochInput, RuntimeServices};
use tea_core::scheduler::ModelProvider;
use tea_core::state::{ModelDescriptor, ThinkingLevel};
use tea_core::tool::ToolRegistry;
use tea_luau::LuauExtensionEngine;
use tea_protocol::{JsonNumber, JsonValue};
use tea_session::{CanonicalHashWriter, Digest, MemoryArtifactStore};

pub(crate) const FACTORY_CAPABILITY_ABI: &str = "factory-capability-v1";
const FACTORY_HOSTED_PROMPT_PROFILE: &str = "factory-sealed-system-v2";
const FACTORY_HOSTED_TOOL_PROFILE: &str = "factory-policy-tools-v1";
const FACTORY_COMPACTION_POLICY: &str = "tea-core-default-compaction-v1";
const FACTORY_PROJECTION_POLICY: &str = "tea-core-default-tool-projection-v1";
const FACTORY_FAILURE_POLICY: &str = "tea-core-default-tool-failure-v1";

/// Provider-independent policy material verified before Tea resolution.
#[derive(Debug)]
pub(crate) struct VerifiedExtension {
    pub(crate) source: ExtensionSourceTree,
    pub(crate) tools: Vec<BoundTool>,
}

/// Verify sealed policy bytes and derive their language-neutral Tea descriptor.
pub(crate) fn verify_extension(admission: &Admission) -> Result<VerifiedExtension, HarnessError> {
    let packet = &admission.packet;
    if packet.policy_entrypoint != factory_protocol::PolicyEntrypointV2::FACTORY_POLICY {
        return Err(HarnessError::invalid_state(
            "packet policy entrypoint is not factory_policy",
        ));
    }
    let bytes = crate::admission::decode_base64(&packet.policy_bytes_b64).ok_or_else(|| {
        HarnessError::invalid_state("packet policy bytes are not canonical base64")
    })?;
    if bytes.is_empty() || bytes.len() > packet.policy_byte_limit as usize {
        return Err(HarnessError::invalid_state(
            "sealed policy source exceeds its admitted byte limit",
        ));
    }
    let expected = ContentDigest::from_str(&packet.policy_digest)
        .map_err(|_| HarnessError::invalid_state("sealed policy digest is invalid"))?;
    if ContentDigest::of_bytes(&bytes) != expected {
        return Err(HarnessError::invalid_state(
            "sealed policy digest mismatches",
        ));
    }
    let policy = String::from_utf8(bytes)
        .map_err(|_| HarnessError::invalid_state("sealed policy source is not UTF-8"))?;
    let extension_id = extension_id(admission, &expected)?;
    let limits = resource_limits(admission)?;
    let manifest = extension_manifest(&extension_id, &limits)?;
    let source = ExtensionSourceTree {
        extension_id,
        files: BTreeMap::from([
            ("main.luau".to_owned(), policy),
            ("manifest.json".to_owned(), manifest),
        ]),
        expected_capabilities: Some(BTreeSet::from([FACTORY_CAPABILITY.to_owned()])),
        limits: ExtensionLimits {
            max_source_bytes: limits.source_bytes,
            max_memory_bytes: limits.memory_bytes,
            max_interrupt_checks: limits.instruction_checks as usize,
        },
    };
    let descriptor = LuauExtensionEngine
        .describe(&source)
        .map_err(|error| HarnessError::invalid_state(error.to_string()))?;
    if descriptor.requested_capabilities != BTreeSet::from([FACTORY_CAPABILITY.to_owned()]) {
        return Err(HarnessError::invalid_state(
            "sealed policy must request exactly the factory capability",
        ));
    }
    let tools = crate::tool_bridge::bind_extension_tools(&descriptor.tools, &packet.tools)
        .map_err(|error| HarnessError::invalid_state(error.to_string()))?;
    Ok(VerifiedExtension { source, tools })
}

/// Resolve and prepare one standard, stateless Tea hosted epoch.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_hosted_epoch(
    admission: &Admission,
    provider: Arc<dyn ModelProvider>,
    verified: VerifiedExtension,
    capability: Arc<dyn ExtensionCapability>,
    effect_gate: Arc<dyn EffectGate>,
    base_hooks: Arc<dyn HookSet>,
) -> Result<HostedEpoch, HarnessError> {
    let system_prompt = decode_prompt(&admission.packet.system_prompt_bytes_b64, "system prompt")?;
    let model = ModelDescriptor {
        provider: admission.packet.model.provider.clone(),
        model: admission.packet.model.model_id.clone(),
        revision: None,
    };
    let thinking = parse_thinking_level(&admission.packet.model.thinking_level)?;
    let profile = ModelHarnessProfile::new(
        model.provider.clone(),
        model.model.clone(),
        model.revision.clone(),
        FACTORY_HOSTED_PROMPT_PROFILE,
        FACTORY_HOSTED_TOOL_PROFILE,
        FACTORY_COMPACTION_POLICY,
        FACTORY_PROJECTION_POLICY,
    )?;
    let limits = resource_limits(admission)?;
    let handler_limits = ExtensionToolLimits {
        max_source_bytes: limits.source_bytes,
        max_memory_bytes: limits.memory_bytes,
        max_interrupt_checks: limits.instruction_checks as usize,
        max_capability_calls: ExtensionToolLimits::default().max_capability_calls,
    };
    let host_identity = capability_host_identity(admission, &verified.tools, handler_limits);
    let binding = PluginCapabilityBinding::new(
        verified.source.extension_id.clone(),
        FACTORY_CAPABILITY,
        FACTORY_CAPABILITY_ABI,
        host_identity,
        handler_limits,
        capability,
    )
    .map_err(|error| HarnessError::invalid_state(error.to_string()))?;
    let binding_reference = CapabilityBindingRef {
        plugin_id: binding.plugin_id().to_owned(),
        capability: binding.capability().to_owned(),
        capability_version: binding.capability_version().to_owned(),
        binding_digest: binding.binding_digest(),
    };
    let artifacts: Arc<dyn tea_session::ArtifactStore> = Arc::new(MemoryArtifactStore::default());
    let policies = HarnessRuntimePolicyDescriptors {
        hook_bundle_digest: hook_bundle_digest(admission),
        compaction_policy_digest: Digest::from_bytes(FACTORY_COMPACTION_POLICY),
        tool_projection_digest: Digest::from_bytes(FACTORY_PROJECTION_POLICY),
        failure_policy_digest: Digest::from_bytes(FACTORY_FAILURE_POLICY),
    };
    let seeded = HarnessSeedBuilder::new(
        artifacts,
        Arc::new(LuauExtensionEngine),
        base_profile_digest(&system_prompt, &model, thinking),
        system_prompt,
        profile,
        SelfExtensionMode::Off,
        limits,
        policies,
    )
    .extensions(vec![HarnessSeedExtension {
        scope: HarnessSeedExtensionScope::Session,
        source: verified.source,
    }])
    .capability_bindings(vec![binding_reference])
    .seed(HarnessActor::Host, 0)?;
    verify_resolved_tool_surface(
        &seeded.snapshot.spec.plugin_tool_presentations,
        &verified.tools,
    )?;
    let revision_id = seeded.revision.revision_id.clone();
    let mut catalog = PluginCapabilityCatalog::new();
    catalog
        .insert(binding)
        .map_err(|error| HarnessError::invalid_state(error.to_string()))?;
    let services = RuntimeServices::new(provider, ToolRegistry::default())
        .model(model)
        .thinking_level(thinking)
        .hooks(base_hooks);
    let resolver = HarnessResolver::new(
        seeded.repository,
        services.clone(),
        BTreeSet::from([FACTORY_CAPABILITY.to_owned()]),
    )
    .self_extension_mode(SelfExtensionMode::Off)
    .capability_catalog(catalog);
    let resolved = resolver.resolve_revision(&revision_id)?;
    services.prepare_hosted_epoch(
        &resolved,
        HostedEpochInput {
            effect_gate,
            provenance: factory_provenance(admission),
            additional_tools: ToolRegistry::default(),
        },
    )
}

fn extension_id(admission: &Admission, digest: &ContentDigest) -> Result<String, HarnessError> {
    let role = admission.packet.assignment_role.replace('_', "-");
    let id = format!(
        "factory.{}.{}.{}",
        admission.packet.application_revision_id,
        role,
        digest.to_hex(),
    );
    if id.len() > 120 {
        return Err(HarnessError::invalid_state(
            "derived Factory policy extension ID exceeds Tea's portable identity limit",
        ));
    }
    Ok(id)
}

fn extension_manifest(
    extension_id: &str,
    limits: &HarnessResourceLimits,
) -> Result<String, HarnessError> {
    JsonValue::object([
        ("abi_version", JsonValue::Number(JsonNumber::Unsigned(1))),
        ("entrypoint", JsonValue::String("main.luau".to_owned())),
        ("id", JsonValue::String(extension_id.to_owned())),
        (
            "modules",
            JsonValue::Array(vec![JsonValue::String("main.luau".to_owned())]),
        ),
        (
            "requested_capabilities",
            JsonValue::Array(vec![JsonValue::String(FACTORY_CAPABILITY.to_owned())]),
        ),
        (
            "resource_limits",
            JsonValue::object([
                (
                    "instruction_checks",
                    JsonValue::Number(JsonNumber::Unsigned(u64::from(limits.instruction_checks))),
                ),
                (
                    "memory_bytes",
                    JsonValue::Number(JsonNumber::Unsigned(limits.memory_bytes as u64)),
                ),
                (
                    "source_bytes",
                    JsonValue::Number(JsonNumber::Unsigned(limits.source_bytes as u64)),
                ),
            ]),
        ),
        ("schema_version", JsonValue::Number(JsonNumber::Unsigned(1))),
    ])
    .to_json_string()
    .map_err(|error| HarnessError::invalid_state(error.to_string()))
}

fn resource_limits(admission: &Admission) -> Result<HarnessResourceLimits, HarnessError> {
    let source_bytes = admission.packet.policy_byte_limit as usize;
    if source_bytes == 0 || source_bytes > HarnessResourceLimits::default().source_bytes {
        return Err(HarnessError::invalid_state(
            "packet policy byte limit exceeds the installed Tea policy ceiling",
        ));
    }
    Ok(HarnessResourceLimits {
        source_bytes,
        ..HarnessResourceLimits::default()
    })
}

fn decode_prompt(value: &str, label: &str) -> Result<String, HarnessError> {
    let bytes = crate::admission::decode_base64(value)
        .ok_or_else(|| HarnessError::invalid_state(format!("{label} is not canonical base64")))?;
    String::from_utf8(bytes)
        .map_err(|_| HarnessError::invalid_state(format!("{label} is not UTF-8")))
}

pub(crate) fn decode_assignment_prompt(admission: &Admission) -> Result<String, HarnessError> {
    decode_prompt(
        &admission.packet.assignment_prompt_bytes_b64,
        "assignment prompt",
    )
}

fn parse_thinking_level(value: &str) -> Result<ThinkingLevel, HarnessError> {
    match value {
        "none" | "off" => Ok(ThinkingLevel::Off),
        "minimal" => Ok(ThinkingLevel::Minimal),
        "low" => Ok(ThinkingLevel::Low),
        "medium" => Ok(ThinkingLevel::Medium),
        "high" => Ok(ThinkingLevel::High),
        "xhigh" => Ok(ThinkingLevel::XHigh),
        _ => Err(HarnessError::invalid_state(format!(
            "unsupported packet thinking level {value:?}",
        ))),
    }
}

fn base_profile_digest(
    system_prompt: &str,
    model: &ModelDescriptor,
    thinking: ThinkingLevel,
) -> Digest {
    let mut writer = CanonicalHashWriter::new("factory-hosted-base-profile-v1", 1, 1);
    writer.string("system_prompt", system_prompt);
    writer.string("provider", &model.provider);
    writer.string("model", &model.model);
    writer.string("thinking", &format!("{thinking:?}"));
    writer.finish()
}

fn hook_bundle_digest(admission: &Admission) -> Digest {
    let mut writer = CanonicalHashWriter::new("factory-hosted-hook-bundle-v1", 1, 1);
    writer.string("base_context", "tea-openai-context-v1");
    writer.string("factory_phase", "factory-engineering-phase-v1");
    writer.string("policy_digest", &admission.packet.policy_digest);
    writer.finish()
}

fn capability_host_identity(
    admission: &Admission,
    tools: &[BoundTool],
    limits: ExtensionToolLimits,
) -> Digest {
    let mut writer = CanonicalHashWriter::new("factory-capability-host-v1", 1, 1);
    writer.string("capability_abi", FACTORY_CAPABILITY_ABI);
    writer.string("rust_host_identity", factory_settings::RUST_HOST_IDENTITY);
    writer.string("kernel_build_id", &admission.packet.kernel_build_id);
    writer.u64("tool_count", tools.len() as u64);
    for tool in tools {
        writer.string("tool", tool.name.as_str());
        writer.string(
            "method",
            crate::tool_bridge::tool_contract(tool.name).capability_method,
        );
        writer.string("execution_mode", &tool.execution_mode);
    }
    writer.u64("max_source_bytes", limits.max_source_bytes as u64);
    writer.u64("max_memory_bytes", limits.max_memory_bytes as u64);
    writer.u64("max_interrupt_checks", limits.max_interrupt_checks as u64);
    writer.u64("max_capability_calls", limits.max_capability_calls as u64);
    writer.finish()
}

fn verify_resolved_tool_surface(
    tea_tools: &[ToolPresentationDescriptor],
    factory_tools: &[BoundTool],
) -> Result<(), HarnessError> {
    if tea_tools.len() != factory_tools.len()
        || tea_tools.iter().zip(factory_tools).any(|(tea, factory)| {
            tea.name != factory.name.as_str()
                || tea.description != factory.description
                || tea.schema != factory.schema
                || tea.execution_mode != factory.execution_mode
        })
    {
        return Err(HarnessError::invalid_state(
            "resolved Tea tool surface differs from the verified Factory packet surface",
        ));
    }
    Ok(())
}

fn factory_provenance(admission: &Admission) -> RunProvenance {
    RunProvenance {
        session_id: Some(admission.frame.session_id.to_string()),
        lane_id: None,
        operation_id: Some(format!(
            "factory-assignment-{}",
            admission.packet.assignment_id
        )),
        epoch_id: Some(format!(
            "factory-hosted-epoch-{}",
            admission.packet.assignment_id
        )),
        core_run_id: Some(format!(
            "factory-core-run-{}",
            admission.packet.assignment_id
        )),
        experiment_id: None,
        ..RunProvenance::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use factory_protocol::{AssignmentPacketWireV2, SessionAdmissionFrameV2};
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tea_core::harness::extension::{
        ExtensionCapabilityFuture, ExtensionCapabilityRequest, ExtensionCapabilityResponse,
    };
    use tea_core::hooks::NoHooks;
    use tea_core::scheduler::{
        CancellationToken, ModelFuture, ModelRequest, ModelStream, ModelStreamEvent,
    };
    use tea_core::state::{AgentToolCall, SerializedJson, StopReason, ToolCallId};

    const POLICY: &str = r#"
        return {
            prompt_sections = {},
            tools = {{
                name = "work_complete",
                description = "Finish the assignment.",
                capability = "factory",
                execution_mode = "sequential",
                schema_json = '{"type":"object","additionalProperties":false}',
                handler_source = [[
                    return function(call)
                        local result = coroutine.yield({
                            kind = "capability",
                            capability = "factory",
                            method = "work.complete",
                            arguments_json = call.arguments_json,
                        })
                        return result
                    end
                ]],
            }},
        }
    "#;

    struct NoProviderCalls(AtomicUsize);

    impl ModelProvider for NoProviderCalls {
        fn stream(
            &self,
            _request: ModelRequest,
            _cancellation: CancellationToken,
        ) -> ModelFuture<'_> {
            self.0.fetch_add(1, Ordering::Relaxed);
            panic!("harness preparation contacted the provider")
        }
    }

    struct NoopCapability;

    impl ExtensionCapability for NoopCapability {
        fn invoke(
            &self,
            _request: ExtensionCapabilityRequest,
            _cancellation: CancellationToken,
        ) -> ExtensionCapabilityFuture {
            Box::pin(std::future::ready(Ok(ExtensionCapabilityResponse {
                value: JsonValue::Null,
            })))
        }
    }

    struct ScriptedProvider {
        streams: Mutex<VecDeque<ModelStream>>,
    }

    impl ModelProvider for ScriptedProvider {
        fn stream(
            &self,
            _request: ModelRequest,
            _cancellation: CancellationToken,
        ) -> ModelFuture<'_> {
            let stream = self
                .streams
                .lock()
                .expect("scripted provider mutex")
                .pop_front()
                .expect("one scripted provider stream remains");
            Box::pin(std::future::ready(Ok(Box::new(stream) as _)))
        }
    }

    struct RecordingCapability(AtomicUsize);

    impl ExtensionCapability for RecordingCapability {
        fn invoke(
            &self,
            request: ExtensionCapabilityRequest,
            _cancellation: CancellationToken,
        ) -> ExtensionCapabilityFuture {
            assert_eq!(request.tool_name, "work_complete");
            assert_eq!(request.capability, FACTORY_CAPABILITY);
            assert_eq!(request.method, "work.complete");
            self.0.fetch_add(1, Ordering::Relaxed);
            Box::pin(std::future::ready(Ok(ExtensionCapabilityResponse {
                value: JsonValue::object([
                    ("content", JsonValue::String("completed".to_owned())),
                    ("details_json", JsonValue::String("{}".to_owned())),
                    ("is_error", JsonValue::Bool(false)),
                    ("terminate", JsonValue::Bool(true)),
                ]),
            })))
        }
    }

    fn admission(policy: &str) -> Admission {
        let mut packet: AssignmentPacketWireV2 = miniserde::json::from_str(include_str!(
            "../../../tests/protocol-fixtures/assignment-packet-v2.json"
        ))
        .expect("generic packet fixture parses");
        packet.policy_bytes_b64 = encode_base64(policy.as_bytes());
        packet.policy_digest = ContentDigest::of_bytes(policy.as_bytes()).to_hex();
        packet.policy_byte_limit = 65_536;
        packet.tools = vec!["work_complete".to_owned()];
        packet.terminal_operations = vec!["work_complete".to_owned()];
        let frame = SessionAdmissionFrameV2 {
            r#type: "session.admitted".to_owned(),
            protocol_version: factory_protocol::PROTOCOL_VERSION_V2,
            assignment_id: packet.assignment_id.to_string(),
            session_id: 9,
            session_revision: 7,
            packet_digest: packet.packet_digest.clone(),
            packet_b64: "AA==".to_owned(),
        };
        Admission {
            frame,
            packet_bytes: Vec::new(),
            packet,
        }
    }

    #[test]
    fn extension_identity_manifest_and_capability_are_deterministic() {
        let admission = admission(POLICY);
        let first = verify_extension(&admission).expect("policy verifies");
        let second = verify_extension(&admission).expect("same policy verifies again");
        assert_eq!(first.source, second.source);
        assert!(
            first
                .source
                .extension_id
                .starts_with("factory.33.engineering.")
        );
        let descriptor = LuauExtensionEngine
            .describe(&first.source)
            .expect("generated manifest and source resolve");
        assert_eq!(
            descriptor.requested_capabilities,
            BTreeSet::from([FACTORY_CAPABILITY.to_owned()])
        );
        assert_eq!(first.tools.len(), 1);
        assert_eq!(first.tools[0].name.as_str(), "work_complete");
    }

    #[test]
    fn sealed_policy_and_packet_tool_mismatches_fail_before_resolution() {
        let mut wrong_digest = admission(POLICY);
        wrong_digest.packet.policy_digest = ContentDigest::of_bytes(b"different").to_hex();
        assert!(
            verify_extension(&wrong_digest)
                .expect_err("digest mismatch is rejected")
                .to_string()
                .contains("digest mismatches")
        );

        let mut missing = admission(POLICY);
        missing.packet.tools.clear();
        assert!(
            verify_extension(&missing)
                .expect_err("missing admitted tool is rejected")
                .to_string()
                .contains("not present in the packet allowlist")
        );

        let mut duplicate = admission(POLICY);
        duplicate.packet.tools.push("work_complete".to_owned());
        assert!(
            verify_extension(&duplicate)
                .expect_err("duplicate admitted tool is rejected")
                .to_string()
                .contains("allowlist contains duplicates")
        );

        let unknown_policy = POLICY.replace("work_complete", "unknown_tool");
        let unknown = admission(&unknown_policy);
        assert!(
            verify_extension(&unknown)
                .expect_err("unknown policy tool is rejected")
                .to_string()
                .contains("policy declares unknown tool")
        );

        let mut extra_packet_tool = admission(POLICY);
        extra_packet_tool
            .packet
            .tools
            .push("workspace_read".to_owned());
        assert!(
            verify_extension(&extra_packet_tool)
                .expect_err("packet tool missing from policy is rejected")
                .to_string()
                .contains("policy does not declare packet tools: workspace_read")
        );

        let unbound_policy = POLICY.replace("capability = \"factory\"", "capability = \"unbound\"");
        assert!(
            verify_extension(&admission(&unbound_policy))
                .expect_err("unbound policy capability is rejected")
                .to_string()
                .contains("requests unbound capability")
        );
    }

    #[test]
    fn preparation_preserves_packet_surface_without_provider_or_hidden_tools() {
        let admission = admission(POLICY);
        let verified = verify_extension(&admission).expect("policy verifies");
        let expected_tools = verified.tools.clone();
        let provider = Arc::new(NoProviderCalls(AtomicUsize::new(0)));
        let hosted = prepare_hosted_epoch(
            &admission,
            provider.clone(),
            verified,
            Arc::new(NoopCapability),
            Arc::new(tea_core::effect::NoopEffectGate),
            Arc::new(NoHooks),
        )
        .expect("hosted epoch prepares without executing");

        assert_eq!(provider.0.load(Ordering::Relaxed), 0);
        let snapshot = hosted.agent().snapshot();
        assert_eq!(snapshot.system_prompt, "sealed system");
        assert_eq!(
            snapshot.model.as_ref().map(|model| model.provider.as_str()),
            Some("provider")
        );
        assert_eq!(
            snapshot.model.as_ref().map(|model| model.model.as_str()),
            Some("model")
        );
        assert_eq!(snapshot.thinking_level, ThinkingLevel::High);
        assert_eq!(
            decode_assignment_prompt(&admission).unwrap(),
            "sealed assignment"
        );
        let definitions = hosted.agent().tool_definitions();
        assert_eq!(definitions.len(), expected_tools.len());
        for (definition, expected) in definitions.iter().zip(&expected_tools) {
            assert_eq!(definition.name, expected.name.as_str());
            assert_eq!(definition.description, expected.description);
            assert_eq!(definition.schema, expected.schema);
            assert_eq!(
                format!("{:?}", definition.execution_mode).to_lowercase(),
                expected.execution_mode,
            );
        }
        let provider_surface = hosted
            .surface_fingerprints()
            .provider_surface_digest
            .to_hex();
        assert_eq!(
            hosted.provenance().provider_surface_digest.as_deref(),
            Some(provider_surface.as_str())
        );
    }

    #[test]
    fn explicit_application_source_policies_match_their_declared_tool_surfaces() {
        let Some(root) = std::env::var_os("FACTORY_APPLICATION_SOURCE_ROOT") else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        let bundle_bytes = std::fs::read(root.join("bundle.v2.json"))
            .expect("explicit application bundle is readable");
        let (bundle, _, _) = factory_protocol::admit_application_bundle_source_v2(&bundle_bytes)
            .expect("explicit application bundle is valid");

        for profile in bundle.assignment_role_profiles {
            let policy_path = root.join(profile.policy.source_path.as_str());
            let policy = std::fs::read_to_string(&policy_path)
                .unwrap_or_else(|error| panic!("read {}: {error}", policy_path.display()));
            assert_eq!(
                ContentDigest::of_bytes(policy.as_bytes()),
                profile.policy.digest
            );
            let mut admission = admission(&policy);
            admission.packet.assignment_role = match profile.assignment_role {
                factory_protocol::AssignmentRole::ProductResearch => "product_research",
                factory_protocol::AssignmentRole::Engineering => "engineering",
                factory_protocol::AssignmentRole::Quality => "quality",
            }
            .to_owned();
            admission.packet.policy_byte_limit = profile.policy.byte_limit;
            admission.packet.policy_entrypoint = profile.policy.entrypoint.as_str().to_owned();
            admission.packet.tools = profile
                .tools
                .iter()
                .map(|tool| tool.as_str().to_owned())
                .collect();
            let verified = verify_extension(&admission)
                .unwrap_or_else(|error| panic!("{}: {error}", policy_path.display()));
            assert_eq!(
                verified
                    .tools
                    .iter()
                    .map(|tool| tool.name)
                    .collect::<Vec<_>>(),
                profile.tools
            );
        }
    }

    #[test]
    fn hosted_epoch_executes_the_resolved_extension_tool() {
        smol::block_on(async {
            let admission = admission(POLICY);
            let verified = verify_extension(&admission).expect("policy verifies");
            let provider = Arc::new(ScriptedProvider {
                streams: Mutex::new(VecDeque::from([ModelStream {
                    events: vec![
                        ModelStreamEvent::ToolCall(AgentToolCall {
                            id: ToolCallId::new("complete-call").expect("stable call ID"),
                            name: "work_complete".to_owned(),
                            arguments: SerializedJson::new("{}"),
                        }),
                        ModelStreamEvent::End(StopReason::ToolUse),
                    ],
                }])),
            });
            let capability = Arc::new(RecordingCapability(AtomicUsize::new(0)));
            let hosted = prepare_hosted_epoch(
                &admission,
                provider,
                verified,
                capability.clone(),
                Arc::new(tea_core::effect::NoopEffectGate),
                Arc::new(NoHooks),
            )
            .expect("hosted epoch prepares");

            hosted
                .agent()
                .start_prompt("sealed assignment")
                .expect("hosted assignment starts")
                .drive()
                .await
                .expect("resolved tool terminates the assignment");
            assert_eq!(capability.0.load(Ordering::Relaxed), 1);
        });
    }

    fn encode_base64(bytes: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
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
}
