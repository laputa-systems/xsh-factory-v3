# ADR-001: Institutional records and state categories

- Status: accepted contract for the tightening work
- Scope: Factory V3 kernel, protocol, database, and application boundary
- Phase: documentation only; no schema or runtime behavior is changed by this ADR

## Decision

Factory V3 distinguishes three categories of durable or observable state:

| Category | Contract | Initial examples |
| --- | --- | --- |
| World and evidence facts | Kernel-captured facts about a repository or execution, plus immutable bytes that prove them | repository snapshot, artifact, candidate tree, validation, delivery, `ExperimentRun`, evaluator receipt |
| Institutional facts | Durable records of Factory responsibility, investigation, claims, and decisions | `Project`, `RFC`, `Ticket`, `Experiment`, `Claim`, `Decision`, `Office`, `Publication` |
| Runtime facts | Bounded materializations or computations whose durable outputs are sealed separately | worktree, assignment, session, process group, harness compilation |

An explicit typed relation or custody record crosses categories. A runtime
fact never becomes an institutional identity, and actor prose never asserts a
world/evidence fact. The existing XSH custody boundary remains authoritative.

## Initial institutional nouns and revisions

The initial vocabulary is closed. Each noun has its own typed identity,
application scope, lifecycle, creation time, and bounded searchable fields.
Long bodies and narratives are sealed artifacts referenced by the row. No noun
uses a JSONB metadata bag or an application-defined workflow payload.

| Noun | Meaning | Content revision model |
| --- | --- | --- |
| `Project` | Bounded institutional area of responsibility | No content revision initially; lifecycle/link changes use an aggregate revision. |
| `RFC` | Architecture or policy proposal | Immutable `RFCRevision` rows; current revision is a constrained pointer. |
| `Ticket` | Reproducible bounded delivery contract | Existing immutable `TicketRevision` rows and sponsorship lifecycle remain authoritative. |
| `Experiment` | Bounded question, intended base/target, budget, and evaluation plan | No content revision initially; the plan artifact is immutable and a materially different question is a new experiment. |
| `ExperimentRun` | One exact execution of an experiment against a captured base and evaluation plan | Append-only world/evidence fact; no mutable revision. |
| `Claim` | A bounded proposition to support or challenge | Claim body is immutable; a changed proposition is a new claim. |
| `Decision` | An authoritative disposition or decision record | Append-only; a changed disposition is a new decision that supersedes the prior one. |
| `Office` | Durable institutional charter, jurisdiction, authority, budget, parent, and lifecycle | Charter artifact is immutable initially; lifecycle/link changes use an aggregate revision. It is distinct from an assignment role and session. |
| `Publication` | Anchored durable discourse such as finding, question, challenge, or correction | Immutable `PublicationRevision`/supersession model when introduced; current Forum rows are not publications. |

Aggregate revisions provide optimistic concurrency for mutable lifecycle and
links. They do not permit historical body overwrite. Revision-bearing objects
always preserve prior immutable content.

## Creation and change authority

The kernel validates and persists every institutional mutation. Application
bundles declare bounded policy, but cannot execute callbacks, write SQL/CAS,
or create authority by themselves. Initial authority is:

| Noun | Creation | Change |
| --- | --- | --- |
| `Project` | Kernel-authorized operator or Grand Architect | Kernel-authorized operator or Grand Architect, subject to aggregate revision and application scope |
| `RFC` / `RFCRevision` | Kernel-authorized office proposal or operator/Grand Architect admission | New immutable revision through the kernel; lifecycle changes require the owning authority and expected aggregate revision |
| `Ticket` / `TicketRevision` | Product office proposal through the existing kernel admission path | Revision and lifecycle transitions retain the existing Product → Architect sponsorship contract |
| `Experiment` | Kernel-authorized owning office or Grand Architect | Owning authority through kernel commands and expected aggregate revision; run evidence is never edited |
| `ExperimentRun` | Kernel runtime/evidence custody for one admitted experiment invocation | Append-only kernel evidence capture; actors cannot rewrite the run or attach unverified results |
| `Claim` | Kernel-authorized owning office or Grand Architect | Immutable proposition; challenge or correction is a new claim/publication, not an overwrite |
| `Decision` | Grand Architect or other explicitly admitted decision authority | Immutable record; a later authoritative decision supersedes it through the kernel |
| `Office` | Kernel-seeded fixed root office for the admitted application | Kernel-authorized office authority may change lifecycle/links under aggregate revision; dynamic office creation and delegation are out of scope |
| `Publication` | Kernel-authorized office publication anchored to an institutional object | Immutable revision/supersession through the kernel; session identity is provenance only |

