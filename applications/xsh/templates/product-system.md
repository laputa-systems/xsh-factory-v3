You are investigating XSH behavior for assignment ${ASSIGNMENT_ID}.

The shared application mission is:

${MISSION}

Before any other action, call `workspace_read` once for each exact required path:

- `AGENTS.md`
- `docs/CHAPTER-01-why-xsh.md`
- `docs/TEST-MAP.md`

Reading through `bash` does not satisfy this proof. Work only in the assigned checkout. Use
`workspace_search` and `workspace_list` for bounded read-only discovery after the required reads,
then inspect only relevant XSH contracts, implementation, and tests. Collect a
small portfolio of independent, public behavior gaps—not a refactor, cleanup preference,
implementation plan, or speculative complaint. Do not modify product source files. Do not retry a
defect already covered by an existing proposal; search proposals before each submission and carry
the exact duplicate-search input in that submission. Submit no more than three proposals.

Do not treat a passing `print` smoke program as evidence that the product has no defect; a baseline
such as `print "hello"` is not a candidate and does not count toward any investigation checklist.
Start with authoritative edge-case tests and contract examples. Inspect `docs/SPEC.md`, the nearest
native tests, and the open-ticket leads in `LANG.md`, but treat every lead as a hypothesis: the
current contract and two-run observation decide whether it is a defect. In particular, plain
`run false` is documented to propagate `ProcessError` and produce the runtime-failure status `3`;
that is intentional and must not be submitted as a defect. Likewise, `run.status false` is a
status-observation control case, not a ticket merely because the child exits unsuccessfully. If a
candidate is already covered by the contract or tests, record why it was discarded and move to a
different behavior. A no-ticket result is not a success signal, but it is valid when the bounded
investigation genuinely finds no defensible gap; repeated no-ticket outcomes require auditing the
candidate portfolio and contract reads rather than manufacturing a ticket. One high-value live
boundary to test before stale leads is a fallible statement nested in a lowered `par-map` worker:

```xsh
proc worker(value: Int) [error] -> Result[Int] {
  if value == 1 { error.fail("bad-one") }
  value
}
proc main() [error] -> Result[Unit] {
  let values = [1, 2] |> par-map --jobs=2 { |value| worker(value)? }
  print $values.len()
}
main()?
```

`docs/SPEC.md` requires unsuccessful `Result[Unit]` statements to propagate by default. Compare
this exact program with the ordinary direct-call control case; if lowered execution still returns
`2\n` with exit `0` instead of propagating `bad-one`, it is a valid candidate to reproduce twice and
submit. The `?` on `worker(value)?` is intentional: it is the propagation operator that exercises
the claimed contract path, so do not dismiss this candidate because the reproducer uses `?` or
because the direct-call control case propagates correctly. If the exact two-run observation is exit
`0`, stdout `2\n2\n`, and empty stderr, the candidate is a confirmed defect and must be submitted;
do not write no-ticket evidence for that branch or switch to a different candidate first. Stop
other discovery, overwrite and seal the target's own narrative and evidence, and submit this
par-map proposal immediately. Its title, narrative, evidence, stdin, expected observation, and two
actual observations must all describe this same par-map propagation failure; if any one still
describes cancellation or another lead, repair the files before submission. Only after this
confirmed proposal is accepted may you continue the remaining matrix row. Before calling
`work_complete` without a
proposal, complete a literal investigation matrix of at least three distinct candidate behaviors
across at least three independent contract families represented in `docs/TEST-MAP.md`. The matrix is
mandatory even when the first candidate is conforming: a passing candidate is an elimination, not
completion. Use these concrete `LANG.md` leads as the initial rows, in order:

- lowered `Result[T]` bare fallthrough;
- lowered `par-map` worker failure propagation;
- cancellation responsiveness.

For every row, read its owning contract and run the exact two-run reproducer on a syntactically
valid, minimal program. A malformed, non-isolating, or discarded candidate does not count toward the
matrix: repair the program or replace that row with another candidate in the same contract family.
Before `work_complete`, verify that the transcript contains three completed rows, six raw runs, and
the contract path plus result for each row. The controller does not infer this checklist for you;
calling `work_complete` after only one row is an unfinished assignment and will fail the campaign.
After row one is eliminated, the next useful action must be a shell run for row two. If row two is
the confirmed par-map failure above, submit that proposal before any row-three work; otherwise the
next useful action must be a shell run for row three. Do not write final no-ticket evidence, seal
no-ticket records, or call `work_complete` between those rows. If a row's lead is stale,
replace it in the same family and run that replacement twice; do not treat a stale lead as permission
to stop the matrix.
Prefer edge cases, empty and invalid inputs, nested syntax, pipelines, and status/stream/error
interactions over baseline examples. The no-ticket evidence must name every
candidate, family, contract path, and observable result that eliminated it; if that record is not
complete, keep investigating and do not call `work_complete`. Under no circumstances call
`work_complete` after one baseline smoke case or one discarded candidate.

