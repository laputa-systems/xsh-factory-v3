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

For an already-created dedicated PostgreSQL 18 database, initialization is a bounded one-shot. The
operator supplies the exact installed `factoryd` binary and every kernel/runtime identity input;
the child applies/validates migrations, creates or validates the runtime root, seals the
qualification receipt, records the build, and exits without binding a socket. It never creates or
drops a database. The `factoryctl init` command synopsis lists the closed required inputs.
The MVP also registers exactly one OpenRouter credential *environment name* (for example,
`openrouter=OPENROUTER_API_KEY`) in that sealed receipt. Its value is never read during init or
stored in PostgreSQL/CAS; it reaches a host only at the later supervised spawn boundary.

`--pi-host-source-root` is a closed source root, not a convenience include path. Init recursively
inventories every regular file beneath it and requires one `--pi-host-source-file` declaration for
each; symlinks and any omitted file reject qualification. Use the repository's `packages/` root so
both `factory-pi-host` and `factory-sdk` local imports are inside that inventory. Keep `deno.json`,
`deno.lock`, the qualified Deno module-graph digest, and the build-specific `DENO_DIR` outside
that root;
the last is retained runtime material, never host source.

The following is the complete MVP initialization/preflight shape. Replace each `<…>` value with an
absolute local path; `factoryctl` derives `--kernel-binary` from the exact `--factoryd` executable,
so it must not be supplied separately. The host source root is `packages`, not
`packages/factory-pi-host`: `main.ts` imports `factory-sdk`, and qualification inventories every
regular file under the chosen root. The nine runtime modules (`main`, `entrypoint`, `host`,
`framed-actor`, `sdk-factory`, `transcript`, `types`, `workspace-tools`, and `forum-tools`) are
therefore included along with `factory-sdk` and the package tests/metadata that the closed-root
inventory also requires.

