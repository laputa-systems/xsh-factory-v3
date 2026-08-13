# Provider-free dry-run and MVP operator record

This document is the Tranche 9 dry-run record. It is intentionally separate from `PLAN.md`: it turns
the plan's acceptance facts into repeatable local checks and an operator record without creating
factory state or making a provider request.

Nothing in this document authorizes a paid campaign. A provider-free result proves only
deterministic kernel, SDK, PostgreSQL, Git, and host behavior. It does not prove a model can
discover a useful XSH defect or make a good product judgment.

## Scope and host boundary

Run these checks from the Factory V3 repository. They use a temporary synthetic repository and fake
Deno/Pi actors only. They must not start a campaign, read provider credentials, call a provider,
push a remote, or write to `../xsh`.

Actors in the eventual MVP remain cooperative same-user host processes. These checks do not upgrade
that boundary into a sandbox or a secrecy claim. See `docs/trust-assumptions.md` before an operator
runs any future campaign.

## Qualification order

Populate the frozen Deno cache as an installation action, then run the normal provider-free suite:

```sh
make cache
make check
```

For PostgreSQL judges, create one disposable PostgreSQL 18 database whose name is exactly
`factory_test_v3_<digits>`. Never point these tests at an operator database.

```sh
FACTORY_TEST_DATABASE_URL=postgresql://USER@localhost/factory_test_v3_<digits> \
  make postgres-test

DATABASE_URL=postgresql://USER@localhost/factory_test_v3_<digits> \
  make sqlx-check
```

`make postgres-test` runs serially because the assertions inspect durable row counts. It is the only
routine dry-run target that needs PostgreSQL. Its name guard is deliberately strict so the make
target cannot use a database with an operator-selected name.

## Real XSH prerequisite qualification

The application-owned full profile is `cargo test --locked`; it does not run a formatter,
autofixer, pre-commit hook, release build, or remote Git command. On
`2026-08-12T23:04:24Z`, that exact command passed outside a Pi session against the clean XSH
checkout below:

```text
canonical checkout: /Users/josh/d/laputa-systems/xsh
branch: master
commit: 04fb98f8c63b63cccffce7ef2c3cabde81bb05ba
tree: e160e847fcddbbffdfefb1f0dd8157fb13c86549
result: 166 library tests passed, 2 ignored; 470 integration tests passed, 28 ignored
elapsed wall time: 37.67 seconds
maximum resident set size: 408,387,584 bytes
```

The exact Product reproducer argv was also exercised with a valid XSH program
on stdin:

```text
cargo run --quiet --locked --bin xsh -- /dev/stdin
stdin: proc main() -> Result[Unit] { print "factory-reproducer-ok" }
exit: 0
stdout: factory-reproducer-ok\n
stderr: empty
```

Ignored tests retain their product-owned reasons; the Factory did not promote them to passes. This
measurement is a host prerequisite and sizing observation, not evidence for any future candidate:
Engineering hard validation and Quality must each rerun the same profile on the exact candidate
tree.

## Forum scale and diagnostics

The PostgreSQL Forum judge constructs its corpus only through typed Forum mutation authority. The
scale case deliberately contains many ordinary posts and a few selective terms. It must establish
all of the following:

- `websearch_to_tsquery('simple', ...)` finds the same required records when the unquoted query
  terms are reversed;
- the result bound, snippets, stable cursor, and zero-write read/search rules still hold under the
  corpus;
- the selective post query has a `forum_posts_search_gin` plan; and
- the post table's generated `simple` tsvector, rather than a copied search document relation,
  remains the indexed source of truth.

The test intentionally has no fixed millisecond or byte ceiling. Those values vary substantially
with PostgreSQL configuration, cache warmth, and the machine. Before the paid campaign, record the
following read-only diagnostics against the disposable test database after the scale test has
populated it:

```sql
EXPLAIN (ANALYZE, BUFFERS, SETTINGS, SUMMARY)
SELECT id
FROM factory.forum_posts
WHERE search_vector @@ websearch_to_tsquery('simple', 'unique scale marker');

SELECT
  pg_relation_size('factory.forum_posts_search_gin'::regclass) AS post_gin_bytes,
  pg_relation_size('factory.forum_posts'::regclass) AS post_table_bytes,
  pg_total_relation_size('factory.forum_posts'::regclass) AS post_total_bytes;
```

