# Evidence and retention

## Durable layers

PostgreSQL contains small typed facts: lifecycle rows, immutable identities,
aggregate revisions, audit receipts, and one row per provider request intent
and settlement. It never stores per-token events, shell chunks, tool-call
rows, prompt/message text, provider bodies, or transcript text. The append-only CAS stores
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
incrementally writes and flushes `session.ndjson`. The compressed
`tea-trace.jsonl.gz` artifact has role `tea_trace_jsonl_gzip` and contains only
Tea trace JSONL: one `EpisodeHeader`, zero or more Tea trajectory records, and
one final `EpisodeEnd`. Factory never appends a record after that terminal Tea
event.

Factory seals its own canonical `factory-execution-summary.json` separately
under `factory_execution_summary_json`. The summary retains Factory
application/assignment/packet/policy/build identities; provider/model; Tea
harness snapshot, revision, model-profile, and surface identities (including
the host-only tool-execution-policy digest); tool diagnostics; cost-stop state;
provider-effect completeness/counts; trace truncation; and the selected
terminal operation. A successful terminal settlement names both sealed
artifacts explicitly. Operator export writes
`session-<id>-trace.ndjson.gz` and
`session-<id>-execution-summary.json`; the latter is not a transcript and is
never gunzipped as NDJSON.

The Tea JSONL records retain turns, bounded assistant output, tool calls and
failures, cache evidence, compaction lifecycle, and stop reason. Model input is
always replaced at the redaction boundary. Tool arguments recursively redact
credential-shaped fields, and arguments, results, and errors are byte bounded.
The sink reserves terminal capacity before accepting ordinary records, records
whether trajectory content was truncated, and uses a
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

## Provider-effect accounting and recovery

`FactoryEffectGate` is installed for every production hosted epoch; production
assignments do not use `NoopEffectGate`. Immediately before provider dispatch
it records the bound session/assignment, Tea core-run/effect identity, resolved
harness identities, requested provider/model, and a content-free request
fingerprint. Settlement records only its closed outcome, bounded failure class,
context-overflow classification, nullable usage counters, and exact parsed
micro-USD provider cost. Factory tools do not enter this ledger: their existing
typed capability RPCs remain their authoritative durability boundary.

Terminal reconciliation rereads this narrow ledger. Every started request must
have a settled record with known exact provider cost before the ledger total is
known. A complete ledger agrees exactly with a supplied terminal cost, or it
recovers cost and usage after a host dies before terminal submission. An
unsettled, failed, conflicting, or cost-unknown request keeps the cost
fail-closed. A session with no provider request has a known zero total.

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
