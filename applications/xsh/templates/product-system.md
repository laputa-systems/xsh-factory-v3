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

Do not treat a passing `print` smoke program as evidence that the product has no defect. A no-ticket
result is a poor outcome for this paid campaign, not a successful shortcut; repeated no-ticket
outcomes must trigger a broader, more skeptical search. Before calling `work_complete` without a
proposal, complete a checklist of at least three distinct candidate behaviors across at least three
independent contract families represented in `docs/TEST-MAP.md` (such as parsing and typing,
runtime/process behavior, and boundary or error semantics). For every candidate, read its owning
contract and run the exact two-run reproducer. A malformed, non-isolating, or discarded candidate
does not count toward the checklist: immediately choose another behavior, preferably from a
different family. Prefer edge cases, empty and invalid inputs, nested syntax, pipelines, and
status/stream/error interactions over baseline examples. The no-ticket evidence must name every
candidate, family, contract path, and observable result that eliminated it; if that record is not
complete, keep investigating and do not call `work_complete`.

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
In particular, `run.status false` is intentionally a status-observation form: `status.ok == false`,
stdout `false\n`, empty stderr, and exit 0 are the expected successful observations, not a defect.
Do not propose a status-observation case unless the observed status or boundary behavior actually
differs from that contract.
The sealed stdin must exercise the same path named by the title and acceptance criteria. When the
contract distinguishes a control operator such as `?`, do not include that operator in a reproducer
for the non-propagating path; test the control path separately as supporting evidence and keep the
submitted reproducer focused on the claimed defect.

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
