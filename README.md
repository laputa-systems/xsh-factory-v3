# Factory V3

Factory V3 is a cleanroom local factory whose first application is improving XSH. Its purpose is to
turn a reproducible, user-visible product defect into an independently reviewed, fully tested,
provenance-bearing local XSH commit at bounded provider cost. It is neither a port of Factory V1 nor
Factory V2; it imports no state or code from either.

The implementation is intentionally staged. Tranches 1–5 establish the trusted contracts,
PostgreSQL authority transitions, append-only CAS custody, Forum, framed daemon/SDK protocol,
installed Deno/Pi runtime identity, one-session process custody, aggregate-cost accounting, and
restart recovery. All of those layers have provider-free judges; no paid campaign is available
until the Product, Engineering, Quality, and delivery tranches are qualified.

## Architecture

The Rust kernel is the sole future authority for durable identity, lifecycle, cost admission,
artifact custody, validation, Git construction, and local delivery. `applications/xsh` is a one-way
Deno consumer of `@factory/sdk` that declares only closed product policy and prompt-template paths.
It does not run inside the daemon.

The organizational metaphor is visible to operators, not to model workers. The XSH mission and all
six office templates speak only about the product assignment, its evidence, and its allowed tools;
they deliberately contain no factory, office, director, Architect, campaign, sponsorship, or
kernel-persona vocabulary. A compile-and-render test guards that boundary.

Read the durable terms and limits in:

- [architecture glossary](docs/architecture-glossary.md)
- [trust assumptions](docs/trust-assumptions.md)
- [repository boundary](docs/repository-boundary.md)
- [implementation plan](PLAN.md)
- [deferred V1 backlog](V1.md)

## Cooperative host boundary

Actors run as ordinary processes under the operator's OS account. This is a cooperative
boundary, not an adversarial sandbox: the kernel prevents accidental and protocol-level authority
confusion and verifies accepted evidence, but it does not claim that a malicious same-user process
cannot read credentials, change unrelated host files, or signal another same-user process. No
container, VM, network mediation, or credential broker is implied.

## Local daemon transport

`factoryd` serves only a Unix-domain operator socket beneath an explicit runtime root; it never
opens a TCP or HTTP port. Startup verifies the installed schema, acquires both the runtime-root
filesystem singleton and PostgreSQL singleton lock, then creates `factoryd.operator.sock` with mode
`0600`. After bootstrap, the database URL remains in the daemon process and is never accepted by an
application or actor host. `factoryctl` never opens SQL; its sole database argument is forwarded
unchanged to one exact `factoryd init` child during bootstrap.

For an already-created dedicated PostgreSQL 18 database, initialization is one bounded command:

```sh
factoryctl init \
  --database-url 'postgresql://USER@localhost/factory_v3' \
  --runtime-root /absolute/path/to/factory-runtime
```

The release supplies the rest. `factoryctl` discovers its installation root, selects its sibling
`factoryd`, resolves the installed Cargo, Git, and pinned Deno executables, inventories the Rust
kernel, migrations, SQLx metadata, Pi host, and TypeScript SDK, and selects the checked-in config,
lock, entrypoint, cache probe, and Pi SDK version. `factoryd init` creates the build-specific Deno
cache beneath the runtime root, qualifies the resolved module graph, seals the complete receipt,
applies or verifies migrations, records the installed build, and exits without binding a socket.
It never creates or drops a database and never contacts a model provider.

The default credential descriptor is `openrouter=OPENROUTER_API_KEY`. Init records only that
environment-variable name; it neither reads nor persists the value. A nonstandard packaged layout
may use `--installation-root` and `--factoryd`, and another OpenRouter environment name may use
`--provider-credential-environment`. These are installation overrides, not a source-manifest API.

The following concise bootstrap outline uses the identities returned by each
read-only probe or receipt as the next command's guards (`jq` is used only to
extract fields). Run it from the installation root after compiling
`applications/xsh/bundle.v1.json` as shown below:

