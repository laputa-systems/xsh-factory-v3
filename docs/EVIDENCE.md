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

## Hosted-epoch trace evidence

Tea's `TraceObserver` produces the machine trajectory; Factory does not project
`AgentEventKind` into a second event model. A Factory-owned `RedactingSink`
removes model inputs and credential-shaped tool fields before a bounded sink
incrementally writes and flushes `session.ndjson`. At terminal state the host
appends one `factory.execution_summary.v1` record and seals
`tea-trace.jsonl.gz` under the versioned `tea_trace_jsonl_gzip` evidence role.
That summary retains the Factory application revision, assignment, packet,
policy, and host-build identities; exact provider/model selection; Tea harness
snapshot, revision, and model-profile identities; standard Tea surface
digests; engineering diagnostics; cost-stop state; and the selected terminal
operation.

The Tea JSONL records retain turns, bounded assistant output, tool calls and
failures, cache evidence, compaction lifecycle, and stop reason. Model input is
always replaced at the redaction boundary. Tool arguments recursively redact
credential-shaped fields, and arguments, results, and errors are byte bounded.
The sink reserves terminal and summary capacity before accepting ordinary
records, records whether trajectory content was truncated, and uses a
conservative gzip upper bound so the sealed member remains within packet
authority. The gzip member uses the `flate2` Rust backend (`miniz_oxide`) and a
stored-block fallback when compression would expand the bounded artifact.

While a session is prepared or running, `factoryctl campaign status` exposes
the absolute path to its flushed `staging/assignment-<id>/session.ndjson`
stream. The host appends the same redacted Tea JSONL records as events settle,
so an operator can follow live progress without reading the actor socket. The
staging path is transient; after terminal cleanup, use the durable cycle
transcript export path instead.

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
is the campaign's final known aggregate of complete provider-reported terminal
costs at the delivery transaction, expressed in micro-USD so the authority
never relies on floating-point currency values. Token usage and model-rate
metadata remain diagnostic evidence and cannot replace a missing provider total.
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
