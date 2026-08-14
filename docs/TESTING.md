# Testing and qualification

## Grand Architect qualification

`make lint` is the single aggressive source qualification before the Grand
Architect commits factory changes. It tests the local `pi-agent-core-rs`
workspace, formats Rust source, applies Rust clippy fixes, checks the Factory
workspace, and runs its unit/integration tests without provider calls or
remote Git. Use focused `cargo test` while changing one boundary; do not use
`make lint` as the ordinary inner-loop command.

The Rust agent core has focused event-observer and trace coverage in its own
workspace. The Factory host has projection, framed-protocol, and policy bridge
coverage in `crates/factory-pi-host`.

## PostgreSQL and SQLx checks

Database tests are opt-in. Use a disposable PostgreSQL 18 database named
exactly `factory_test_v3_<digits>`:

```sh
FACTORY_TEST_DATABASE_URL='postgresql://USER@localhost/factory_test_v3_123' \
  make postgres-test

DATABASE_URL='postgresql://USER@localhost/factory_test_v3_123' make sqlx-check
```

The name guard prevents using an operator database. Fixed SQL depends on the
committed `.sqlx` metadata; change it together with any modified query or
migration.

## Complete provider-free acceptance

`make pi-agent-core-rs-acceptance` composes ordinary checks, PostgreSQL
authority,
candidate/delivery transitions, real XSH bundle admission, generic vertical
flow, and backup/restore qualification. It requires distinct externally
created disposable databases and explicit backup variables; see
[`provider-free-dry-run.md`](provider-free-dry-run.md) for the exact inputs.

The gate is the complete Rust runtime qualification, not a partial host test.
It proves the pinned local core checkout and toolchain, the installed host
receipt, deterministic V2 bundle compilation, inherited-descriptor packet
verification, exact role-policy/tool allowlists, required-read accounting,
terminal reconciliation, cancellation, unknown-cost rejection, denied
capabilities, the generic Product → Engineering → Quality → delivery flow,
real-XSH bundle admission, SQLx metadata, and backup/restore integrity. The
qualified source, build, installation, and operational paths are all Rust-only
inputs to this gate.

Provider-free success proves deterministic infrastructure. It does not prove a
model will discover a valuable defect or that a paid cycle will complete; live
campaign evidence remains required.
