# XSH Factory V3 implementation plan

## 1. Decision and purpose

Factory V3 is a cleanroom software company whose first application is improving
XSH. Its product objective is concrete:

> Reliably discover, implement, independently review, and locally deliver
> high-quality XSH commits at useful throughput and bounded provider cost.

V3 is not a port of Factory V1 and does not import Factory V1 state. It is not
the Society research program implemented by Factory V2 and does not import
Factory V2 state or code. V1 and V2 are design evidence only.

The redesign keeps the parts of each predecessor that directly support the
product objective:

- From V1: a comprehensible company, explicit offices, an evidence-qualified
  ticket buffer, exact assignments, isolated product worktrees, independent
  review, hard aggregate budgets, guarded Git delivery, and commit provenance.
- From V2: a Rust/PostgreSQL authority, typed transactional commands,
  append-only artifact custody, a narrow Pi coding-agent SDK host, exact
  process ownership, immutable session evidence, and a permanent Forum.
- New in V3: a TypeScript application SDK that keeps XSH-specific mission,
  prompts, repository policy, task discovery, and validation outside the
  generic factory kernel.

The factory is a black box with respect to its product. The trusted Rust kernel
must not contain the word `XSH` in a domain type, transition, database table,
or scheduler decision. `applications/xsh` is a one-way consumer of the generic
SDK. The product checkout at `../xsh` contains no factory workflow state and is
never used as the factory database or artifact store.

### 1.1 MVP acceptance gate

The MVP is accepted by one bounded campaign that:

1. starts from a clean V3 PostgreSQL database with no imported V1/V2 tickets,
   Forum posts, handbook, runs, or decisions;
2. exercises a real generation transition from the bootstrap generation to the
   candidate generation, including a schema migration, clone qualification,
   quiescent live migration, candidate health check, and pre-admission rollback
   rehearsal;
3. uses a Product/Research Pi session to discover at least one previously
   unseeded, user-observable XSH defect and submit an exact reproducible ticket;
4. receives an explicit sponsorship decision from the external Grand
   Architect;
5. uses a fresh Engineering Pi session to checkpoint a regression that fails
   before the fix, implement the root fix, and submit a clean candidate tree;
6. passes the kernel-owned reproducer and the XSH application's full product
   suite on the exact candidate tree;
7. uses a fresh, independent Quality Pi session to inspect the exact candidate
   tree, invoke the full suite again, and submit a review;
8. receives an explicit final decision from the Grand Architect;
9. has the kernel construct one provenance-bearing commit and guardedly
   fast-forward the clean local `../xsh` default branch to that exact commit;
10. never pushes a remote; and
11. reports total factory-launched Pi provider spend at or below 500,000
    micro-USD ($0.50), with a breakdown by office, assignment, model, and
    session.

The cost of the external Grand Architect, commonly an already-paid Codex
session, is out of band and cannot be included in the factory total. Local CPU,
PostgreSQL, storage, builds, and tests are reported as durations and byte counts,
not assigned fictional dollar values.

The bootstrap-to-candidate upgrade used by this gate may be prepared before the
campaign. Future factory changes use the Engineering and Quality circuit in
section 13. The combined gate proves that one campaign can cross a generation
boundary and then perform useful product work; it does not require spending Pi
budget to rediscover the already-prepared bootstrap upgrade.

## 2. Constitutional model and explicit trust assumptions

### 2.1 What is trusted

The currently active Rust generation is the sole authority for:

- durable identity and legal lifecycle transitions;
- PostgreSQL writes and schema migration admission;
- aggregate provider-cost admission and reconciliation;
- application, prompt, model-profile, assignment, and repository-snapshot
  identity;
- spawning, observing, cancelling, and reconciling Pi SDK host processes;
- content-addressed artifact adoption and verification;
- ticket readiness, WIP limits, and exact assignment claims;
- required-read assertions;
- deterministic validation custody;
- candidate Git tree and commit construction;
- guarded local product delivery; and
- generation qualification and activation.

Applications, actors, Forum posts, prompts, reports, and external operators do
not write SQL or manufacture these facts.

### 2.2 Cooperative host boundary

MVP actors run as ordinary host processes under the same OS account as the
operator. They receive normal host networking with no factory restriction and
the common Pi workspace tools, including shell access. There is no container,
VM, seccomp profile, credential broker, private-network filter, or claim that
the system withstands a malicious same-user process.

Therefore V3's safety claim is deliberately limited:

- the kernel prevents accidental or protocol-level authority confusion;
- it proves the exact inputs and outputs that it accepts;
- it detects repository, tree, artifact, and lifecycle mismatches at durable
  boundaries;
- only kernel-owned Git operations can produce or deliver an accepted commit;
- it does **not** claim that a hostile actor process cannot inspect credentials,
  modify unrelated host files, signal another same-user process, or tamper with
  unprotected host state.

This assumption must appear in the root README and operator preflight output.
Adding adversarial isolation is a future architecture change, not an implied
MVP property.

### 2.3 Fully self-authorizing generations

The Grand Architect may authorize changes to TypeScript policy, the Rust
kernel, the database schema, qualification logic, and the activation mechanism
itself. There is no immutable warden and no permanent human activation gate.

V3 can prove a chain of predecessor-authorized generations, exact candidate
bytes, qualification results, activation time, and retained recovery material.
It cannot prove that every future generation will preserve the safeguards in
this document, because a future generation may change the very rules that
evaluate or activate its successor. Documentation and status output must state
this limitation plainly rather than describing the kernel as an immutable
root of trust.

Automatic rollback is guaranteed only until the new generation opens paid or
mutating work admission. After a generation accepts new commands, recovery is
roll-forward. V3 never discards accepted post-activation work to restore an old
database snapshot silently.

## 3. Company and authority

### 3.1 Fixed offices

The first usable company has four named offices:

```text
Grand Architect (external operator/agent)
├── Product & Research Director
├── Engineering Director
└── Quality Director
```

The corporate structure is intentionally familiar and fixed. Office authority
is a Rust contract, not a prompt convention. Prompts, providers, models,
thinking settings, turn limits, wall limits, output limits, Forum guidance, and
application policy are versioned and replaceable. Arbitrary role graphs,
reporting-line mutation, and actor-genome evolution are not part of V3.

Directors execute work themselves in the MVP. Each office occupation is one
fresh Pi SDK session for one exact assignment. Later versions may let a
director delegate to bounded subordinate workers without changing the office's
authority contract, but no subordinate-worker scheduler is implemented for the
MVP.

### 3.2 Grand Architect

The Grand Architect is normally an external agent/operator such as Codex using
`factoryctl` or `@factory/sdk`. The office is not a long-lived Pi process and
its cost is not factory spend.

For a normal product campaign the Architect makes two mandatory typed
decisions:

1. **Sponsor** one or more Product-submitted ticket revisions.
2. **Deliver, rework, or reject** an independently reviewed candidate.

The Architect may override a qualitative Quality rejection only with a bounded
written rationale linked to the rejected review. It cannot override:

- an exceeded or unknown campaign cost;
- a missing required read;
- an unsealed or mismatched artifact;
- a changed product base or candidate tree;
- a failed exact reproducer;
- a failed full product suite;
- an illegal lifecycle transition;
- a dirty delivery checkout;
- a non-fast-forward delivery; or
- a failed generation qualification.

The Architect may request at most one fresh Engineering rework session for a
product candidate in the MVP. The rework receives the sealed prior candidate,
Quality review, and Architect rationale as inputs. It is followed by one fresh
Quality review. A second failure terminates the ticket attempt for that
campaign.

### 3.3 Product & Research Director

The Product Director owns useful ticket supply. A replenishment assignment may
submit up to three ticket proposals and must:

- read the exact pinned product guidance required by the application;
- inspect the current XSH checkout, tests, history, existing V3 ticket buffer,
  and Forum;
- use unrestricted web research when useful;
- avoid seeded or preassigned product tasks in the cleanroom MVP;
- identify a user-observable behavior defect rather than a code-cleanup
  preference;
- supply a deterministic reproducer that fails twice the same way on the
  current clean base;
