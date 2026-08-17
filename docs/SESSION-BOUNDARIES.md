# Session boundaries and limiters

This is the living inventory of Factory V3 session boundaries. It distinguishes
economic and custody boundaries from provider/model progress hints. A number in
this document is a contract only when the linked owner enforces it; changing an
owner requires updating this document and its focused tests in the same change.

## Progress and cost

| Boundary | Current behavior | Owner |
| --- | --- | --- |
| Product turns, repeated tool calls, and tool-less turns | No host-side numeric progress cap. Product may continue distinct or repeated investigation until it reaches its terminal operation or the run is stopped by custody, cancellation, or cost. | `crates/factory-pi-host/src/tool_bridge.rs` |
| Engineering turns, repeated calls, shell calls, discovery, recovery, and protocol retries | No numeric actor-progress cap. The controller still requires the regression checkpoint before mutation, prevents pre-mutation shell validation after an owner read, and closes unrelated discovery after the checkpoint. | `crates/factory-pi-host/src/tool_bridge.rs` |
| Quality actor progress | No host-side progress counter. Independent validation and review remain required terminal evidence. | `crates/factory-pi-host/src/tool_bridge.rs`, `crates/factory-kernel/src/session_runtime.rs` |
| Provider output-token request | Factory does not send the application `output_token_limit` as `max_tokens`. The OpenRouter adapter's default is no request cap; an embedding may still opt into an explicit provider hint with `with_max_tokens`. Provider/model-native capability limits remain external facts. | `/Users/josh/d/pi-agent-core-rs/crates/pi-agent-core/src/provider/openrouter/config.rs`, `payload.rs`, `crates/factory-pi-host/src/main.rs` |
| Provider semantic length guards | Repetition and tool-less-response cutoffs are removed. The transport still has a no-progress stall timeout so a wedged subprocess cannot hold a session forever. | `/Users/josh/d/pi-agent-core-rs/crates/pi-agent-core/src/provider/openrouter/transport.rs` |
| Live provider spend | While a run is active, the host polls the explicit provider accounting callback every `PROVIDER_COST_POLL_INTERVAL` (100 ms). A reported total at or above the packet's remaining campaign allowance cancels the core run and emits `cost_limit`. | `crates/factory-settings/src/settings.rs`, `crates/factory-pi-host/src/execution.rs`, `crates/factory-pi-host/src/main.rs` |
| Terminal cost | The provider total is authoritative only when complete. Factory cost is recomputed from admitted rates and captured usage; unknown cost remains fail-closed. A known `cost_limit` terminal is recorded as exceeded and cannot submit a deferred operation. | `crates/factory-kernel/src/process.rs`, `crates/factory-protocol/src/process.rs` |

Live cancellation can only react to spend reported by completed provider turns;
the cost of an in-flight provider request is not knowable until that provider
response returns. This is why terminal reconciliation remains authoritative.

## Process and packet custody

These are safety and evidence boundaries, not model-progress budgets:

- The role packet retains a positive wall deadline and `output_byte_limit`; the
  XSH bundle currently declares 15 minutes Product, 20 minutes Engineering,
  10 minutes Quality, and 16 MiB of session output. The model
  `context_token_limit` and `output_token_limit` remain packet identity and
  provider capability metadata; Factory no longer turns the latter into a
  request cap.
- The host derives provider request and stall timeouts from the role wall limit,
  capped by `MAX_PROVIDER_REQUEST_TIMEOUT` (20 minutes) and
  `MAX_PROVIDER_STALL_TIMEOUT` (10 minutes), and allows at most
  `MAX_PROVIDER_RETRIES` (2) with the configured backoff bounds.
- Command supervision uses the settings-owned argument (128), argument-byte
  (32 KiB), environment (32 entries), environment-value (4 KiB), input (64 MiB),
  stream (64 MiB), and command-timeout (one hour) limits plus a one-second
  termination grace. Shell capability requests additionally cap one command at
  300 seconds and 128 KiB of returned output.
- Admission and framed RPC remain bounded by `DEFAULT_MAX_PACKET_BYTES`,
  `REQUEST_FRAME_MAX_BYTES` (1 MiB), and `RESPONSE_FRAME_MAX_BYTES` (4 MiB).
  These limits prevent unbounded allocation and are not provider output caps.
- CAS, transcript, workspace-read, artifact-read, validation-log, receipt, and
  installed-runtime source-graph limits remain in their owning settings or
  protocol modules. They protect durable evidence and runtime qualification.

## Policy VM and capability boundaries

Default Luau policy limits are 64 KiB source, 1 MiB VM memory, and 10,000
interrupt checks per evaluation. Each handler has the same source and memory
limits, 10,000 interrupt checks per resume, and at most 64 capability calls.
JSON schemas and capability handlers add domain-specific shape limits (Forum
pages, search queries, shell commands, artifact byte limits, and ticket/evidence
fields). They bound one operation's allocation and authority; they do not stop
an actor merely because it has used many operations.

## Campaign and lifecycle boundaries

- One campaign has one aggregate budget and future deadline. Each packet carries
  the remaining allowance used by live cancellation.
- The current application admits at most three Product assignments per campaign
  (`MAX_PRODUCT_ASSIGNMENTS_PER_CAMPAIGN`) and one in-flight ticket
  (`IN_FLIGHT_TICKET_MAXIMUM`). These are campaign scheduling limits, not actor
  session turn limits.
- Process custody owns the child group, wall deadline, output capture,
  cancellation, direct wait, and terminal reconciliation. A daemon shutdown
  cancels and reconciles active sessions before releasing its lock.
- Required reads, sealed policy/prompt artifacts, transcript archives, candidate
  trees, validation logs, and delivery receipts are immutable evidence. A
  terminal operation cannot bypass cost, evidence, hard validation, clean
  checkout, or fast-forward guards.

## Change procedure

When changing a boundary or limiter:

1. Update the owning Rust type/constant and the focused contract or regression
   test.
2. Update this inventory and the nearest lifecycle/evidence documentation.
3. Rebuild and requalify the installed host if the Factory source graph,
   dependency checkout, selected build, or runtime changes.
4. Inspect live identities again before any paid admission. A paid cycle is not
   authorized by this refactor alone.

The dependency checkout is intentionally named above rather than hidden behind
ambient discovery: its source and commit are part of installed-runtime
qualification.
