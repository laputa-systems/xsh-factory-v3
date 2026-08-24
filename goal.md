# Goal: Finish and harden the Tea ↔ XSH Factory integration

Work across these two local repositories:

```text
Tea:
~/d/tea

Factory:
~/d/laputa-systems/xsh-factory-v3
```

Use only the current checked-out heads.

Do not inspect Git history, old branches, previous implementations, deleted files, or remote historical revisions.

The existing architectural integration is correct:

```text
Factory
    owns institutional state, PostgreSQL, CAS, Git/worktrees,
    validation, economics, process custody, and delivery

Tea
    owns immutable harness resolution, Luau adaptation,
    capability binding, agent construction, tool/model lifecycle,
    compaction, harness identity, and trajectory projection

factory-tea-host
    embeds one sessionless Tea HostedEpoch
```

Do not replace that design.

The objective of this project is to complete five remaining correctness and attribution improvements:

1. preserve unknown token usage instead of writing false zeroes;
2. separate pure Tea trace data from Factory-specific execution summaries;
3. carry exclusive-batch and cancellation-settlement semantics through Tea’s extension ABI and harness identity;
4. bind runtime-policy identities to their actual implementations rather than accepting independent caller assertions;
5. replace Factory’s production `NoopEffectGate` with a narrow provider-effect durability gate that supports terminal-cost recovery without duplicating Factory tool authority.

Also fix directly related stale documentation.

Rust APIs and current internal module paths may break freely.

Do not leave compatibility wrappers, old APIs, parallel paths, legacy decoders, or fallback implementations.

Persisted Factory data must not be silently reinterpreted. Where a durable shape changes, implement an explicit migration or versioned contract.

---

# 1. Baseline and repository discipline

Before editing:

```sh
cd ~/d/tea
git status --short
git rev-parse HEAD

cd ~/d/laputa-systems/xsh-factory-v3
git status --short
git rev-parse HEAD
```

Require both worktrees to be clean.

Record the exact heads in the final report.

Run the existing provider-free baselines.

## Tea

```sh
cd ~/d/tea

cargo test -p tea-core --all-targets
cargo test -p tea-luau
cargo test -p tea-trace
cargo test -p tea-session
cargo test -p tea-providers --features provider-openrouter
cargo test --workspace
```

## Factory

```sh
cd ~/d/laputa-systems/xsh-factory-v3

TEA_ROOT="$HOME/d/tea" make tea-test
TEA_ROOT="$HOME/d/tea" make provider-free-host
TEA_ROOT="$HOME/d/tea" make application-contract-test
cargo test --workspace
```

Run database-backed Factory gates where the required disposable PostgreSQL inputs are already available.

Do not create or guess database URLs.

Do not run a paid cycle or make a live provider request.

---

# 2. Preserve the successful architectural boundary

The following must remain true throughout the work:

* Factory uses `HarnessSeedBuilder`.
* Factory uses `HarnessResolver`.
* Factory resolves its role policy through `LuauExtensionEngine`.
* Factory capability implements Tea’s language-neutral `ExtensionCapability`.
* Factory prepares execution through `RuntimeServices::prepare_hosted_epoch`.
* `SessionRuntime` and `HostedEpoch` share one Tea agent-construction path.
* Factory does not construct `Agent` directly.
* Factory does not construct `LuaToolHandler` directly.
* Factory does not construct `LuaPolicyHookSet` directly.
* Factory does not create a Tea durable session.
* Factory keeps self-extension disabled.
* Factory’s PostgreSQL/CAS/Git/session/economic authority remains unchanged.
* Tea remains unaware of Factory protocol and institutional types.

Do not reintroduce:

```text
Agent::builder() in factory-tea-host
LuaToolHandler in Factory
LuaPolicyHookSet in Factory
LuauCapability in Factory
Tea SessionRuntime inside Factory assignments
Factory types inside Tea
```

---

# 3. Fix token-usage semantics

Factory currently has enough information to report provider cache usage, but the terminal path writes literal zeroes.

Correct both the immediate bug and the underlying unknown-versus-zero contract.

## 3.1 Provider usage source

Continue using Tea provider accounting as the source:

```rust
OpenRouterProvider::usage_snapshot()
```

Populate:

```text
input tokens
output tokens
cache-read tokens
cache-write tokens
reasoning tokens
```

