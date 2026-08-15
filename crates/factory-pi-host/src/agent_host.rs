//! Explicit `pi-agent-core` construction for one admitted assignment.

use crate::Admission;
use pi_agent_core::RunHandle;
use pi_agent_core::agent::Agent;
use pi_agent_core::scheduler::ModelProvider;
use pi_agent_core::state::{ModelDescriptor, SerializedJson, ThinkingLevel};
use pi_agent_luau::{LuaPolicy, PolicyError, PolicyLimits};
use std::fmt;
use std::sync::Arc;

/// A verified V2 assignment with an agent state machine but no provider capability.
///
/// This is useful for admission/policy qualification. It deliberately cannot run a model turn
/// until [`Self::bind_provider`] is called; no provider is discovered from environment or packet
/// fields by this type.
pub struct BareAgentHost {
    admission: Admission,
    agent: Agent,
}

impl fmt::Debug for BareAgentHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BareAgentHost")
            .field("assignment_id", &self.admission.packet.assignment_id)
            .field("has_model_provider", &self.agent.has_model_provider())
            .finish()
    }
}

impl BareAgentHost {
    /// Construct an agent with the sealed prompts and model identity, but no provider.
    pub fn new(admission: Admission) -> Result<Self, AgentHostError> {
        let packet = &admission.packet;
        let system_prompt = String::from_utf8(
            decode_packet_text(&packet.system_prompt_bytes_b64)
                .map_err(AgentHostError::PacketText)?,
        )
        .map_err(|_| AgentHostError::PacketText("system prompt is not UTF-8".to_owned()))?;
        let assignment_prompt = String::from_utf8(
            decode_packet_text(&packet.assignment_prompt_bytes_b64)
                .map_err(AgentHostError::PacketText)?,
        )
        .map_err(|_| AgentHostError::PacketText("assignment prompt is not UTF-8".to_owned()))?;
        let model = ModelDescriptor {
            provider: packet.model.provider.clone(),
            model: packet.model.model_id.clone(),
            revision: None,
        };
        let thinking_level = parse_thinking_level(&packet.model.thinking_level)?;
        let agent = Agent::builder()
            .system_prompt(system_prompt)
            .model(model)
            .thinking_level(thinking_level)
            .host_message(SerializedJson::new(assignment_prompt))
            .build();
        Ok(Self { admission, agent })
    }

    /// Borrow the immutable startup admission.
    pub fn admission(&self) -> &Admission {
        &self.admission
    }

    /// Borrow the provider-free agent for qualification inspection.
    pub fn agent(&self) -> &Agent {
        &self.agent
    }

    /// Bind the caller-owned provider and obtain a runnable host.
    pub fn bind_provider(
        self,
        provider: Arc<dyn ModelProvider>,
    ) -> Result<AgentHost, AgentHostError> {
        let snapshot = self.agent.snapshot();
        let model = snapshot.model.ok_or(AgentHostError::MissingModel)?;
        let mut builder = Agent::builder()
            .system_prompt(snapshot.system_prompt)
            .model(model)
            .thinking_level(snapshot.thinking_level)
            .model_provider(provider);
        for message in snapshot.host_messages {
            builder = builder.host_message(message);
        }
        Ok(AgentHost {
            admission: self.admission,
            agent: builder.build(),
        })
    }
}

/// A host with an explicit model provider and exactly one agent state machine.
pub struct AgentHost {
    admission: Admission,
    agent: Agent,
}

impl fmt::Debug for AgentHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentHost")
            .field("assignment_id", &self.admission.packet.assignment_id)
            .field("has_model_provider", &self.agent.has_model_provider())
            .finish()
    }
}

impl AgentHost {
    /// Construct a provider-bound host directly from a verified admission.
    pub fn new(
        admission: Admission,
        provider: Arc<dyn ModelProvider>,
    ) -> Result<Self, AgentHostError> {
        BareAgentHost::new(admission)?.bind_provider(provider)
    }

