# Architecture review before the product vertical slice

This review is the complexity-budget gate required by `PLAN.md` before the
implementation grows through the complete Product, Engineering, Quality, and
delivery path. It does not revise an authority contract. It records the
simplifications that the implementation must preserve while adding Tranches
6 through 9.

## Measured shape at the gate

- The Rust workspace still has exactly four crates.
- PostgreSQL has 15 Factory-owned tables after the ticket-buffer migration.
- The generic Rust source remains below the 20,000 non-test-line review
  threshold, but the remaining vertical slice would cross it without an
  explicit constraint.
- The only resident service is `factoryd`; paid work remains globally serial.
- There is no HTTP stack, ORM, workflow framework, application callback, or
  internal event bus.

The initial migration now records the fixed seven application templates as
named artifact foreign keys on `application_revisions`. The former generic
`application_revision_templates` relation is intentionally absent. With the
three ticket tables added in Tranche 6, the remaining candidate, validation,
review, decision, and delivery tables still fit within the 20-table budget in
`PLAN.md`.

## Required simplifications

1. Keep the seven named application-template artifact foreign keys. The fixed
   office set makes these columns more precise than a generic child collection
   and preserves the planned 20-table maximum.
2. Keep one typed module per physical/domain boundary, not
   repository/service/handler layers. Ticket scheduling is one deterministic
   read/transition module; Git command custody is one module; candidate,
   validation, review, decision, and delivery transitions share the existing
   SQLx transaction helpers.
3. Keep one canonical assignment-packet digest. Persistence may project typed
   fields for SQL eligibility, but it must not compute an independent packet
   seal or accept packet artifact bytes through a second path.
4. Add no durable scheduler polls, process heartbeats, tool events, read
   observations, validation chunks, or generated reports. Derived status and
   pressure remain read-only queries.
5. Do not generalize the behavior-defect ticket, fixed offices, single rework,
   serial paid-session WIP, validation profiles, or guarded fast-forward into
   configurable workflow machinery.
6. Prefer closed structs and direct functions over new traits. Traits remain
   limited to the actual Pi/provider, process, filesystem/Git-command, and
   clock/failure-test boundaries that need provider-free doubles.

## Stop conditions

Pause for another design review rather than expanding the implementation if a
later tranche would require a fifth crate, a twenty-first Factory table, a
second resident process, a new direct dependency, or a generic workflow or
metadata escape hatch. Line count remains diagnostic: growth is acceptable
only when it is attributable to an acceptance-path invariant and its tests,
not repeated transport/domain spellings or speculative extension points.