“Kernel-authorized” means the kernel checks principal, office, application
scope, lifecycle, expected revision, and all relevant evidence/cost guards.
It does not mean that a process, actor, application, or Forum post can bypass
those checks. The current paid-cycle guarantees—one bounded campaign, exact
application/repository pin, measured cost, deterministic validation, clean
checkout, local fast-forward, and exactly one delivery target—remain in force.

## Initial edge policy

The institutional graph begins with a small closed set. An implementation may
use concrete foreign-key tables or an `institutional_objects` identity
directory plus constrained edges, but it must reject dangling IDs, illegal
source/target kinds, cross-application links, and cycles where a parent
relation is introduced. Semantic fields remain in their typed noun tables.

The exact initial edge kinds are:

| Edge | Legal source → target | Purpose |
| --- | --- | --- |
| `ProjectContains` | `Project` → `RFC`, `Ticket`, `Experiment`, `Office` | Project scope and navigation |
| `RFCRevisionMotivates` | `RFCRevision` → `Ticket`, `Experiment`, `Decision` | Proposal-to-work/decision trace |
| `ExperimentTests` | `Experiment` → `Claim`, `RFCRevision` | Question target |
| `ExperimentRunProduces` | `ExperimentRun` → candidate tree, artifact, evaluation receipt | Exact evidence outputs |
| `ClaimSupportedBy` | `Claim` → `ExperimentRun` or sealed evidence artifact | Positive support |
| `ClaimChallengedBy` | `Claim` → `ExperimentRun` or sealed evidence artifact | Counterevidence |
| `DecisionDecides` | `Decision` → `RFCRevision`, `TicketRevision`, `Experiment` | Authoritative disposition |
| `OfficeOwns` | `Office` → `Project`, `RFC`, `Ticket`, `Experiment` | Institutional responsibility |

These are the only polymorphic institutional edges in the initial contract.
Existing custody relations remain concrete and authoritative; the graph does
not duplicate candidate, validation, review, delivery, campaign, or runtime
fields merely to make them navigable. New edge kinds require a named query,
harness selection rule, or authority transition and an amended ADR.

## Mechanism and policy boundary

The kernel supplies mechanism:

- typed IDs, SQL relations, immutable artifacts, revisions, and edge integrity;
- authentication/authorization, application scope, lifecycle transitions, and
  optimistic-concurrency checks;
- repository, worktree, process, validation, cost, CAS, and delivery custody;
- bounded search/navigation projections and replayable evidence.

An admitted application revision supplies policy:

- repository binding and path/tool/template/model configuration;
- office profiles and assignment-role capabilities;
- ticket limits, evaluation plans, required reads, validation commands, and
  commit policy;
- allowed publication kinds and bounded application-specific selection rules.

Application policy cannot create new kernel nouns, edges, permissions,
authority transitions, arbitrary metadata, SQL/worktree access, or executable
callbacks. Actors receive a bounded packet and produce evidence; they cannot
select unlisted capabilities or context, create durable authority, or waive
cost and validation guards.

## Forum compatibility and migration

Forum conversation remains useful as shared, permanent, non-authoritative
discussion. Existing `forum_topics`, `forum_threads`, and `forum_posts` are a
legacy discussion projection, not institutional objects. The existing read
routes remain valid during this tightening work. `forum://` may remain as a UI
route, but new institutional publications must resolve to exactly one typed
anchor, for example `project://P17`, `rfc://R42/revision/3`,
`ticket://T9/revision/2`, or `experiment://E14`.

Old rows are not silently inferred to be projects, RFCs, tickets, experiments,
or offices. Before any write migration, a later decision must choose one of:

1. retain old rows as read-only legacy discussion;
2. explicitly map selected rows to new anchored publications with provenance;
3. start from a clean pre-release institutional schema while preserving a
   documented export/read path.

Until that decision is made, no new code may add unanchored institutional
meaning to Forum topics, threads, or posts. Forum prose remains evidence and
structured disagreement; it cannot create tickets, certify validation, grant
authority, or change a decision.

## Non-goals

This ADR does not introduce universal logical worlds, `smolworld`, parallel
paid actors, dynamic organizations, automated delegation, agent social graphs,
personal reputation, semantic retrieval, embeddings, remote workers, or a
generic workflow engine. Those are separate proposals and must not be smuggled
into the initial institutional model.
