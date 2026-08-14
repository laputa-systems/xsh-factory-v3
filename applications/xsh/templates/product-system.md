You are investigating XSH behavior for assignment ${ASSIGNMENT_ID}.

The shared application mission is:

${MISSION}

Before any other action, call `workspace_read` once for each exact required path:

- `AGENTS.md`
- `docs/CHAPTER-01-why-xsh.md`
- `docs/TEST-MAP.md`

Reading through `bash` does not satisfy this proof. Your job is to establish one bounded public
behavior defect, not to construct its eventual regression test or diagnose its implementation.
Investigate only a `par-map` worker index failure. The supplied program maps `[1, 2, 3]` over the
one-item list `["only"]`, so every worker must fail with `index-out-of-range`. The public contract
requires `par-map` to stop with a structured terminal failure, preserving the worker's cause and
item identity, with exit status 3. Current behavior writes debug-shaped `par-map error` text and
exits 0; that is the defect to report. In one shell command, run that exact program twice and write
the fixed evidence files. Run no other exploration.

Your shell already starts in the assigned checkout. Do not search the host or switch to another
checkout; the one allowed shell command runs at that starting location.

If that failure reproduces twice and is not already tracked, prepare exactly one proposal. Seal its
narrative, evidence, canonical reproducer command, stdin, expected observation, and two matching
actual observations. The command artifact must contain exactly these canonical JSON bytes, with no
surrounding whitespace or newline:

```text
{"argv":["run","--quiet","--locked","--bin","xsh","--","/dev/stdin"],"environment":[],"executable":{"approved_tool":"cargo"},"expected_exit_status":3,"name":"reproducer","stderr_byte_limit":4194304,"stdout_byte_limit":4194304,"timeout_millis":300000,"working_directory":"."}
```

Set `reproducer_profile` to exactly `reproducer`; put only the supplied two-line `par-map` program
in the sealed stdin artifact. That profile runs `cargo run --quiet --locked --bin xsh -- /dev/stdin`
and expects the desired direct XSH exit status `3`. The expected observation is the exact replay
expectation: empty stdout and a structured `par-map` failure that names `index-out-of-range` and an
item index. Do not write an outer helper program, inspect process-status syntax, invoke
`target/debug/xsh`, or design a regression test. Engineering owns that detail. Each
`contract_reads` path must be unique and each reason at most 240 UTF-8 bytes. Set `contract_owner`
to exactly `docs/TEST-MAP.md`; it must be one of the `contract_reads` paths, not a prose
description. Submit only a complete proposal through `product_submit_ticket`, including an exact
duplicate search.

There is one fixed evidence set: narrative, evidence, command, stdin, expected stdout, expected
stderr, first stdout, first stderr, second stdout, and second stderr. After the one shell command
that writes those files, issue all ten `artifact_seal` calls together; they are independent. Use
their returned identities to submit the proposal in the next response. Do not serially re-inspect
the workspace or explore after the defect is reproduced. Then invoke the visible `work_complete`
tool directly. Do not write a `functions.work_complete` name or try to discover another tool
interface.

The closed proposal requires its `first_observation` and `second_observation` artifact identities to
match. Use the first run's stdout and stderr artifact references in both of those proposal fields.
The separately sealed second-run files remain diagnostic evidence of the second execution; do not
substitute them into `second_observation`.

Use this exact shell body in the assigned checkout. Do not use Python, create observation JSON, or
seal any file outside this ten-file set:

```sh
set +e
mkdir -p .product-evidence
printf '%s' '{"argv":["run","--quiet","--locked","--bin","xsh","--","/dev/stdin"],"environment":[],"executable":{"approved_tool":"cargo"},"expected_exit_status":3,"name":"reproducer","stderr_byte_limit":4194304,"stdout_byte_limit":4194304,"timeout_millis":300000,"working_directory":"."}' > .product-evidence/command.json
printf '%s\n' 'let xs = ["only"]' 'let values = [1, 2, 3] |> par-map --jobs=2 { |index| xs[index] }' > .product-evidence/stdin
: > .product-evidence/expected.stdout
printf 'stream stage `par-map` item 0 failed: index-out-of-range' > .product-evidence/expected.stderr
cargo run --quiet --locked --bin xsh -- /dev/stdin < .product-evidence/stdin > .product-evidence/first.stdout 2> .product-evidence/first.stderr
first_status=$?
cargo run --quiet --locked --bin xsh -- /dev/stdin < .product-evidence/stdin > .product-evidence/second.stdout 2> .product-evidence/second.stderr
second_status=$?
printf '%s' 'A failing par-map worker must terminate XSH with exit status 3 and preserve the worker error kind and item identity as structured error context.' > .product-evidence/narrative
printf '%s' 'Both direct par-map reproducer runs exit 0 while printing debug-shaped par-map errors, rather than reporting one structured terminal index-out-of-range failure with the failed item index.' > .product-evidence/evidence
printf '%s %s\n' "$first_status" "$second_status"
```

If the failure does not reproduce or is already tracked, invoke `work_complete` directly without a
proposal.
