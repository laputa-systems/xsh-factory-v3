# Tightening Factory V3

## Purpose

Factory V3 already has the right custody boundary for its current product:
the kernel owns durable facts, evidence, validation, Git delivery, and cost;
application code and actors do not. This plan tightens the *institutional data
model* and the *assignment boundary* around that foundation.

The desired result is not an agent society, a generic workflow engine, or a
universal world runtime. It is a small, inspectable control plane in which:

```text
durable institutional records + durable evidence/world facts
                         │
                         ▼
              compiled, bounded harness
                         │
                         ▼
              fungible actor invocation
                         │
                         ▼
              evidence and typed publications
```

The present XSH lane remains a one-repository, one-daemon, one-paid-actor,
one-delivery campaign system. Git trees, portable patches, application
revisions, and sealed CAS evidence remain the only supported world codec.
This work must not weaken the existing custody, cost, validation, clean
checkout, or local-fast-forward guards.

## Decisions this plan makes

### Three kinds of state

Every new durable datum must belong to exactly one of these categories. Do not
make a record whose meaning spans categories merely to avoid an explicit link.

| Category | Meaning | Current examples | New examples |
| --- | --- | --- | --- |
| World and evidence facts | What happened to a repository or an execution, and the immutable bytes that prove it. | artifact, repository snapshot, ticket revision, candidate tree, validation, delivery | experiment run, evaluator receipt |
| Institutional facts | What Factory is responsible for, investigating, claiming, or deciding. | campaign, Architect decision | project, RFC, experiment, claim, durable office |
| Runtime facts | A bounded materialization or computation that may disappear after its durable outputs are sealed. | worktree, assignment, session, process group | harness compilation invocation |

An `Experiment` is an institutional object that asks a bounded question. An
`ExperimentRun` is world/evidence data that records an actual execution against
one exact base tree and evaluation plan. A `Ticket` remains a bounded delivery
contract. It must not become the catch-all representation for exploratory
research, architecture proposals, or discussion.

### Durable offices; fungible occupants

The existing `factory_protocol::Office` enum (`ProductResearch`, `Engineering`,
`Quality`) denotes the shape and privileges of an assignment packet. It is an
**assignment role**, not a durable office. The distinction is a contract change
and must be made explicit.

- `AssignmentRole` is a closed protocol enum that controls packet shape and
  actor capabilities.
- `Office` is a durable institutional record with a charter, jurisdiction,
  authority, budget, parent, subscriptions, and lifecycle.
- `Assignment` and `Session` are runtime records. They are one temporary
  occupant invocation of an office under an assignment role; they carry no
  enduring social identity or authority.

V3 begins with a small fixed set of root offices. It does not need dynamic
hierarchy creation or automated office creation to make this distinction
useful. Do not add agent reputation. If calibration is later needed, attach it
to a durable model configuration, procedure, evaluator, office, or harness
version, never to a session identity.

### Institutional graph, not generic metadata

The durable institutional vocabulary is closed and ordinary SQL, not JSONB,
an EAV table, application-defined metadata, or a user-programmable workflow
language. The initial nouns are:

```text
Project
RFC + RFC revision
Ticket + ticket revision
Experiment + experiment run
Claim
Decision
Office
Publication
```

Each noun has its own table, typed Rust ID, lifecycle, ownership/application
scope, creation time, immutable body/evidence references, and explicit
searchable text fields. Revision-bearing nouns use immutable revision rows;
current state is a constrained pointer or a small lifecycle projection, never
an overwritten historical body.

Introduce an `institutional_objects` directory only if it is needed to give
all nouns one foreign-keyable public identity for a strictly closed
`institutional_edges` relation. It must contain only identity, object kind,
and creation facts. Semantic fields remain in their typed noun tables. Edge
kinds are a closed Rust/SQL enum and must be validated for legal
source-kind/target-kind pairs. This is a graph index, not a place to hide
semantics.

At minimum, model these links explicitly:

