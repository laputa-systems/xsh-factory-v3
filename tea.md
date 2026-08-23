The prompt below targets the concrete duplication at the current heads: Factory manually loads `LuaPolicy`, constructs `LuaToolHandler`s and capability bindings, assembles `Agent`, and projects its own event transcript, while Tea already owns language-neutral extension resolution, immutable capability bindings, `ResolvedHarness`, `RuntimeServices`, hosted agent construction internals, and `TraceObserver`.

# Goal: deduplicate `xsh-factory-v3` against Tea

You are working across two sibling repositories:

```text
~/d/tea
~/d/laputa-systems/xsh-factory-v3
```

Use only the current checked-out heads. Do not inspect Git history, prior implementations, old branches, or deleted code.

The objective is to eliminate Factory-owned reimplementations of functionality that Tea already provides, while preserving Factory’s institutional authority, evidence custody, safety properties, paid-session semantics, provider behavior, and model-facing behavior.

This is a cross-repository internal refactor. Rust APIs may break freely. Do not leave compatibility modules, deprecated paths, parallel implementations, or dormant fallback code.

The intended final relationship is:

```text
Factory
    owns:
        institutional state
        PostgreSQL
        CAS custody
        tickets / offices / decisions / Forum
        assignment compilation
        worktree and Git custody
        validation
        delivery
        campaign economics
        process supervision
        provider credential injection
        terminal reconciliation

Tea
    owns:
        immutable agent-harness resolution
        extension-language adaptation
        capability binding mechanics
        agent construction
        model/tool lifecycle
        hooks
        compaction
        provider-surface fingerprints
        harness identity
        compact trace projection

factory-tea-host
    owns only:
        translation from one sealed Factory assignment
        into one standard Tea hosted epoch
```

The architectural sentence is:

> Factory compiles and governs the work order; Tea compiles and executes the cognitive policy.

---

## 1. Hard boundaries

Do not move these Factory responsibilities into Tea:

* PostgreSQL schemas or queries;
* Factory CAS;
* `ApplicationBundleV2`;
* offices, tickets, RFCs, claims, decisions, publications, or Forum state;
* campaign scheduling;
* assignment admission;
* required-read authority;
* repository qualification;
* worktree creation;
* candidate tree capture;
* hard validation;
* Quality review;
* commit construction;
* local fast-forward delivery;
* provider-budget authority;
* provider credential discovery;
* process spawning or supervision;
* Factory terminal reconciliation;
* operator RPCs;
* daemon lifecycle.

Do not replace Factory’s process/session runtime with `tea_core::runtime::SessionRuntime`.

A Factory paid session and a Tea durable session are different abstractions. Factory remains the authoritative outer session.

Do not make Tea depend on Factory.

Do not add PostgreSQL, Factory protocol, Git custody, or Factory CAS concepts to Tea.

Do not add a new crate merely for this integration.

Do not enable Tea self-extension in Factory as part of this refactor. The final static integration must make later bounded self-extension possible, but this project must first establish exact static parity.

---

## 2. Relevant source only

Read the current versions of these files before editing.

### Tea

```text
docs/architecture.md

crates/tea-core/src/harness/mod.rs
crates/tea-core/src/harness/extension.rs
crates/tea-core/src/harness/capability.rs
crates/tea-core/src/harness/lineage.rs
crates/tea-core/src/harness/profile.rs
crates/tea-core/src/harness/resolver.rs

crates/tea-core/src/runtime/mod.rs
crates/tea-core/src/runtime/services.rs
crates/tea-core/src/runtime/session.rs

crates/tea-core/src/agent/builder.rs
crates/tea-core/src/effect.rs
crates/tea-core/src/tool.rs
crates/tea-core/src/trace.rs

crates/tea-luau/src/extension_engine.rs
crates/tea-trace/src/lib.rs
crates/tea-session/src/artifact.rs

crates/tea-agent/src/app/durable.rs
```

### Factory

