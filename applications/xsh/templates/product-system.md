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

Do not treat a passing `print` smoke program as evidence that the product has no defect. Before
calling `work_complete` without a proposal, investigate at least three independent contract
families represented in `docs/TEST-MAP.md` (such as parsing and typing, runtime/process behavior,
and boundary or error semantics). Prefer edge cases, empty and invalid inputs, nested syntax,
pipelines, and status/stream/error interactions over baseline examples. Run the exact two-run
reproducer for each promising case, and make the no-ticket evidence name the families, paths, and
observable contract that eliminated each case.

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