    /// Borrow the immutable startup admission.
    pub fn admission(&self) -> &Admission {
        &self.admission
    }

    /// Borrow the underlying agent for the caller-owned run loop.
    pub fn agent(&self) -> &Agent {
        &self.agent
    }

    /// Start one prompt using the packet's sealed assignment prompt.
    pub fn start(&self) -> Result<RunHandle, AgentHostError> {
        let prompt = String::from_utf8(
            decode_packet_text(&self.admission.packet.assignment_prompt_bytes_b64)
                .map_err(AgentHostError::PacketText)?,
        )
        .map_err(|_| AgentHostError::PacketText("assignment prompt is not UTF-8".to_owned()))?;
        self.agent
            .start_prompt(prompt)
            .map_err(AgentHostError::Core)
    }
}

/// Load a sealed Luau policy with explicit finite VM limits.
///
/// The caller still has to validate the resulting declarations against packet-admitted tool
/// names and bind each capability in Rust. Loading source alone grants no daemon authority.
pub fn load_luau_policy(source: &str, limits: PolicyLimits) -> Result<LuaPolicy, PolicyError> {
    LuaPolicy::load_with_limits(source, limits)
}

/// Agent construction and packet prompt failures.
#[derive(Debug)]
pub enum AgentHostError {
    /// A prompt's base64 representation was malformed.
    PacketText(String),
    /// The packet's thinking level was not one of the closed core values.
    ThinkingLevel(String),
    /// The agent core rejected a run transition.
    Core(pi_agent_core::CoreError),
    /// The verified host attempted to bind a provider without a selected model.
    MissingModel,
}

impl fmt::Display for AgentHostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PacketText(error) => write!(f, "packet prompt invalid: {error}"),
            Self::ThinkingLevel(value) => write!(f, "unsupported packet thinking level: {value}"),
            Self::Core(error) => write!(f, "agent core rejected operation: {error}"),
            Self::MissingModel => f.write_str("agent host has no selected model"),
        }
    }
}

impl std::error::Error for AgentHostError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Core(error) => Some(error),
            _ => None,
        }
    }
}

fn parse_thinking_level(value: &str) -> Result<ThinkingLevel, AgentHostError> {
    match value {
        "none" | "off" => Ok(ThinkingLevel::Off),
        "minimal" => Ok(ThinkingLevel::Minimal),
        "low" => Ok(ThinkingLevel::Low),
        "medium" => Ok(ThinkingLevel::Medium),
        "high" => Ok(ThinkingLevel::High),
        "xhigh" => Ok(ThinkingLevel::XHigh),
        "max" => Ok(ThinkingLevel::Max),
        "default" => Ok(ThinkingLevel::Default),
        other => Err(AgentHostError::ThinkingLevel(other.to_owned())),
    }
}

fn decode_packet_text(value: &str) -> Result<Vec<u8>, String> {
    if value.is_empty() || !value.len().is_multiple_of(4) {
        return Err("invalid base64 prompt".to_owned());
    }
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len() / 4 * 3);
    let (chunks, remainder) = bytes.as_chunks::<4>();
    debug_assert!(remainder.is_empty(), "base64 length was validated");
    for chunk in chunks {
        let a = b64(chunk[0]).ok_or_else(|| "invalid base64 prompt".to_owned())?;
        let b = b64(chunk[1]).ok_or_else(|| "invalid base64 prompt".to_owned())?;
        let c = if chunk[2] == b'=' {
            0
        } else {
            b64(chunk[2]).ok_or_else(|| "invalid base64 prompt".to_owned())?
        };
        let d = if chunk[3] == b'=' {
            0
        } else {
            b64(chunk[3]).ok_or_else(|| "invalid base64 prompt".to_owned())?
        };
        output.push((a << 2) | (b >> 4));
        if chunk[2] != b'=' {
            output.push((b << 4) | (c >> 2));
        }
        if chunk[3] != b'=' {
            output.push((c << 6) | d);
        }
    }
    Ok(output)
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