- state expected and actual behavior, mission value, scope, acceptance
  criteria, likely contract owner, risk, and evidence;
- search the live ticket buffer for duplicates before submission; and
- leave implementation choice to Engineering.

A Forum post or persuasive narrative is not a ticket. Only a typed ticket
proposal accepted by the kernel enters the proposal queue.

### 3.4 Engineering Director

The Engineering Director receives exactly one sponsored ticket and one exact
base snapshot. It may not select another ticket or change ticket semantics. It
must:

- satisfy all required reads through the wrapped read tool;
- work in the assigned disposable product worktree;
- create the smallest test checkpoint expressing the regression before fixing
  it;
- ask the SDK to seal that regression tree;
- show that the declared targeted regression command fails on the regression
  tree;
- implement the root fix and required canonical documentation;
- run useful focused checks while developing;
- leave the final worktree clean except for the intended uncommitted changes;
- submit a candidate tree, concise commit message, test identity, and risks;
  and
- never commit, merge, update a branch, or push.

The kernel, not the Engineering actor, constructs the Git commit.

### 3.5 Quality Director

Quality is independent of Engineering and receives a fresh session and a fresh
workspace materialized from the exact candidate tree. It receives the ticket,
base evidence, regression checkpoint, candidate patch, validation receipts,
and Engineering report. It must:

- satisfy the same product required reads;
- inspect whether the regression captures the general public contract;
- challenge scope, semantics, compatibility, documentation, test quality, and
  unnecessary API surface;
- invoke the application-owned full suite through the exact validation tool;
- record any additional probes it ran;
- state `accept` or `reject` with bounded reasons and remaining risks; and
- post useful cross-task observations to the Forum at its discretion.

Quality has the same full workspace tool set as other actors. Any edits in its
workspace are discarded and cannot change the candidate. The required full
suite runs in a separate kernel-owned pristine validation worktree at the
candidate tree, so Quality cannot accidentally certify its own exploratory
edits.

## 4. Throughput, ticket buffer, and backpressure

### 4.1 Why the buffer exists

Consistent delivery requires qualified work to exist before Engineering is
idle. V3 therefore retains a ticket buffer, but the buffer is metabolism rather
than proof of institutional health. Occupancy changes which department is
scheduled; it never lowers evidence or quality requirements.

The MVP policy is:

| Lane | Low water | Target | Hard maximum | In flight |
| --- | ---: | ---: | ---: | ---: |
| Architect-sponsored ready tickets | 2 | 3 | 5 | n/a |
| Unsponsored Product proposals | n/a | 0 | 3 | n/a |
| Product replenishment assignments | n/a | n/a | 1 | 1 |
| Engineering assignments | n/a | n/a | 1 | 1 |
| Quality assignments | n/a | n/a | 1 | 1 |
| Paid Pi sessions globally | n/a | n/a | 1 | 1 |

These values live in the versioned XSH application policy rather than Rust
constants, but the kernel validates `0 < low_water <= target <= maximum` and
enforces the selected revision exactly.

### 4.2 Ticket states

A stable `TicketId` owns append-only revisions. A ticket revision has one of
these states:

```text
Proposed
  -> Sponsored
  -> Engineering
  -> Candidate
  -> QualityReview
  -> Accepted
  -> Delivered

Proposed | Sponsored | Engineering | Candidate | QualityReview
  -> Rejected | Blocked | Superseded | Resolved
```

`Sponsored` is the ready-buffer state. An Engineering claim removes exactly one
ticket from ready inventory. Terminal states never count toward buffer
occupancy. Rework creates a new candidate attempt beneath the same ticket
revision; it does not clone or repurpose the ticket.

### 4.3 Reproducibility and moving product heads

Sponsorship binds the problem contract and the immutable ticket revision, not
an assumption that the product head will never move. Each ticket revision
records the discovery base and its exact reproducer. Immediately before an
Engineering claim, the kernel snapshots the then-current clean product head and
runs the reproducer twice:

- If both runs reproduce the sponsored failure identically, that current head
  becomes the exact Engineering base.
- If the expected behavior already passes, the ticket becomes `Resolved` with
  the observed commit and replenishment pressure increases.
- If results diverge from the sponsored failure or from each other, the ticket
  becomes `Blocked`; Product or the Architect must create a new revision.
- A worker never decides that an old ticket is "close enough" for a new base.

This pre-claim requalification lets useful queued tickets survive unrelated
product deliveries without silently using stale evidence.

### 4.4 Scheduling and pressure propagation

The one-daemon scheduler applies these rules in order:

1. Never start a paid session while another paid session is active.
2. Never admit paid work when aggregate cost is unknown, the campaign is
   terminal, the wall deadline has passed, or a generation transition is in
   progress.
3. If a sponsored ticket has an accepted candidate awaiting Quality, schedule
   Quality before Product discovery.
4. If the Architect has sponsored a ticket and Engineering is idle, pull the
   oldest sponsored revision after current-head requalification.
5. If projected ready inventory is below target, no Product proposal awaits an
   Architect decision, and the proposal cap is not full, schedule one Product
   replenishment assignment.
6. Do not schedule Product merely because the low-water condition exists when
   unsponsored proposals, blocked decisions, Engineering, or Quality already
   constrain flow.
7. Never invent a proposal, weaken a reproducer, auto-sponsor work, or count a
   Forum post as a ticket to satisfy the target.

The scheduler is deterministic over PostgreSQL state and the pinned
application revision. Pi actors do not poll queues or launch one another.

### 4.5 Constraint reporting

`factoryctl status` computes, without writing, the current constraint and the
conversion counts and durations for:

```text
Product assignment -> reproducible proposal
proposal -> Architect sponsorship
sponsorship -> Engineering candidate
candidate -> hard validation pass
validation -> Quality acceptance
Quality -> Architect decision
decision -> guarded delivery
```

It reports ready-buffer occupancy, age, blocked reasons, time in each state,
cost per delivered commit, failed/reworked attempts, and the current projected
low-water condition. It does not synthesize a scalar health or reward score.

## 5. Architecture and dependency direction

### 5.1 Planes

```text
applications/xsh
  mission, fixed company policy, prompts, model profiles,
  ticket discovery contract, product required reads, full-suite commands
                    |
packages/factory-pi-host + packages/factory-sdk
  Pi coding-agent SDK, common tools, Forum tools, assignment submission,
  framed local protocol client
                    |
factoryd / Rust kernel
  authority, lifecycle, scheduler, Postgres, CAS, process custody,
  budgets, validation, Git, generation activation
                    |
PostgreSQL 18 + append-only filesystem CAS + local Git repositories
```

Dependencies point downward only. The Rust crates must compile and test without
`applications/xsh`. The XSH application may import only the public TypeScript
SDK and protocol types. It may not connect to PostgreSQL, open kernel-owned CAS
paths directly, construct Git commits, or spawn a Pi session outside the
daemon-owned assignment path.

### 5.2 Proposed repository layout

```text
Cargo.toml
Cargo.lock
rust-toolchain.toml
package.json
package-lock.json

crates/
  factory-domain/       identifiers, enums, transitions, invariants
  factory-protocol/     miniserde operation structs and framed protocol
  factory-content/      append-only CAS adoption and verified reads
  factory-git/          qualified worktrees, trees, commits, delivery
  factory-kernel/       SQLx store, scheduler, budgets, process and upgrade logic
  factoryd/             resident daemon executable
  factoryctl/           operator/Grand Architect CLI

packages/
  factory-sdk/          typed TypeScript client and application interfaces
  factory-pi-host/      one-assignment Pi SDK runtime and common/custom tools

applications/
  xsh/                  XSH mission, policies, prompts, result validators

schema/
  migrations/           forward-only SQLx PostgreSQL migrations

tests/
  protocol-fixtures/    Rust/TypeScript golden frames
  integration/          daemon, process, Git, upgrade, and fake-Pi judges

var/                    ignored runtime root: CAS, worktrees, transcripts, backups
```

The seven Rust crates are the maximum initial workspace shape, not an invitation
to split every concept into a crate. New crates require a dependency-boundary
reason. There is no generic application/plugin loader beyond the one typed SDK
contract needed by the XSH application.

