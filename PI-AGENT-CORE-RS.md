# Pure-Rust `pi-agent-core-rs` cutover plan

**Status:** implemented; the provider-free qualification inputs remain
operator-supplied. This is an execution contract, not an implementation
authorization for a paid campaign. It preserves Factory's
kernel custody, evidence, cost, validation, and delivery authority while
removing the upstream Pi SDK and every Deno/TypeScript runtime path.

## Outcome

Factory V3 will run one Rust actor executable, `factory-pi-host`, directly on
the `pi-agent-core-rs` crates.  It will receive the existing inherited actor
descriptor and sealed assignment packet, construct a bare `Agent`, bind only
packet-admitted tools, execute one assignment, write bounded evidence, and
submit the existing terminal RPC.  The Rust kernel stays the authority.

The end state has all of the following properties:

- no `vendor/pi-headless`, `@factory/pi-headless`, upstream Pi SDK, Node/NPM,
  Deno, TypeScript, or JavaScript in Factory's build, installed-runtime, actor,
  application, test, or operational-tool paths;
- direct, pinned Rust dependencies on `pi-agent-core`, `pi-agent-protocol`,
  `pi-agent-luau`, and, where useful, `pi-agent-trace`;
- one checked, explicit Rust toolchain shared with `pi-agent-core-rs`;
- every model-visible Factory actor tool declared in a sealed, bounded Luau
  policy and effected only by an explicit Rust capability binding;
- Rust-owned application compilation/admission and Rust-owned operator tools;
  XSH remains inert declarative policy plus Markdown and Luau source, never
  daemon-evaluated application code;
- the current protocol, packet, required-read, session, CAS, Git, validation,
  cost, transcript, and local-delivery contracts remain fail-closed.

The term *Pi* below means the new Rust microkernel project only.  It never
permits a compatibility adapter around the upstream SDK.

## Baseline found during research

The existing implementation has four TypeScript/Deno ownership areas:

| Current owner | Current responsibility | Pure-Rust replacement |
| --- | --- | --- |
| `vendor/pi-headless` plus `sdk-factory.ts` | model catalog, provider runtime, session loop, builtin tools, audit projection | `pi-agent-core` agent loop and a Factory-specific, packet-bound provider/observer host |
| `packages/factory-pi-host` | FD 0 admission, framed actor RPC, tool adaptation, limits, transcript, terminal summary | Rust `factory-pi-host` crate/binary and shared host library |
| `packages/factory-sdk` | closed JSON validation, application compiler, actor/operator clients | consolidate with `factory-protocol`, `factory-kernel`, `factoryctl`, and the Rust host |
| `applications/xsh/mod.ts` | application declaration and dead application-side validators | static V2 source bundle, Markdown, and per-role sealed Luau policy |

The Rust kernel already owns the important lower boundary: canonical packet
types, length-framed JSON, actor socket custody, session admission, terminal
reconciliation, application admission, worktrees, CAS, validation, and
delivery.  It must not be weakened or moved into the new host.

`pi-agent-core-rs` already supplies the appropriate agent state machine,
structured cancellation, JSON-schema tool boundary, Smol-compatible execution,
provider port, optional trace contract, and hermetic Luau coroutine handlers.
Runebench is useful prior art for explicit provider ownership, cancellation,
event logging, capability manifests, and Luau-backed tools.  Its MCP/world
client must not be copied into Factory.

## Decisions to lock before implementation

1. **Dependency provenance (confirmed local bootstrap).** Depend directly on
   the checkout at `/Users/josh/d/pi-agent-core-rs` for now.  Cargo manifests
   must use that literal absolute path—`~` is not expanded by Cargo.  Record
   the checkout's exact `HEAD`, dirty-state refusal, and closed source
   inventory in the installed-runtime receipt so the local path cannot drift
   beneath a running Factory.  This deliberate temporary nonportable path is
   replaced by a published, pinned source mechanism later; it is not a
   submodule, copied crate, floating Git revision, or compatibility shim.
