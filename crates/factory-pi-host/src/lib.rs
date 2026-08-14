//! Rust process boundary for one Factory assignment.
//!
//! This crate is intentionally only an actor adapter.  The daemon remains the authority for
//! session identity, packet verification, workspace reads, evidence, terminal submission, and
//! process custody.  The host accepts one admission line on its inherited descriptor, verifies
//! the sealed packet, and exposes a caller-owned [`pi_agent_core::Agent`] only after an explicit
//! model provider has been supplied.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod admission;
mod agent_host;
mod execution;
mod tool_bridge;
mod transport;

pub use admission::{
    Admission, AdmissionConfig, AdmissionError, DEFAULT_MAX_PACKET_BYTES,
    MAX_ADMISSION_FRAME_BYTES, read_admission, read_admission_from_fd0,
};
pub use agent_host::{AgentHost, AgentHostError, BareAgentHost, load_luau_policy};
pub use execution::{
    CostReader, ExecutionError, ExecutionInput, ExecutionResult, PreparedExecution,
    SealedPolicySource, UsageSummary, build_factory_execution_input, build_policy_tools,
    factory_capability_bindings,
};
pub use tool_bridge::{
    BoundTool, CommandContext, DaemonError, DaemonFuture, DeferredTerminal, FACTORY_CAPABILITY,
    FactoryCapability, FramedDaemon, LocalToolExecutor, PolicyBindingError, TerminalDeferral,
    ToolName, bind_policy,
};
pub use transport::{
    FrameClient, FrameTransportError, MAX_REQUEST_FRAME_BYTES, MAX_RESPONSE_FRAME_BYTES,
    read_frame, write_frame,
};

/// The local Rust runtime identity used by the host build receipt.
///
/// This does not discover or read a provider credential.  The process supervisor supplies a
/// provider implementation explicitly after admission, keeping credentials out of packets and
/// out of the generic host boundary.
pub const RUNTIME_NAME: &str = "factory-pi-host";

/// The pi-agent-core checkout used by this temporary local bootstrap.
///
/// Cargo resolves this path from the manifest.  Qualification must record the checkout's exact
/// revision before launching a host; this constant is descriptive and is not a qualification
/// substitute.
pub const PI_AGENT_CORE_SOURCE: &str = "/Users/josh/d/pi-agent-core-rs";