```text
Project       --contains--> RFC / Ticket / Experiment / Office
RFC revision  --motivates--> Ticket / Experiment / Decision
Experiment    --tests--> Claim or RFC revision
ExperimentRun --produces--> candidate tree / artifact / evaluation receipt
Claim         --supported_by|challenged_by--> evidence or ExperimentRun
Decision      --decides--> RFC revision / Ticket revision / Experiment
Office        --owns--> Project / RFC / Ticket / Experiment
```

The exact first edge set must be small. Add an edge only when an application
query, harness selection rule, or authority transition needs it.

### Publication replaces a social Forum primitive

Forum conversation remains useful as durable discourse and structured
disagreement. It is not a social network, task system, or authority system.

All new publications must have one typed institutional anchor. The primary
stable paths become, for example:

```text
project://P17
rfc://R42/revision/3
ticket://T9/revision/2
experiment://E14
claim://C71
decision://D8
office://O3
```

`forum://` may remain as a user-interface path, but it resolves through an
anchored `Publication`, not through an unowned topic. A publication can have
an immutable body artifact, authoring office, originating session for
provenance, typed kind (`Finding`, `Question`, `Challenge`, `Correction`,
etc.), attached artifacts, reply/supersession links, and search projection.
The office is the institutional speaker; the session is diagnostic provenance.

Existing `forum_topics`, `forum_threads`, and `forum_posts` are a legacy
discussion projection. Do not expand their unanchored semantics. Preserve
their read path until an intentional migration and compatibility decision has
been made.

### A harness is a compiled artifact

Today the kernel renders sealed templates and emits a sealed assignment packet.
That is the beginning of harness compilation. Make the operation explicit and
fully reproducible:

```text
HarnessSpec {
  office,
  assignment_role,
  objective,
  world/evidence target,
  selected ContextItem references,
  capabilities,
  resource budget,
  application revision,
  compiler version
}
    -> sealed system prompt + assignment prompt + packet
```

A `ContextItem` is not prompt text. It is a typed reference to a durable
object or sealed artifact, plus a bounded selection reason and inclusion
class. The harness compiler resolves the references, renders the already
admitted templates, seals its inputs/outputs, and records their exact digest.
An actor receives only the resulting packet and access to the listed evidence.

Do not modify Pi for this work. Do not give applications callbacks or a
templating escape hatch. Do not reintroduce broad agent memory. The initial
selection policy can be a deterministic, explicit list constructed by the
kernel from the target and required links; sophisticated retrieval is deferred.

### Boundary discipline

Every API value must be one of four things:

| Value | May contain | Must not contain |
| --- | --- | --- |
| Command | caller intent, expected revision, IDs the caller is allowed to name | kernel-derived facts or an asserted lifecycle result |
| Durable fact | kernel-verified identity, links, lifecycle, sealed artifact references | mutable prompt assembly or untrusted prose as authority |
| Policy | admitted, immutable application configuration | executable callbacks, arbitrary maps, or SQL/worktree authority |
| Projection | read-only display/search data | a second authoritative contract |

The Rust protocol crate remains the semantic owner of closed values and
invariants. Wire and Deno SDK shapes are transport adapters with parity tests;
they do not independently define domain meaning. A new database relation
requires its typed ID, domain type, wire conversion if externally visible,
authorization rule, migration constraints, navigation/search projection, and
focused contract tests in the same change.

Large structs are acceptable only when they describe one immutable boundary
object. Do not pass a broad `*Context` or `*Evidence` aggregate merely because
several callers currently need overlapping fields. Prefer a typed ID and a
kernel resolver. Conversely, do not replace explicit semantic fields with a
generic string map to make a struct smaller.

## Ordered implementation plan

Each phase is separately reviewable and must leave `make provider-free-acceptance`
green before the next phase begins. Use focused tests while changing one
boundary. Do not run pre-commit hooks or alter `../xsh` directly.

### Phase 0 — Record the contract and freeze accidental expansion

1. Add the durable vocabulary and the three-state classification to
   `docs/architecture-glossary.md` and `docs/ARCHITECTURE.md`.
2. Mark the present Forum as a non-authoritative legacy discussion projection;
   document that new institutional work must not add unanchored Forum meaning.