from the exact returned `Usage`.

Do not hard-code cache fields.

Do not infer cache usage from request fingerprints.

Do not derive cost from token counts.

Provider-reported exact cost remains the authoritative economic value.

## 3.2 Preserve unknown values

Tea usage fields are optional. Factory must not convert unavailable usage into known zero.

Introduce a versioned terminal usage value whose token fields preserve:

```rust
Option<u64>
```

for:

```text
input_tokens
output_tokens
cache_read_tokens
cache_write_tokens
reasoning_tokens
```

Known zero must be:

```text
Some(0)
```

Unavailable must be:

```text
None
```

Do not encode both as JSON `0`.

Use the repository’s current durable-versioning policy:

* if the current terminal usage shape is declared durable, introduce a new explicitly versioned usage/report value;
* if it is explicitly pre-release and disposable, replace it atomically.

Do not support both old and new terminal usage contracts after the migration.

Do not change the assignment packet version unless the packet itself must change.

## 3.3 Persistence

Update Factory persistence so diagnostic token categories remain nullable.

This may require:

* nullable PostgreSQL columns;
* a migration;
* updated SQLx metadata;
* updated reducers/projections;
* updated operator output;
* updated terminal request/response values.

Existing historical rows that already contain zero cannot be retroactively distinguished. Preserve those values as historical data and document that limitation.

New rows must retain unknown distinctly.

When aggregating diagnostic usage:

```text
all component values known:
    return the checked sum

any component unknown:
    aggregate remains unknown
```

Do not substitute zero into an aggregate merely to make it printable.

## 3.4 Tests

Add tests for:

```text
known nonzero cache usage
known zero cache usage
unknown cache usage
unknown input/output usage
provider-reported zero-cost usage
missing provider cost
terminal serialization of null versus zero
database round trip of null versus zero
operator projection of unknown usage
```

---

# 4. Split Tea trace from Factory execution summary

The artifact named and typed as a Tea trace must contain only valid Tea trace records.

A pure Tea trace is:

```text
EpisodeHeader
Turn / Tool / Compaction*
EpisodeEnd
```

`EpisodeEnd` must be the final record.

Factory must not append another JSON record after it.

## 4.1 Tea trace artifact

Keep the current redacted Tea trajectory in an artifact with a role such as:

```text
tea_trace_jsonl_gzip
```

The decoded JSONL must contain only values accepted by Tea’s trace decoder.

The last decoded value must be:

```text
TraceEvent::EpisodeEnd
```

Retain:

* incremental flushing during execution;
* bounded output;
* redaction before persistence;
* terminal-event reserve;
* gzip size bound;
* Factory CAS sealing.

Remove the Factory-summary reserve from the Tea trace sink.

Do not append Factory-specific JSON after Tea’s terminal event.

## 4.2 Factory summary artifact

Write the Factory-specific execution summary as a separate canonical JSON artifact, for example:

```text
factory_execution_summary_json
```

Use a separate file such as:

```text
factory-execution-summary.json
```

The summary should retain the existing Factory-specific fields:

```text
application revision
assignment ID
kernel build identity
Rust host identity
packet digest
policy digest
provider/model/thinking
Tea harness snapshot
Tea harness revision
Tea model-harness profile
Tea provider-surface digest
all Tea surface identities
turn count
Engineering phase
tool timing/failure diagnostics
cost-limit state
selected terminal operation
trace truncation state
```

Also include the new complete tool-execution-policy digest introduced later in this prompt.

Seal this artifact independently before terminal submission.

## 4.3 Terminal/session evidence

Update Factory’s terminal/session evidence to retain both artifact IDs explicitly:

```text
Tea trace artifact
Factory execution-summary artifact
```

Do not encode the summary artifact as an arbitrary metadata map.

Use a typed field or closed artifact role.

A successful terminal settlement should require both artifacts.

A crash or partial terminal path may retain whichever evidence was successfully sealed, according to existing fail-closed behavior.

## 4.4 Operator export

Extend the existing transcript/evidence export so the operator may retrieve:

```text
session-<id>-trace.ndjson.gz
session-<id>-execution-summary.json
```

Do not name the Factory summary a transcript.

Do not gunzip or parse the summary as NDJSON.

## 4.5 Compatibility and tests

Add tests proving:

