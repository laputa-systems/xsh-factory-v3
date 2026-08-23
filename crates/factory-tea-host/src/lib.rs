//! Rust process boundary for one Factory assignment.
//!
//! This crate is intentionally only an actor adapter.  The daemon remains the authority for
//! session identity, packet verification, workspace reads, evidence, terminal submission, and
//! process custody.  The host accepts one admission line on its inherited descriptor, verifies
//! the sealed packet, and exposes a caller-owned [`tea_core::Agent`] only after an explicit
//! model provider has been supplied.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod admission;
mod execution;
mod tea_harness;
mod tool_bridge;
mod transport;

pub use admission::{
    Admission, AdmissionConfig, AdmissionError, DEFAULT_MAX_PACKET_BYTES,
    MAX_ADMISSION_FRAME_BYTES, read_admission, read_admission_from_fd0,
};
pub use factory_settings::{RUNTIME_NAME, TEA_SOURCE};
pub use execution::{
    CostReader, CostSnapshot, ExecutionDiagnostics, ExecutionError, ExecutionInput, ExecutionResult,
    PreparedExecution, build_factory_execution_input,
};
pub use tool_bridge::{
    BoundTool, CommandContext, DaemonError, DaemonFuture, DeferredTerminal, FACTORY_CAPABILITY,
    FactoryCapability, FramedDaemon, LocalToolExecutor, PolicyBindingError, TerminalDeferral,
    ToolExecutionDiagnostic, ToolName, bind_extension_tools, tool_contract,
};
pub use transport::{
    FrameClient, FrameTransportError, MAX_REQUEST_FRAME_BYTES, MAX_RESPONSE_FRAME_BYTES,
    read_frame, write_frame,
};