Capture the `Execution Time`, planning time, buffer hits/reads, and three byte values in the dry-run
evidence. The plan should use the post GIN index for the selective query. A sequential scan after
`ANALYZE` and a warm cache is a diagnosis to resolve, not a reason to silently weaken the query-plan
judge.

For a same-session memory observation, an operator with the necessary PostgreSQL privilege may run
this _after_ the `EXPLAIN` in that same `psql` connection:

```sql
SELECT sum(total_bytes) AS total_bytes, sum(used_bytes) AS used_bytes
FROM pg_backend_memory_contexts;
```

This is diagnostic-only. It must not become a privileged daemon requirement, a persisted metric, or
a fixed memory limit until a measured environment demands one.

## Transcript and write-amplification evidence

The 1,000-event fake actor session writes 1,000 NDJSON records, compresses them as the ordinary
actor transcript, seals the one gzip artifact, and finishes through the real inherited descriptor
protocol. The PostgreSQL assertion records the pre- and post-session fact counts and permits only
the named session, artifact, and audit facts. It rejects a design that adds one row for each Pi
event or one row for each required-read observation.

The assertion is deliberately about PostgreSQL facts, not compressed byte size. Content compression
is workload dependent; its acceptance fact is that the complete stream is one sealed CAS artifact
and not a database event log.

## Failure-drill matrix

Run the focused judges below before relying on their wider `make` targets. All are provider-free and
operate only on synthetic state.

| Drill                             | Focused judge                                                                                                                                                       | Required result                                                                                                                                              |
| --------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Daemon crash / partial transcript | `cargo test -p factory-kernel --test process_lifecycle daemon_restart_reconciles_exact_group_and_freezes_unknown_cost_without_resume -- --ignored --test-threads=1` | Reconcile only the recorded PGID, preserve a structurally readable partial transcript when available, mark cost unknown, freeze admission, and never resume. |
| Explicit cancellation             | `cargo test -p factory-kernel --test process_lifecycle tranche5_lifecycle_judges -- --ignored --test-threads=1`                                                     | Cancellation refuses a running session until process custody ends, then records a terminal campaign without rolling back evidence.                           |
| Unknown cost                      | `cargo test -p factory-kernel --test session_runtime real_deno_fake_actor_unknown_cost_fails_closed_without_a_resume -- --ignored --test-threads=1`                 | One fresh actor session becomes terminal, campaign cost is `Unknown`, and no second paid admission succeeds.                                                 |
| 1,000 transcript events           | `cargo test -p factory-kernel --test session_runtime real_deno_fake_actor_with_one_thousand_events_has_bounded_postgres_writes -- --ignored --test-threads=1`       | Real Deno fake actor seals 1,000 NDJSON events and the durable row delta stays bounded.                                                                      |
| Dirty product checkout            | `cargo test -p factory-kernel --test git_custody qualification_rejects_dirty_and_moved_primary_head`                                                                | Qualification fails closed before a worktree or candidate is created.                                                                                        |
| Moved product head                | `cargo test -p factory-kernel --test git_custody qualification_rejects_dirty_and_moved_primary_head`                                                                | A snapshot cannot be reused after the default-branch head changes.                                                                                           |

The PostgreSQL-focused rows require `FACTORY_TEST_DATABASE_URL` to be set to the disposable database
described above. The Git judge does not require PostgreSQL.

## Fake full-workflow acceptance outline

The final provider-free vertical judge must use fake actors and a synthetic local Git repository. It
should assemble the already-defined transitions in this order:

1. Start a clean application revision and one campaign with a `$0.50` aggregate _test_ cap; create
   no Product seed ticket.
2. Run a fake Product session that submits a bounded proposal whose kernel-run reproducer fails
   twice, performs the duplicate query, and then completes.
3. Have a fake external Architect sponsor that exact immutable ticket revision.
4. Claim it on the current synthetic head; have a fake Engineering session submit a regression
   checkpoint that fails and a candidate tree that passes the exact reproducer and hard full suite.
5. Materialize the candidate into a fresh fake Quality workspace. Run its kernel-owned full suite,
   submit an independent `accept` review, and discard any Quality workspace edits.
6. Have the fake Architect accept the exact candidate; construct the provenance-bearing commit and
   guardedly fast-forward the synthetic local main branch. Assert that no Git remote command occurs.