```text
AGENTS.md
docs/ARCHITECTURE.md
docs/CONTROL-PLANE.md
docs/CONSTITUTION.md
docs/EVIDENCE.md
docs/SESSION-BOUNDARIES.md
docs/TESTING.md

crates/factory-protocol/src/application.rs
crates/factory-protocol/src/harness.rs

crates/factory-kernel/src/session_runtime.rs
crates/factory-kernel/src/harness_store.rs

crates/factory-tea-host/src/agent_host.rs
crates/factory-tea-host/src/execution.rs
crates/factory-tea-host/src/tool_bridge.rs
crates/factory-tea-host/src/main.rs
crates/factory-tea-host/src/runtime.rs

applications/xsh/POLICY-CONTRACT.md
applications/xsh/bundle.v2.json
applications/xsh/policies/*.luau
applications/xsh/templates/*.md
```

Do not broadly map unrelated Factory kernel internals.

---

# 3. Preserve behavior before changing architecture

Before modifying integration code, create deterministic provider-free evidence for the current model-facing surface of every role:

```text
product_research
engineering
quality
```

Capture exactly:

* decoded system prompt bytes;
* decoded assignment prompt bytes;
* requested model descriptor;
* thinking level;
* ordered tool names;
* tool descriptions;
* exact JSON schemas;
* execution modes;
* role-policy source digest;
* packet tool order;
* terminal-operation set;
* hook-visible behavior for representative calls.

Add a provider-free golden test or semantic fixture that can compare the old and new assembly paths during migration.

The final path must preserve:

```text
system prompt bytes
assignment prompt bytes
tool order
tool names
tool descriptions
tool schemas
execution modes
thinking level
model identity
before-tool behavior
terminal behavior
phase gating
```

Do not update the golden merely because the implementation changed.

If a difference is intentional, isolate it in a separately reviewed follow-up. This refactor itself is not a prompt or tool-surface redesign.

---

# 4. Add the missing Tea embedding seam

Factory currently has to reach below Tea’s intended abstraction because Tea exposes:

```text
Agent
SessionRuntime
```

but not a public way for an external durable authority to execute one already-resolved harness epoch.

Add a narrow public hosted-epoch API under:

```text
tea_core::runtime
```

A suitable design is approximately:

```rust
pub struct HostedEpoch {
    agent: Agent,
    identity: HarnessIdentity,
    surfaces: HarnessSurfaceFingerprints,
}

pub struct HostedEpochInput {
    pub effect_gate: Arc<dyn EffectGate>,
    pub provenance: RunProvenance,
    pub additional_tools: ToolRegistry,
}

impl RuntimeServices {
    pub fn prepare_hosted_epoch(
        &self,
        harness: &ResolvedHarness,
        input: HostedEpochInput,
    ) -> Result<HostedEpoch, HarnessError>;
}
```

Exact names may improve, but preserve the semantics.

## Hosted-epoch requirements

The hosted API must:

* reuse the exact same internal agent-construction path as `SessionRuntime`;
* combine trusted tools, resolved extension tools, and explicit additional tools;
* reject all name collisions;
* install the resolved prompt;
* install resolved hooks;
* install compaction policy;
* install tool-result projection;
* install failure policy;
* install the caller’s provider;
* install the caller’s effect gate;
* attach exact immutable harness provenance;
* expose exact harness identity and provider-surface fingerprints;
* create no session;
* create no files;
* discover no provider;
* discover no extension engine;
* discover no capabilities;
* add no artifact tools;
* add no harness-authoring tool;
* add no hidden tools;
* start no task;
* spawn no executor.

The API must automatically populate or validate these provenance fields from `ResolvedHarness`:

```text
harness_snapshot_id
harness_revision_id
model_harness_profile_id
provider_surface_digest
```

Do not allow a caller to accidentally attach provenance that disagrees with the resolved harness.

External caller-owned fields may remain supplied explicitly:

```text
session_id
lane_id
operation_id
epoch_id
core_run_id
experiment_id
```

## Context and lifecycle policy

Do not silently discard resolved context or lifecycle contributions.

Choose one of these designs:

1. expose a small hosted-policy object through `HostedEpoch` so an external authority can invoke the relevant lifecycle/context ports; or
2. reject preparation with a typed error when the resolved harness contains context/lifecycle functionality that the caller has not explicitly elected to handle.

The current Factory role policies do not need these features, so the Factory migration may use the stateless hosted path.

Do not silently treat unsupported policy surfaces as no-ops.

## Shared construction

Refactor the existing crate-private:

```text
RuntimeServices::build_agent_with_tools
```

rather than copying its implementation.

`SessionRuntime` and `HostedEpoch` must use one common construction function.

Add tests proving that equivalent inputs produce identical:

