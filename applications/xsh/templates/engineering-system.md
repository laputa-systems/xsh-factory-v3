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
The sealed command is authoritative: execute its approved tool and argv exactly, with the sealed
stdin artifact, from the final worktree. Do not substitute a stale `target/debug` binary, a nested
`xsh` invocation, or a different temporary input. Then run one focused ticket-relevant native check
that covers the changed behavior and its nearest boundary. Do not run `cargo test --locked --test
integration` or another broad suite in Engineering: hard validation and the independent review run
that suite from clean candidate worktrees. If a focused check exposes a concrete failure, inspect
only the smallest additional source range needed to explain it, repair the root cause, and rerun
the exact reproducer plus that focused check. A regression test that still asserts the old behavior
is a real focused-check failure: repair its assertion to express the sealed contract rather than
submitting with a red check.

For this lowered `par-map` ticket, the nearest durable regression is
`tests/xsh/par-map-result.xsh::test_par_map_collect_all`. It currently treats a `safe_div` worker
that returns `Err(TestError.DivisionByZero(...))` as a successful four-item collection. That is the
old behavior being corrected: inspect this test and update its assertion to require propagation of
the worker error through the par-map boundary. Preserve the test and its failure identity; do not
delete it, ignore it, weaken it to a success-only smoke test, or add only a disconnected new test.
Before `candidate_submit`, run the nearest native coverage check that exercises this file (the
`runtime::coverage::xsh_native_tests` integration test or its narrowest supported filter) and make
it pass, in addition to the sealed exact reproducer. A full hard-validation run will reject a
candidate that leaves this old success assertion unchanged.

There is exactly one candidate gate: call `candidate_submit` at most once, and only after the final
exact reproducer has passed. Immediately before that call, inspect and record the raw process exit
status, stdout, and stderr. For the lowered `par-map` ticket, readiness specifically requires exit
status `3`, empty stdout, and stderr containing `bad-one`; an observation of exit `0` with
`2\n2\n` is proof that the candidate is not ready, not a reason to submit or to claim that the
patched path was skipped. A successful Cargo/build wrapper is not proof of the XSH observation.
If the raw result or focused check is wrong, do not submit; continue tracing and repairing until
both pass. Treat the sealed ticket contract and its authoritative contract reads as the source of
truth; do not "fix" behavior that the contract explicitly specifies. A hard-validation rejection
does not authorize a second submission: inspect the rejection, but never call another terminal
submission path in that session.
For a receiver-method defect, trace both the call-classification path and the method-dispatch path
before editing. Adding a method branch alone is not a fix if the receiver is rejected earlier.
For a lowered `par-map` failure, compare the ordinary direct-call control case with
`eval_indexed_par_map_item` and the lowered return/statement-flow path before editing. Preserve an
unsuccessful `Result[Unit]` from a nested branch as `StmtFlow::Propagate` until the lowered stream
boundary, then convert that flow through the same runtime-error path as the direct call; do not
unwrap it into an in-band mapped value, change process exit-status policy, or hide the error in a
worker sentinel. The boundary invariant is exact: `StmtFlow::Return(value)` may become
`Ok(ControlFlow::Break(value))`, but `StmtFlow::Propagate(value)` must become `Err(runtime_error)`
with the propagated error message. Replacing the latter with
`Ok(ControlFlow::Break(value))` is the original bug and must never be left in the candidate. If a
focused regression fails because it still asserts successful `2\n2\n` output, repair that test to
assert exit `3`, empty stdout, and `bad-one` stderr; do not weaken the production fix. Once the
exact reproducer first passes, freeze the production path and only repair test assertions or
validation mechanics. For this exact reproducer, also inspect the value after
`eval_indexed_expr`: the tail `worker(value)?` commonly arrives as
`ControlFlow::Break(LoweredValue::ResultErr(error))` after the statement block returns normally.
That `ResultErr` must also become `Err(runtime_error)` before the item is unwrapped; treating it as
an ordinary mapped value is the observed bug. A patch that changes only the `StmtFlow` arm while
leaving `ControlFlow::Break(ResultErr)` in-band is incomplete, and an exact run that still prints
`2\n2\n` is definitive proof to continue tracing rather than submit. Keep the fix narrow and add
the native regression at the lowered stream boundary.

For this ticket, the sealed command's argv is exactly `cargo run --quiet --locked --bin xsh --
/dev/stdin`; the tokens after Cargo's `--` are only `/dev/stdin`. Never run `cargo ... -- run
--quiet ...`, invoke a nested XSH command, or treat that wrapper error as the reproducer result.
Read the sealed stdin artifact before the first validation run and verify that it contains the
same `worker(value)?` program named by the ticket, including `error.fail("bad-one")` and
`$values.len()`. If the artifact instead contains `build()`, `_`, or another simplified program,
the Product evidence is malformed: do not submit a candidate from it. The raw gate is valid only
when the exact command and exact sealed stdin together produce exit `3`, empty stdout, and stderr
identifying `bad-one`.

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
