Complete the XSH implementation assignment ${ASSIGNMENT_ID}.

Execution guardrails: required reads and sealed evidence come first, then one targeted search and
the regression checkpoint. Do not spend turns repeating a source search or explaining the same
hypothesis. After the first confirmed reproduction, edit the nearest owner immediately. Keep the
change narrow, run the exact reproducer plus one focused check, and call `candidate_submit` by turn
18; a passing check is the handoff point, not a reason to keep investigating.

Trusted target and evidence map:

${TARGET}

The assigned contract and base snapshot are immutable. Do not broaden, rewrite, or substitute the
problem. Keep the regression checkpoint free of an implementation fix. Make the smallest root fix
and any required canonical documentation update. Your final `candidate_submit` supplies only the
normalized commit message and regression test identity; the provided tooling derives the worktree,
patch, completion record, and risks record from controller-owned evidence. Do not commit, change
HEAD, invoke pre-commit, or contact a Git remote. A bounded `xsht fmt` or `xsht lint --fix` pass
on files changed by this ticket is allowed when required for candidate hygiene; do not run either
command repository-wide.

If validation exposes a flaky test unrelated to the assigned contract, follow the bounded flaky-test
policy in the Engineering system instructions: spend at most ten minutes and two focused reruns
trying to repair it, then use one narrow reversible named disable only if the test passes in
isolation. Never delete the test or its assertions. Include the failing command, isolated pass,
timebox, diagnosis, and any disable rationale in the candidate evidence and normalized commit
message. A target-related or unexplained disable is not permitted.

For `candidate_checkpoint_regression`, use these exact fixed values:

- `regression_command`: `${REGRESSION_COMMAND}`
- `expected_failure`: `${REGRESSION_EXPECTED_FAILURE}`

`regression_command` is an assigned identity, not a shell command. Use that same value as
`regression_test_identity` in the final `candidate_submit`.
