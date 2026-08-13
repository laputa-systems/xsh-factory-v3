# Architecture glossary

## Kernel

The installed Rust authority. It alone admits durable state, owns PostgreSQL
and CAS, manages processes and worktrees, validates trees, constructs commits,
and performs guarded local delivery.

## Application bundle and revision

`ApplicationBundleV1` is immutable, closed policy: repository binding,
templates, tools, model profile, required reads, ticket limits, reproducer,
validation commands, path policy, and commit-message policy. Registration
adopts one bundle and its named source artifacts into CAS as an application
revision. A campaign pins exactly one revision.

## Aggregate revision

A monotonically increasing optimistic-concurrency revision. Every mutating
command supplies the revision it observed; a mismatch is a conflict, never an
implicit overwrite.

## Artifact and CAS

An artifact is bounded bytes sealed by the kernel and addressed by BLAKE3.
PostgreSQL stores immutable identity and domain relations; CAS stores bytes.
The relation, not an actor-supplied label, gives the artifact its durable role.

## Assignment and session

An assignment is one immutable packet for one exact office/task. A session is
one fresh kernel-custodied actor process for that assignment. Neither is a
reusable identity or a source of authority.

## Required read

An exact repository path and BLAKE3 digest that must be read through the
wrapped tool. Shell output, a prompt quotation, or an actor assertion does not
satisfy it.

## Ticket, checkpoint, candidate, and review

Product proposes a reproducible behavior-defect ticket. Engineering creates a
kernel-captured pre-fix regression checkpoint, then a candidate is the exact
changed tree the kernel captures and hard-validates. Quality receives a fresh
materialization and supplies independent qualitative review; it cannot replace
deterministic validation. The Architect sponsors a ticket and decides delivery,
rework, or rejection.

## Campaign

A bounded spend and time envelope pinned to one kernel build, application
revision, repository snapshot, aggregate cost cap, and delivery target. Ticket
inventory and Forum history outlive campaigns.

## Compact audit transcript

The locally built Pi headless runtime projects its event stream before it hits
disk. The projection retains bounded assistant text and tool diagnostics while
discarding interactive session snapshots, forks, and thinking blocks. Its gzip
archive is one session artifact, not a PostgreSQL event log.

## Forum

Permanent, shared, non-authoritative discussion. A Forum post cannot grant
authority, create a ticket, certify validation, or make a delivery decision.