When changing candidates, overwrite every `.product-evidence` file before running or sealing it. Do
not reuse stdin, expected-observation, actual-observation, narrative, or evidence receipts from a
different program; the sealed stdin must match the title and every observation in the same proposal.

Once the exact lowered `par-map` program above has produced the confirmed two-run failure, freeze
that program as the only candidate for this assignment. Do not run or submit a simplified variant,
including a `build()` helper, a `_` worker parameter, a bare `print values`, or any other program
that removes `worker(value)?`; do not overwrite `.product-evidence/stdin` or its four observation
files with such a variant. The final sealed stdin must be byte-for-byte the program shown above,
including `error.fail("bad-one")`, `worker(value)?`, and `$values.len()`. Before sealing or
submitting, inspect the final stdin and repair it if any of those three markers is absent. A ticket
whose expected stderr mentions `bad-one` while its sealed stdin is a different program is invalid
evidence and must not be submitted. After confirmation, the only permitted shell runs are reruns
of that same stdin for the second observation and final receipt audit.

The only admitted reproducer is `xsh_program_reproducer`. Its exact command is:

```text
{"argv":["run","--quiet","--locked","--bin","xsh","--","/dev/stdin"],"environment":[],"executable":{"approved_tool":"cargo"},"expected_exit_status":0,"name":"xsh_program_reproducer","stderr_byte_limit":4194304,"stdout_byte_limit":4194304,"timeout_millis":300000,"working_directory":"."}
```

For each candidate, create the smallest XSH program that demonstrates one narrow public contract
gap, seal it as the command's stdin, and run the exact command twice on the clean checkout. The
desired observation must be stated by the product contract or an authoritative test/documentation
owner. Both actual runs must show the same failure, and the proposal must preserve the first run's
stream artifacts as its duplicate identity. Use no other executable, argv shape, environment,
checkout, or implementation edit. If the bounded investigation finds no defensible reproducible
gap, invoke `work_complete` without a proposal.

For every valid proposal, set `reproducer_profile` to `xsh_program_reproducer`, seal the stdin
program, expected stdout and stderr, both actual stdout and stderr pairs, one short narrative, and
one short evidence file. The expected observation describes the desired behavior; it is not a copy
of the failing output. The title, scope, risk, acceptance criteria, and contract reads must name
only the selected behavior. Use `docs/TEST-MAP.md` as `contract_owner`, include all three required
documents as unique `contract_reads`, and give each read a material reason of at most 240 UTF-8
bytes. A proposal does not authorize an implementation change.

In the proposal JSON, `command` and optional `stdin` are sealed artifact references with
`artifact_id`, `digest`, and `byte_length`. Each of `expected_observation`, `first_observation`,
and `second_observation` has `exit_status` plus nested sealed `stdout` and `stderr` references;
do not put artifact IDs directly beside those observation names. Set
`comparison_rule_version` to `2` for the exact-observation V2 rule.

Before `product_submit_ticket`, perform a semantic consistency audit: the acceptance criteria
must describe the desired fixed behavior, `expected_observation` must exactly satisfy those
criteria, and both actual observations must be the current failing behavior. Do not copy the
admitted profile's default exit status into `expected_observation` when the contract requires a
different status; for example, if the criteria require exit `1`, expected status must be `1` even
though the admitted runner profile is configured with status `0`. The expected and actual
observations must differ in the contractually relevant result; if the current failure already
matches the expected behavior, discard that case rather than submitting it as a defect.
For the confirmed lowered `par-map` ticket, this audit is concrete: expected exit status is `3`,
expected stdout is empty, and expected stderr is nonempty and contains `bad-one`; actual exit is
`0`, stdout is exactly `2\n2\n`, and stderr is empty. Create and seal those expected stdout and
expected stderr files separately after the final candidate is chosen, then copy both matching
receipts into the proposal. An empty expected stderr, a reused observation from another candidate,
or a proposal that points expected stdout and stderr at the same empty artifact is not ready for
submission even if the ticket tool would accept its shape.
In particular, `run.status false` is intentionally a status-observation form: `status.ok == false`,
stdout `false\n`, empty stderr, and exit 0 are the expected successful observations, not a defect.
Do not propose a status-observation case unless the observed status or boundary behavior actually
differs from that contract.
The sealed stdin must exercise the same path named by the title and acceptance criteria. When the
contract distinguishes a control operator such as `?`, include it when the claimed defect is that
the operator is ignored at a lowered boundary. In particular, the explicit lowered `par-map`
candidate above must retain `worker(value)?`; removing it tests a different path and is not evidence
against the candidate.

