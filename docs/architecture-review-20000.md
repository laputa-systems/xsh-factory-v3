# Architecture review at the 20,000-line diagnostic

`PLAN.md` requires an explicit architecture review when non-test Rust crosses
20,000 lines before the complete provider-free product vertical exists. This
review was triggered on 2026-08-12. The line threshold is diagnostic, not a
revised budget and not evidence that more layers are acceptable.

## Measured shape

At review time the four-crate workspace contained roughly 32,000 Rust code
lines under `crates/*/src`, including substantial inline `#[cfg(test)]` judges.
A conservative prefix count that excludes ordinary end-of-file test modules
still exceeded 20,000. PostgreSQL remained at exactly 20 Factory tables. The
dependency set, one-daemon/one-actor topology, local Unix/FD0 transports, and
no-HTTP boundary had not expanded.

The dominant production files were the concrete authorities themselves:
`decision_store.rs`, `process.rs`, `command_supervision.rs`, `git.rs`,
`local_transport.rs`, and `ticket_store.rs`. Their size comes mainly from
closed typed transitions, explicit SQL, process/Git failure handling, and
evidence validation. There is no ORM, workflow engine, dependency-injection
container, generated protocol layer, internal bus, or general repository /
service / handler stack to remove.

## Findings and simplifications

- The in-memory `ForumLedger` was a second test-only implementation beside
  the real SQLx-backed `ForumStore`. It and its detached transition test were
  deleted. Provider-free Forum behavior is now judged through the same typed
  PostgreSQL authority used in production.
- Regression checkpoint provenance extends `factory.candidates` rather than
  creating a checkpoint table. Application activation likewise adds a closed
  column/index rather than an application-policy subsystem.
- `scheduler.rs` remains a pure priority function plus direct typed mutation
  calls. It must not grow a generic workflow or persisted waiting-state
  engine.
- `ArchitectTransitionResolver` and
  `CandidateQualityAuthorityResolver` are accepted only if the resident daemon
  gains concrete trusted implementations before the provider-free vertical.
  “Unavailable” is a useful fail-closed test, but is not a production
  implementation and cannot justify a permanent trait by itself.
- Transport operation matches may stay repetitive because each closed wire
  shape is an authority boundary. A generic JSON command registry would save
  lines by weakening the contract and is rejected.
- SQLx transition code may stay explicit. Hiding it behind an ORM, generic
  aggregate repository, or event-sourcing framework would increase conceptual
  size while obscuring transaction and write-amplification evidence.

## Gate on further implementation

New work after this review must close an existing acceptance path: installed
runtime recovery, application/campaign operator control, concrete scheduling
and session composition, or the single provider-free vertical. It may not add
a crate, table, resident service, generic trait, compatibility layer, or
parallel test implementation. The review is complete only when the vertical
uses these exact authorities end to end and remaining unused seams are removed.
