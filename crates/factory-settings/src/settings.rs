//! Bounded Factory runtime policy knobs.
//!
//! Keep this file as the single source of truth for values that tune execution
//! without changing a durable wire or database identity.  Callers should
//! import these names rather than introducing another local copy.  Values that
//! identify a protocol, operation, audit subject, schema, or evidence format
//! intentionally stay with the contract that owns them.

use std::time::Duration;

// Process and command supervision.
pub const COMMAND_ARGUMENT_LIMIT: usize = 128;
pub const COMMAND_ARGUMENT_BYTE_LIMIT: usize = 32 * 1024;
pub const COMMAND_ENVIRONMENT_LIMIT: usize = 32;
pub const COMMAND_ENVIRONMENT_VALUE_BYTE_LIMIT: usize = 4 * 1024;
pub const COMMAND_STREAM_BYTE_LIMIT: u64 = 64 * 1024 * 1024;
pub const COMMAND_TIMEOUT_LIMIT: Duration = Duration::from_secs(60 * 60);
pub const COMMAND_INPUT_BYTE_LIMIT: usize = 64 * 1024 * 1024;
pub const DEFAULT_TERMINATION_GRACE: Duration = Duration::from_secs(1);
pub const MINIMAL_ENVIRONMENT: [(&str, &str); 4] = [
    ("LANG", "C"),
    ("LC_ALL", "C"),
    ("PATH", "/usr/bin:/bin"),
    ("TZ", "UTC"),
];
pub const CARGO_TOOLCHAIN_ENVIRONMENT_NAMES: [&str; 2] = ["RUSTC", "RUSTDOC"];
pub const KERNEL_ENVIRONMENT_NAMES: [&str; 2] = ["NO_COLOR", "PATH"];

// Local operator transport and daemon scheduling.
pub const RUNTIME_LOCK_FILENAME: &str = "factoryd.lock";
pub const OPERATOR_SOCKET_FILENAME: &str = "factoryd.operator.sock";
pub const DEFAULT_READ_DEADLINE: Duration = Duration::from_secs(5);
pub const DEFAULT_OPERATION_DEADLINE: Duration = Duration::from_secs(30);
pub const DEFAULT_WRITE_DEADLINE: Duration = Duration::from_secs(5);
pub const MAX_OPERATOR_REQUEST_ID_BYTES: usize = 160;
pub const FACTORYD_ASSIGNMENT_POLL_INTERVAL: Duration = Duration::from_millis(250);
pub const FACTORYD_VAULT_COMMAND: &str = "vault";
pub const FACTORYD_PRINTENV_COMMAND: &str = "/usr/bin/printenv";
pub const FACTORYCTL_OPERATION_DEADLINE_MILLIS: u64 = 900_000;
pub const DEFAULT_PROVIDER_CREDENTIAL_ENVIRONMENT: &str = "OPENROUTER_API_KEY";
pub const DEFAULT_FORUM_PAGE_LIMIT: u8 = 20;

// Kernel assignment, campaign, and recovery bounds.
pub const MAX_PRODUCT_ASSIGNMENTS_PER_CAMPAIGN: u32 = 3;
pub const IN_FLIGHT_TICKET_MAXIMUM: u32 = 1;
pub const CAMPAIGN_SESSION_COST_AGGREGATE_MAXIMUM: usize = 18;
pub const RECOVERY_STAGING_DIRECTORY: &str = "restart-recovery";
pub const RECOVERY_POLL_INTERVAL: Duration = Duration::from_millis(10);
pub const RECOVERY_TERMINATION_GRACE: Duration = Duration::from_millis(250);
pub const KERNEL_PRINCIPAL: &str = "factoryd-assignment";
pub const DRIVER_PRINCIPAL: &str = "factoryd-campaign-driver";
pub const RECOVERY_PRINCIPAL: &str = "kernel";
pub const CLAIM_REQUALIFICATION_PRINCIPAL: &str = "kernel-ticket-claim-requalification";
pub const ASSIGNMENT_TERMINATION_GRACE: Duration = Duration::from_secs(1);

// Git, CAS, workspace, and validation bounds.
pub const DEFAULT_STREAM_LIMIT: u64 = 4 * 1024 * 1024;
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(30);
pub const WORKSPACE_READ_MAX_BYTES: u64 = 2 * 1024 * 1024;
pub const ARTIFACT_READ_MAX_BYTES: u64 = 2 * 1024 * 1024;
pub const SESSION_STDOUT_RELATIVE_PATH: &str = "stdout.log";
pub const SESSION_STDERR_RELATIVE_PATH: &str = "stderr.log";
pub const SESSION_PARTIAL_TRANSCRIPT_RELATIVE_PATH: &str = "session.ndjson";
pub const SESSION_BOUNDED_PARTIAL_TRANSCRIPT_RELATIVE_PATH: &str = "session.partial.ndjson";
pub const COMMAND_SET_LIMIT: usize = 256 * 1024;
pub const VALIDATION_LOG_LIMIT: usize = 16 * 1024 * 1024;
pub const TREE_PROBE_TIMEOUT_MILLIS: u64 = 30_000;
pub const TREE_PROBE_STREAM_LIMIT: u32 = 64 * 1024;

// Installed-runtime qualification and host identity.
pub const MAX_SOURCE_GRAPH_FILES: usize = 1_024;
pub const MAX_HOST_SOURCE_GRAPH_FILES: usize = 256;
pub const MAX_VERSION_OUTPUT_BYTES: usize = 8 * 1024;
pub const MAX_RECEIPT_BYTES: usize = 256 * 1024;
pub const TEA_SOURCE: &str = "/Users/josh/d/tea";
pub const OPENROUTER_PROVIDER: &str = "openrouter";
pub const RUST_HOST_IDENTITY: &str = "factory-tea-host-rust-v1";
pub const RUNTIME_NAME: &str = "factory-tea-host";
pub const RUST_TOOLCHAIN: &str = "nightly-2026-07-24";
pub const TEA_HEAD_MAX_BYTES: usize = 128;

// Fixed PostgreSQL pool bounds.
pub const STORAGE_MAX_CONNECTIONS: u32 = 4;
pub const STORAGE_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);

// Actor admission and provider execution policy.
pub const DEFAULT_MAX_PACKET_BYTES: usize = 3 * 1024 * 1024 - 64 * 1024;
pub const MAX_PROVIDER_RETRIES: u32 = 2;
pub const PROVIDER_RETRY_INITIAL_BACKOFF: Duration = Duration::from_millis(250);
pub const PROVIDER_RETRY_MAX_BACKOFF: Duration = Duration::from_secs(8);
pub const HOST_FALLBACK_PATH: &str = "/usr/bin:/bin";
pub const HOST_SHELL_EXECUTABLE: &str = "/bin/sh";
pub const HOST_KILL_EXECUTABLE: &str = "/bin/kill";
pub const FACTORY_CAPABILITY: &str = "factory";
