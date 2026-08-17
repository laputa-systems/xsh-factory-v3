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
pub use factory_settings::{PI_AGENT_CORE_SOURCE, RUNTIME_NAME};
pub use agent_host::{AgentHost, AgentHostError, BareAgentHost, load_luau_policy};
pub use execution::{
    CostReader, ExecutionDiagnostics, ExecutionError, ExecutionInput, ExecutionResult,
    PreparedExecution, SealedPolicySource, UsageSummary, build_factory_execution_input,
    build_policy_tools, factory_capability_bindings,
};
pub use tool_bridge::{
    BoundTool, CommandContext, DaemonError, DaemonFuture, DeferredTerminal, FACTORY_CAPABILITY,
    FactoryCapability, FramedDaemon, LocalToolExecutor, PolicyBindingError, TerminalDeferral,
    ToolExecutionDiagnostic, ToolName, bind_policy,
};
pub use transport::{
    FrameClient, FrameTransportError, MAX_REQUEST_FRAME_BYTES, MAX_RESPONSE_FRAME_BYTES,
    read_frame, write_frame,
};