* system prompt;
* model;
* thinking level;
* ordered tool definitions;
* hook chain behavior;
* compaction configuration;
* provider-surface digest.

---

# 5. Add a reusable Tea harness-seeding API where needed

Do not copy Tea terminal-host snapshot construction into Factory.

If Factory would otherwise have to recreate the details of:

```text
ModelHarnessProfile
HarnessSnapshotSpec
policy descriptor digests
tool presentation descriptors
resource limits
initial snapshot
initial revision
```

extract the smallest reusable builder into:

```text
tea_core::harness
```

For example:

```rust
pub struct HarnessSeedBuilder { ... }

pub struct SeededHarness {
    pub snapshot: HarnessSnapshotV1,
    pub revision: HarnessRevisionV1,
    pub profile: ModelHarnessProfile,
}
```

The builder should accept explicit values only:

```text
base system prompt
model-harness profile
runtime policy descriptors
ordered extension bundles
trusted tool presentations
capability binding references
resource limits
extension engine
artifact store
```

It must not:

* discover files;
* discover a provider;
* discover a model;
* load an application;
* know Factory;
* create a session;
* choose self-extension mode implicitly.

Migrate `tea-agent` to this shared builder if it currently owns equivalent generic seed logic.

Do not introduce a broad framework. The purpose is solely to prevent every Tea embedding from reconstructing snapshot invariants manually.

---

# 6. Convert Factory to Tea’s language-neutral capability boundary

Refactor `FactoryCapability`.

It currently implements the Luau-specific:

```text
LuauCapability
CapabilityRequest
CapabilityResponse
CapabilityError
```

Instead, make its primary interface implement:

```text
tea_core::harness::extension::ExtensionCapability
```

using:

```text
ExtensionCapabilityRequest
ExtensionCapabilityResponse
ExtensionCapabilityError
```

The `tea-luau` adapter already translates those core-owned values into its coroutine ABI.

Factory must no longer import or construct:

```text
LuauCapability
CapabilityBindings
CapabilityRequest
CapabilityResponse
CapabilityError
LuaToolHandler
ToolHandlerSpec
HandlerLimits
```

Map Factory failures into Tea’s typed extension-capability errors:

```text
Cancelled
MethodDenied
InvalidArguments
Execution
```

Keep Factory-owned behavior unchanged:

* packet tool admission;
* exact method matching;
* phase gates;
* command IDs;
* expected revisions;
* daemon RPC;
* local workspace operations;
* terminal deferral;
* response validation;
* task-safe error messages;
* tool timing diagnostics.

Factory policy source never grants authority merely by naming `factory`.

The host still constructs one explicit `PluginCapabilityBinding` for:

```text
plugin_id = the exact role-policy extension ID
capability = "factory"
capability_version = a stable Factory capability ABI
```

The host identity digest must derive from stable, non-secret facts such as:

* Factory capability ABI version;
* installed Factory host/build identity where available;
* exact admitted tool-method contract;
* invocation limits.

Do not include:

* API keys;
* process IDs;
* timestamps;
* session revisions;
* temporary paths;
* mutable workspace contents.

Build a `PluginCapabilityCatalog` and let `HarnessResolver` bind it to the immutable snapshot.

---

# 7. Adapt sealed Factory policy bytes into a standard Tea extension tree

Do not change the Factory application format merely to satisfy Tea’s internal bundle representation.

Factory currently seals one role-policy source blob.

Create a deterministic adapter that materializes this source as a standard Tea extension tree:

```text
manifest.json
main.luau
```

Where:

```text
main.luau
    exact sealed policy bytes from the assignment packet

manifest.json
    deterministic host-generated manifest
    schema_version = 1
    abi_version = Tea Luau bundle ABI
    id = deterministic role-policy extension ID
    entrypoint = main.luau
    modules = [main.luau]
    requested_capabilities = [factory]
    resource_limits = exact admitted limits
```

The extension ID should be deterministic from immutable Factory identity, for example:

```text
application revision
assignment role
policy digest
```

Do not include the assignment ID if identical policy source should produce an identical extension identity across assignments.

Use the exact sealed source bytes. Never reread:

```text
applications/xsh/policies/
```

inside a running actor.

The adapter must verify:

* packet policy digest;
* UTF-8;
* source-byte limit;
* extension descriptor;
* requested capability set;
* exact packet tool-name set;
* no missing tools;
* no extra tools;
* no duplicate tools;
* exact execution modes;
* valid tool schemas.

