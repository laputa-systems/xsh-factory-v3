# Operations

## Build and qualification

Use the local `pi-agent-core-rs` checkout at
`/Users/josh/d/pi-agent-core-rs`. It supplies the reviewed Rust agent crates;
factory never calls a system Pi binary or resolves runtime code through a
package registry.

```sh
make cache
make lint
make release-build
```

`make release-build` first fetches both locked workspaces, then builds
`factoryd`, `factoryctl`, and `factory-pi-host` from the same release workspace.
`factoryctl init` selects the actor host from the same Cargo profile as the
selected `factoryd`, preventing mixed debug and release protocol binaries from
entering an installed runtime.

`make cache` fetches the two locked Cargo workspaces. `make lint` is the Grand
Architect's pre-commit gate: it runs the local `pi-agent-core-rs` tests,
formats and checks Rust source, and runs the Factory workspace tests. Neither
starts a provider-backed actor or uses remote Git. The shared toolchain is the
pinned `nightly-2026-07-24`; qualification also refuses a missing or dirty
`/Users/josh/d/pi-agent-core-rs` checkout and records its exact source
identity.

## Initialize and serve

The database must be already created and dedicated to this factory. Choose a
runtime root outside source control, initialize it, then serve with explicit
paths:

For the ordinary Grand Architect path, use the idempotent lifecycle wrapper;
it performs locked dependency fetch, release qualification, initialization
when needed, daemon launch under a tracked process group, readiness polling,
and XSH application activation:

```sh
FACTORY_DATABASE_URL='postgresql://USER@localhost/factory_live_v3' \
FACTORY_RUNTIME_ROOT=/absolute/path/to/factory-runtime \
make factory-start
```

Keep this database/runtime pair for the lifetime of the qualified application
lane. Campaigns are rows in the same durable PostgreSQL authority; a paid cycle
does not create, select, or rotate a database. Do not derive database names or
runtime roots from a campaign number or date, and do not restart the daemon with
a fresh pair merely because the previous campaign reached a terminal state.
That would strand retained ticket and campaign history in an unrelated
authority and make the apparent queue smaller than the durable one.

The live database name is fixed at `factory_live_v3`. The lifecycle guard
rejects cycle-specific names such as `factory_live_v3_20260817_cycle88` before
initialization or service, so every fresh paid cycle stays in the same durable
authority.

If historical cycle databases already exist, renaming one of them is not a
consolidation: it preserves only that database's rows and leaves the other
authorities stranded. Do not drop or splice them manually. A real consolidation
requires a qualified migration of every referenced row, ID relationship,
artifact/CAS identity, and runtime-evidence boundary before the continuous
authority can claim the historical backlog.

Stop the same runtime through the typed operator socket and preserve all durable
factory state:

```sh
FACTORY_DATABASE_URL='postgresql://USER@localhost/factory_live_v3' \
FACTORY_RUNTIME_ROOT=/absolute/path/to/factory-runtime \
make factory-stop
```

The lower-level commands remain useful for diagnostics and deployment work:

```sh
factoryctl init \
  --database-url 'postgresql://USER@localhost/factory_live_v3' \
  --runtime-root /absolute/path/to/factory-runtime

FACTORY_DATABASE_URL='postgresql://USER@localhost/factory_live_v3' \
FACTORY_RUNTIME_ROOT=/absolute/path/to/factory-runtime \
make factoryd-serve
```

The serve target starts `target/release/factoryd serve ...` without an
OpenRouter credential in its environment. Before binding its mode-`0600` Unix
operator socket, the daemon verifies `vault OPENROUTER_API_KEY -- ...` can
provide the required credential; it resolves the credential through Vault again
for every provider-backed assignment. The credential is never copied into
source, the database, CAS, prompts, or shell command arguments. Stop the
daemon before replacing a kernel, schema, Rust dependency source, or
agent-core build input, then run `factoryctl init` again before serving the new
build. A runtime replacement retires the old runtime state: start the new
daemon with a fresh database and runtime root rather than attempting
compatibility recovery or dual dispatch.

Before each paid admission, compare the live daemon/build identity with the
current Factory source checkout and recent campaign diagnostics. Do not commit,
rebuild, or change dependency inputs after qualification while a paid campaign
is relying on that runtime. If the source graph changes, retire the runtime and
repeat initialization and qualification before spending again; an installed
runtime-drift fault is evidence of stale admission inputs, not an Engineering
result to retry in place.

## Application revisions and campaigns

Use the checked-in `applications/xsh/bundle.v2.json`, then use `factoryctl
daemon status` to obtain the installed build ID/revision. XSH's bundle template
paths are relative to `applications/xsh`, so admission uses that directory as
`--source-root` and `bundle.v2.json` as `--bundle-relative-path`; the repository
root is not a valid substitute. Register with those exact guards, seal a short
operator rationale, and activate only while no campaign is running. Start
campaigns through `factoryctl campaign start` with an explicit active
application revision, aggregate micro-USD budget, deadline, and delivery
target. Do not launch Pi or an actor host directly.

