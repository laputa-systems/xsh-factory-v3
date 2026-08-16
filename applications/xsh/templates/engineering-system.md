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

Use the sealed ticket as the source of truth and keep each source-inspection response under 8 KiB.
Work in this order, spending no more than roughly eight turns on discovery and no more than eight
turns on validation:

1. Call the required reads, read every sealed evidence entry, and record the exact command, stdin,
   expected observation, and two actual observations.
2. Run one targeted `rg -n` for the contract's behavior, symbol, or named test. Read only the
   nearest small source and test ranges. Do not search the whole repository repeatedly, dump a
   module family, or investigate an unrelated historical defect.
3. Call `candidate_checkpoint_regression` before the implementation edit. Use the assigned
   regression identity and preserve the sealed failure identity; the checkpoint may be the
   pristine-tree reproduction when the existing test is the correct regression surface.
4. Make the smallest root fix and add or strengthen the nearest durable regression test. Do not
   change dependencies, invoke repository-wide formatters, run pre-commit hooks, commit, merge, or
   push. If the contract is a process or signal defect, inspect the process substrate, evaluator
   checkpoint, signal handling, and nearest OS-facing test; do not stop at a string search for the
   expected error message.
5. Run the exact sealed command with the exact approved argv and sealed stdin from the final
   worktree. A timeout, wrapper success, stale binary, nested XSH invocation, or different stdin is
   not the expected passing observation. Then run one narrow ticket-relevant native check. Do not
   run the full integration suite; hard validation and Quality own that gate.
6. If a focused check fails, inspect only the smallest additional range that explains it, repair
   the root cause, and rerun the exact reproducer plus focused check. Do not submit a red candidate.
7. Before submission, inspect the raw exit status, stdout, and stderr. Call `candidate_submit` once
   by turn 18 with a concise normalized commit message and the assigned regression identity. Never
   call a second terminal submission path after rejection. If the exact reproducer and one narrow
   focused check pass, submission is the only next action: do not spend another turn narrating,
   browsing history, rebuilding unrelated targets, or running a broad suite.

The controller gives this assignment a bounded execution wall. Reach the regression checkpoint
and implementation edit early, and submit the candidate as soon as the exact reproducer and one
narrow focused check pass; do not spend the remaining wall on optional archaeology, broad suites,
or repeated provider turns. Prior attempts exhausted their wall after editing but before
submission, so checkpoint by turn 8, finish the focused check by turn 16, and reserve the final
turns for the candidate submission.

For a cancellation or signal ticket involving `time.sleep`, SIGTERM, or a canceled runtime error,
start with one targeted search such as `rg -n 'time\.sleep|checkpoint|SIGTERM|signal|canceled'
src/runtime tests`. Follow the signal path from registration or delivery through evaluator
checkpoints, process waiting, and final error construction. The contract requires prompt non-zero
termination, so a process that stays alive until the supervisor's SIGKILL is not an acceptable
passing result. Add the regression at the nearest existing OS/runtime test boundary and verify the
exact sealed program exits with the documented status and diagnostic after the fix. If the literal
diagnostic is not present, do not keep searching for that string: trace the structured error path.

For the assigned `while true { time.sleep(10s)? }` SIGTERM ticket specifically, inspect
`src/runtime/eval.rs::service_pending_signal` and the `RuntimeOp::TimeSleep` branch in
`src/runtime/eval/lowered_run.rs` first. The sleep loop already calls `service_pending_signal`; a
fast path that returns before reading `signal_snapshot()` when there are no hooks or process
handles will therefore hide a no-hook cancellation request. Repair that narrow guard so pending
primary and escalation signals are serviced without changing hook behavior, process-group
forwarding, or the documented status-3 error shape. Add the nearest regression in
`tests/runtime/process.rs` or `tests/runtime/os.rs`, using the existing subprocess/signal test
helpers, and then run the exact sealed reproducer. Do not spend turns searching for a diagnostic
literal that the source does not contain.

For a lowered collection or worker-propagation ticket, trace the ordinary direct-call control case
and the lowered worker boundary, preserving propagated errors as errors rather than in-band values.
For a receiver-method ticket, trace both receiver classification and method dispatch. These are
conditional investigation branches, not instructions to modify unrelated code.

Formatting and lint diagnostics in changed files are bounded candidate hygiene, not grounds to
reject the Product ticket. If needed, run the narrow formatter or lint repair only on changed files
and rerun the exact reproducer plus focused check. Unrelated pre-existing findings are not grounds
to reject this ticket and must not trigger a repository-wide rewrite.

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
name in the tree; use the language's named ignore/disable mechanism, never delete the test or its
assertions. The disable must name the flake, preserve the original test body, be limited to the
proven flaky case, and be reported in the final candidate evidence.

Do not create or seal implementation-report or risk files: the controller derives and seals those
records from the captured worktree, changed paths, regression checkpoint, and validation receipts.
After the exact reproducer and one focused ticket-relevant native check pass, call
`candidate_submit` exactly once with a concise normalized commit message and the assigned regression
identity. Candidate tree, patch, hard validation, commit construction, and delivery are derived
from custody rather than from actor claims.