Tea’s extension engine should validate and compile the source.

Factory should retain only Factory-specific packet-versus-role admission checks.

---

# 8. Build one standard Tea harness per assignment

Add a focused module such as:

```text
crates/factory-tea-host/src/tea_harness.rs
```

It should perform this pipeline:

```text
verified Factory admission
        ↓
decode exact system prompt
decode exact assignment prompt
derive model-harness profile
adapt sealed policy to ExtensionSourceTree
stage immutable Tea source tree
create explicit Factory capability binding
stage immutable Tea harness snapshot
seed immutable Tea harness revision
construct HarnessResolver
resolve revision
prepare HostedEpoch
```

Use:

```text
HarnessRepository
HarnessResolver
HarnessSnapshotSpec or shared seed builder
ModelHarnessProfile
PluginBundleRef
PluginCapabilityBinding
PluginCapabilityCatalog
LuauExtensionEngine
RuntimeServices
HostedEpoch
```

## Artifact backing

For the first static hosted-epoch integration, using Tea’s `MemoryArtifactStore` is acceptable only if all exact source bytes remain durably sealed by Factory and the resulting Tea identities are exported into Factory evidence.

Prefer a Factory-controlled `FileArtifactStore` rooted beneath the assignment staging directory if it can be introduced without adding a second authority or complicating cleanup.

Do not create an independent long-lived Tea home directory.

Do not make Tea’s artifact store authoritative over Factory CAS.

Factory CAS remains the durable authority.

## Runtime services

Construct `RuntimeServices` from:

* explicit provider;
* exact model;
* thinking level;
* current host hook;
* current compaction policy;
* current tool-result projection;
* current failure policy;
* current trusted tools, if any.

No environment lookup occurs here.

## Hook order

Preserve the current effective order of:

```text
OpenAI context adaptation
Luau policy hooks
Factory engineering phase stop behavior
```

Prove equivalence with tests.

If Tea’s standard extension wrapping order cannot express the exact existing order, add the smallest language-neutral hosted-hook composition seam to Tea.

Do not retain manual `LuaPolicyHookSet` construction in Factory.

---

# 9. Replace manual Factory agent construction

Delete the architecture represented by:

```text
BareAgentHost
AgentHost
load_luau_policy
build_policy_tools
factory_capability_bindings
manual LuaToolHandler construction
manual policy prompt-section append
manual Agent::builder assembly
```

`factory-tea-host` must not directly call:

```rust
Agent::builder()
```

for a production assignment after the migration.

The only production construction path must be:

```text
HarnessRepository
    → HarnessResolver
    → ResolvedHarness
    → RuntimeServices::prepare_hosted_epoch
```

A `PreparedExecution` may retain:

```text
Admission
HostedEpoch or Agent
TerminalDeferral
CostReader
CommandContext
Tea harness identity
Tea provider-surface fingerprints
```

It must not retain a raw `LuaPolicy` merely to keep the VM alive. Tea’s resolved extension/runtime should own the necessary executable policy objects.

Preserve the current caller-driven:

```text
start prompt
drive run
collect snapshot/events
settle terminal
```

behavior.

Factory remains responsible for deciding whether the assignment completed successfully.

---

# 10. Do not force Tea `SessionRuntime` into Factory

The hosted epoch should continue to execute as one Factory-owned assignment process.

Do not create:

```text
JsonlSession
MemorySession
Tea lane
Tea operation record
Tea recovery plan
Tea artifact tools
Tea harness-authoring tool
```

inside the Factory host in this refactor.

Factory’s daemon already provides outer durability and reconciliation.

The purpose of `HostedEpoch` is precisely to use Tea’s harness compiler and execution substrate without adopting Tea’s full durable session authority.

Document this distinction explicitly:

```text
Factory session:
    process, assignment, budget, institutional and delivery authority

Tea hosted epoch:
    immutable model-facing agent policy within that process
```

---

# 11. Use Tea trace instead of reimplementing agent tracing

Replace Factory’s custom direct `AgentEventKind`-to-transcript implementation with:

```text
tea_core::trace::TraceObserver
tea_trace::RedactingSink
Factory-owned TraceSink
```

Implement:

```rust
struct FactoryTraceSink { ... }
struct FactoryTraceRedactor { ... }
```