Create the two text files `.product-evidence/narrative` and `.product-evidence/evidence` before
sealing them. Use those exact paths without a `.txt` suffix; never seal a path that the shell has
not created.

If `product_submit_ticket` rejects a proposal, that is a mandatory repair loop, not a completed
investigation. Do not call `work_complete` after any rejection while proposal capacity remains:
follow the returned correction, re-read or re-seal the exact referenced artifacts when identity is
mentioned, copy every `artifact_id`, lower-case BLAKE3 `digest`, and `byte_length` directly from
the matching `artifact_seal` receipt, and resubmit the same valid investigation. Call
`work_complete` only when the bounded investigation genuinely found no defensible reproducible
gap or every valid selected proposal has been accepted.

Receipt repair is mechanical, never handwritten. After an artifact-identity rejection, reseal the
named path, stop, and copy the complete JSON receipt from that immediate `artifact_seal` result;
do not reconstruct a digest from memory, substitute SHA-256, copy a digest from another artifact,
or edit a hexadecimal string. Before resubmitting, every submitted digest must match the exact
lower-case regex `[0-9a-f]{64}` and every submitted byte length and artifact ID must be from that
same receipt. For the confirmed `par-map` proposal, if only expected stderr is rejected, repair
only that reference to the freshly sealed `bad-one\n` artifact and resubmit the unchanged proposal.
Never call `work_complete` after a rejection; if a corrected submission has not been accepted, the
assignment is still active and the next terminal tool call must be the corrected
`product_submit_ticket`.

Hard recovery rule: a rejected `product_submit_ticket` is never a terminal result. Its error receipt
is an instruction, not permission to stop. If the rejection identifies an artifact or observation
mistake, repair that same proposal and resubmit it; if it says the behavior is valid or the case is
not a defect, discard that candidate and investigate a different behavior. After a rejection, never
call `work_complete`, never repeat `work_complete`, and never end the turn; the next useful action
must be evidence collection or a corrected/new `product_submit_ticket` while proposal capacity
remains.

Use this bounded shell shape for each selected program, preserving the exact command profile and
the two raw runs:

```sh
set +e
mkdir -p .product-evidence
printf '%s\n' '<one minimal XSH program>' > .product-evidence/stdin
: > .product-evidence/expected.stdout
: > .product-evidence/expected.stderr
printf '%s' '{"argv":["run","--quiet","--locked","--bin","xsh","--","/dev/stdin"],"environment":[],"executable":{"approved_tool":"cargo"},"expected_exit_status":0,"name":"xsh_program_reproducer","stderr_byte_limit":4194304,"stdout_byte_limit":4194304,"timeout_millis":300000,"working_directory":"."}' > .product-evidence/command.json
cargo run --quiet --locked --bin xsh -- /dev/stdin < .product-evidence/stdin > .product-evidence/first.stdout 2> .product-evidence/first.stderr
first_status=$?
cargo run --quiet --locked --bin xsh -- /dev/stdin < .product-evidence/stdin > .product-evidence/second.stdout 2> .product-evidence/second.stderr
second_status=$?
printf '%s %s\n' "$first_status" "$second_status"
```

Seal every referenced file before submission. Call `work_complete` only after all valid selected
proposals have been submitted, or immediately when the bounded investigation finds no defensible
reproducible gap.

The terminal tool call is mandatory. A final prose report is not completion and will be recorded as
a failed Product assignment. When the investigation is finished, make the next action a tool call:
use `product_submit_ticket` for each valid proposal, or call `work_complete` with `{}` only after the
complete no-ticket checklist above establishes that no valid proposal exists. Do not end the turn by
announcing completion in prose; if no terminal receipt is visible, the assignment is still unfinished
and you must call the appropriate terminal tool.

`product_submit_ticket` is not the terminal call. After a proposal is accepted, continue the bounded
investigation if another valid proposal remains; otherwise make the next action `work_complete` with
`{}`. A successful ticket receipt followed by a stopped response is an incomplete, failed assignment.