3. Add an architecture decision record or equivalent design document defining:

   - the initial noun list and which nouns have revisions;
   - authority to create/change each noun;
   - the exact initial edge kinds;
   - what is kernel mechanism versus admitted application policy;
   - the compatibility and migration approach for current Forum rows.

4. Add no schema or runtime behavior in this phase. The result is a reviewed
   contract that later coding agents can search before extending the model.

Acceptance evidence: documentation names one spelling for each term, has no
claim that a session is an office, and explicitly rejects universal worlds,
dynamic organization, generic metadata, and social reputation.

### Phase 1 — Split assignment role from durable office

1. Rename the fixed protocol `Office` enum to `AssignmentRole` across Rust,
   Deno, SQL discriminants, application bundles, packet validation, tests, and
   documentation. This is a semantic rename, not a behavior change.
2. Add `OfficeId` and a durable `factory.offices` relation. Its first version
   includes application scope, optional parent office, charter artifact,
   closed authority mask, budget ceiling where applicable, lifecycle, and
   aggregate revision. Enforce parent scope and acyclicity.
3. Bind every new assignment to exactly one durable office and exactly one
   assignment role. The kernel derives both from durable state; actors never
   supply them.
4. Keep the current Product, Engineering, and Quality authorization behavior
   unchanged. Seed only the fixed root offices required by the active
   application revision; do not add dynamic delegation tools.
5. Attribute later publications to `OfficeId` plus optional `SessionId`.

Acceptance evidence: an assignment/session can be replaced without changing
the office record; a session cannot attribute work to another office; current
Product → Engineering → Quality lifecycle tests still prove the same delivery
guards.

### Phase 2 — Add the minimal institutional objects and references

1. Introduce typed IDs and closed protocol domain values for `Project`, `RFC`,
   `RFCRevision`, `Experiment`, `ExperimentRun`, `Claim`, `Decision`, and
   `Publication`. Add a typed object reference enum only at boundaries that
   genuinely need polymorphic navigation.
2. Add tables and constraints one noun at a time. Every body or long narrative
   is a sealed artifact; PostgreSQL stores identity, bounded summary/search
   fields, ownership, lifecycle, revision, and links.
3. Make `Ticket` linkable to a project/RFC without changing its current
   reproducible-defect admission contract. A ticket may have no RFC initially.
4. Add `Experiment` and `ExperimentRun` before adding any experiment scheduler.
   An experiment records its question, owner, intended base/target, bounded
   budget, and evaluation plan. A run records the exact base tree, runtime
   invocation, resulting candidate/patch/artifacts, and evaluator receipt.
5. Keep candidate, validation, review, and delivery as their current precise
   custody records. Link them to experiment runs; do not duplicate their
   fields into new tables.
6. Implement reference integrity in SQL. Either use concrete foreign-key edge
   tables or a constrained object directory plus trigger validation. Never
   accept dangling `kind + id` pairs.
7. Add full-text search and read-only navigation for each noun. Search must be
   bounded and database-side, following the existing Forum paging discipline.

Acceptance evidence: a user can create and find an RFC, connect it to an
experiment, connect a resulting run to immutable evidence, and navigate from a
ticket/candidate/decision back to that chain. Invalid cross-application,
dangling, cyclic-parent, and illegal-edge writes are rejected by focused
database tests.

### Phase 3 — Introduce anchored publications; retire new unanchored Forum writes

1. Add a `Publication` relation and immutable `PublicationRevision` or
   supersession model, anchored to exactly one institutional object.
2. Move semantic post kinds, attachments, search, reply, and correction links
   behind the publication contract. Preserve the current bounded payload and
   artifact rules.
3. Add operator and actor read/write APIs that receive an anchor typed ID,
   never a free-form topic ID. Authorization derives the authoring office from
   the connection/assignment.
4. Provide projections for object discussion, office inbox, decision log, and
   search. These are views over publications and typed records, not sources of
   authority.
5. Decide separately whether to migrate existing Forum data, expose it as
   read-only legacy data, or start a clean pre-release schema. Do not silently
   reinterpret old topics as anchors.

Acceptance evidence: every new finding/question/challenge is discoverable from
its project/RFC/ticket/experiment/claim/decision path; actor prose cannot
create a ticket, certify validation, or change a decision; existing Forum
read tests remain valid until deliberately replaced.

