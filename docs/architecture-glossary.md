# Architecture glossary

## Kernel

The installed Rust authority. It alone admits durable state, owns PostgreSQL
and CAS, manages processes and worktrees, validates trees, constructs commits,
and performs guarded local delivery.

## Application bundle and revision

`ApplicationBundleV2` is immutable, closed policy: repository binding,
templates, tools, model profile, required reads, ticket limits, reproducer,
validation commands, path policy, and commit-message policy. Registration
adopts one bundle and its named source artifacts into CAS as an application
revision. A campaign pins exactly one revision.

## Aggregate revision

A monotonically increasing optimistic-concurrency revision. Every mutating
command supplies the revision it observed; a mismatch is a conflict, never an
implicit overwrite.

## Artifact and CAS

An artifact is bounded bytes sealed by the kernel and addressed by BLAKE3.
PostgreSQL stores immutable identity and domain relations; CAS stores bytes.
The relation, not an actor-supplied label, gives the artifact its durable role.

## Assignment and session

An assignment is one immutable packet for one exact assignment role and task.
A session is one fresh kernel-custodied actor process for that assignment. A
durable `Office` is an institutional record; it is not the same thing as the
closed assignment-role value currently used to shape a packet. Neither an
assignment nor a session is a reusable identity or a source of authority. A
session is only execution provenance for work attributed to its office.

## State categories

Every durable datum belongs to exactly one state category. Cross-category
meaning is represented by an explicit typed link or custody relation.

### World and evidence fact

A world/evidence fact records what happened to a repository or execution, or
identifies immutable bytes that prove it. Repository snapshots, candidate
trees, validations, delivery records, artifacts, experiment runs, and
evaluator receipts are world/evidence facts. These facts are kernel-captured;
an actor's prose cannot assert one into existence.

### Institutional fact

An institutional fact records what Factory is responsible for, investigating,
claiming, or deciding. The initial vocabulary is `Project`, `RFC`, `Ticket`,
`Experiment`, `Claim`, `Decision`, `Office`, and anchored `Publication`.
Institutional facts have typed identity, application scope, lifecycle, and
searchable bounded fields. Their relationships are explicit and constrained;
they are not a generic metadata graph or workflow language.

### Runtime fact

A runtime fact is a bounded materialization or computation that may disappear
after its durable outputs are sealed. Worktrees, assignments, sessions,
process groups, and harness compilation invocations are runtime facts. Runtime
facts do not become institutional identity merely because an actor interacted
with them.

## Institutional object and revision

An institutional object is one of the closed institutional nouns named in the
architecture decision record. Its semantic fields remain in a typed relation;
an optional object directory supplies a single identity only for the closed
typed-edge index. It is never a JSONB or string-map escape hatch.

`RFCRevision`, `TicketRevision`, and (when anchored publications are
introduced) `PublicationRevision` are immutable content revisions. Other
initial institutional nouns use aggregate revisions for lifecycle and link
updates; their body or charter artifacts are immutable, and a new object is
required when a new claim or decision is needed. A revision mismatch is a
conflict, never an implicit overwrite.

## Required read

An exact repository path and BLAKE3 digest that must be read through the
wrapped tool. Shell output, a prompt quotation, or an actor assertion does not
satisfy it.

## Ticket, checkpoint, candidate, and review

Product proposes a reproducible behavior-defect ticket. Engineering creates a
kernel-captured pre-fix regression checkpoint, then a candidate is the exact
changed tree the kernel captures and hard-validates. Quality receives a fresh
materialization and supplies independent qualitative review; it cannot replace
deterministic validation. The Architect sponsors a ticket and decides delivery,
rework, or rejection.

## Campaign

A bounded spend and time envelope pinned to one kernel build, application
revision, repository snapshot, aggregate cost cap, and delivery target. Ticket
inventory and Forum history outlive campaigns.

## Compact audit transcript

The Rust `pi-agent-core-rs` host projects its event stream before it hits disk.
The projection retains bounded assistant text and tool diagnostics while
discarding interactive session snapshots, forks, and thinking blocks. Its gzip
archive is one session artifact, not a PostgreSQL event log.

## Forum

Permanent, shared, non-authoritative discussion. A Forum post cannot grant
authority, create a ticket, certify validation, or make a delivery decision.
New institutional publications are anchored to one typed institutional object
(`project://…`, `rfc://…`, `ticket://…`, `experiment://…`, `claim://…`,
`decision://…`, or `office://…`). The current `forum_topics`,
`forum_threads`, and `forum_posts` relations remain a legacy discussion
projection and are not a source of institutional identity or authority.
Existing Forum reads remain compatible until an explicit migration decision;
new code must not add unanchored institutional meaning to those rows.
