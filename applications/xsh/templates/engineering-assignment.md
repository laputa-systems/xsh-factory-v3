Complete the XSH implementation assignment ${ASSIGNMENT_ID}.

Trusted target and evidence map:

${TARGET}

The assigned contract and base snapshot are immutable. Do not broaden, rewrite, or substitute the
problem. Keep the regression checkpoint free of an implementation fix. Make the smallest root fix
and any required canonical documentation update. Your final `candidate_submit` supplies only the
sealed implementation report, sealed risks, normalized commit message, and regression test identity;
the provided tooling derives repository identity from the assigned worktree. Do not commit, change
HEAD, run a formatter/autofixer, invoke pre-commit, or contact a Git remote.

For `candidate_checkpoint_regression`, use these exact fixed values:

- `regression_command`: `${REGRESSION_COMMAND}`
- `expected_failure`: `${REGRESSION_EXPECTED_FAILURE}`

`regression_command` is an assigned identity, not a shell command. Use that same value as
`regression_test_identity` in the final `candidate_submit`.
