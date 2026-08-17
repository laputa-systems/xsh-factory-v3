# Evidence and retention

## Durable layers

PostgreSQL contains small typed facts: lifecycle rows, immutable identities,
aggregate revisions, and audit receipts. It never stores per-token events,
shell chunks, tool-call rows, or transcript text. The append-only CAS stores
the actual bounded bytes: application/template sources, assignment evidence,
command receipts, patches, reports, and session archives.

## Installed runtime identity

The stopped-daemon installation receipt is evidence for the executable that
may launch actors. It binds the Rust host binary and complete host source
graph, the core lockfile, the exact local core checkout `HEAD`, complete core
source inventory, Rust toolchain, and all corresponding digests. The kernel
rechecks those files and the clean core checkout before spawning a host. A
packet's runtime identity is therefore a qualified fact, not an actor claim;
the provider credential remains only an inherited process-boundary value.

`var/` is an operational runtime root. Its sockets, locks, staging files,
temporary worktrees, and local build caches are transient. Terminal controller
cleanup removes each owned worktree and its staging directory after necessary
artifacts have been sealed; it never uses broad Git worktree pruning. The CAS
object tree is intentionally retained today. A reference-safe collector is a
documented missing capability, not permission to delete runtime data broadly.

## Session transcript projection

The Rust Pi agent implementation emits a compact audit projection before the
host writes `session.ndjson`. The host streams it and seals one gzip archive at
terminal state. This retains the information useful for diagnosis:

- bounded assistant text;
- tool names, boundaries, inputs, results, retries, and terminal reason;
- usage, provider cost when reported, the `cost_limit` stop reason when live
  cancellation fires, and the admitted-rate Factory cost.

It deliberately removes cumulative interactive message snapshots, session-tree
and fork state, and provider thinking blocks. Tool arguments/results are size
bounded; embedded base64 payloads are redacted to lengths. This prevents an
interactive UI representation from becoming the factory's permanent evidence
format while retaining actionable actor and tool diagnostics.

Typed boundaries preserve the important authoritative material separately:
required-read manifests, sealed artifacts, reproducer observations, regression
and validation logs, candidate patches, reviews, and delivery receipts. A
transcript is diagnostic provenance, not the only source of accepted facts.
The read-only `make status` projection can reconstruct those durable transcript
artifacts for the newest terminal campaign into an OS temporary directory. It
reads only the PostgreSQL artifact references and verified CAS bytes; the
operator client receives file metadata and a path, not raw transcript bytes on
the socket. Failed and cancelled campaigns are eligible, and sessions without
an available transcript remain explicitly reported.
`make status` preserves each complete `.ndjson.gz` archive and gunzips a readable
`.ndjson` copy beside it in that temporary directory; the compressed artifact
remains the durable-evidence projection.

## Factory-Cost on delivered commits

Every immutable local delivery stores
`factory.deliveries.factory_cost_micro_usd` with the resulting XSH commit. This
is the campaign's final known aggregate cost from admitted model rates and
sealed token usage at the delivery transaction, expressed in micro-USD so the
authority never relies on floating-point currency values. Provider-reported
cost remains diagnostic evidence and cannot replace the admitted-rate total.
The same value is included in the sealed local delivery receipt, exposed by
campaign status and candidate navigation, and written by the kernel into the
delivered Git commit as a
`Factory-Cost: $0.000000` trailer.

The reviewed candidate commit remains immutable; delivery constructs the
resulting one-parent product commit from the exact candidate tree and appends
the final cost trailer before the guarded fast-forward. A delivery cannot
proceed with unknown or exceeded campaign cost, so a delivered commit always
has an exact Factory-Cost. Human-readable operator output formats `1,000,000`
micro-USD as `$1.000000`.

## Worktree evidence

An Engineering candidate is not accepted from a reported tree identity. The
engineer supplies only its normalized commit message and regression identity;
the kernel captures the owned worktree, computes its portable binary patch,
checks changed paths, materializes fresh validation worktrees, and seals
command receipts plus the Engineering completion/risk records. The resulting
CAS patch and Git identities are the durable product artifact; the full
temporary worktree is not retained.

This boundary still needs controller-owned recovery when an actor dies before
candidate submission. That gap is tracked in the [V1 backlog](../V1.md); do
not attempt manual worktree salvage or direct product commits as a workaround.