## 6. Rust and TypeScript dependency budget

### 6.1 Rust runtime dependencies

Pin exact versions in `Cargo.lock` and use only these direct runtime families:

| Dependency | Features/purpose |
| --- | --- |
| `smol` | Executor, timers, async Unix I/O, and async process integration. No Tokio compatibility layer. |
| `sqlx 0.9.x` | `postgres`, `runtime-smol`, `migrate`, and macros only. No ORM or `Any` driver. |
| `tracing 0.1.x` | Structured in-process diagnostics. Libraries install no subscriber. |
| `tracing-subscriber 0.3.x` | Compact daemon/CLI stderr diagnostics only; no JSON or appender feature unless measured need appears. |
| `blake3 1.x` | Artifact, prompt, policy, transcript, patch, tree-packet, and generation identities. |
| `miniserde 0.1.46+` | Local JSON wire structs only. No untyped `Value` in authoritative commands. |
| `thiserror 2.x` | Closed errors at domain and physical boundaries. |
| `rustix 1.x` | Narrow process/signal/filesystem primitives needed for PGID custody, file identity, and atomic operations. |
| `fastrand 2.5.x` | WyRand operational suffixes and jitter, with default entropy features disabled and explicit insecure seed. |

`fastrand` is never used for authority, secrets, content identity, durable
entity identity, or evidence. One daemon-local `Rng::with_seed` is seeded from
a BLAKE3 mix of generation ID, PID, and startup timestamp. PostgreSQL sequences
own durable numeric identities; BLAKE3 owns content identities.

Test-only dependencies may include `tempfile` if its behavior is isolated to
tests. Before adding any other dependency, document the exact boundary it
replaces, why the standard library or current set is insufficient, and its
transitive runtime impact.

Explicit exclusions:

- no `tokio`, Axum, Hyper, Tower, Actix, or HTTP framework;
- no `async-std`, which is discontinued;
- no Serde in the initial wire path;
- no ORM or dynamic SQL query builder;
- no Git library;
- no UUID or cryptographic RNG crate;
- no generic configuration, metrics, event-sourcing, workflow, actor, or plugin
  framework; and
- no Rust compression dependency solely for Pi transcripts.

### 6.2 TypeScript dependencies

Use Node's built-in `net`, `fs`, `path`, `child_process`, `zlib`, and `node:test`
modules. Pin:

- the exact Pi coding-agent SDK version already qualified by the implementation
  effort, initially `@mariozechner/pi-coding-agent` 0.84.1 unless a pre-code
  compatibility spike proves the package name/version has changed;
- one exact TypeScript compiler version; and
- the matching exact `@types/node` development version.

There is no TypeScript HTTP client/server, validation framework, ORM, logger,
search library, Git library, or test framework dependency. `package-lock.json`
is authoritative. The live application revision records Node executable/version,
lockfile digest, compiled bundle digest, Pi SDK version, and resolved package-set
digest.

## 7. Local protocol and SDK

### 7.1 Transport

The transport is intentionally invisible to application and tool authors.
`@factory/sdk` exposes ordinary typed methods such as:

```text
forum.search(...)
forum.listTopics(...)
forum.readThread(...)
forum.createTopic(...)
forum.createThread(...)
forum.post(...)
artifact.sealWorkspaceFile(...)
candidate.checkpointRegression(...)
candidate.submit(...)
quality.runFullSuite(...)
quality.submitReview(...)
product.submitTicket(...)
work.complete(...)
```

Underneath, `factoryd` uses local Unix sockets. No TCP port is opened. Actor
hosts receive a daemon-created connected descriptor whose server-side context
already binds the exact `SessionId`, office, assignment, application revision,
and command jurisdiction. Actors never send an authoritative identity in a
tool payload. The operator socket is mode `0600` in the runtime root; same-user
access is a cooperative trust boundary, not authentication against hostile
same-user processes.

### 7.2 Frame grammar

Each frame is:

```text
4-byte unsigned big-endian payload length
UTF-8 JSON payload
```

Limits:

- request frame: 1 MiB;
- response frame: 4 MiB;
- maximum in-flight request per actor connection: one;
- bounded daemon-side read deadline and operation deadline;
- invalid UTF-8, invalid JSON, unsupported protocol, unknown operation, missing
  required field, duplicate terminal action, and trailing bytes reject.

Large bytes never ride in JSON. A worker writes beneath its assigned staging
root and asks the daemon to adopt a bounded file. The daemon canonicalizes the
path, rejects symlinks/escape, streams and hashes it, atomically installs it in
CAS, and returns an `ArtifactId` and BLAKE3 digest.

### 7.3 Miniserde shape

Because miniserde supports named structs and fieldless enum variants rather
than payload-bearing tagged enums, the wire uses operation-specific flat
messages. The server first parses a small routing envelope:

```text
protocol_version
request_id
operation
```

It then parses the same bytes into the closed request struct for that
operation. Required fields reject when absent. Known discriminants are closed.
Additional fields are ignored for forward compatibility; a client must not
depend on an ignored field taking effect. Semantic validation errors identify
the operation and field even though raw miniserde syntax errors are coarse.

Rust and TypeScript share checked-in golden request, success, conflict, and
error fixtures. There is no generated schema tool in the MVP; changing a
protocol struct requires updating both implementations and the cross-language
fixtures in one commit.

### 7.4 Idempotency and conflicts

Every mutating SDK call carries a bounded client command ID and expected
aggregate revision. The accepted audit row has a unique `(principal,
command_id)` key and a BLAKE3 fingerprint of the canonical operation fields.

- exact retry returns the original typed receipt;
- reuse with changed content returns `IdempotencyConflict`;
- stale expected revision returns `RevisionConflict` with current revision;
- no command is inferred successful from a dropped socket.

Wire `request_id` is connection-local and exists only to match responses.
Client command IDs are retry identities, not secrets. Collision is a safe
explicit conflict.

## 8. PostgreSQL authority and write discipline

### 8.1 Baseline

- PostgreSQL 18 is the supported local baseline.
- One resident daemon holds a dedicated PostgreSQL advisory singleton lock and
  a runtime-root filesystem lock.
- SQLx uses a small fixed pool, initially four connections.
- Commands use ordinary transactions, row-level locks on the affected
  aggregate, foreign keys, uniqueness, checks, and expected revisions.
- Closed Rust enums persist as constrained `SMALLINT` values, avoiding stringly
  discriminants and hard-to-evolve PostgreSQL enum types.
- Currency is nonnegative `BIGINT` micro-USD. Never store floating-point money.
- Times are `TIMESTAMPTZ`; durations are integer milliseconds.
- Paths stored in PostgreSQL are safe runtime-root-relative paths only.
- No actor, application, CLI, or TypeScript process receives a database URL.

Use SQLx checked queries and checked-in offline query metadata so ordinary
builds do not require a live database. There is one forward-only migration
lineage. Every migration used for self-upgrade must be transaction-safe on the
live database; concurrent/nontransactional migration forms are rejected for
MVP activation.

### 8.2 Tables

The initial schema is deliberately relational and purpose-specific:

1. `kernel_generations` — source/binary/schema/application artifact identities,
   predecessor, lifecycle, qualification, activation, and admission-open fact.
2. `generation_qualifications` — one candidate's clone database, commands,
   result artifact, duration, and incumbent decision.
3. `application_revisions` — sealed application bundle, mission, policy, prompt
   set, tool revision, and predecessor.
4. `model_profiles` — application revision, office, provider, model, thinking,
   turn/wall/output bounds, and Pi runtime identity.
5. `repositories` — logical repository identity, canonical local path, default
   branch, and allowed delivery mode.
6. `repository_snapshots` — repository, commit, tree, cleanliness, observed
   generation, and qualification time.
7. `campaigns` — application/generation, lifecycle, aggregate micro-USD cap,
   deadline, delivery target, measured totals, and revision.
8. `tickets` — stable identity, application, lifecycle, and current revision.
9. `ticket_revisions` — immutable problem contract, discovery snapshot,
   narrative/evidence artifacts, scope, acceptance, and supersession.