2. **Toolchain (recommended: adopt the core's pinned nightly).**
   `pi-agent-core-rs` requires `nightly-2026-07-24`; Factory currently declares
   Rust 1.94 but is presently being driven by the compatible nightly.  Add a
   Factory `rust-toolchain.toml`, align CI/Make/docs, and make the pinned
   nightly part of installed-build qualification.  A stable-only fallback is
   not part of this migration.
3. **Cutover state (confirmed absolute hard cutover).** Stop the daemon and
   start Factory with a new empty database and runtime root after migration.
   Do not retain, parse, migrate, reconcile, launch, recover, or expose any
   legacy Deno session/application/runtime compatibility path.  The operator,
   not Factory code, is responsible for retiring the old state.
4. **Application format (confirmed V2-only).** Replace the old application
   and packet formats with explicit V2 types and artifacts.  No V1 parser,
   history/navigation compatibility, or dual-runtime dispatch remains.  V2
   adds sealed per-role Luau policy artifacts; it does not smuggle executable
   callbacks or free-form metadata into the generic application model.
5. **New dependencies.** The direct `pi-agent-*` crates (and transitive Luau
   runtime) are the intended dependency change. Factory emits its bounded
   transcript as a standards-compliant stored-block gzip stream using the
   standard library, so no compression dependency was introduced. Do not add
   an HTTP, JSON, async-runtime, ORM, or scripting dependency without a
   separate decision.

## Target boundaries

```text
XSH static source bundle + Markdown + Luau policy
        -> Rust application compiler -> sealed V2 application revision
        -> Rust factory-pi-host (pi-agent-core + pi-agent-luau)
        -> inherited framed descriptor -> Rust kernel -> PostgreSQL + CAS
        -> owned XSH worktree -> kernel validation and guarded local delivery
```

`factory-pi-host` is a Rust process adapter, not a control plane.  It may own
the agent process, in-memory secret, provider transport, structured
cancellation, bounded transcript projection, and packet-admitted tool
bindings.  It may not own SQL, CAS adoption, Git identity, worktree lifecycle,
candidate identity, validation receipt, campaign state, or delivery.

Each Luau policy is data-bearing policy, not authority.  A policy describes a
tool name, schema, description, execution mode, and coroutine handler.  The
host accepts only a sealed policy artifact named by the admitted role, checks
its declarations against the packet's exact tool allowlist, and binds each
declared capability only to narrow Rust methods.  Rust validates every yielded
argument and maps it to an existing closed daemon operation.  No
policy can open files, use the network, run a process, discover modules, or
choose a different packet/session identity.

## Required `pi-agent-core-rs` work

Complete these additions in `~/d/pi-agent-core-rs` first, each with a focused
regression fixture/test and public documentation.  Commit them there before
updating Factory's qualified local-checkout revision.

1. **Factory-grade OpenRouter adapter.** Either extend the opt-in
   `provider-openrouter` feature or add an equally narrow host-facing port
   that accepts a caller-supplied key and exact packet configuration.  It must
   validate the requested provider/model, send the packet output-token cap and
   thinking level, preserve tool-call/usage semantics, report per-turn and
   aggregate cost in a lossless host-readable form, and kill/reap its transport
   promptly on the run's cancellation token.  The current convenience adapter
   is finite-response and does not yet carry all of these Factory semantics.
   It must remain free of environment discovery and Factory types.
2. **Auditable tool-call event data.** Factory's transcript contract needs
   bounded tool names, inputs, results, retries, and terminal reason.  Today
   the generic trace surface deliberately lacks serialized tool arguments.
   Add an explicit, redaction-ready argument field at the correct event/trace
   boundary, or an equivalent observer capability that records it before tool
   dispatch.  Do not infer arguments from model text or add a hidden recorder.
3. **Host-controlled transcript sink.** Provide or document the stable event
   sequence needed for a bounded Factory NDJSON projection.  Factory may own
   its exact JSONL/gzip encoding, but the core must guarantee its observer
   ordering, terminal settlement, cancellation behavior, and no
   post-settlement events under a deterministic test.
4. **Luau handler affordances, only if evidence exposes a gap.** The existing
   `LuaPolicy`, `LuaToolHandler`, capability bindings, manifest, budgets, and
   cancellation path are the intended design.  Add no ambient Factory module.
   If one tool needs progress reporting or a multi-step capability exchange
   beyond the current coroutine contract, specify and test it as a generic
   core feature before Factory depends on it.

The core's default coding profile is not the Factory policy.  Factory has
sealed prompts and differently named, daemon-bound tools; it will construct a
bare agent and use only its own Luau-declared tools.

## Factory implementation sequence

Focused tests in these steps are development evidence, not independently
shippable migration gates.  The first release decision is the complete gate
defined later in this document.

### 1. Establish the Rust dependency and build contract

- Add direct absolute-path `pi-agent-core-rs` workspace dependencies, the
  toolchain file, and lockfile changes.  Qualification must refuse a missing,
  dirty, or `HEAD`-changed local checkout.  Keep `pi-agent-core` upstream of
  Factory; `factory-protocol` must not depend on it.
- Create `crates/factory-pi-host` as a library plus the one production binary.
  It may depend on `pi-agent-core`, `pi-agent-luau`, `pi-agent-protocol`, and
  `factory-protocol`; `factory-kernel` launches it but does not depend on its
  agent-loop internals.
- Add an installed-runtime receipt for the exact host executable, Cargo lock,
  Rust toolchain, and exact local-core `HEAD`/source inventory.  Replace all
  Deno executable/config/lock/cache/module-graph/Pi-SDK receipt fields with
  these Rust facts.  The kernel build identity must change when any of them
  changes.
- Add a narrow dependency-direction test: the kernel/protocol compile without
  `applications/xsh`, and application policy cannot reference runtime, CAS,
  database, socket, or process APIs.

### 2. Define Rust V2 application and packet contracts

- Replace the current application and assignment packet types with
  `ApplicationBundleV2` and `AssignmentPacketWireV2` in `factory-protocol`.
  Delete V1 parsers, fixtures, migrations, and navigation code rather than
  preserving history.  Replace Deno/Pi runtime fields with a Rust
  host/runtime identity.
- Add a closed `ActorPolicyArtifactV2` for each fixed assignment role.  It
  names a safe relative source path, BLAKE3 digest, byte limit, and the exact
  policy entrypoint format.  The compiler seals explicit source bytes; the
  daemon never evaluates them.
- Move the remaining TypeScript application compiler behavior into Rust:
  strict source-bundle parsing, canonical JSON, template/policy path safety,
  digest materialization, placeholder validation, one-pass rendering, byte
  ceilings, and deterministic output.  Reuse the existing Rust application
  validation and template renderer rather than keeping duplicate validators.
- Keep the protocol operation names, frame limits, canonical request/response
  forms, response identity checks, and typed error/conflict behavior stable
  unless a V2 field requires an explicit golden update.  Rewrite the current
  TypeScript golden tests as Rust protocol contract tests.
- Explicitly retire the current XSH-only TypeScript validators after proving
  they were test-only and not an actor/kernel authority boundary.  If any
  desired XSH restriction is still needed, encode it as a V2 declarative field
  checked by the generic kernel rather than an application callback.

### 3. Port the actor host and its transport to Rust

- Reimplement FD 0 admission exactly: bounded newline admission frame,
  canonical packet decode, packet/frame identity equality, then
  `session.verify_packet` over the inherited full-duplex framed descriptor.
  No host accepts a socket path, database URL, or resume/session input.
- Port the serialized, length-prefixed frame client with short-read/write,
  EOF/loss, response identity, frame-limit, and error-shape checks.  Reuse
  `factory-protocol` codecs; delete `factory-sdk/protocol.ts` only after Rust
  goldens prove byte-level parity where V2 intentionally has not changed it.
- Implement one `Agent` per assignment, exactly one active run, and a
  packet-validated OpenRouter `ModelProvider`.  Read the selected credential
  once from the inherited environment, give it only to the provider, remove it
  from child/tool environments, and retain no secret in evidence or errors.
- Recreate all current host limits: wall timer, turns, transcript/output
  bytes, cost budget, authority loss, single legal terminal operation, exact
  required-read ledger, cost-unknown failure, sealing order, and one terminal
  summary.  `Agent::abort` must drive cancellation through provider, Luau
  handlers, tool capability futures, transcript observer, and direct child
  reconciliation.
- Implement a Rust bounded NDJSON observer and gzip artifact writer that
  preserves the documented diagnostic projection: assistant text, tool
  boundary/input/result/retry data, usage/cost, and terminal reason; it must
  omit reasoning, session trees, and interactive snapshots.  Keep typed
  kernel evidence authoritative over this diagnostic transcript.

### 4. Put all actor tools behind sealed Luau policies

- Add one XSH `.luau` policy per role under `applications/xsh/policies/`.
  These policies declare every allowed model tool and its closed schema:
  `workspace_*`, `shell`, Forum reads, publication creation, artifact
  operations, Product submission, Engineering checkpoint/submission, Quality
  validation/review, and `work_complete` as appropriate to the role.
- Port the current TypeScript descriptions, schemas, terminal deferral rules,
  model-visible result filtering, bounded task-level corrections, and Forum
  author-office stripping into Luau declarations plus Rust capability code.
  Keep the task-level copy reviewable in source and test it through the exact
  returned model result; raw daemon/transport diagnostics stay in sealed host
  evidence.
- Bind a host-owned `factory` capability (or several narrower names if the
  capability manifest makes that clearer) only to the exact admitted methods.
  Every method maps to the existing framed daemon operation and validates the
  full parsed JSON shape before forwarding it.  Capability strings, tool
  names, and policy source never grant a method on their own.
- Implement local workspace write/edit/search/list/shell as equally explicit
  Rust capabilities scoped to the kernel-created worktree.  `workspace_read`
  remains daemon-bound so only its connection-owned ledger can prove a
  required read.  Tool execution environment stays closed and uses only the
  kernel-qualified paths already allowed by the assignment.
- The policy loader receives bytes from the sealed V2 packet, constructs an
  explicit `LuaPolicy`/`LuaToolHandler` set, validates policy declarations
  against the packet, then gives the tools to the agent.  It never reads
  application source from disk at actor runtime.

### 5. Port XSH application authoring

- Replace `applications/xsh/mod.ts` with a static V2 source bundle (JSON or
  another inert data format parsed by Rust), the existing Markdown templates,
  and the new Luau policy files.  The JSON must declare the same repository
  pin, fixed roles, models, budgets, reads, command profiles, Git policy, and
  template ceilings now expressed in TypeScript.
- Add a Rust application compiler entrypoint, preferably a `factoryctl`
  subcommand, that materializes `bundle.v2.json` deterministically from the
  static source bundle, templates, and policy artifacts.  Registration seals
  those bytes through the existing application admission path.
- Replace the Pi offline-model-catalog test with Rust checks that each XSH
  model descriptor is structurally valid and exactly consumable by the new
  packet-bound provider.  There is intentionally no replacement catalog or
  runtime discovery.
- Convert XSH application/template tests to Rust.  They must cover identical
  compilation bytes, required policy/template paths and digests, the exact
  per-role tool surface, prohibited application authority, and the existing
  Product/Engineering/Quality policy invariants.

### 6. Port runtime installation and operations

- Change `factoryctl init`, `factoryd init`, `InstalledRuntimeManifest`,
  `RuntimeIdentityV2`, spawn specifications, packet validation, status output,
  and recovery to qualify/spawn the Rust `factory-pi-host` binary.  The launch
  still passes only the inherited connected descriptor and the selected
  credential environment variable.
- Remove Deno cache installation/preflight and replace it with host binary
  verification plus a provider-free Rust host self-check that cannot issue a
  model request.  No ordinary actor process is launched during qualification.
- Move `paid-cycle-verify` JSON parsing from `deno eval` into a read-only
  Rust `factoryctl` command or Make target backed solely by `factoryctl` and
  Git inspection.  Preserve the exact one-delivery, clean-HEAD, and
  `Factory-Cost` checks.
- Port `tools/backup_restore_check.ts` and its tests into Rust, most naturally
  as a dedicated `factoryctl` module/subcommand.  There may be no residual
  Deno utility path after cutover.
- Rewrite `Makefile`, operations/testing/trust/repository-boundary docs, and
  installation discovery for a Rust-only toolchain.  Remove `make cache`
  unless a new Rust-only preparation has a specific contract; `make lint` and
  acceptance never invoke Deno, npm, or an upstream Pi build.

### 7. Prove the cutover, then delete the old implementation

- Convert all meaningful Deno tests to focused Rust unit/integration tests:
  frame and packet goldens; V2 application compiler; policy declaration and
  denied-capability cases; provider/cancellation/cost accounting; transcript
  projection/gzip; FD 0 host integration; terminal/required-read behavior;
  operator and backup/restore tools.
- Add test-only deterministic model streams to the host library.  They must
  exercise the real Rust agent, Luau policies, capability bindings, framed
  daemon transport, and process custody without giving the production binary a
  provider-selection backdoor.
- Once the complete Rust gate passes, remove every Deno/TypeScript artifact:
  workspace/config/lock, `packages/factory-sdk`, TypeScript
  `factory-pi-host`, TypeScript XSH module/tests, `backup_restore_check.ts`,
  all Deno test tooling, built headless artifacts, and the
  `vendor/pi-headless` gitlink.  Remove old installed-runtime fields,
  migrations, and protocol types, not merely their use.
- Add a release guard which fails if active Factory source/build/installation
  material imports or invokes Deno, Node, npm, TypeScript, JavaScript,
  `pi-headless`, `@factory/pi-headless`, or the upstream Pi SDK.  Historical
  migration explanation may mention the retired implementation, but it cannot
  be executable, qualified, or launchable.

## First acceptance gate: one full Rust integration qualification

Introduce one formal target, `make pi-agent-core-rs-acceptance`.  Do not label
any partial package port as accepted.  The target uses the pinned nightly and
externally created disposable PostgreSQL databases, just like the current
provider-free acceptance.  It performs all of these in one run:

1. Build/check/test Factory and the pinned `pi-agent-core-rs` revision using
   the common toolchain; run the core parity, provider, Luau, and trace tests
   affected by the new Factory-required features.
2. Compile the actual XSH V2 source bundle twice, prove byte-identical
   template/policy identities, and admit/activate it through the real Rust
   application path.
3. Start a real Rust daemon from a fresh qualified installed-runtime receipt
   with no Deno inputs.  Exercise the actual Rust `factory-pi-host` over its
   inherited FD 0 using deterministic provider scripts for Product,
   Engineering, and Quality.  Prove exact packet verification, policy/tool
   allowlists, required-read attestation, transcript seal, terminal
   reconciliation, cancellation, unknown-cost rejection, and forbidden
   capability denial.
4. Run the existing-style complete provider-free candidate-to-delivery flow
   against a disposable repository with the actual Rust host process, all
   three roles, kernel validation, Quality review, provenance, and guarded
   local fast-forward.  This proves the entire control-plane path without
   spending provider budget or touching `../xsh`.
5. Run the real-XSH bundle admission and role-policy integration fixtures in
   the same qualified build.  It need not manufacture a product defect in
   `../xsh`; that product checkout remains outside Factory test state.
6. Run Rust-only backup/restore qualification, SQLx metadata validation, and
   the legacy-absence/repository-boundary guard.

This is intentionally provider-free, as confirmed.  Passing it proves the
full deterministic integration, not that a provider will find a useful XSH
defect.  A live paid cycle remains a separate Grand Architect action and
requires explicit fresh authorization, live-state inspection, budget,
deadline, and command ID after this gate has passed.

## Completion criteria

The migration is complete only when:

- `make pi-agent-core-rs-acceptance` passes from a clean checkout;
- an installed daemon receipt and every launched V2 packet identify the Rust
  host/core revision and contain no Deno/Pi-SDK fields;
- the real host's three role policies execute only packet-admitted Rust
  capabilities and prove current Factory custody invariants;
- no active source, build, runtime receipt, command, test, or documentation
  directs an operator toward an upstream Pi SDK/Deno/TypeScript path; and
- a subsequent live paid cycle, if separately authorized, is able to use only
  this Rust runtime and is verified by the unchanged one-delivery proof.

## Confirmed scope

- The cutover is absolute: Factory starts fresh after migration and carries no
  legacy state or compatibility code.
- The temporary direct dependency is the local
  `/Users/josh/d/pi-agent-core-rs` checkout.  Publication can replace that
  source mechanism later without changing Factory's Rust host boundary.
- All TypeScript/Deno—including the operational backup/restore utility,
  configuration, locks, and tests—moves to Rust or is removed.
- The first complete acceptance gate is provider-free.  Paid XSH execution is
  intentionally outside this migration gate and requires a separate request.