The sink may stream bounded trace records into the assignment staging directory.

It must preserve:

* bounded output;
* incremental writes;
* flush behavior;
* no secret leakage;
* final CAS sealing;
* exact Factory assignment provenance;
* provider/model identity;
* Tea harness identity;
* Tea provider-surface digest;
* turns;
* tool calls;
* tool failures;
* cache evidence;
* compaction;
* stop reason.

Factory-specific execution diagnostics may be appended as one separate summary record:

```text
engineering phase
per-tool elapsed time
per-tool failures
cost-limit status
terminal operation
```

Do not add Factory-only fields to `tea-trace`.

## Existing transcript compatibility

Inspect all current readers and tests of:

```text
session.ndjson
session.ndjson.gz
pi_transcript_gzip
```

Then choose one explicit migration:

### When exact current line schema is externally relied upon

Implement a thin compatibility encoder from `tea_trace::TraceEvent` to the existing Factory transcript records.

Do not continue matching directly over every `AgentEventKind`.

### When no external reader requires exact bytes

Adopt Tea trace JSONL as a newly versioned evidence role and update all readers and tests atomically.

Do not retain old and new machine traces indefinitely.

A separate bounded human-readable stdout/stderr transcript may remain if operationally useful.

Remove stale Pi naming from new evidence contracts.

Do not silently reinterpret old CAS artifacts.

---

# 12. Preserve provider and cost ownership

Do not move provider construction into Tea core.

Factory host continues to:

* read the packet-selected provider;
* obtain the injected credential;
* construct `OpenRouterProvider`;
* configure timeouts;
* configure retry policy;
* read provider cost;
* cancel at the campaign allowance;
* submit final usage and cost to Factory daemon.

Use Tea’s ordinary model-turn accounting and trace records where available.

Do not derive aggregate usage from tool-result usage as a substitute for provider usage.

Preserve null/unknown distinctions.

Unknown cost must remain a failed Factory terminal outcome under the current contract.

All cost consumed during Tea harness preparation must be zero because preparation is provider-free.

---

# 13. Unify tool identity where safe

Factory currently has both:

```text
factory_protocol::ActorToolV2
factory_tea_host::ToolName
```

Eliminate this duplicated closed identity if possible.

Prefer making `ActorToolV2` the canonical role/tool identity, with stable methods such as:

```rust
as_str()
parse()
is_terminal()
is_mutating()
role eligibility
```

Factory-host execution metadata may remain in a separate catalog keyed by `ActorToolV2`:

```rust
FactoryToolContract {
    tool: ActorToolV2,
    capability_method: &'static str,
    daemon_operation: Option<&'static str>,
}
```

Do not put Tea types into `factory-protocol`.

Do not move model-facing schemas out of the sealed application policy in this phase unless exact surface parity is already green and the change can be independently validated.

The primary objective is to delete generic Luau/Tea integration duplication first.

A later cleanup may establish one canonical Rust tool catalog and reduce role-policy schema boilerplate.

Do not combine that later surface redesign into the initial hosted-epoch migration.

---

# 14. Keep Factory-specific phase logic

Do not delete or generalize these Factory controls:

```text
EngineeringPhase
CommandContext
required checkpoint before mutation
regression identity binding
candidate submission sequencing
product submission rejection state
terminal deferral
expected aggregate revision
command ID minting
tool timing diagnostics
cost stop
```

These represent Factory’s institutional workflow, not generic Tea functionality.

They may be reorganized into focused Factory modules, but they remain Factory-owned.

Do not add Engineering, Product, or Quality concepts to Tea.

---

# 15. Provider-surface provenance

The Factory terminal evidence must include the standard Tea identities for the executed epoch:

```text
harness revision ID
harness snapshot ID
model-harness profile ID
system-prompt digest
ordered-tool-definitions digest
hook-bundle digest
capability-bindings digest
provider-surface digest
```

These must be produced by Tea, not independently rehashed in Factory.

Also retain Factory’s own identities:

```text
application revision ID
assignment ID
harness/assignment compilation ID
packet digest
policy source digest
host build identity
```

The combined provenance chain should be:

```text
Factory application revision
    → Factory assignment compilation
    → Factory packet
    → Tea harness snapshot
    → Tea harness revision
    → Tea hosted epoch
    → Factory terminal result
```