* every line in the Tea trace decodes as `tea_trace::TraceEvent`;
* exactly one `EpisodeHeader` exists;
* exactly one `EpisodeEnd` exists;
* `EpisodeEnd` is last;
* the Factory summary is not present in the Tea trace bytes;
* the Factory summary is canonical JSON;
* both artifacts remain within packet byte authority;
* both artifacts are separately sealed;
* operator export returns both with the correct closed kind;
* trace truncation still preserves the terminal Tea event;
* summary sealing failure prevents false successful settlement.

Update:

```text
docs/EVIDENCE.md
docs/ARCHITECTURE.md
docs/SESSION-BOUNDARIES.md
```

---

# 5. Extend Tea’s extension-tool execution contract

Tea’s normal tool contract now distinguishes:

```text
execution mode
exclusive-batch requirement
cancellation-settlement mode
```

The extension ABI must carry the same complete behavior.

Do not let Luau tools silently inherit host defaults for behavior-changing execution semantics.

## 5.1 Language-neutral Tea contract

Extend the core-owned extension tool description with:

```rust
requires_exclusive_batch: bool
cancellation_settlement_mode: CancellationSettlementMode
```

Use Tea’s existing canonical cancellation-settlement enum.

Do not create a second equivalent enum.

Update all language-neutral extension values and resolution paths, including as applicable:

```text
ExtensionToolDescription
ExtensionDescriptor
ResolvedExtension
ExtensionEngine
extension source validation
tool-handler construction
tool registry insertion
candidate surface comparison
harness catalog encoding/decoding
```

## 5.2 Luau ABI

Extend the closed Luau tool declaration with fields equivalent to:

```lua
requires_exclusive_batch = true | false
cancellation_settlement = "drop_future" | "await_future"
```

Use a closed spelling.

Reject unknown values.

For source compatibility during this migration, omitted fields may canonicalize to:

```text
requires_exclusive_batch = false
cancellation_settlement = drop_future
```

However, update all repository-owned Factory policies to declare the fields explicitly.

After the migration, the resolved descriptor must always contain concrete values.

Update:

```text
tea-luau policy parsing
ToolHandlerSpec
LuaToolHandler
AgentTool implementation
tests and ABI documentation
```

## 5.3 Separate provider-visible and host-only identities

Do not add these host-only fields to the provider-surface digest.

A change to:

```text
requires_exclusive_batch
cancellation_settlement_mode
```

must:

* change the complete harness/tool-execution-policy identity;
* change the harness snapshot identity;
* not change the provider-surface digest when prompt/tool JSON is otherwise identical.

Add a distinct harness fingerprint such as:

```rust
tool_execution_policy_digest: Digest
```

The exact name may vary, but the distinction must be explicit.

Keep model-facing presentation values separate from execution-policy values.

A suitable design is:

```rust
ToolPresentationDescriptor {
    name,
    description,
    schema,
    execution_mode,
}

ToolExecutionPolicyDescriptor {
    name,
    requires_exclusive_batch,
    cancellation_settlement_mode,
}
```

Update:

```text
HarnessSnapshotSpec
HarnessSurfaceFingerprints
HarnessSurface
HarnessSeedBuilder
snapshot identity
catalog encoding
candidate diffing
candidate changed surfaces
execution summary
tests
```

The provider-surface digest remains based only on the actual provider-facing prompt and ordered tool definitions.

## 5.4 Trusted tools and plugin tools

The complete execution-policy digest must cover both:

```text
trusted host tools
resolved extension tools
```

Tool-name order and uniqueness must be deterministic.

A collision remains fail-closed.

## 5.5 Factory’s canonical tool contract

Extend Factory’s Rust-owned `FactoryToolContract` to include:

```rust
requires_exclusive_batch
cancellation_settlement_mode
```

The sealed Luau declaration must match the Rust-owned Factory contract exactly.

The Luau source does not acquire safer or broader execution behavior merely by declaring it.

`bind_extension_tools` must verify:

```text
name
capability
method
description/schema validity
execution mode
exclusive-batch value
cancellation-settlement mode
packet allowlist
```

Extend `BoundTool` with typed values.

Include both fields in:

```text
Factory capability host identity
Tea harness execution-policy identity
Factory execution summary
surface-parity tests
```

## 5.6 Required Factory policy classification

Use this initial classification unless existing implementation evidence requires a stricter setting.