7. Inspect only status/audit/CAS rows and prove the required reads, ticket evidence, validation
   receipts, review, decision, delivery, known aggregate cost, and office/session cost breakdown
   explain the delivered commit.

This is an acceptance outline, not a substitute implementation or a mock workflow engine. At the
time of this record, the repository does **not** yet provide a single executable Product-to-delivery
fake-workflow harness: Tranche 6 Product ticket submission/requalification and Tranche 8
candidate/review/Architect/delivery transports must first expose their typed transitions. The final
judge must call those real transitions directly; it must not fabricate SQL rows or add a parallel
test-only lifecycle.

## Paid MVP preflight checklist

Complete this checklist only after the entire provider-free suite above is green and the vertical
fake-workflow judge exists and passes.

- [ ] The V3 checkout is clean and contains no V1/V2 code or imported state.
- [ ] `../xsh` is clean on the intended local default branch; its commit and tree identity are
      recorded before campaign start.
- [ ] PostgreSQL 18 is reachable through an already-created dedicated V3 database. No test database
      is reused as live factory state.
- [ ] The daemon is stopped before any manual build/schema change; the installed Rust binary, Deno
      executable/version, `deno.json`, `deno.lock`, Pi SDK version, resolved dependency graph,
      schema identity, and `KernelBuildId` have been qualified together.
- [ ] The immutable Deno cache has been populated and verified with `--frozen --cached-only`; no
      `node_modules` directory or Node command is in the path.
- [ ] A database/CAS backup pair and installed-build manifest are retained.
- [ ] The runtime root is factory-owned, the operator socket is local mode `0600`, and no earlier
      daemon or child process is alive.
- [ ] The actual provider credentials are available through the configured explicit source. Their
      values have not been copied into the database, CAS, prompt, transcript, shell history, or this
      request.
- [ ] The active XSH application revision pins exact provider/model descriptors, prices, prompts,
      tools, required reads, repository binding, and `cargo test` plus
      `git diff --check <base> <candidate>` validation argv.
- [ ] A live buffer may contain only independently discovered V3 proposals; the Product assignment
      itself has no seeded task.
- [ ] The campaign budget is exactly `500000` micro-USD, its delivery target is exactly `1`, and no
      other paid session or nonterminal campaign exists.
- [ ] The external Architect is available to make both typed decisions: sponsorship and final
      delivery/rework/reject. It understands that qualitative override cannot bypass hard evidence
      or aggregate/unknown-cost failures.
- [ ] The operator has reviewed the cooperative same-user trust boundary and accepts that it is not
      an adversarial sandbox.

## Exact MVP campaign request record

Fill this record before issuing the eventual typed `factoryctl campaign start` command. It is a
human authorization record, not an additional durable object and not a source of authority over the
kernel's own checks.

```text
Factory V3 MVP campaign request

Requested at (UTC):
Operator / external Grand Architect principal:
Purpose: Produce exactly one local XSH behavior-defect commit discovered by Product.

Application key and immutable revision: xsh /
Installed KernelBuildId:
Dedicated PostgreSQL database identity (no credential):
Runtime root:
Product checkout canonical path: ../xsh
Local default branch:
Recorded base commit:
Recorded base tree:

Aggregate Pi cost cap (micro-USD): 500000
Delivery target: 1
Wall deadline (UTC):
Concurrent paid sessions allowed: 1 (MVP fixed limit)
Remote Git operation authorized: no
Imported V1/V2 state authorized: no
Seeded Product task authorized: no

Pinned provider/model profiles reviewed (Product, Engineering, Quality):
Required XSH reads and full-suite argv reviewed:
Provider-free qualification receipt and date:
Forum scale diagnostic evidence location:
Backup/CAS/installed-build manifest location:

Architect sponsorship authority confirmed: yes/no
Architect final delivery/rework/reject authority confirmed: yes/no
One-rework maximum acknowledged: yes/no
Unknown-cost and hard-gate non-override acknowledged: yes/no
Cooperative same-user boundary acknowledged: yes/no

Authorization signature or durable external decision reference:
```

The subsequent campaign start must pass its own exact expected revision, application, kernel build,
repository, deadline, budget, and singleton-WIP checks. If any item differs from this record, stop
and prepare a new record; do not amend a running campaign.
