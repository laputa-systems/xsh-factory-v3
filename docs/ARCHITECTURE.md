# Architecture

Factory V3 is a deliberately small generic control plane for improving a local
product repository. Its first application is XSH, but XSH vocabulary and
policy belong only in `applications/xsh` and its templates.

```text
application source -> sealed application revision -> assignment packet
                                                       |
Rust Tea host -> local framed descriptor -> Rust kernel -> PostgreSQL + CAS
                                                       |
                                                isolated ../xsh worktrees
                                                       |
                                             validated commit + local delivery
```

The Rust crate graph is strictly product-neutral. No file under `crates/`
(including tests and diagnostics) may embed the current application's key,
checkout paths, executable names, owner paths, prompts, or bundle fixtures.
Those facts live under `applications/<key>/` and cross into Rust only through
the typed application-bundle boundary. The generic command-line layer may
accept an application source root supplied by the operator, but it must not
select or name a product in crate code.

Shared runtime policy knobs live in the dependency-free `factory-settings`
crate (`crates/factory-settings/src/settings.rs`). Kernel, daemon, CLI, and
host code import those bounded limits, deadlines, retry counts, paths, and
runtime identities from that one source. Wire protocol versions, operation
names, persisted state/audit codes, and evidence-format identities remain in
their owning crates because changing them changes a durable contract rather
than tuning runtime behavior. The complete session-boundary inventory, including
the deliberately open-ended actor/provider progress policy, lives in
[session boundaries](SESSION-BOUNDARIES.md).

## Authority split

Rust owns facts whose failure would make accepted work false, unsafe,
unaffordable, or irreproducible: state transitions, process custody, artifact
identity, repository qualification, worktree/tree capture, validation, commit
construction, and delivery. PostgreSQL holds mutable lifecycle and audit
relations; CAS holds immutable bytes.

One qualified application lane retains its campaign and ticket history in that
same authority across paid cycles. Database rotation is a deployment/runtime
replacement boundary, not a campaign boundary; independent authorities are
not implicitly mergeable because their IDs and CAS provenance are local to the
qualified runtime/evidence set.

`factory-tea-host` validates closed host shapes, adapts the sealed assignment
packet to Tea, and speaks the framed kernel descriptor. It has
no independent lifecycle authority.

The active actor runtime is one qualified Rust executable built against the
direct local `/Users/josh/d/tea-copy` checkout and the pinned
`nightly-2026-07-24` toolchain. The installed-runtime receipt binds the host
binary, complete host source graph, core `Cargo.lock`, exact core `HEAD`,
complete core source inventory, and their digests. Qualification refuses a
missing or dirty core checkout, and the kernel rechecks the receipt before
every launch. The packet carries the host path, core `HEAD`, core source
digest, toolchain, and credential-variable name; it never carries the
credential value.

The host admits one bounded newline frame on inherited FD 0, verifies the
canonical packet and identity, and then uses only the inherited full-duplex
descriptor. It does not accept a socket path, database URL, resume input, or
application source path. The daemon remains the owner of SQL, CAS, Git,
worktrees, validation, campaign state, terminal reconciliation, and delivery.
The selected credential is resolved from Vault by the daemon at assignment
launch, is installed only in the provider host's exact child environment, and
is never retained in daemon environment state. Child tools and evidence do not
receive it.

The application bundle is V2-only and inert. Each role's sealed Luau policy is
carried in the packet, checked against the exact packet tool allowlist, and
bound to narrow Rust capabilities. A policy declaration is prompt-facing data,
not authority: it cannot open files, inspect the environment, access a socket
or database, run a process, or choose another assignment. The host never reads
mutable application source at actor runtime.

Runtime replacement is explicit. Deployment changes are made with the daemon
stopped, then begin from a fresh database and runtime root; prior runtime state
is not parsed, migrated, recovered, or dual-dispatched. The provider-free
`tea-acceptance` gate proves this Rust runtime, the V2 bundle,
packet and policy boundaries, terminal evidence, and the full candidate-to-
delivery path before paid work is separately authorized.

The XSH application declares only data: exact repository pin, templates,
allowed tools, model descriptors, required reads, reproducer/validation
profiles, and commit policy. It is compiled to an inert bundle and admitted by
the kernel; no application code is evaluated in the daemon.

Actors exercise qualitative judgement inside one assigned worktree. Their
prose, tool use, and Forum contributions are evidence, not authority. The
Architect alone supplies the two human decisions—ticket sponsorship and final
candidate disposition—but cannot waive deterministic or cost failures.

## State categories and durable vocabulary

Factory keeps three kinds of state distinct. World and evidence facts record
what happened to a repository or execution and the immutable bytes that prove
it: snapshots, artifacts, candidate trees, validations, deliveries,
experiment runs, and evaluator receipts. Institutional facts record what
Factory is responsible for, investigating, claiming, or deciding: `Project`,
`RFC`, `Ticket`, `Experiment`, `Claim`, `Decision`, `Office`, and anchored
`Publication`. Runtime facts are bounded materializations or computations that
may disappear after their durable outputs are sealed: worktrees, assignments,
sessions, process groups, and harness compilation invocations.