10. `ticket_reproducers` — exact command-spec artifact, expected observation,
    discovery runs, and latest pre-claim qualification.
11. `assignments` — campaign, office, exact work target, immutable input packet,
    lifecycle, attempt ordinal, and revision.
12. `assignment_required_reads` — assignment, canonical path, expected BLAKE3,
    and reason.
13. `sessions` — assignment/model/process identity, start/terminal state,
    transcript artifact, normalized usage/cost totals, and failure class.
14. `session_read_assertions` — terminal normalized satisfaction for each
    required read; one row per requirement, not one row per read call.
15. `artifacts` — digest, byte length, media role/type, CAS relative path,
    creation generation, and seal time.
16. `candidates` — ticket/base, regression tree, candidate tree, patch,
    Engineering session/report, commit identity, attempt, and lifecycle.
17. `validations` — candidate/generation, validation profile, pristine tree,
    exact command-set revision, terminal result, duration, and log artifact.
18. `reviews` — candidate, Quality session, full-suite validation, verdict,
    rationale/risks artifact, and override relation.
19. `architect_decisions` — sponsor/deliver/rework/reject/activate decision,
    exact subject revision, bounded rationale, and principal.
20. `deliveries` — candidate commit, expected old ref, resulting ref/tree,
    method, time, and recovery status.
21. `forum_topics` — immutable name/description, creator, creation time, and
    optional superseding topic.
22. `forum_threads` — topic, immutable title, creator, creation time, and
    optional superseding thread.
23. `forum_posts` — thread, globally ordered ID, author occurrence, bounded
    immutable UTF-8 body, kind, reply/supersession relation, and creation time.
24. `forum_attachments` — post-to-artifact relation with bounded label.
25. `audit_log` — one slim ordered receipt per accepted semantic transition.

SQLx's own migration history table is also present. No generic object, edge,
EAV, JSONB metadata, workflow graph, report, projection, outbox, lease,
heartbeat, notification, tool-call, search-read, token-event, transcript-chunk,
or trace-event table is created.

Long narratives, prompts, ticket packets, model transcripts, patches, command
logs, and validation output live in CAS. PostgreSQL stores the fields needed
for authority, eligibility, lifecycle queries, cost breakdown, Forum search,
and provenance plus direct artifact references.

### 8.3 One semantic transition, one transaction

An accepted mutating command performs one transaction that:

1. checks principal jurisdiction, expected revision, references, lifecycle,
   WIP, budget, and generation;
2. inserts or updates the one authoritative domain fact set;
3. inserts one `audit_log` receipt with command fingerprint and resulting
   revision; and
4. commits or changes nothing.

There is no command row plus event row plus outbox row plus projection row for
one fact. The audit row doubles as the idempotency receipt and provenance index.
It stores no duplicated command payload; operation-specific authoritative data
remain in their named tables or CAS artifact.

### 8.4 What is not written

Do not write PostgreSQL rows for:

- daemon trace/log events;
- rejected or malformed wire calls;
- Forum searches, topic listings, thread reads, or snippets returned;
- individual Pi SDK events, tool calls, tool results, token updates, thinking
  blocks, or transcript messages;
- shell stdout/stderr chunks;
- process heartbeats or scheduler polling;
- derived campaign totals on every session event; or
- generated Markdown/status projections.

The daemon streams operational diagnostics through `tracing` to stderr or an
operator-selected file. Pi events stream to one temporary session file and are
compressed and sealed once. Validation streams go to one temporary log and are
sealed once. Campaign totals and office breakdowns are SQL queries over terminal
session rows.

### 8.5 Write-count acceptance tests

Tests assert row/transition counts, not merely outcomes. In particular:

- a Pi session with 1,000 SDK events creates no SDK-event rows and only the
  required assignment/session/read-assertion/audit facts;
- 100 Forum searches and reads create zero writes;
- one Forum post creates one post, zero copied inbox/read rows, optional
  attachment relations, and one audit receipt;
- cost updates once at session terminal, not per turn;
- status and report queries are read-only; and
- a failed transaction leaves neither material state nor audit receipt.

## 9. Artifact and transcript custody

### 9.1 CAS layout

The append-only content store lives outside PostgreSQL under the runtime root:

```text
var/objects/blake3/<first-two-hex>/<remaining-hex>
```

Adoption:

1. open a canonical file beneath an assigned staging root without following a
   final symlink;
2. enforce role-specific byte limit while streaming;
3. compute BLAKE3 and byte length;
4. write a uniquely suffixed temporary object on the same filesystem;
5. `fsync` the file, atomically rename if absent, and `fsync` its directory;
6. verify an existing object with the same digest has the same length;
7. insert or reuse the immutable `artifacts` row; and
8. link it from the domain transaction that admits its meaning.

Content identity proves bytes only. The owning table supplies role and
provenance. CAS files are never updated in place and are not garbage-collected
in MVP.

### 9.2 Pi transcript

Every assignment has one fresh Pi session and no resume. The TypeScript host
retains the complete SDK event, message, tool-call/result, usage, retry, and
terminal stream as newline-delimited JSON in its assigned staging directory.
At terminal state it uses Node's built-in gzip to produce one stream artifact;
the daemon adopts it and records:

- BLAKE3 and byte length;
- role, assignment, model/runtime revision;
- start/end time and stop/failure reason;
- turns and token buckets when reported;
- reasoning tokens when reported, never guessed;
- tool calls and nonzero tool results;
- provider retry/error summary;
- exact provider cost rounded upward to micro-USD; and
- full transcript artifact relation.

Raw transcripts are retained indefinitely. They are not copied into
PostgreSQL, indexed for Forum search, treated as institutional memory, or
automatically added to later prompts.

### 9.3 Crash behavior

The host continuously appends to its local temporary stream, not PostgreSQL.
The daemon-session socket is also a liveness channel: if authority disappears,
the host stops new model calls, disposes the Pi session, and exits. The daemon
owns the host process group and records PID/PGID in the session fact.

On daemon restart:

- reacquire singleton locks before serving;
- inspect every nonterminal session and recorded PGID;
- terminate/reap any surviving owned group;
- adopt the partial transcript when structurally readable;
- mark the old assignment attempt interrupted and terminal; and
- never resume that Pi session.

If exact cost cannot be recovered, campaign cost becomes `Unknown`, all further
paid admission stops, and the Architect must close the campaign. Unknown is
never zero. A retry, when allowed, is a fresh session and assignment attempt.

## 10. Forum

### 10.1 Purpose and non-authority

The Forum is permanent shared communication and institutional memory. Any
admitted actor or the Grand Architect may create topics, create threads, post,
reply, and supersede under byte/count quotas. Agents may use it to organize
emergently without the kernel assigning a social topology.

A Forum post is untrusted peer content. Publication does not:

- create or sponsor a ticket;
- complete an assignment;
- grant authority or budget;
- become evidence or truth automatically;
- change a lifecycle state;
- alter a prompt or validation profile; or
- certify a candidate.

Those effects require their typed commands. The Forum has no ranking,
reputation, karma, consensus, votes, private inbox, subscriptions, unread
digest, or live notification in MVP.

### 10.2 Data and limits

- Topic name: 160 UTF-8 bytes; description: 4 KiB.
- Thread title: 240 UTF-8 bytes.
- Post body: 16 KiB, NUL rejected.
- Post kind: `Note`, `Question`, `Finding`, `Proposal`, `Challenge`,
  `Correction`, or `DecisionLink`.
- Reply and supersession targets must be earlier posts in the same thread.
- Posts are immutable. Correction/supersession preserves original bytes.
- Attachments use CAS and a bounded label; no large body is duplicated in
  PostgreSQL.
- Global `BIGINT` post identity supplies stable chronological order. Per-thread
  order is the same ID filtered by thread, avoiding a thread-head update on
  every post.

History is retained forever in normal operation. Backup/restore must preserve
Forum rows and referenced CAS objects. There is no delete endpoint in MVP.

### 10.3 Search

`forum_search` must be useful in one call. It accepts:

- ordinary unquoted terms;
- optional exact quoted phrases;
- topic, thread, author-office, post-kind, and time filters;
- `limit` from 1 through 20; and
- an optional stable continuation cursor.

Use PostgreSQL full-text search with the `simple` configuration so code and
domain terms are not English-stemmed unexpectedly. Topics, threads, and posts
each have a stored generated `tsvector` and a GIN index. A bounded union query
searches topic name/description, thread title, and post body without copying
all text into a separate search-document table. `websearch_to_tsquery('simple',
$query)` makes unquoted term order irrelevant; typo/fuzzy matching is not
implemented. Results use `ts_rank_cd`, deterministic post-ID tie breaking, and
bounded `ts_headline` snippets with topic/thread context.

`forum.readThread` reads a bounded chronological page after a post ID.
`forum.listTopics` and `forum.listThreads` derive recent activity with indexed
queries rather than updating `last_activity` columns on every post. Searches
and reads write no receipt rows.

Unread digests and live notifications are desired future treatments but
strictly deferred until explicit browsing has measured shortcomings.

## 11. Pi host, tools, and required reads

### 11.1 One assignment per session

The daemon creates one exact immutable assignment packet and launches one
TypeScript Pi host. The packet pins:

- campaign, office, assignment, ticket/candidate where applicable;
- kernel generation and XSH application revision;
- provider/model/thinking/turn/wall/output profile;
- aggregate campaign cost remaining at launch;
- system prompt and task prompt digests;
- workspace and staging roots;
- exact product/factory base identities;
- required reads; and
- legal terminal submission operations.

The host rejects a changed packet digest. A session terminates after one
assignment; Quality feedback or restart creates a new session with prior
evidence explicitly included.

### 11.2 Common tool surface

All Product, Engineering, and Quality actors receive the same workspace
capabilities:

- canonical workspace-bound `read`, `write`, and `edit` tools;
- search/list tools;
- a general shell rooted at the assigned worktree;
- ordinary host network access;
- Forum list/search/read/create/post tools;
- artifact-seal and role-appropriate typed submission tools; and
- the application-owned full-suite tool where the assignment permits it.

Prompts explain office duty, but safety does not depend on hiding file tools
from a director. Structural authority differs by accepted terminal command:
Product can materialize tickets only, Engineering can materialize a candidate
only, and Quality can materialize a review only.

The host does not scrub or mediate outbound network access. Web endpoints are
not independently logged by the kernel. Research sources that matter must be
listed in the ticket/report; the raw transcript remains supporting evidence.

### 11.3 Required-read assertion

An assignment may contain:

```text
ReadExactFile {
  canonical_path,
  blake3,
  reason
}
```

The wrapped Pi `read` tool emits an internal observation only after it returns
the exact canonical file bytes. Shell `cat`, search output, a prompt quotation,
or an actor assertion does not satisfy the gate. At terminal submission the
host supplies its observed read set; the daemon independently verifies path and
digest against the assignment and stores one normalized row per requirement.

Every XSH Product, Engineering, and Quality assignment requires the pinned:

- `../xsh/AGENTS.md`; and
- `../xsh/docs/CHAPTER-01-why-xsh.md`.

Engineering and Quality also require `../xsh/docs/TEST-MAP.md` and the exact
nearest product contract(s) selected in the sponsored ticket. Product may add
contract reads to its proposal, but it cannot remove the two universal reads.
A missing or changed required read rejects terminal submission.

There is no V3 rolling handbook. Durable XSH truths belong in XSH's own
AGENTS/contracts/docs/tests; cross-task discussion belongs in the Forum.

## 12. Product change circuit and Git provenance

### 12.1 Ticket reproducer

A Product proposal includes a sealed, versioned command specification and
expected observation. The generic kernel understands only a bounded process
contract:

- executable path or application-approved tool identity;
- exact argv, no shell interpolation by the kernel;
- repository-relative working directory;
- declared environment additions over a minimal deterministic baseline;
- stdin artifact or empty stdin;
- timeout and stdout/stderr byte ceilings;
- expected exit status and optional exact stdout/stderr artifacts; and
- comparison rule revision.

The XSH application validates which executables and environment fields are
permitted. Product may use the common shell during research, but only this
sealed command contract is authoritative.

Before proposal admission, the kernel runs the command twice against the clean
discovery snapshot. Both actual observations must be byte/exit-identical and
must differ from the expected observation. The actual and expected bytes are
sealed. This is an exact failing reproducer, not a source-level hunch.

### 12.2 Engineering worktree

For an Engineering claim the kernel:

1. proves the configured product default branch and clean main checkout;
2. resolves current commit/tree and reruns the ticket reproducer twice;
3. creates a detached disposable Git worktree at that exact commit under the
   V3 runtime root;
4. records the snapshot and worktree ownership;
5. renders/seals the immutable assignment packet; and
6. launches the Engineering host in that worktree.

The actor must not alter `HEAD`, create a commit, merge, or update refs. A
changed `HEAD` rejects the candidate even if the files look useful.

### 12.3 Regression checkpoint

Before implementing the fix, Engineering invokes
`candidate.checkpointRegression` with the targeted regression command. The
kernel captures the complete working tree through a temporary Git index without
changing the actor's index:

```text
read base tree into temporary index
add all worktree changes to temporary index
write immutable Git tree
derive changed paths and binary patch
```

The checkpoint must contain only the test/fixture/documentation needed to
express the regression, not implementation changes. The kernel materializes a
fresh worktree at that tree and runs the targeted regression. It must fail in
the declared way. Quality later judges whether the checkpoint accidentally
contains a fix or encodes a task-specific assertion.

### 12.4 Candidate capture and hard validation

On candidate submission the kernel:

1. proves the actor worktree `HEAD` is still the assigned base;
2. captures the candidate through a fresh temporary index;
3. rejects empty or forbidden-path changes;
4. records exact base tree, regression tree, candidate tree, changed paths, and
   portable `git diff --binary` patch in CAS;
5. proves the Product reproducer now matches the expected observation;
6. runs the XSH full-suite profile on a fresh worktree at the candidate tree;
7. requires exit zero and bounded complete output for every command;
8. verifies no validation command changed the candidate tree;
9. requires `git diff --check` success; and
10. rejects any missing/unknown result.

The XSH MVP full-suite profile is code-owned by `applications/xsh`, not parsed
from Markdown. It always runs exact argv for:

```text
cargo test
git diff --check <base> <candidate>
```

The reproducer and regression commands are additional hard gates. Quality may
run extra checks, but cannot replace or narrow the full suite. V3 never runs
XSH formatters, autofixers, pre-commit hooks, release builds, or remote Git
commands as an implicit gate.

### 12.5 Kernel-constructed commit

After Engineering validation succeeds, the kernel constructs the candidate
commit with argv-based Git plumbing and a temporary index. It does not accept
an actor-authored commit and does not amend later.

Inputs:

- exact base commit and candidate tree;
- normalized bounded subject/body proposed by Engineering;
- fixed factory author and committer identities;
- recorded construction timestamp;
- provenance trailers for campaign, ticket, ticket revision digest, kernel
  generation, application revision, base, regression tree, candidate tree,
  patch BLAKE3, Engineering session BLAKE3, and validation identity.

Quality and Architect decisions remain database provenance because they occur
after candidate construction. Both identify the exact candidate commit/tree.
The kernel creates `refs/heads/factory/<ticket-id>/<candidate-id>` with an
expected-absent compare-and-swap. It never pushes.

Every Git invocation uses an exact configured Git executable, argv rather than
shell, disabled global/system config, disabled hooks, disabled replacement
refs, disabled external diff/text conversion, bounded streams, and a deadline.
Repository configuration that enables filters or unsafe includes rejects
qualification.

### 12.6 Independent Quality replay

The kernel materializes the exact candidate commit/tree into a new Quality
workspace. Quality invokes `quality.runFullSuite`; the daemon runs the same
code-owned full-suite command set in a separate pristine validation worktree
and returns its receipt. A review cannot be submitted without a passing
Quality-owned validation identity.

