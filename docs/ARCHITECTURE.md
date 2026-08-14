# Architecture

Factory V3 is a deliberately small generic control plane for improving a local
product repository. Its first application is XSH, but XSH vocabulary and
policy belong only in `applications/xsh` and its templates.

```text
application source -> sealed application revision -> assignment packet
                                                       |
Pi headless host -> local framed descriptor -> Rust kernel -> PostgreSQL + CAS
                                                       |
                                                isolated ../xsh worktrees
                                                       |
                                             validated commit + local delivery
```

## Authority split

Rust owns facts whose failure would make accepted work false, unsafe,
unaffordable, or irreproducible: state transitions, process custody, artifact
identity, repository qualification, worktree/tree capture, validation, commit
construction, and delivery. PostgreSQL holds mutable lifecycle and audit
relations; CAS holds immutable bytes.

The Deno SDK validates closed client shapes. `factory-pi-host` adapts the
sealed assignment packet to a locally built Pi headless runtime and the framed
kernel descriptor. It has no independent lifecycle authority.

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
not yet collected—see `PLAN.md`.

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