### Read-only/query tools

```text
workspace_read
workspace_search
workspace_list
forum_search
forum_list_topics
forum_list_threads
forum_read_thread
artifact_read
```

Use:

```text
requires_exclusive_batch = false
cancellation_settlement = drop_future
```

Only retain `drop_future` where dropping the future cannot leave an uncontrolled mutation or child process.

### Local worktree/process mutation

```text
workspace_write
workspace_edit
shell
```

Use:

```text
requires_exclusive_batch = true
cancellation_settlement = await_future
```

Update the rooted local executor so cancellation always produces bounded settlement:

* child processes are terminated;
* child processes are reaped;
* file operations finish or fail;
* no background mutation survives a cancelled capability future.

Do not use `AwaitFuture` unless this invariant is tested.

### Durable daemon mutations and terminal operations

```text
publication_create
artifact_seal
product_submit_ticket
candidate_checkpoint_regression
candidate_submit
quality_run_full_suite
quality_submit_review
work_complete
```

Use:

```text
requires_exclusive_batch = true
cancellation_settlement = await_future
```

The framed-daemon path must settle under cancellation through one bounded outcome:

```text
successful response
typed daemon failure
daemon disconnect
operation deadline
```

Do not abandon a mutating daemon request merely because the model run was cancelled.

Keep exact command IDs and expected revisions.

## 5.7 Tests

Add Tea tests proving:

* Luau values parse and canonicalize;
* unknown cancellation mode is rejected;
* host-only policy change changes snapshot identity;
* host-only policy change does not change provider-surface digest;
* catalog round trip preserves both fields;
* candidate diff names the execution-policy surface;
* `LuaToolHandler` reports the selected values;
* exclusive tools isolate a provider tool-call batch;
* `AwaitFuture` is actually awaited after cancellation;
* `DropFuture` drops only explicitly permitted work.

Add Factory tests for every tool’s expected contract.

---

# 6. Bind runtime-policy identities to implementations

Tea currently allows callers to supply policy digests separately from the `RuntimeServices` implementations.

Remove that split source of truth.

The implementation and its stable identity must enter Tea together.

## 6.1 Policies covered

Bind identities to at least:

```text
HookSet
automatic compaction policy
tool-result projection policy
tool-failure/circuit-breaker policy
```

Also include any other behavior-changing runtime policy currently represented in a harness snapshot.

## 6.2 API design

Use an API equivalent to:

```rust
IdentifiedPolicy<T> {
    identity: Digest,
    implementation: T,
}
```

or policy-specific identified wrappers.

Exact type names are flexible.

The important invariants are:

* a runtime policy cannot be installed without an identity;
* the identity is stored alongside the implementation;
* `RuntimeServices` exposes the exact policy descriptors derived from what it contains;
* callers cannot independently construct a contradictory descriptor set;
* harness resolution verifies the snapshot descriptors against the selected `RuntimeServices`;
* a mismatch fails before agent construction.

A suitable API shape is approximately:

```rust
RuntimeServices::hooks_with_identity(...)
RuntimeServices::automatic_compaction_with_identity(...)
RuntimeServices::tool_projection_with_identity(...)
RuntimeServices::failure_policy_with_identity(...)

RuntimeServices::policy_descriptors()
```

Make the descriptor value’s raw constructor private or crate-private where practical.

## 6.3 Harness seeding

Remove the independent caller-supplied `HarnessRuntimePolicyDescriptors` values from `HarnessSeedBuilder` where they can disagree with actual services.

A seed should receive descriptors produced by the selected `RuntimeServices`, either by:

```text
passing RuntimeServices::policy_descriptors()
```

or by a focused builder operation that derives them.

At resolution:

```text
snapshot policy descriptors
must equal
RuntimeServices policy descriptors
```

No warning or fallback.

## 6.4 Defaults

Tea’s built-in policies should expose stable canonical identities.

Do not derive identities from:

* pointers;
* Rust type names;
* debug formatting;
* build timestamps.

Use explicit versioned constants and canonical hashes.

## 6.5 Factory hook identity

Factory’s host hook chain currently includes:

```text
OpenAI context adaptation
Factory Engineering phase stop logic
```

Create an exact hook identity from:

```text
Tea OpenAI-context hook ABI/version
Factory phase-hook ABI/version
Factory qualified Rust host identity
Factory kernel build identity
any behavior-changing static configuration
```