Quality's narrative verdict and extra probes are qualitative. A hard failure
ends delivery eligibility regardless of prose.

### 12.7 Delivery

After an `accept` Architect decision, the kernel:

1. reacquires the repository delivery lock;
2. proves the local default branch and checkout are at the candidate's exact
   base, clean, and have no unexpected worktrees/operation state affecting
   delivery;
3. proves the candidate commit is a one-parent descendant with the exact
   validated tree;
4. runs guarded local `git merge --ff-only` with hooks/config disabled;
5. verifies resulting ref, `HEAD`, index, working tree, and tree identity;
6. records the delivery receipt and marks the ticket delivered; and
7. triggers read-only buffer re-evaluation against the new head.

If the branch moved, checkout is dirty, fast-forward fails, or postcondition
differs, delivery fails closed and preserves the candidate branch and evidence.
There is no merge commit, rebase, force update, remote fetch, or push.

## 13. Factory policy and generation change circuits

### 13.1 Application-policy change

Prompts, model profiles, turn/wall/output limits, Forum guidance, ticket-buffer
limits, and other declarative XSH application policy form an immutable
`ApplicationRevision`. The Grand Architect may author and activate a new
declarative revision directly at a quiescent assignment boundary after the
kernel validates:

- fixed office set and authority contracts;
- buffer inequalities and WIP bounds;
- model/profile completeness;
- prompt/tool/required-read artifact identities;
- no department-specific dollar ceilings;
- no changed product repository identity; and
- exact predecessor/application lineage.

Activation is an explicit Architect decision and affects only new assignments.
Active sessions retain their pinned old revision.

A TypeScript code change is not a declarative policy edit; it follows the full
factory Engineering/Quality circuit.

### 13.2 Factory source change

The Grand Architect may originate a typed factory change request directly,
without Product sponsorship. It states objective, affected trusted boundary,
acceptance tests, migration impact, rollback boundary, and expected operational
improvement.

1. A fresh Engineering Director session works in an isolated V3 source
   worktree and submits source tree plus any migration.
2. The incumbent kernel runs the complete provider-free V3 qualification set.
3. A fresh Quality Director session reviews exact source/tree/migration and
   invokes the qualification set again.
4. The Architect accepts or rejects the exact candidate generation.
5. An accepted candidate enters clone qualification and activation below.

The incumbent generation owns every gate. The candidate never certifies itself
by writing incumbent state.

### 13.3 Generation package

An immutable candidate generation package contains:

- predecessor generation ID;
- source commit/tree and portable patch artifact;
- Rust binaries with BLAKE3/length;
- compiled TypeScript bundles, Node/Pi lock/runtime identities;
- ordered SQL migration artifact and expected resulting schema identity;
- application revision set;
- exact qualification command-set revision;
- test and Quality receipts; and
- Architect activation decision.

### 13.4 Clone qualification

The incumbent daemon:

1. closes new work admission and waits for all sessions, validation children,
   Git materializations, and pending writes to become terminal;
2. takes a custom-format `pg_dump` of the live database and records its BLAKE3;
3. creates a uniquely named qualification database;
4. restores the dump;
5. starts the candidate in qualification mode against only the clone and a
   cloned runtime manifest;
6. applies candidate migrations to the clone;
7. verifies expected schema identity, FKs/checks/indexes, audit/material-state
   consistency, CAS references, ticket readiness, campaign cost derivation,
   Forum search, and read-only status queries;
8. runs restart and idempotency probes;
9. proves the candidate does not touch the live database or product refs;
10. seals the complete qualification log; and
11. destroys the clone only after its evidence is sealed.

Qualification never requires rebuilding every materialized row from the audit
log. V3 uses audit-ledger-plus-state, not full event sourcing.

### 13.5 Live activation and pre-admission rollback

After clone qualification passes:

1. retain the live dump, prior binaries/bundles, prior active-generation
   manifest, and append-only CAS;
2. stop the incumbent serving socket while retaining process custody;
3. apply the already-qualified transaction-safe migration to live PostgreSQL;
4. atomically replace the active-generation manifest using write/fsync/rename;
5. `exec` or spawn the candidate daemon;
6. require singleton-lock acquisition, schema check, audit/material-state
   audit, CAS sample verification, and read-only health response;
7. rehearse rollback before opening work admission: stop candidate, restore the
   old database from the retained dump, start old generation and health-check,
   then repeat the qualified migration and candidate health check; and
8. only then record candidate activation and open admission.

Failure before step 8 restores the old database/generation automatically and
records a failed qualification/activation. Once admission opens and the new
generation accepts a command, automatic snapshot rollback is forbidden;
recovery must roll forward.

The tiny boot shim that reads the active-generation manifest provides no policy
or qualification judgment and is not called an immutable warden. A future
generation may replace it through the same self-authorized package, consistent
with the explicit limitation in section 2.3.

## 14. Budget and process supervision

### 14.1 Aggregate cost only

There are no department, role, or fixed per-stage dollar allocations. One
campaign owns one aggregate micro-USD cap. The status/report breakdown groups
terminal session cost by department, office, assignment, model, attempt, and
outcome without reserving those groups a share.

Because MVP admits only one paid Pi session at a time, no concurrent cost
reservation race exists. Before each session the daemon supplies the Pi host
with the campaign's then-known remaining allowance. Turn, wall, and output-token
limits come from the model profile and control runaway work without earmarking
department money.

The host observes provider usage/cost from the Pi SDK and stops further turns
when the remaining campaign allowance is reached. One provider response may
report cost only after completion, so the aggregate cap is a fail-closed
admission and shutdown bound rather than a promise that an opaque provider can
never overshoot within its final response. Any observed total above cap fails
the campaign and the MVP gate.

Unknown cost stops all later paid admission. Costs use provider-reported values
rounded upward to integer micro-USD; absent cost is unknown, not zero.

### 14.2 Proven initial model split

The first XSH application revision reuses the already demonstrated economical
shape rather than running a separate paid model tournament:

- Product and Quality use the proven low-cost director/reviewer provider/model
  class with high thinking;
- Engineering uses the proven more capable engineer provider/model class with
  high thinking; and
- all exact provider, model, thinking, sampling, Pi SDK, tool, turn, wall, and
  output settings are pinned in the application revision.

The implementation must resolve the exact currently available model IDs during
a provider-free catalog/SDK compatibility check. It must not silently fall back
to another model at runtime. A future profile change is a new application
revision.

### 14.3 Process custody

Every host or deterministic validation child has:

- exact executable and argv;
- assigned cwd and environment;
- PID and process group;
- owner generation/campaign/assignment/session;
- stream byte ceilings;
- wall deadline;
- cancellation state; and
- direct wait/final status.

The daemon registers ownership before treating a process as admitted. On
cancel, deadline, budget stop, daemon shutdown, or host protocol failure it
closes admission, signals the exact process group, escalates after a bounded
grace period, directly waits, seals available streams, and records terminal
state. It never scans or kills by executable name.

## 15. Operator surface

`factoryctl` is the only operator/Grand Architect entry point. It speaks the
same Unix-socket protocol and supports stable text plus `--format json` for
automation. Initial commands:

```text
factoryctl init
factoryctl daemon status
factoryctl generation status
factoryctl generation qualify <package>
factoryctl campaign start --application xsh --budget-micro-usd 500000 \
  --delivery-target 1 --upgrade-package <package>
factoryctl campaign status <id>
factoryctl campaign cancel <id>
factoryctl ticket list [--state ...]
factoryctl ticket show <id>
factoryctl ticket sponsor <revision> --expected-revision ... --reason ...
factoryctl candidate show <id>
factoryctl candidate decide <id> --accept|--rework|--reject --reason ...
factoryctl forum topics
factoryctl forum threads <topic>
factoryctl forum search <terms> [filters]
factoryctl forum read <thread> [--after-post ...]
factoryctl forum post ...
factoryctl audit show <subject>
```

`init` creates/validates only a specifically named administrator-approved
database/schema and runtime root. It never discovers or deletes databases.
Status commands are read-only. Mutating commands require explicit expected
revision and idempotency key internally. No command pushes Git, starts an
unbounded autonomous loop, or imports V1/V2 state.