### Phase 4 — Make harness compilation explicit

1. Add `HarnessSpec`, `ContextItem`, `HarnessCompilation`, and the minimal
   closed context inclusion classes to `factory-protocol`. Reuse existing
   application templates and `AssignmentPacketV1`; do not redesign model,
   cost, process, or Git custody at the same time.
2. Replace ad-hoc assembly of assignment target/evidence/prompt substitutions
   in `assignment_runtime` and `durable_authority` with one compiler entry
   point. It receives only durable IDs and policy; it resolves facts itself.
3. Seal and persist the spec identity, selected object/artifact references,
   compiler version, rendered prompt artifacts, and resulting packet digest.
4. Start with deterministic selection rules: required reads, direct target,
   direct evidence, current decision/RFC/ticket links, and explicitly mandated
   constraints. Do not add embedding search, ranking, or autonomous context
   selection in this phase.
5. Add replay tests showing the same spec and admitted application revision
   produce identical context references, prompts, and packet identity.

Acceptance evidence: an operator can inspect why each context item was
included; a later actor invocation can be recreated from durable references;
template rendering remains closed and an actor cannot add unlisted context or
capabilities.

### Phase 5 — Reduce boundary duplication and strengthen contract tests

1. Audit `factory-protocol`, `wire.rs`, the Deno SDK, and kernel resolver
   inputs. For each type, label it Command, Durable Fact, Policy, or
   Projection. Split mixed types before adding new fields.
2. Replace repeated field collections with IDs plus resolver methods where the
   kernel can recover the fact under its authority. Retain complete sealed
   packet evidence where replay requires it.
3. Keep protocol structs closed. New variants or fields must have a named
   lifecycle/authority reason, wire parity test, database constraint, and a
   migration/backward-compatibility decision.
4. Add cross-layer contract tests for every new object and transition:

   - Rust protocol validation;
   - Deno SDK parse/serialization parity;
   - framed wire rejection of unknown/malformed fields;
   - PostgreSQL constraints and idempotency;
   - kernel authorization and replay;
   - bounded search/navigation;
   - an end-to-end provider-free vertical flow.

5. Break up files only along the new domain seams. Do not create a
   repository/service abstraction layer or an internal event bus merely to
   make files shorter.

Acceptance evidence: no boundary type mixes actor assertion with kernel fact;
no new free-form metadata map exists; protocol and SDK agree on all new wire
fixtures; focused tests demonstrate that an invalid reference, authority,
revision, or evidence link cannot be persisted.

## Deferred work

The following are intentionally outside this plan:

- universal logical worlds, multi-channel deltas, VM snapshots, or `smolworld`;
- parallel paid actors, frontier scheduling, long-lived alternate world lines,
  or merge/reconciliation policy;
- automated Grand Architect occupancy or dynamic office delegation;
- agent social graphs, personal reputation, markets, or social ranking;
- semantic retrieval/ranking, embeddings, or a general information router;
- remote workers, new sandboxing claims, or changes to same-user trust;
- direct edits, commits, worktree manipulation, or remote operations in
  `../xsh`.

Those are separate proposals. They may build on the tightened model only after
the current one-delivery XSH lane remains demonstrably correct.

## Required implementation practice

Before any phase, read the owning architecture, control-plane, evidence,
testing, trust, and repository-boundary documents, plus the present callers,
schema, protocol tests, Deno fixtures, and database tests for the boundary
being changed. Start with a failing focused test for a new invariant, then
make the smallest implementation that passes it.

Use `apply_patch` for edits. Do not add dependencies without user approval.
Do not run pre-commit hooks, push, or mutate `../xsh` directly. Run focused
Rust/Deno/database checks while developing, then the narrowest appropriate
broader qualification. Report both the checks run and the checks not run.

## Completion condition

This plan is complete only when the current paid-cycle invariants still hold
and a future actor can be given a sealed, reproducible harness that explains
its bounded objective through typed references to durable institutional
records and kernel-captured evidence. The system should be easier to query and
reason about because it has fewer ambiguous concepts—not because it has built
more machinery.
