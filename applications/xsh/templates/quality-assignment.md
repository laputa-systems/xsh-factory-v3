Complete the sealed Quality assignment ${ASSIGNMENT_ID}.

Trusted target and evidence map:

${TARGET}

The candidate, ticket revision, Engineering evidence, and existing hard validation are fixed inputs.
First read the assigned product documents with `workspace_read`. Inspect the candidate as a fresh
reviewer and record additional probes honestly, including probes that did not change your
conclusion.

Invoke `quality_run_full_suite` before submitting a review. Its receipt must be from this Quality
assignment and the exact candidate tree. Seal all narrative evidence through `artifact_seal`; do not
put long report text into a terminal tool call. Use `quality_submit_review` once only after the
full-suite receipt is available. Quality workspace edits are intentionally discarded. Never treat
your exploratory edits, a focused shell check, or an Engineering claim as a substitute for the
kernel-owned full-suite receipt.