Generated human views are stdout or explicitly requested files derived from
PostgreSQL and CAS. They are not authoritative state and are not written after
every transition.

## 16. Implementation sequence

Each tranche must pass its provider-free judges before the next. Do not begin a
paid Pi call until tranche 10 is complete.

### Tranche 1 — contracts and skeleton

- Write root `AGENTS.md`, `README.md`, architecture glossary, trust assumptions,
  and repository boundary.
- Create the bounded Rust workspace and TypeScript workspaces.
- Pin toolchains and direct dependencies; commit lockfiles.
- Define identifier newtypes, closed enums, error taxonomy, currency/duration
  types, path types, and aggregate revisions.
- Add a dependency-direction test preventing Rust references to XSH/application
  vocabulary and preventing application database imports.
- Add CI/local make targets for provider-free checks only.

Exit: empty daemon/CLI/SDK compile; dependency graph and cleanroom boundary
tests pass.

### Tranche 2 — PostgreSQL command core

- Implement schema bootstrap/migrations for generations, applications,
  repositories, campaigns, audit, and artifacts first.
- Implement advisory singleton lock, schema identity comment/check, SQLx offline
  metadata, transactions, expected revisions, and audit-backed idempotency.
- Add remaining tables only with the transition that first needs them.
- Implement read-only status queries and row-count/write-amplification tests.
- Add fresh-schema, rollback, duplicate-command, changed-body, stale-revision,
  FK/check corruption, and daemon-restart tests against PostgreSQL 18.

Exit: accepted commands atomically produce one authoritative state change and
one audit receipt; failures produce neither.

### Tranche 3 — CAS and framed protocol

- Implement bounded CAS adoption, atomic installation, verified reads, and
  append-only semantics.
- Implement Unix actor/operator sockets and frame limits.
- Implement operation-specific miniserde request/response structs.
- Implement Rust/TypeScript golden fixtures and malformed/truncated/oversize
  frame tests.
- Implement inherited actor connection binding so actor payloads cannot choose
  their office/session identity accidentally.

Exit: TypeScript SDK can perform typed test commands and seal/read artifacts
without HTTP or database access.

### Tranche 4 — Forum

- Add topic/thread/post/attachment transitions and quotas.
- Add generated `simple` tsvectors and GIN indexes to each text owner table.
- Implement one-call order-independent `forum.search`, bounded snippets,
  filters, cursor, and chronological reads.
- Add immutability, reply/supersession, attribution, search-plan, result-bound,
  and zero-write read/search tests.
- Add SDK and Pi custom-tool adapters.

Exit: disposable fake actors can organize through permanent Forum history; no
Forum content changes authority or lifecycle.

### Tranche 5 — process and Pi host custody

- Implement assignments, model profiles, session lifecycle, PID/PGID custody,
  cancellation, direct wait, and terminal reconciliation.
- Build the one-assignment Pi host using the coding-agent SDK with fake-provider
  injection for tests.
- Install common workspace tools and full-network host behavior.
- Stream full raw events to disk, gzip and seal once, normalize terminal usage,
  and halt on unknown cost.
- Implement required-read observations and terminal assertion.
- Test disconnect, daemon crash, child refusal, timeout, output limit, signal
  escalation, partial transcript, missing usage, and no-resume retry.

Exit: all lifecycle/cost/session tests are provider-free; a fake Pi assignment
has exact provenance and bounded shutdown.

### Tranche 6 — ticket buffer and Product workflow

- Implement ticket/revision/reproducer/sponsorship transitions.
- Implement XSH application mission, Product prompt, model profile, mandatory
  reads, proposal validator, duplicate-search input, and buffer policy.
- Implement deterministic two-run discovery and pre-claim reproduction.
- Implement scheduler priority, low-water/target/max pressure, proposal cap,
  one-paid-session WIP, and constraint reporting.
- Test buffer empty/low/target/full, unsponsored-proposal backpressure,
  downstream blockage, resolved-on-new-head, divergent reproducer, duplicate,
  and no-quality-lowering behavior.

Exit: a fake Product session can replenish up to target but cannot auto-sponsor
or create nonreproducible work.

### Tranche 7 — Git candidate and validation core

- Implement repository qualification and safe Git environment.
- Implement detached worktree ownership and cleanup.
- Implement temporary-index regression and candidate tree capture, changed
  paths, binary patch, and exact-tree rematerialization.
- Implement bounded deterministic command supervision and application-owned
  validation profiles.
- Implement kernel commit construction, provenance trailers, candidate refs,
  and guarded local fast-forward.
- Test dirty/moved heads, filters/config/includes/hooks, symlink paths, changed
  actor HEAD, empty tree, regression containing fix, failing/dirty validation,
  tree/patch mismatch, idempotent commit construction, ref CAS, checkout
  failure, and no remote operation.

Exit: a fake Engineering result can become a kernel commit and only a fully
accepted exact commit can reach a synthetic product main branch.

### Tranche 8 — Quality and Architect decisions

- Implement candidate packet, independent Quality workspace, Quality-owned
  full-suite invocation, review schema, override relation, and one-rework limit.
- Implement external Architect sponsorship and final-decision CLI/SDK.
- Enforce qualitative override versus non-overridable hard failures.
- Test accept, reject, override, rework pass/fail, stale decision, changed
  candidate, missing full-suite invocation, and discarded Quality edits.

Exit: complete provider-free product workflow passes against synthetic Git and
fake Pi sessions.

### Tranche 9 — self-update

- Implement generation packages, source/binary/bundle/migration digests, and
  qualification command sets.
- Implement quiescence and prohibition on active children/writes.
- Implement `pg_dump`/restore clone qualification and live backup.
- Implement candidate qualification mode, schema/audit/material-state audits,
  active manifest atomic switch, health handshake, rollback rehearsal, second
  activation, and admission opening.
- Implement failure injection after every boundary and prove pre-admission
  restoration.
- Prove that automatic rollback refuses after the new generation accepts its
  first command.

Exit: provider-free generation A upgrades to B, rehearses rollback, returns to
B, preserves campaign identity, and continues scheduling.

### Tranche 10 — complete XSH application and dry runs

- Finalize exact XSH Product, Engineering, and Quality prompts and result
  validators.
- Pin Node, Pi SDK, provider/model profiles, tool schema, full-suite argv, and
  required reads.
- Run all flows with scripted fake Pi outputs and synthetic XSH-like repos.
- Run the real XSH build/full suite outside a paid session to qualify host
  prerequisites and expected duration.
- Exercise Forum search with a large synthetic corpus and inspect query plans,
  index size, memory, and latency.
- Exercise 1,000-event transcripts and assert bounded PostgreSQL writes.
- Run crash/cancel/unknown-cost/dirty-repo/moved-head/generation-failure drills.
- Produce an operator checklist and exact combined MVP campaign request.

Exit: no known deterministic defect requires a provider call to diagnose.

### Tranche 11 — one paid combined MVP campaign

- Verify clean V3 and XSH checkouts, PostgreSQL 18, exact active bootstrap
  generation, Node/Pi/runtime identities, authentication, aggregate $0.50 cap,
  and no prior live state import.
- Start one campaign with the prepared bootstrap-to-candidate upgrade package.
- Complete and preserve the upgrade/rollback rehearsal before paid Product
  work.
- Run one Product replenishment session; the task is not seeded.
- Have the external Architect sponsor a reproducible ticket.
- Run Engineering, hard validation, Quality, and final decision sequentially.
- Use one rework only if justified and affordable under the same cap.
- Deliver the exact commit locally and never push.
- Inspect total and office/session cost, transcripts, required reads, ticket
  buffer, validation, review, decision, delivery, audit, Postgres row counts,
  and CAS artifacts.
- Mark MVP pass only if every gate in section 1.1 holds.

## 17. Test and evidence matrix

### 17.1 Domain/state tests

- every legal and illegal campaign, ticket, assignment, session, candidate,
  review, delivery, Forum, and generation transition;
