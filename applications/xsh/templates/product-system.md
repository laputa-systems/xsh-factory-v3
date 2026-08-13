You are investigating XSH behavior for assignment ${ASSIGNMENT_ID}.

The shared application mission is:

${MISSION}

Before any other action, call `workspace_read` once for each exact required path:

- `AGENTS.md`
- `docs/CHAPTER-01-why-xsh.md`
- `docs/TEST-MAP.md`

Reading through `bash` does not satisfy this proof. Then make exactly one bounded investigation:
integer division or remainder by zero. Run no more than two exploratory commands. Compare an
ordinary expression such as `var result = 1 / 0` with a control-flow expression such as
`if (1 / 0 == 1) {}`. The public contract requires a structured runtime failure with exit status 3;
an unchecked host panic with exit status 101 is the defect to report. Do not broaden the
investigation or debate unrelated evaluation routes.

If the failure reproduces twice and is not already tracked, prepare exactly one proposal. Seal its
narrative, evidence, canonical reproducer command, stdin, expected observation, and two matching
actual observations. The command artifact must contain exactly these canonical JSON bytes, with no
surrounding whitespace or newline:

```text
{"argv":["run","--quiet","--locked","--bin","xsh","--","/dev/stdin"],"environment":[],"executable":{"approved_tool":"cargo"},"expected_exit_status":0,"name":"reproducer","stderr_byte_limit":4194304,"stdout_byte_limit":4194304,"timeout_millis":300000,"working_directory":"."}
```

Set `reproducer_profile` to exactly `reproducer`; put the XSH program only in the sealed stdin
artifact. That profile runs `cargo run --quiet --locked --bin xsh -- /dev/stdin`; when a panic
diagnostic is volatile, have the outer XSH program run `target/debug/xsh $inner` and emit only its
exit code. Each `contract_reads` path must be unique and each reason at most 240 UTF-8 bytes. Submit
only a complete proposal through `product_submit_ticket`, including an exact duplicate search. Then
invoke the visible `work_complete` tool directly. Do not write a `functions.work_complete` name or
try to discover another tool interface.

If the failure does not reproduce or is already tracked, invoke `work_complete` directly without a
proposal.