```sh
DATABASE_URL='postgresql://USER@localhost/factory_v3'
RUNTIME_ROOT=/absolute/path/to/factory-runtime
SOCKET="$RUNTIME_ROOT/factoryd.operator.sock"

factoryctl init --database-url "$DATABASE_URL" --runtime-root "$RUNTIME_ROOT"
FACTORY_DATABASE_URL="$DATABASE_URL" FACTORY_RUNTIME_ROOT="$RUNTIME_ROOT" \
  make factoryd-serve &
DAEMON_PID=$!
trap 'kill "$DAEMON_PID"' EXIT

STATUS=$(factoryctl daemon status --socket "$SOCKET" --format json)
BUILD_ID=$(printf '%s' "$STATUS" | jq -r .current_kernel_build_id)
BUILD_REVISION=$(printf '%s' "$STATUS" | jq -r .aggregate_revision)

REGISTER=$(factoryctl application register --socket "$SOCKET" \
  --client-command-id register-xsh-1 --expected-revision 0 \
  --expected-kernel-build-revision "$BUILD_REVISION" --kernel-build-id "$BUILD_ID" \
  --source-root "$PWD/applications/xsh" --bundle-relative-path bundle.v1.json \
  --principal operator --format json)
APPLICATION_ID=$(printf '%s' "$REGISTER" | jq -r .application_revision_id)
APPLICATION_REVISION=$(printf '%s' "$REGISTER" | jq -r .aggregate_revision)

printf '%s\n' 'Activate the locally qualified XSH application.' > "$RUNTIME_ROOT/activate.md"
RATIONALE=$(factoryctl artifact seal --socket "$SOCKET" \
  --client-command-id seal-activation-1 \
  --expected-kernel-build-revision "$BUILD_REVISION" \
  --source-root "$RUNTIME_ROOT" --source-relative-path activate.md \
  --principal operator --format json)
ACTIVATE=$(factoryctl application activate xsh "$APPLICATION_ID" --socket "$SOCKET" \
  --client-command-id activate-xsh-1 --expected-revision "$APPLICATION_REVISION" \
  --rationale-artifact-id "$(printf '%s' "$RATIONALE" | jq -r .artifact_id)" \
  --rationale-digest "$(printf '%s' "$RATIONALE" | jq -r .digest)" \
  --rationale-byte-length "$(printf '%s' "$RATIONALE" | jq -r .byte_length)" \
  --principal operator --format json)

factoryctl campaign start --socket "$SOCKET" \
  --client-command-id campaign-1 \
  --application-revision-id "$APPLICATION_ID" \
  --expected-application-revision "$(printf '%s' "$ACTIVATE" | jq -r .aggregate_revision)" \
  --aggregate-budget-micro-usd 1000000 \
  --deadline-unix-millis "$((($(date +%s) + 3600) * 1000))" \
  --delivery-target 1 --principal operator --format json
```

Actor hosts do not reconnect to that socket path. The daemon creates their connected Unix
descriptor and retains an immutable kernel binding for the exact session, office, assignment,
campaign, and application revision. Dropping that descriptor is a liveness event, never an implied
successful command.

## Local provider-free checks

The pinned toolchains are Rust `nightly-2026-07-24` and Deno `2.9.4`
([`.deno-version`](.deno-version)). The repository uses only the root
`Cargo.lock` and `deno.lock`; there is no Node toolchain or `node_modules`
directory.

Populate Deno's frozen cache once after installation:

```sh
make cache
```

Run all provider-free formatting, compile, boundary, and unit checks:

```sh
make check
```

These commands must not invoke a Pi/model provider or a remote Git operation. They are local
qualification only and do not start a factory campaign.

## Compile the XSH application bundle

The application package is compiled outside the daemon. This Deno-only command validates all seven
declared Markdown templates and writes the canonical, inert bundle bytes to stdout:

```sh
deno run --allow-read --no-prompt --frozen --cached-only applications/xsh/mod.ts \
  > applications/xsh/bundle.v1.json
```

Run it twice and compare the outputs before registration. `factoryctl application register` sends
only the explicit source root and bundle-relative path to the local daemon; Rust re-reads the bundle
and every declared template, verifies their BLAKE3 identities, and adopts them into CAS. Neither the
daemon nor a Pi session imports or evaluates `applications/xsh`.

The first registration for a repository also creates the immutable repository binding declared by
the admitted bundle in the same transaction and with its own audit receipt. Later application
revisions must name the exact same repository key, canonical path, branch, and delivery mode.

The generated `applications/xsh/bundle.v1.json` is ignored installation output. Register it with
the canonical `applications/xsh` directory as `--source-root` and `bundle.v1.json` as
`--bundle-relative-path`, so every declared `templates/*.md` path remains beneath that same root.

## PostgreSQL 18 integration and SQLx metadata

The durable-storage tests are opt-in because they require a disposable local PostgreSQL 18 database.
Create one named exactly `factory_test_v3_<digits>`, then run:

```sh
FACTORY_TEST_DATABASE_URL=postgresql://USER@localhost/factory_test_v3_<digits> make postgres-test
```

Fixed SQL uses committed [`.sqlx`](.sqlx) metadata, so ordinary Rust checks work without a database.
After changing a fixed query or migration, use matching external `sqlx-cli 0.9.0` against the same
disposable PostgreSQL 18 database:

```sh
DATABASE_URL=postgresql://USER@localhost/factory_test_v3_<digits> make sqlx-check
```
