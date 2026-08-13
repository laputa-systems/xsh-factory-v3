# Evidence and retention

## Durable layers

PostgreSQL contains small typed facts: lifecycle rows, immutable identities,
aggregate revisions, and audit receipts. It never stores per-token events,
shell chunks, tool-call rows, or transcript text. The append-only CAS stores
the actual bounded bytes: application/template sources, assignment evidence,
command receipts, patches, reports, and session archives.

`var/` is an operational runtime root. Its sockets, locks, staging files,
temporary worktrees, and local build caches are transient. Terminal controller
cleanup removes each owned worktree and its staging directory after necessary
artifacts have been sealed; it never uses broad Git worktree pruning. The CAS
object tree is intentionally retained today. A reference-safe collector is a
documented missing capability, not permission to delete runtime data broadly.

## Session transcript projection

The vendored Pi headless implementation emits a compact audit projection before
the host writes `session.ndjson`. The host streams it and seals one gzip archive
at terminal state. This retains the information useful for diagnosis:

- bounded assistant text;
- tool names, boundaries, inputs, results, retries, and terminal reason;
- usage and provider cost when reported.

It deliberately removes cumulative interactive message snapshots, session-tree
and fork state, and provider thinking blocks. Tool arguments/results are size
bounded; embedded base64 payloads are redacted to lengths. This prevents an
interactive UI representation from becoming the factory's permanent evidence
format while retaining actionable actor and tool diagnostics.

Typed boundaries preserve the important authoritative material separately:
required-read manifests, sealed artifacts, reproducer observations, regression
and validation logs, candidate patches, reviews, and delivery receipts. A
transcript is diagnostic provenance, not the only source of accepted facts.

## Worktree evidence

An Engineering candidate is not accepted from a reported tree identity. The
engineer supplies only its normalized commit message and regression identity;
the kernel captures the owned worktree, computes its portable binary patch,
checks changed paths, materializes fresh validation worktrees, and seals
command receipts plus the Engineering completion/risk records. The resulting
CAS patch and Git identities are the durable product artifact; the full
temporary worktree is not retained.

This boundary still needs controller-owned recovery when an actor dies before
candidate submission. That gap is tracked in `PLAN.md`; do not attempt manual
worktree salvage or direct product commits as a workaround.