Every mutation uses a client command ID and observed aggregate revision.
`factoryctl campaign status`, `ticket list/show`, `candidate show`, and `audit
show` are the navigation surface. The Architect must supply sealed rationale
artifacts for sponsorship and final candidate decisions.

For a compact read-only overview of the live lane, run `make status`. It uses an
explicit `FACTORY_STATUS_SOCKET` when supplied, otherwise the
`FACTORY_RUNTIME_ROOT` environment variable, and finally the runtime root
advertised by a running `factoryd`. It reports the daemon identity, active XSH
revision, complete ticket-state counts, campaign totals, and session/cost
warnings without opening PostgreSQL from the operator shell.
After the aggregate projection, the daemon selects the newest terminal
campaign (completed, failed, or cancelled), reads its session artifact IDs from
PostgreSQL, verifies the corresponding CAS bytes, and writes available
transcripts under the OS temporary directory at
`cycle-<campaign-id>-status/`. Complete archives are named
`session-<id>-transcript.ndjson.gz`; interrupted-session evidence is retained as
`session-<id>-partial.ndjson`. The status output reports the directory and any
terminal sessions for which no transcript artifact exists.

## One-commit paid cycle

The exact request for a bounded paid run is encoded by `make paid-cycle`. Before
admission it runs `make release-build`, recomputes the closed build identity
from the release binaries, Cargo locks, Factory source graph, and local
`pi-agent-core-rs` checkout, and compares that identity with the live daemon.
It fails closed on a stale runtime. Only then does it admit one provider-backed
campaign with `--delivery-target 1`, after checking that the installed
`factoryctl` exists, the operator socket is live, the product checkout is clean,
and the application revision, budget, and deadline were supplied explicitly.
The target does not make the two Architect decisions or bypass Product,
Engineering, or Quality; those remain durable lifecycle gates in the daemon.

This deployment boundary is distinct from campaign turnover: a source/build
change requires a newly qualified runtime, but a new paid campaign on the same
qualified build stays on the existing database. Factory V3 currently has no
supported in-place merger for independent PostgreSQL authorities. Preserve old
databases and their runtime/CAS roots until a dedicated, reference-preserving
migration is qualified; copying ticket rows alone would leave their artifact,
application-revision, attempt, and evidence links invalid.

`factoryctl build identity --format json` is the read-only identity probe used
by that guard. If it differs from `factoryctl daemon status`, stop the daemon,
initialize a fresh database/runtime pair with the newly built release, serve it
with `make factoryd-serve`, and reread the application revision before
requesting another paid cycle.

Supply the values read from the live daemon/application status and choose a
fresh client command ID:

```sh
FACTORY_PAID_CYCLE_SOCKET=/absolute/path/to/factoryd.operator.sock \
FACTORY_PAID_CYCLE_APPLICATION_REVISION_ID=<active-application-revision-id> \
FACTORY_PAID_CYCLE_EXPECTED_APPLICATION_REVISION=<application-revision> \
FACTORY_PAID_CYCLE_BUDGET_MICRO_USD=<aggregate-budget> \
FACTORY_PAID_CYCLE_DEADLINE_UNIX_MILLIS=<future-absolute-deadline> \
FACTORY_PAID_CYCLE_CLIENT_COMMAND_ID=paid-cycle-<unique-id> \
make paid-cycle
```

The terminal proof is separate and explicit. After the Architect delivers,
run `make paid-cycle-verify` with `FACTORY_PAID_CYCLE_SOCKET` and
`FACTORY_PAID_CYCLE_ID`; it requires a completed campaign with exactly one
delivered attempt, a nonempty delivered commit, and a clean `../xsh` `HEAD`
matching that commit. A successful campaign admission alone is not a shipped
commit. `factoryctl campaign status --format json` and
`factoryctl candidate show --format json` expose the exact
`*_factory_cost_micro_usd` value paired with the delivered commit; human
readable output also prints the equivalent six-decimal USD amount.
The kernel also writes the same amount into the delivered commit's
`Factory-Cost: $0.000000` trailer, and `make paid-cycle-verify` checks that
commit-visible proof.

If a campaign terminates without a delivery, an explicit request to “run a
fresh paid cycle” authorizes one new invocation of `make paid-cycle`. Re-read
the live daemon and application state and choose a fresh client command ID;
do not reuse the failed campaign's admission as an idempotent retry. If the
fresh campaign also fails, stop and report it. A further campaign requires a
new explicit fresh-cycle request.

## Runtime hygiene

Never delete a live runtime root, broad worktree tree, or CAS object tree.
Terminal worktrees/staging are controller-owned transient material; artifacts
referenced from PostgreSQL are durable. There is not yet a supported CAS GC, so
monitor disk use and preserve the runtime/database pair until a reference-safe
collector exists.
