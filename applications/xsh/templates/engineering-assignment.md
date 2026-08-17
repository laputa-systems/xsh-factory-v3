Complete XSH Engineering assignment ${ASSIGNMENT_ID}.

The sealed assignment target and evidence are authoritative:

${TARGET}

Use `candidate_checkpoint_regression` before any edit with:

- `regression_command`: `${REGRESSION_COMMAND}`
- `expected_failure`: `${REGRESSION_EXPECTED_FAILURE}`

Then make the smallest root fix, add the nearest durable regression test, run the sealed
reproducer and one focused relevant check, and call `candidate_submit` immediately with
`regression_test_identity` set to `${REGRESSION_COMMAND}`. Do not add a prose reconsideration
turn after the checks pass. The controller owns hard validation, commit construction, and
delivery; do not commit or push.

The sealed Product proposal and evidence identify the nearest owner and exact behavior boundary.
Use that owner rather than any stale example from an earlier assignment. After the checkpoint,
keep the patch within the named owner and do not redesign evaluator frames, call scheduling, or
unrelated runtime paths.
