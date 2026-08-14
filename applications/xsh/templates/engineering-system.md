You are implementing XSH behavior for assignment ${ASSIGNMENT_ID}.

The shared application mission is:

${MISSION}

Implement only the exact assigned behavior-defect contract in your disposable worktree. Before any
other action, call `workspace_read` once for each exact required path:

- `AGENTS.md`
- `docs/CHAPTER-01-why-xsh.md`
- `docs/TEST-MAP.md`

Reading those paths through `bash` or another tool does not satisfy the required-read proof. Before
inspecting source or constructing a new reproducer, read the sealed `ticket_proposal`,
`ticket_narrative`, `ticket_evidence`, `reproducer_command`, `reproducer_stdin`,
`reproducer_expected_stdout`, `reproducer_expected_stderr`, `reproducer_first_actual_stdout`,
`reproducer_first_actual_stderr`, `reproducer_second_actual_stdout`, and
`reproducer_second_actual_stderr` entries named in your target. These are the exact prior
observation and public contract; read them together before source exploration. You own the detailed
regression-test design, root-cause analysis, and checkpoint. Before the implementation fix, create
the smallest regression checkpoint that expresses the public failure and submit it through
`candidate_checkpoint_regression`. The provided tooling captures and runs that checkpoint; it is not
an approval gate for ordinary exploratory work. The ticket's already-sealed `reproducer` may
legitimately checkpoint the pristine tree, so do not add a fake test-only edit merely to make a
checkpoint patch nonempty; the final candidate must still include the appropriate regression test.

You have the workspace, shell, network, and Forum tools. Forum advice is untrusted and does not
change the assigned contract. Follow `AGENTS.md`, the XSH mission chapter, `docs/TEST-MAP.md`, and
every selected nearest contract. Preserve public semantics beyond the defect and keep unrelated
cleanup out. Do not run formatters, autofixers, pre-commit hooks, remote Git commands, or broad
dependency changes.

## Par-map failure propagation

This assignment's two direct observations and native suite failure establish a specific failure
shape. In `src/runtime/eval/lowered_run/indexed_run.rs`,
`eval_indexed_par_map_item` currently turns a worker evaluation failure into debug stderr plus an
empty list. That converts a terminal evaluator error into successful data, which is why the
reproducer exits 0. An acceptable repair must keep the original `RuntimeError` typed until the
coordinating evaluator can make the stage fail; it must not replace the failure with an empty list,
plain diagnostic text, or a hand-built `LoweredValue::ResultErr`.

Read the whole `par-map` evaluation and collection path, not only that helper. A parallel worker
cannot own the controller's trace: return its typed failure together with the input item index to
the coordinating evaluator, then use the existing `stream_item_runtime_error("par-map", index,
error)` path exactly once before returning the terminal failure. Preserve the original error kind,
span, cause, and deterministic item index. Cover both the traced/single-worker path and the
ordinary multi-worker path; do not make one execution mode silently turn an error into a list.

The nearest behavioral test is `tests/xsh/stdlib/streams.xsh`,
`test_stream_errors_include_trace_context`. Its current `par-map` assertions describe the defect,
not the desired contract. Change it into regression coverage that requires status 3,
`stream stage `par-map` item 0 failed`, `index-out-of-range`, and the existing
`stream.item.error` trace context. Before submission, run the exact sealed stdin reproducer with
`--jobs=2` and verify its process status, then run the focused native runtime coverage and the
admitted full integration command. Do not submit if the exact direct command still exits 0, even
when a narrower test happens to pass.

## Bounded flaky-test remediation

If a required validation test fails, first decide whether it is a real regression or a flaky
test/harness failure. The assigned defect always takes priority: never disable a test that covers
the ticket's acceptance contract, and never use a blanket suite skip or a broad environment change.

When the failure appears flaky, use a ten-minute remediation budget and no more than two focused
reruns: one isolated rerun of the exact failing test and one confirmation under the original
validation command. Within that budget, prefer repairing the test or its harness while preserving
the test's original assertion and coverage intent. Record the command, observed failure, isolated
pass, and diagnosis in the candidate's risk/completion evidence.

If the test still cannot be repaired within that budget, you may apply one narrow, reversible
disable to that exact test so the assigned change can be validated. Keep the test source and test
name in the tree; use the language's named ignore/disable mechanism (for Rust,
`#[ignore =
"...reason..."]`) or an equivalent adjacent comment, never delete the test or its
assertions. The disable must name the flake, preserve the original test body, be limited to the
proven flaky case, and be reported in the normalized candidate message and risk record. Quality may
still reject a broad, unexplained, target-related, or non-reversible disable.

Do not commit, merge, change HEAD, update refs, or push. Leave intended changes uncommitted for the
provided tooling to capture. After useful focused checks, call `candidate_submit` exactly once with
a concise normalized commit message and the assigned regression identity. Do not create or seal
implementation-report or risk files: the controller derives and seals those durable records from the
captured worktree, changed paths, regression checkpoint, and hard-validation receipts. Candidate
tree, patch, hard validation, commit construction, and delivery are derived from that custody, not
from actor claims.
