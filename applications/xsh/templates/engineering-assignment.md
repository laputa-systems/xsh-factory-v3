Complete XSH Engineering assignment ${ASSIGNMENT_ID}.

The sealed assignment target and evidence are authoritative:

${TARGET}

Use `candidate_checkpoint_regression` before any edit with:

- `regression_command`: `${REGRESSION_COMMAND}`
- `expected_failure`: `${REGRESSION_EXPECTED_FAILURE}`

Then make the smallest root fix, add the nearest durable regression test, run the sealed
reproducer and one focused relevant check, and call `candidate_submit` once with
`regression_test_identity` set to `${REGRESSION_COMMAND}`. The controller owns hard validation,
commit construction, and delivery; do not commit or push.

For the sealed `main()?` print lead, the nearest owner is the compact evaluator's explicit-main
predicate in `src/runtime/eval.rs`. It must recognize a zero-argument `main()` call (including the
`Try` wrapper) before deciding whether implicit main dispatch is required. Keep the patch within
that predicate/decision boundary; do not redesign evaluator frames or investigate unrelated
runtime paths.
