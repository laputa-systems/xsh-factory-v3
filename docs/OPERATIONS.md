# Operations

## Build and qualification

Use the checked-out `vendor/pi-headless` submodule. It supplies reviewed local
headless ESM artifacts and a checked-in provider catalog; factory never calls a
system Pi binary or resolves Pi through Deno/NPM at runtime.

```sh
make cache
make lint
```

`make cache` runs the submodule's locked dependency installation with lifecycle
scripts disabled, builds local headless artifacts, and populates Deno's frozen
cache. `make lint` is the Grand Architect's pre-commit gate: it rebuilds the
local artifacts, formats and fixes Rust/Deno source, runs Rust and Deno checks,
and runs their tests. Neither starts a provider-backed actor or uses remote Git.

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
schema, Deno graph, or Pi submodule build input, then run `factoryctl init`
again before serving the new build.

## Application revisions and campaigns

Compile `applications/xsh/bundle.v1.json`, then use `factoryctl daemon status`
to obtain the installed build ID/revision. Register with those exact guards,
seal a short operator rationale, and activate only while no campaign is
running. Start campaigns through `factoryctl campaign start` with an explicit
active application revision, aggregate micro-USD budget, deadline, and delivery
target. Do not launch Pi or an actor host directly.

Every mutation uses a client command ID and observed aggregate revision.
`factoryctl campaign status`, `ticket list/show`, `candidate show`, and `audit
show` are the navigation surface. The Architect must supply sealed rationale
artifacts for sponsorship and final candidate decisions.

## Runtime hygiene

Never delete a live runtime root, broad worktree tree, or CAS object tree.
Terminal worktrees/staging are controller-owned transient material; artifacts
referenced from PostgreSQL are durable. There is not yet a supported CAS GC, so
monitor disk use and preserve the runtime/database pair until a reference-safe
collector exists.
