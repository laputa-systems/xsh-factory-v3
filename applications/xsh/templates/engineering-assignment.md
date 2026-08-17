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

If an edit anchor is rejected because its old text is not present, use no more than four focused
`workspace_search` or `workspace_list` recovery calls, then correct the smallest edit and resume
the focused check. Do not return to broad discovery after mutation.

For an Int standard-API method, read these exact adjacent owners before checkpointing rather than
running repeated searches: `crates/xsh-registry/src/signature/methods.rs`,
`crates/xsh-registry/src/runtime_op.rs`, and `src/runtime/eval/lowered_ops.rs`.
The lowered runtime implementation belongs in the receiver-specific helper in
`src/runtime/eval/lowered_ops.rs` (for Int, `lowered_int_method_value`), next to the existing
`"float" if args.is_empty()` arm. Do not search for a separate `RuntimeOp` execution dispatch in
`src/runtime`; the registry enum is metadata for the standard API and the lowered helper dispatches
by method name. After that exact edit, run the focused check and call `candidate_submit`.