- expected-revision and idempotency behavior;
- fixed office jurisdiction;
- aggregate budget known/unknown/exceeded;
- one paid session, one Engineering WIP, one Quality WIP;
- ticket buffer pressure and proposal caps;
- one rework maximum; and
- Quality override only for qualitative rejection.

### 17.2 PostgreSQL tests

- fresh bootstrap and each forward migration;
- SQLx offline query metadata check;
- transaction rollback at each injected boundary;
- duplicate command and changed fingerprint;
- constraints/FKs and safe relative paths;
- singleton advisory lock;
- material-state/audit consistency audit;
- write-count assertions;
- backup/restore and clone isolation;
- Forum FTS correctness, word-order independence, cursor stability, and query
  plans using GIN indexes; and
- campaign/office/model/session cost breakdown from terminal rows.

### 17.3 Protocol/SDK tests

- Rust/TypeScript golden frames for every operation;
- missing, extra, invalid, oversized, truncated, and trailing input;
- coarse JSON parse error wrapped in useful operation context;
- actor connection identity cannot be replaced by payload;
- dropped response plus exact idempotent retry;
- large artifacts rejected from JSON and adopted by path; and
- no HTTP listener or dependency in the graph.

### 17.4 CAS tests

- exact BLAKE3/length, duplicate adoption, corrupted existing object, partial
  write, atomic rename, fsync error, symlink/path escape, size limit, verified
  read, and append-only retention;
- DB failure after physical seal leaves an unreferenced safe object rather than
  false provenance; and
- DB restore works because CAS is append-only and preserved.

### 17.5 Process/Pi tests

- fake provider success and all terminal stop reasons;
- timeout, cancellation, daemon disconnect, nonzero child exit, output limit,
  protocol error, and process-group escalation;
- exact model/runtime/prompt/tool identity;
- full compressed event stream and normalized totals;
- cost rounding, absent cost, retry telemetry, and aggregate stop;
- no session resume; and
- required reads pass only through exact wrapped read results.

### 17.6 Git/product tests

- clean base, changed base, dirty checkout, actor-changed `HEAD`, branch
  collision, worktree cleanup, binary patches, symlinks, submodules if present,
  filters, hooks, config includes, replace refs, and stream/time bounds;
- deterministic base reproducer twice;
- regression tree fails, candidate tree passes;
- full suite runs on pristine exact tree and leaves it unchanged;
- kernel commit trailers bind every precommit input;
- Quality sees the candidate exact tree;
- guarded fast-forward, ref movement, postcondition mismatch, and recovery
  evidence; and
- explicit proof no remote Git command is reachable.

### 17.7 Generation tests

- candidate cannot touch live DB during clone qualification;
- migration/schema mismatch;
- corrupt dump/CAS reference;
- active session prevents quiescence;
- failure before/after migration, manifest switch, candidate start, health,
  rollback restore, and second activation;
- old generation is healthy after rehearsal;
- new generation resumes the same campaign and preserves Forum/ticket state;
- first post-admission command closes automatic rollback; and
- predecessor/generation/audit lineage is queryable.

### 17.8 No-paid-test rule

All ordinary, integration, failure, replay-audit, process, Git, Postgres, Forum,
SDK, and upgrade tests use fake Pi providers and synthetic repositories. No
test target may call a model provider. The only paid action before MVP
acceptance is the explicitly authorized combined campaign.

## 18. Operational acceptance and observability

The daemon emits compact `tracing` diagnostics with generation, campaign,
assignment, session, ticket/candidate, and operation IDs where applicable.
Tracing is operational and disposable; provenance is PostgreSQL plus CAS.

`factoryctl campaign status` must show at least:

- generation and application revision;
- campaign state, deadline, and delivery target;
- current office/assignment and elapsed wall time;
- ready/proposed/blocked/in-flight buffer counts versus policy;
- current throughput constraint;
- known aggregate Pi spend and remaining allowance;
- spend by department, office, session, model, and outcome;
- unknown-cost/budget-stop state;
- latest hard validation, Quality review, and Architect decision;
- product base/candidate/delivered commit identities; and
- concise actionable blocker.

Post-campaign inspection must be possible without reading raw transcripts.
Raw transcripts remain available for disputed or subtle judgments.

Backups pair a PostgreSQL custom-format dump with the append-only CAS root and
active-generation manifest. Restoring into a blank database and runtime root
must pass schema, CAS-reference, and audit/material-state checks before serving.

## 19. Explicit non-goals and bloat exclusions

The following are not in the MVP and must not receive placeholder tables,
traits, feature flags, protocol fields, or unused abstractions:

- importing or converting V1/V2 tickets, reports, sessions, graph nodes, Forum
  state, handbook state, or lifecycle state;
- a universal causal/epistemic graph;
- objectives, hypotheses, conflicts, lessons, or outcomes as generic graph
  ontology;
- scientific episodes, treatment arms, counterfactual forks, actor replacement
  studies, or correction-latency experiments;
- mutable actor genomes, arbitrary organization graphs, role reproduction,
  selection, reinforcement learning, or scalar institutional fitness;
- ticket creation merely to meet buffer occupancy;
- autonomous eval design, package-owned benchmark portfolios, V1 eval-manager
  loops, or a rolling handbook;
- per-department dollar budgets;
- concurrent paid sessions or multiple concurrent campaigns;
- distributed daemons, leader election, database leases, heartbeats, remote
  workers, or multi-host artifact transfer;
- HTTP/HTTPS control APIs, web frameworks, dashboards, or a TUI;
- PostgreSQL JSONB, EAV, generic metadata maps, arbitrary workflow definitions,
  full event sourcing, projections, or outboxes;
- storing Pi events, tool calls, trace logs, searches, reads, or transcript text
  in PostgreSQL;
- Forum ranking, fuzzy search, votes, consensus, reputation, karma,
  subscriptions, digests, notifications, live steering, deletion, or private
  messages;
- adversarial sandboxing, network mediation, credential isolation, or security
  claims against same-user actor processes;
- remote Git operations, pull requests, pushes, deployment, or release
  packaging;
- automatic rollback after a new generation has accepted work; and
- an immutable human/warden authority above self-updating generations.

Desired but deferred Forum digests and notifications must begin with measured
evidence that explicit indexed browse/search is insufficient. Remote workers
and concurrent paid sessions require a new resource-reservation and fencing
design; they are not incremental flag changes.

## 20. Definition of done

V3 MVP is done only when:

- the cleanroom repository contains no dependency/import from V1 or V2;
- generic Rust has no XSH application vocabulary;
- all provider-free checks in section 17 pass on a fresh PostgreSQL 18 schema;
- SQLx offline metadata and both lockfiles are current;
- the dependency budget contains no Tokio/HTTP/ORM/generic workflow stack;
- Forum search is indexed, bounded, order-independent, and read-only;
- a 1,000-event fake Pi session demonstrates bounded PostgreSQL writes;
- exact required reads are proven for each XSH office;
- the ticket buffer enforces target/maximum/backpressure without weakening its
  reproducer/sponsorship contract;
- generation A upgrades to B, rehearses rollback, returns to B, and resumes the
  same campaign before work admission;
- the combined paid campaign discovers rather than receives its XSH task;
- Product's reproducer fails deterministically on the base;
- Engineering's regression checkpoint fails and candidate passes;
- the full XSH suite passes twice on the exact candidate tree, once as the hard
  Engineering validation and once under Quality;
- the Architect's exact sponsorship and delivery decisions are durable;
- the kernel constructs and guardedly fast-forwards one provenance-bearing XSH
  commit locally;
- no remote is pushed;
- total factory Pi provider cost is known and at most $0.50 with the required
  breakdown; and
- status, audit, CAS, PostgreSQL, transcript, review, validation, generation,
  and Git evidence are sufficient to explain the delivered commit without
  reconstructing state from chat.

The success metric is not that the company was busy or that the buffer was
full. It is that a clean, understandable institution converted an unseeded,
reproducible XSH problem into an independently reviewed, fully tested,
provenance-bearing local commit within the declared aggregate cost—and left a
qualified buffer and evidence that make the next commit easier rather than
more mysterious.