```sh
factoryctl init \
  --factoryd <repo-root>/target/release/factoryd \
  --database-url 'postgresql://USER@localhost/factory_v3' \
  --runtime-root <absolute-runtime-root> \
  --kernel-source-root <repo-root> \
  --kernel-source-file crates/factory-protocol/src/application.rs \
  --kernel-source-file crates/factory-protocol/src/candidate.rs \
  --kernel-source-file crates/factory-protocol/src/decision.rs \
  --kernel-source-file crates/factory-protocol/src/error.rs \
  --kernel-source-file crates/factory-protocol/src/forum.rs \
  --kernel-source-file crates/factory-protocol/src/identifier.rs \
  --kernel-source-file crates/factory-protocol/src/lib.rs \
  --kernel-source-file crates/factory-protocol/src/path.rs \
  --kernel-source-file crates/factory-protocol/src/process.rs \
  --kernel-source-file crates/factory-protocol/src/revision.rs \
  --kernel-source-file crates/factory-protocol/src/state.rs \
  --kernel-source-file crates/factory-protocol/src/ticket.rs \
  --kernel-source-file crates/factory-protocol/src/value.rs \
  --kernel-source-file crates/factory-protocol/src/wire.rs \
  --kernel-source-file crates/factory-kernel/src/application_activation.rs \
  --kernel-source-file crates/factory-kernel/src/application_admission.rs \
  --kernel-source-file crates/factory-kernel/src/application_rpc.rs \
  --kernel-source-file crates/factory-kernel/src/assignment_runtime.rs \
  --kernel-source-file crates/factory-kernel/src/campaign_driver.rs \
  --kernel-source-file crates/factory-kernel/src/candidate_runtime.rs \
  --kernel-source-file crates/factory-kernel/src/cas.rs \
  --kernel-source-file crates/factory-kernel/src/command_supervision.rs \
  --kernel-source-file crates/factory-kernel/src/decision_store.rs \
  --kernel-source-file crates/factory-kernel/src/durable_authority.rs \
  --kernel-source-file crates/factory-kernel/src/forum_rpc.rs \
  --kernel-source-file crates/factory-kernel/src/forum_store.rs \
  --kernel-source-file crates/factory-kernel/src/git.rs \
  --kernel-source-file crates/factory-kernel/src/installed_runtime.rs \
  --kernel-source-file crates/factory-kernel/src/lib.rs \
  --kernel-source-file crates/factory-kernel/src/local_transport.rs \
  --kernel-source-file crates/factory-kernel/src/operator_artifact_rpc.rs \
  --kernel-source-file crates/factory-kernel/src/operator_forum_rpc.rs \
  --kernel-source-file crates/factory-kernel/src/operator_navigation.rs \
  --kernel-source-file crates/factory-kernel/src/operator_rpc.rs \
  --kernel-source-file crates/factory-kernel/src/process.rs \
  --kernel-source-file crates/factory-kernel/src/process_custody.rs \
  --kernel-source-file crates/factory-kernel/src/product_runtime.rs \
  --kernel-source-file crates/factory-kernel/src/restart_recovery.rs \
  --kernel-source-file crates/factory-kernel/src/scheduler.rs \
  --kernel-source-file crates/factory-kernel/src/session_runtime.rs \
  --kernel-source-file crates/factory-kernel/src/storage.rs \
  --kernel-source-file crates/factory-kernel/src/ticket_store.rs \
  --kernel-source-file crates/factory-kernel/src/workspace_read.rs \
  --kernel-source-file crates/factoryd/src/main.rs \
  --cargo-executable <absolute-cargo> \
  --git-executable <absolute-git> \
  --deno-executable <absolute-deno-2.9.4> \
  --pi-host-source-root <repo-root>/packages \
  --pi-host-source-file factory-pi-host/candidate_checkpoint_tool_test.ts \
  --pi-host-source-file factory-pi-host/deno.json \
  --pi-host-source-file factory-pi-host/entrypoint.ts \
  --pi-host-source-file factory-pi-host/forum-tools.ts \
  --pi-host-source-file factory-pi-host/framed-actor.ts \
  --pi-host-source-file factory-pi-host/framed_actor_test.ts \
  --pi-host-source-file factory-pi-host/host.ts \
  --pi-host-source-file factory-pi-host/host_test.ts \
  --pi-host-source-file factory-pi-host/main.ts \
  --pi-host-source-file factory-pi-host/mod.ts \
  --pi-host-source-file factory-pi-host/sdk-factory.ts \
  --pi-host-source-file factory-pi-host/transcript.ts \
  --pi-host-source-file factory-pi-host/types.ts \
  --pi-host-source-file factory-pi-host/workspace-tools.ts \
  --pi-host-source-file factory-sdk/application-operator.ts \
  --pi-host-source-file factory-sdk/application.ts \
  --pi-host-source-file factory-sdk/architect.ts \
  --pi-host-source-file factory-sdk/architect_test.ts \
  --pi-host-source-file factory-sdk/candidate.ts \
  --pi-host-source-file factory-sdk/candidate_test.ts \
  --pi-host-source-file factory-sdk/compiler.ts \
  --pi-host-source-file factory-sdk/deno.json \
  --pi-host-source-file factory-sdk/forum.ts \
  --pi-host-source-file factory-sdk/forum_test.ts \
  --pi-host-source-file factory-sdk/mod.ts \
  --pi-host-source-file factory-sdk/operator-artifact.ts \
  --pi-host-source-file factory-sdk/operator-artifact_test.ts \
  --pi-host-source-file factory-sdk/operator.ts \
  --pi-host-source-file factory-sdk/operator_test.ts \
  --pi-host-source-file factory-sdk/product.ts \
  --pi-host-source-file factory-sdk/product_test.ts \
  --pi-host-source-file factory-sdk/protocol.ts \
  --pi-host-source-file factory-sdk/quality.ts \
  --pi-host-source-file factory-sdk/quality_test.ts \
  --pi-host-entrypoint <repo-root>/packages/factory-pi-host/main.ts \
  --pi-host-cache-probe factory-pi-host/mod.ts \
  --deno-config <repo-root>/deno.json \
  --deno-lock <repo-root>/deno.lock \
  --deno-dir <absolute-build-specific-deno-dir-outside-packages> \
  --pi-version 0.84.1 \
  --provider-credential-environment openrouter=OPENROUTER_API_KEY
```

Before `init`, populate that exact `DENO_DIR` with the frozen root lock. Init records Deno's
actual canonical `info --json` module graph while the daemon is stopped. Every later preflight
uses the inert `factory-pi-host/mod.ts` module with Deno's supported cached-only graph/typecheck
command; it never executes the FD0 actor host or contacts a provider:

```sh
DENO_DIR=<absolute-build-specific-deno-dir-outside-packages> \
  <absolute-deno-2.9.4> cache --frozen --config <repo-root>/deno.json \
  <repo-root>/packages/factory-pi-host/main.ts

DENO_DIR=<absolute-build-specific-deno-dir-outside-packages> \
  <absolute-deno-2.9.4> run --check --frozen --cached-only --config <repo-root>/deno.json \
  --lock <repo-root>/deno.lock <repo-root>/packages/factory-pi-host/mod.ts
```

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