Do not conflate Factory’s `HarnessCompilationV2` with Tea’s `HarnessSnapshotV1`.

As a documentation-only cleanup, rename the Factory concept if practical:

```text
HarnessSpecV2
    → AssignmentProgramSpecV2 or ActorProgramSpecV2

HarnessCompilationV2
    → AssignmentProgramCompilationV2 or ActorProgramCompilationV2
```

Only perform this rename if it does not force an unnecessary database-format migration. Otherwise document the distinction now and defer the durable vocabulary rename.

---

# 16. Self-extension remains off

Construct the base Factory Tea harness with:

```text
SelfExtensionMode::Off
```

Do not expose Tea’s harness-authoring tool.

Do not permit:

* candidate staging;
* revision rollover;
* task-local plugins;
* capability expansion;
* context-policy mutation;
* tool-presentation mutation.

However, do not hard-code the integration in a way that prevents a later mode from using:

```text
HarnessCandidate
HarnessRevision
one bounded rollover
Factory-sealed candidate evidence
```

The hosted-epoch API should remain general.

Factory’s static integration is the prerequisite for later Engineering-only JIT adaptation.

---

# 17. Exact code expected to disappear

After the final migration, production Factory code should contain no direct references to:

```text
LuaPolicyHookSet
LuaToolHandler
ToolHandlerSpec
CapabilityBindings
LuauCapability
CapabilityRequest
CapabilityResponse
CapabilityError
build_policy_tools
factory_capability_bindings
load_luau_policy
append_policy_prompt_sections
BareAgentHost
AgentHost
```

There should be no production:

```rust
Agent::builder()
```

inside `factory-tea-host`.

There should be no production code that manually reconstructs Tea’s:

* extension handler runtime;
* capability adapter;
* resolved hook chain;
* provider-surface fingerprint;
* trace event model.

Do not satisfy this requirement by moving identical code to another Factory file.

Delete it.

---

# 18. Migration sequence

Perform the work incrementally.

## Phase A — Baseline and parity fixtures

1. Run Tea tests.
2. Run Factory’s provider-free host and vertical tests.
3. Capture role provider surfaces.
4. Add exact parity tests.
5. Make no behavior changes.

## Phase B — Tea hosted-epoch API

1. Add the public hosted-epoch API.
2. Share construction with `SessionRuntime`.
3. Add provenance and collision tests.
4. Add unsupported-context/lifecycle tests.
5. Keep Tea’s existing terminal application green.

## Phase C — Core-owned Factory capability adapter

1. Implement Tea `ExtensionCapability`.
2. Remove the Luau-specific capability implementation.
3. Keep the existing dispatch and phase machinery.
4. Add direct capability-boundary tests.

## Phase D — Standard Tea harness assembly

1. Adapt sealed policy bytes to `ExtensionSourceTree`.
2. Stage a Tea tree/snapshot/revision.
3. Build the capability catalog.
4. Resolve through `LuauExtensionEngine`.
5. Prepare a hosted epoch.
6. Compare its provider surface against the baseline fixture.

Do not change production execution until exact parity passes.

## Phase E — Switch production execution

1. Replace `ExecutionInput::prepare`.
2. Replace manual agent construction.
3. Drive the hosted agent.
4. Preserve terminal and cost behavior.
5. Delete old construction code.

## Phase F — Standard Tea tracing

1. Add Factory trace sink/redactor.
2. Switch machine evidence to `TraceObserver`.
3. Preserve or explicitly version transcript compatibility.
4. Delete direct event projection.

## Phase G — Identity cleanup

1. Unify `ActorToolV2` and `ToolName` where safe.
2. Add Tea harness identities to terminal evidence.
3. Update architecture/evidence docs.
4. Delete all transitional code.

Do not start prompt shrinking or Factory self-extension until every static acceptance gate passes.

---

# 19. Required tests

## Tea tests

Add focused tests proving:

* hosted epoch and managed epoch produce identical agent configuration;
* provider-surface fingerprints match;
* resolved extension tools execute;
* capability bindings remain snapshot-bound;
* additional tool collisions fail;
* trusted tool collisions fail;
* caller provenance cannot disagree with harness identity;
* no session or artifact tool appears implicitly;
* no harness-authoring tool appears implicitly;
* unsupported hosted context/lifecycle policy is rejected or explicitly exposed;
* trace observer works on a hosted epoch;
* sessionless `Agent` behavior remains unchanged;
* `SessionRuntime` behavior remains unchanged.