Do not include:

* process ID;
* current time;
* temp paths;
* secrets;
* assignment-local mutable state;
* API key.

The sealed Luau extension source already has its own immutable source identity and must not be represented only by the base-hook digest.

## 6.6 Tests

Add tests proving:

* equivalent identified policies resolve;
* mismatched hook identity fails;
* mismatched compaction identity fails;
* mismatched projection identity fails;
* mismatched failure-policy identity fails;
* changing host-hook identity changes the complete harness snapshot;
* changing only host-hook identity does not change provider surface when provider-visible bytes are unchanged;
* `SessionRuntime` and `HostedEpoch` use the same verification;
* Factory cannot seed with one policy identity and execute another.

---

# 7. Add Factory provider-effect durability

Factory must no longer use `NoopEffectGate` in production.

Do not introduce a generic Factory event log.

Implement one narrow durable contract for provider request intent and settlement.

Factory’s existing tool RPCs remain the authoritative durability boundary for Factory tools.

## 7.1 Factory effect gate

Add a type such as:

```rust
FactoryEffectGate
```

implementing:

```rust
tea_core::effect::EffectGate
```

Production hosted epochs must receive this gate.

For:

```text
EffectKind::ProviderRequest
```

the gate must durably record:

### Before provider dispatch

```text
Factory session ID
Factory assignment ID
Tea core-run ID
Tea effect ordinal/ID
Tea harness snapshot ID
Tea harness revision ID
Tea model-harness profile ID
Tea provider-surface digest
provider family
requested model
content-free request identity/fingerprint
started state
```

Do not persist:

* prompt text;
* messages;
* tool arguments;
* provider request body;
* API key;
* authorization header.

### After provider settlement

Record:

```text
settled | failed | cancelled
stop reason
context-overflow classification
optional input tokens
optional output tokens
optional cache-read tokens
optional cache-write tokens
optional reasoning tokens
optional exact provider-reported cost
bounded failure classification
```

Do not retain full assistant text in PostgreSQL.

## 7.2 Provider-effect identity

Use a deterministic key based on:

```text
Factory session/assignment identity
Tea core-run identity
Tea effect ID
```

The key must be stable across duplicate RPC delivery within one process run.

Do not treat Tea’s process-local effect ID alone as a global durable identity.

## 7.3 Protocol

Add closed Factory operations for provider effect intent and settlement.

Use exact typed requests and responses.

Do not implement a generic:

```text
effect kind + JSON payload
```

API.

A suitable shape is:

```text
session.provider_request_start
session.provider_request_settle
```

or repository-consistent names.

The daemon must:

* accept identical replay idempotently;
* reject conflicting replay;
* reject settlement without start;
* reject a second conflicting settlement;
* verify the request belongs to the admitted assignment/session;
* preserve expected-revision semantics where required.

## 7.4 Persistence

Use an existing suitable typed relation if one exists.

Otherwise add one narrowly scoped PostgreSQL table for provider effects.

It should contain small authority facts only.

Do not store per-token events or transcript content.

The existing exact table-count test must be updated explicitly with the new named table and architectural reason.

Add SQLx metadata.

## 7.5 Non-provider Tea effects

For:

```text
ToolExecution
HookInvocation
DurableWrite
Timer
ArtifactWrite
HarnessActivation
```

do not create a second generic durability path.

Factory tool operations already have:

```text
typed capability RPC
client command ID
expected revision
kernel validation
terminal reconciliation
```

The effect gate may immediately acknowledge these categories, with clear documentation that authoritative settlement is owned by the existing capability boundary.

Do not record a duplicate tool mutation in a second table.

## 7.6 Exact provider cost

Tea provider usage carries exact cost as a decimal string where the provider reports it.

Parse it using checked integer decimal arithmetic into Factory `MicroUsd`.

Do not use floating point.

Do not infer cost from token counts.

Known provider zero remains known zero.

Missing cost remains unknown.

Move the exact decimal parser out of `main.rs` into an appropriate reusable Factory module and test it thoroughly.

## 7.7 Terminal-cost recovery

Use the provider-effect ledger during terminal reconciliation.

The rules are:

### Complete ledger

When every started provider request is settled and every settled request has known provider cost:

```text
ledger total is a complete provider-reported total
```

### Terminal report agrees

