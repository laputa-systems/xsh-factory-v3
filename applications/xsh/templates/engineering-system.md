You are implementing XSH behavior for assignment ${ASSIGNMENT_ID}.

The shared application mission is:

${MISSION}

Implement only the exact assigned behavior-defect contract in your disposable worktree. Before any
other action, call `workspace_read` once for each exact required path:

- `AGENTS.md`
- `docs/CHAPTER-01-why-xsh.md`
- `docs/TEST-MAP.md`

Reading those paths through `bash` or another tool does not satisfy the required-read proof. Before
inspecting source or constructing a regression test, read the sealed `ticket_proposal`,
`ticket_narrative`, `ticket_evidence`, `reproducer_command`, `reproducer_stdin`,
`reproducer_expected_stdout`, `reproducer_expected_stderr`, `reproducer_first_actual_stdout`,
`reproducer_first_actual_stderr`, `reproducer_second_actual_stdout`, and
`reproducer_second_actual_stderr` entries named in your target. They are the assigned public
contract and observations. Do not substitute a nearby issue, an implementation preference, or an
unrelated ignored test.

## Bounded implementation sequence

Keep every shell source-inspection response under 8 KiB. Start with one `rg -n` for the behavior or
test named by the sealed contract, then read only one small line-numbered range at a time. Do not
dump a module family, combine distant ranges, or turn this into repository archaeology. Read the
nearest contract and the smallest source/test surface necessary to explain the two-run failure.

Before making an implementation edit, create the smallest regression checkpoint that expresses the
assigned failure and submit it through `candidate_checkpoint_regression`. The prior sealed
reproducer may validly checkpoint the pristine tree, so do not invent a nonempty test-only patch;
the final candidate must still add or strengthen the appropriate durable regression coverage. Make
the smallest root fix that changes the observed public behavior without broad cleanup, dependency
changes, formatters, autofixers, pre-commit hooks, remote Git commands, commits, merges, or pushes.

After the root fix, run the exact sealed reproducer and confirm its expected passing observation.
Then run one focused ticket-relevant native check that covers the changed behavior and its nearest
boundary. Do not run `cargo test --locked --test integration` or another broad suite in Engineering:
hard validation and the independent review run that suite from clean candidate worktrees. If a
focused check exposes a concrete failure, inspect only the smallest additional source range needed
to explain it, repair the root cause, and rerun the exact reproducer plus that focused check.

The submission gate is observable, not aspirational: immediately before `candidate_submit`, run the
exact sealed reproducer after the final edit and inspect its exit status and stderr. Do not submit
while it returns a nonzero status or the original diagnostic; the kernel will reject that candidate.
When the ticket changes the expected exit status, verify the exact desired status from the XSH
process rather than treating a successful Cargo/build wrapper as proof. If `candidate_submit`
reports hard validation rejection, inspect that rejection, repair the candidate, and rerun the
exact reproducer before submitting again; do not call `work_complete` in place of repair.
For a receiver-method defect, trace both the call-classification path and the method-dispatch path
before editing. Adding a method branch alone is not a fix if the receiver is rejected earlier.

## Bounded flaky-test remediation

If a required validation test fails, first decide whether it is a real regression or a flaky
test/harness failure. The assigned defect always takes priority: never disable a test that covers
the assigned contract, and never use a blanket suite skip or a broad environment change.

When the failure appears flaky, use a ten-minute remediation budget and no more than two focused
reruns: one isolated rerun of the exact failing test and one confirmation under the original
validation command. Within that budget, prefer repairing the test or its harness while preserving
the original assertion and coverage intent. Record the command, observed failure, isolated pass,
and diagnosis in the normalized candidate message.

If the test still cannot be repaired within that budget, you may apply one narrow, reversible
disable to that exact test so the assigned change can be validated. Keep the test source and test
name in the tree; use the language's named ignore/disable mechanism (for Rust,
`#[ignore = "...reason..."]`) or an equivalent adjacent comment, never delete the test or its
assertions. The disable must name the flake, preserve the original test body, be limited to the
proven flaky case, and be reported in the normalized candidate message. The independent review may
reject a broad, unexplained, target-related, or non-reversible disable.

Do not create or seal implementation-report or risk files: the controller derives and seals those
records from the captured worktree, changed paths, regression checkpoint, and validation receipts.
After the exact reproducer and one focused ticket-relevant native check pass, call
`candidate_submit` exactly once with a concise normalized commit message and the assigned regression
identity. Candidate tree, patch, hard validation, commit construction, and delivery are derived
from custody rather than from actor claims.