## Factory host tests

Add tests proving:

* policy digest mismatch fails before Tea resolution;
* generated extension manifest is deterministic;
* extension ID is deterministic;
* requested capability is exactly `factory`;
* packet and resolved tool sets must match exactly;
* unknown policy tool fails;
* missing policy tool fails;
* duplicate tool fails;
* unbound capability fails;
* handler limits remain bounded;
* Factory capability method mismatch fails;
* phase gates remain unchanged;
* terminal deferral remains one-shot;
* system prompt bytes are unchanged;
* assignment prompt bytes are unchanged;
* tool order and schemas are unchanged;
* thinking level is unchanged;
* model identity is unchanged;
* no provider call occurs during harness preparation;
* hosted epoch can execute a deterministic fake-provider assignment;
* Tea harness identity enters terminal evidence;
* trace output is redacted and bounded;
* cost stop still prevents terminal submission;
* unknown cost still fails closed.

## Factory kernel and acceptance tests

Preserve:

* packet verification;
* runtime identity verification;
* required-read accounting;
* process cancellation;
* terminal reconciliation;
* candidate submission;
* hard validation;
* Quality review;
* delivery;
* backup and restore;
* generic Product → Engineering → Quality flow.

---

# 20. Verification commands

Use each repository’s pinned toolchain and documented commands.

At minimum, run in Tea:

```sh
cargo test -p tea-core --all-targets
cargo test -p tea-luau
cargo test -p tea-trace
cargo test -p tea-session
cargo test -p tea-providers --features provider-openrouter
cargo test --workspace
git diff --check
```

Run in Factory:

```sh
make tea-test

cargo test -p factory-tea-host
cargo test -p factory-protocol
cargo test -p factory-kernel

make provider-free-host
make provider-free-vertical
make application-contract-test

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

Where disposable database inputs are available, also run the complete documented:

```sh
make tea-acceptance
```

Do not run a live paid cycle merely to validate this refactor.

Do not make provider calls in ordinary tests.

---

# 21. Acceptance criteria

The refactor is complete only when all of these are true:

* Factory’s institutional architecture is unchanged.
* Factory still owns all SQL, CAS, Git, validation, delivery, and budget authority.
* Tea has a public hosted-epoch API.
* Tea `SessionRuntime` and hosted epochs share one agent-construction path.
* Factory role policy is resolved through `LuauExtensionEngine`.
* Factory capability implements Tea’s language-neutral extension boundary.
* Factory no longer constructs Luau handlers.
* Factory no longer constructs a Luau hook set.
* Factory no longer builds `Agent` directly.
* Factory no longer manually appends policy prompt sections.
* Exact model-facing role surfaces are unchanged.
* Exact tool admission remains fail-closed.
* Factory terminal behavior is unchanged.
* Cost behavior is unchanged.
* Provider behavior is unchanged.
* Tea harness identities are retained in Factory evidence.
* Factory machine tracing uses Tea’s trace contract.
* No Tea session store is introduced into Factory.
* Self-extension remains disabled.
* Provider-free Factory acceptance remains green.
* No compatibility implementation remains beside the new path.

---

# 22. Final report

Report concisely:

1. Tea hosted-epoch API added;
2. common agent-construction code shared;
3. Factory production assembly before and after;
4. files and symbols deleted;
5. Luau-specific imports removed from Factory;
6. Factory capability ABI and binding identity;
7. exact role-surface parity result;
8. trace migration result;
9. transcript compatibility decision;
10. Factory and Tea provenance now retained;
11. tests run;
12. provider-free acceptance result;
13. any deliberately deferred cleanup.

Do not claim that Factory now supports self-evolving assignments.

The correct completion statement is:

> Factory now uses Tea as an immutable harness compiler and hosted agent runtime, while retaining exclusive authority over the software factory’s institutional state, effects, validation, economics, and delivery.

---

The critical implementation rule is: **do not “deduplicate” by forcing Factory into Tea’s `SessionRuntime`.** The missing abstraction is one externally hosted, fully resolved Tea epoch. Factory’s outer session is legitimately different; its manual Luau and agent assembly is not. Tea already distinguishes provider-independent resolved policy from host-owned executable services, and its capability binding explicitly separates source requests from authority grants.