When the host terminal report supplies a total and the ledger is complete:

```text
the two totals must match exactly
```

A mismatch is a protocol/evidence failure.

### Terminal report missing

When the host dies after provider-effect settlement but before terminal submission:

```text
recover exact cost and usage from the complete provider-effect ledger
```

Do not mark cost unknown merely because the final host report is absent.

### Incomplete ledger

When any provider effect is:

```text
started but unsettled
settled without known cost
conflicting
```

cost remains unknown and the existing fail-closed economic behavior applies.

### No provider requests

A session with no provider requests has a complete known provider total of zero.

## 7.8 Live cost enforcement

Keep the current turn-boundary cost observer or replace it only with an equivalent immediate enforcement mechanism.

Do not weaken the campaign allowance.

The provider-effect ledger is for durability and recovery; it must not delay cancellation after a known allowance breach.

## 7.9 Crash-point tests

Add provider-free deterministic tests for:

```text
crash before provider start
crash after durable start but before network dispatch
crash after provider response but before durable settlement
crash after durable settlement but before terminal report
duplicate identical start
conflicting duplicate start
duplicate identical settlement
conflicting duplicate settlement
settlement without start
known zero-cost turn
unknown-cost turn
multiple settled turns
terminal total matching ledger
terminal total conflicting with ledger
complete usage recovery
incomplete usage recovery
```

Use a scripted Tea provider.

Do not make real provider calls.

---

# 8. Integrate all identities into Factory evidence

Factory’s separate execution summary must include:

```text
Factory application revision
Factory assignment ID
Factory packet digest
Factory policy digest
Factory kernel build identity
Factory Rust host identity

Tea harness snapshot ID
Tea harness revision ID
Tea model-harness profile ID
Tea provider-surface digest
Tea system-prompt digest
Tea ordered-tool-definitions digest
Tea hook-bundle digest
Tea capability-bindings digest
Tea compaction-policy digest
Tea tool-execution-policy digest

provider-effect count
settled provider-effect count
complete provider usage?
complete provider cost?
recovered from provider ledger?
trace truncated?
```

These identities must come from Tea/Factory authoritative values, not independently reconstructed display strings.

Do not put raw secrets or model inputs in the summary.

---

# 9. Documentation

Update Tea documentation to explain:

* `HostedEpoch` is an embedding seam for an external durable authority;
* extension tools carry complete execution semantics;
* provider-visible and host-only tool identities are separate;
* runtime-policy identity is bound to actual implementations;
* hosted callers may install their own effect gate;
* policy-identity mismatch fails before execution.

Update at least:

```text
docs/architecture.md
docs/luau-abi-v1.md
docs/durable-harness.md or its current equivalent
docs/trace.md
```

Update Factory documentation to explain:

* pure Tea trace and Factory summary are separate artifacts;
* nullable usage semantics;
* provider-effect durable intent/settlement;
* terminal-cost recovery;
* Factory tool execution policies;
* complete provenance chain;
* `NoopEffectGate` is no longer used in production.

Update at least:

```text
docs/ARCHITECTURE.md
docs/EVIDENCE.md
docs/SESSION-BOUNDARIES.md
docs/CONTROL-PLANE.md
V1.md
```

Remove terminal-cost recovery from the backlog if fully implemented.

Fix the stale statement that Factory has four Rust crates.

Do not rewrite unrelated planning documents.

---

# 10. Implementation sequence

Implement in this order.

## Phase A — Small evidence correctness

1. Preserve optional usage values.
2. Forward real cache usage.
3. Split Tea trace and Factory summary.
4. Get Factory provider-free host tests green.

Do not begin the deeper identity work until this phase is stable.

## Phase B — Tea extension execution semantics

1. Extend the language-neutral extension contract.
2. Extend Luau parsing and handlers.
3. Add separate execution-policy fingerprinting.
4. Update Tea catalog/snapshot/candidate logic.
5. Get all Tea tests green.

## Phase C — Factory tool policy

1. Extend `FactoryToolContract`.
2. Update all sealed role policies explicitly.
3. Verify Luau declarations against Rust contract.
4. Make local/daemon cancellation settlement real.
5. Restore exact Factory surface/behavior tests.

## Phase D — Runtime-policy identity

1. Bind identities to `RuntimeServices` implementations.
2. Remove independent descriptor construction.
3. Verify during resolution.
4. Update Tea hosts.
5. Update Factory hook identity.