No durable record may blur these categories to avoid an explicit link. In
particular, an experiment is an institutional question and plan, while its
experiment run is world/evidence data for one exact execution. A session is a
runtime fact and never an office, durable agent identity, or authority source.

The initial institutional model is intentionally closed and ordinary SQL.
`RFC` and `Ticket` have immutable content revisions; anchored publications
will use immutable publication revisions. Other initial nouns have aggregate
revisions for lifecycle/link concurrency while their body or charter artifacts
remain immutable; a new claim or decision is a new object. The authority,
revision, edge, and Forum compatibility contract is recorded in
[ADR-001: institutional records and state categories](ADR-001-institutional-records.md).

This model does not introduce universal logical worlds, dynamic organization,
generic metadata maps, an agent social graph, or personal reputation. The
current one-repository, one-daemon, one-paid-actor, one-delivery XSH lane and
its kernel custody, cost, validation, clean-checkout, and local-fast-forward
guards remain unchanged.

## Forum compatibility boundary

Forum conversation remains useful as durable, shared, non-authoritative
discussion and structured disagreement. The existing `forum_topics`,
`forum_threads`, and `forum_posts` relations are a legacy discussion
projection. `forum://` can remain a user-interface route for compatibility, but
new institutional work must be anchored to one typed object path such as
`project://P17`, `rfc://R42/revision/3`, or `experiment://E14`. A Forum row
cannot create a ticket, certify validation, grant authority, or change a
decision. Existing rows are not silently reinterpreted as institutional
anchors; migration or read-only retention is a separate, explicit decision.

## Compiled harness and replay

`HarnessSpecV2` is the explicit, reproducible input to one actor invocation.
It names the application revision, durable office, assignment role, bounded
objective, admitted capabilities, remaining campaign allowance, compiler
version, and an ordered set of `ContextItemV2` references. A context item is a
typed institutional ID or sealed artifact plus one closed inclusion class
(`DirectTarget`, `RequiredConstraint`, `DirectEvidence`, or
`CurrentDecision`) and a bounded selection reason; it is never raw prompt
text or a generic `kind + id` pair.

The kernel resolves those references, renders the already-admitted templates,
and seals the canonical spec, prompt artifacts, packet, and packet digest in a
`HarnessCompilationV2` receipt. Initial selection is deterministic and
limited to the direct target, required constraints, direct evidence, and
current decision links. The actor receives only the resulting packet and
listed evidence. Harness compilation is materialized runtime state with
sealed replay outputs, not agent memory or a retrieval/plugin escape hatch.

## Boundary value discipline

Every externally visible value is one of four kinds: a command carrying caller
intent and expected revision; a durable fact whose identity and links the
kernel verified; admitted policy; or a read-only projection. Commands do not
assert kernel-derived facts, policy does not carry executable callbacks or
authority, and projections do not become a second source of truth. New
relations therefore require a typed ID/domain value, wire conversion when
needed, authorization, migration constraints, navigation/search behavior, and
focused contract tests in the same change. Prefer narrow IDs plus kernel
resolvers over broad context aggregates or generic maps.

## Worktree and delivery custody

The kernel qualifies the clean local XSH default head and materializes owned
worktrees. It captures the engineering tree and portable patch itself, runs
the required reproducer plus full suite on a fresh exact worktree, constructs
the commit only after validated evidence is available, and fast-forwards only
a clean unchanged local default branch. It never pushes.

The same generic Git custody contract applies to every future repository lane,
including `factory-engineer`: candidate commits use the complete
`CommitProvenance` trailer set, and the final delivered commit receives the
kernel-created `Factory-Cost` trailer. XSH-specific vocabulary may shape an
application packet, but it cannot create a weaker or different provenance
format for Factory work.

Assignment and validation worktrees are transient. Exact owned worktree
removal is verified rather than using broad pruning; durable output is the CAS
evidence, candidate tree/commit identity, and portable patch. CAS retention is
not yet collected—see the [V1 backlog](../V1.md).

## Design constraints

- One daemon and one paid actor at a time in the current MVP.
- Closed typed protocol values; no metadata maps, plugin callbacks, or generic
  workflow language.
- Four Rust crates, no ORM or internal event bus.
- Local Unix socket for operator control and inherited connected descriptors
  for actor RPC; no HTTP control plane.
- Same-user cooperation, not adversarial isolation.

See [control-plane lifecycle](CONTROL-PLANE.md) and
[evidence custody](EVIDENCE.md) for the operational consequences. See the
[architecture glossary](architecture-glossary.md) for the stable terms used by
the protocol and kernel.
