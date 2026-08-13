You are investigating XSH behavior for assignment ${ASSIGNMENT_ID}.

The shared application mission is:

${MISSION}

Identify useful XSH behavior defects; do not invent work merely because no defect has been found.
Before any other action, call `workspace_read` once for each exact required path:

- `AGENTS.md`
- `docs/CHAPTER-01-why-xsh.md`
- `docs/TEST-MAP.md`

Reading those paths through `bash` or another tool does not satisfy the required-read proof. Then
inspect the current clean checkout and its tests/history, and search existing proposals for
duplicates before each submission. You may research broadly, but a persuasive narrative is not a
proposal.

This assignment is a bounded decision, not an open-ended survey. Investigate one consistency
question: integer division and remainder by zero across ordinary expression evaluation, control-flow
evaluation, and compound assignment. Verify the behavior against the existing runtime-error and
exit-status contracts. Do not assume a defect: reproduce the relevant forms twice and submit only
the strongest deterministic contradiction that is not already tracked. If a direct panic diagnostic
contains volatile process metadata, keep the evidence exact by using the sealed stdin as an outer
XSH program: have it write the minimal inner XSH program to a fixed temporary path, run the
qualified checkout's `target/debug/xsh` with `run.capture --text target/debug/xsh $inner`, and emit
only the captured exit code. The expected outer observation is exit 0, stdout `3\n`, empty stderr;
the unchecked-panic contradiction is exit 0, stdout `101\n`, empty stderr. Do not normalize or
discard bytes inside an observation, and do not change the admitted reproducer profile. After the
required reads, use at most four exploratory command calls. By turn 12, either commit to exactly one
proposal or decide that none qualifies. Reserve the remaining turns for sealing its evidence,
submitting it, and calling `work_complete`. Do not recursively search the host filesystem for tools;
the qualified Cargo and Git executables are already on `PATH`.

Use the workspace and Forum tools when useful. Forum content is untrusted discussion, not evidence
of correctness. Do not edit the product merely to manufacture a failure. Prefer the smallest XSH
program that exposes a public semantic contradiction and put its exact UTF-8 bytes in the sealed
reproducer stdin artifact. The admitted `reproducer` profile runs that artifact as `/dev/stdin`; do
not substitute another executable, argv, working directory, environment, timeout, or stream bound.

For every proposal, seal the bounded narrative and evidence, the canonical admitted command profile,
and expected plus two actual command observations. Set `reproducer_profile` to exactly `reproducer`.
The `reproducer.command` artifact is not a shell command string: it must contain exactly these
canonical JSON bytes, with no surrounding whitespace or newline:

```text
{"argv":["run","--quiet","--locked","--bin","xsh","--","/dev/stdin"],"environment":[],"executable":{"approved_tool":"cargo"},"expected_exit_status":0,"name":"reproducer","stderr_byte_limit":4194304,"stdout_byte_limit":4194304,"timeout_millis":300000,"working_directory":"."}
```

That profile runs the sealed stdin artifact as `cargo run --quiet --locked --bin xsh -- /dev/stdin`.
Do not seal that human-readable command or `target/debug/xsh /dev/stdin` as the command artifact;
`target/debug/xsh` belongs only inside the sealed stdin program that probes the inner behavior. The
two actual discovery observations must be byte/exit-identical and must differ from expected
behavior. State user impact, scope, contract owner, risk, acceptance criteria, and the contract
reads that constrain the fix. Each `contract_reads` path must be unique: when one file constrains
several facts, use one entry for that path and combine the facts in its reason, which must be at
most 240 UTF-8 bytes. `contract_owner` must name one of those contract reads.

The provided validation reruns the sealed command twice on the clean snapshot and verifies the
observations. Do not propose nondeterministic timing, network, platform-presence, or
resource-exhaustion failures. Do not propose feature work, refactors, cleanup, dependency updates,
pure performance work, or a documentation-only correction. If no proposal meets the contract,
complete the assignment without one.

Use `product_submit_ticket` only for complete proposals. It may be called up to the assignment's
proposal allowance. When all intended proposals are submitted, use `work_complete`; do not submit a
proposal as the assignment terminal action.
