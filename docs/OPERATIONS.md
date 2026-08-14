# Operations

## Build and qualification

Use the local `pi-agent-core-rs` checkout at
`/Users/josh/d/pi-agent-core-rs`. It supplies the reviewed Rust agent crates;
factory never calls a system Pi binary or resolves runtime code through a
package registry.

```sh
make cache
make lint
```

`make cache` fetches the two locked Cargo workspaces. `make lint` is the Grand
Architect's pre-commit gate: it runs the local `pi-agent-core-rs` tests,
formats and checks Rust source, and runs the Factory workspace tests. Neither
starts a provider-backed actor or uses remote Git.

## Initialize and serve

The database must be already created and dedicated to this factory. Choose a
runtime root outside source control, initialize it, then serve with explicit
paths:

```sh
factoryctl init \
  --database-url 'postgresql://USER@localhost/factory_v3' \
  --runtime-root /absolute/path/to/factory-runtime

FACTORY_DATABASE_URL='postgresql://USER@localhost/factory_v3' \
FACTORY_RUNTIME_ROOT=/absolute/path/to/factory-runtime \
make factoryd-serve
```

The serve target runs `vault OPENROUTER_API_KEY -- target/release/factoryd
serve ...`; the credential is neither copied into source, the database, CAS,
prompts, nor shell command arguments. It binds a mode-`0600` Unix operator
socket under the runtime root. Stop the daemon before replacing a kernel,
schema, Rust dependency source, or agent-core build input, then run `factoryctl init`
again before serving the new build.

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

## One-commit paid cycle

The exact request for a bounded paid run is encoded by `make paid-cycle`. It
admits one provider-backed campaign with `--delivery-target 1`, after checking
that the installed `factoryctl` exists, the operator socket is live, the
product checkout is clean, and the application revision, budget, and deadline
were supplied explicitly. The target does not make the two Architect decisions
or bypass Product, Engineering, or Quality; those remain durable lifecycle
gates in the daemon.

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
