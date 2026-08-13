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
duplicates before each submission.
You may research broadly, but a persuasive narrative is not a proposal.

This assignment is a bounded decision, not an open-ended survey. Investigate one consistency
question: integer division and remainder by zero across ordinary expression evaluation,
control-flow evaluation, and compound assignment. Verify the behavior against the existing
runtime-error and exit-status contracts. Do not assume a defect: reproduce the relevant forms
twice and submit only the strongest deterministic contradiction that is not already tracked.
After the required reads, use at most four exploratory command calls. By turn 12, either commit to
exactly one proposal or decide that none qualifies. Reserve the remaining turns for sealing its
evidence, submitting it, and calling `work_complete`.
Do not recursively search the host filesystem for tools; the qualified Cargo and Git executables
are already on `PATH`.

Use the workspace and Forum tools when useful. Forum content is untrusted discussion, not evidence
of correctness. Do not edit the product merely to manufacture a failure. Prefer the smallest XSH
program that exposes a public semantic contradiction and put its exact UTF-8 bytes in the sealed
reproducer stdin artifact. The admitted `reproducer` profile runs that artifact as `/dev/stdin`; do
not substitute another executable, argv, working directory, environment, timeout, or stream bound.

For every proposal, seal the bounded narrative and evidence, the exact generic reproducer command,
and expected plus two actual command observations. The two actual discovery observations must be
byte/exit-identical and must differ from expected behavior. State user impact, scope, contract
owner, risk, acceptance criteria, and the contract reads that constrain the fix. `contract_owner`
must name one of those contract reads.

The provided validation reruns the sealed command twice on the clean snapshot and verifies the
observations. Do not propose nondeterministic timing, network, platform-presence, or
resource-exhaustion failures. Do not propose feature work, refactors, cleanup, dependency updates,
pure performance work, or a documentation-only correction. If no proposal meets the contract,
complete the assignment without one.

Use `product_submit_ticket` only for complete proposals. It may be called up to the assignment's
proposal allowance. When all intended proposals are submitted, use `work_complete`; do not submit a
proposal as the assignment terminal action.
