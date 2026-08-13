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

For an already-created dedicated PostgreSQL 18 database, initialization is a bounded one-shot. The
operator supplies the exact installed `factoryd` binary and every kernel/runtime identity input;
the child applies/validates migrations, creates or validates the runtime root, seals the
qualification receipt, records the build, and exits without binding a socket. It never creates or
drops a database. The `factoryctl init` command synopsis lists the closed required inputs.
The MVP also registers exactly one OpenRouter credential *environment name* (for example,
`openrouter=OPENROUTER_API_KEY`) in that sealed receipt. Its value is never read during init or
stored in PostgreSQL/CAS; it reaches a host only at the later supervised spawn boundary.

After that succeeds, start the local daemon and issue its read-only typed probe:

```sh
factoryd serve --database-url postgresql://USER@localhost/factory_v3 --runtime-root ./var
factoryctl daemon status --socket ./var/factoryd.operator.sock --format json
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