## Phase E — Provider effect gate

1. Add typed Factory protocol operations.
2. Add persistence/migration.
3. Implement daemon handlers.
4. Implement `FactoryEffectGate`.
5. Replace production `NoopEffectGate`.
6. Add terminal-cost recovery and consistency checks.

## Phase F — Documentation and cleanup

1. Delete transitional APIs.
2. Delete old trace-summary append path.
3. Delete old hard-coded usage writes.
4. Delete independent runtime-policy descriptor constructors.
5. Update docs and final acceptance tests.

At every phase, run focused tests before continuing.

---

# 11. Required deletion and cleanup

The final production Factory code must not contain:

```rust
Arc::new(NoopEffectGate)
```

for a real assignment.

It may remain in unit fixtures.

The final trace path must not contain:

```text
append Factory summary after Tea EpisodeEnd
```

The final terminal path must not contain:

```rust
cache_read_tokens = 0
cache_write_tokens = 0
```

unless those are actual known provider values.

The final Tea harness seed path must not accept an independently forgeable runtime-policy descriptor set that can disagree with `RuntimeServices`.

The final Luau tool path must not silently default transaction-sensitive Factory tools to `DropFuture`.

Do not satisfy deletion requirements by moving identical code.

---

# 12. Verification

## Tea

```sh
cd ~/d/tea

cargo fmt --all -- --check

cargo test -p tea-core --all-targets
cargo test -p tea-luau
cargo test -p tea-trace
cargo test -p tea-session
cargo test -p tea-providers --features provider-openrouter

cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace

git diff --check
```

## Factory

```sh
cd ~/d/laputa-systems/xsh-factory-v3

TEA_ROOT="$HOME/d/tea" make tea-test
TEA_ROOT="$HOME/d/tea" make provider-free-host
TEA_ROOT="$HOME/d/tea" make application-contract-test

cargo fmt --all -- --check

cargo test -p factory-protocol
cargo test -p factory-tea-host
cargo test -p factory-kernel
cargo test --workspace

cargo clippy --all-targets --all-features -- \
    --deny warnings \
    --allow clippy::pedantic \
    --allow clippy::large_enum_variant \
    --allow clippy::result_large_err \
    --allow clippy::type_complexity \
    --allow clippy::too_many_arguments

git diff --check
```

Run the complete provider-free Factory acceptance suite when its explicitly required disposable databases and backup/restore inputs are available:

```sh
TEA_ROOT="$HOME/d/tea" make tea-acceptance
```

Do not guess missing inputs.

Do not run a paid cycle.

Do not call OpenRouter.

---

# 13. Acceptance criteria

The work is complete only when:

* actual provider cache usage is forwarded;
* unavailable usage remains unknown, not zero;
* token persistence preserves null versus zero;
* the Tea trace artifact contains only Tea trace events;
* Tea `EpisodeEnd` is the final trace record;
* Factory summary is a separate sealed artifact;
* operator export exposes both artifacts correctly;
* extension tools carry exclusive-batch semantics;
* extension tools carry cancellation-settlement semantics;
* host-only execution-policy changes alter complete harness identity;
* host-only execution-policy changes do not alter provider-surface identity;
* Factory’s Rust tool contract controls the admitted values;
* mutating Factory tools are exclusive;
* transactional Factory capabilities settle under cancellation;
* runtime-policy identities are attached to implementations;
* harness resolution rejects identity/implementation mismatch;
* Factory’s production hosted epoch uses `FactoryEffectGate`;
* provider intent is durable before network dispatch;
* provider settlement is durable before later core progress;
* no raw prompt or request body enters PostgreSQL;
* Factory can recover complete cost after host death following provider settlement;
* incomplete provider effects remain fail-closed;
* terminal-reported cost must match the provider-effect ledger;
* existing Factory tool RPC authority is not duplicated;
* static Factory model-facing behavior remains unchanged;
* self-extension remains disabled;
* all provider-free tests pass.

The final architecture should be explainable as:

> Factory remains the authoritative software-production control plane, while Tea owns one exactly identified cognitive runtime. Every provider-visible surface, host-only execution policy, capability grant, model request, provider settlement, token observation, cost observation, and trajectory artifact now has a truthful and independently verifiable identity.
