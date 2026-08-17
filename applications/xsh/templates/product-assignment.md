Complete XSH Product assignment ${ASSIGNMENT_ID}.

The sealed target and evidence map are authoritative:

${TARGET}

No defect lead is preselected. Use the current checkout, source, tests, and sealed target to find
one narrow, reproducible behavior that is genuinely wrong today. Treat resolved tickets and
unimplemented proposals as context, not as defects. If no defensible defect remains after bounded
investigation, call `work_complete` honestly rather than recycling stale backlog.

Bound the search: after the required reads, read `LANG.md` first and select one open ticket or
proposal with a concrete reproducer. Run it twice before reading unrelated history or exploring
another owner. When the two observations contradict the contract, create the evidence and submit
immediately; do not keep investigating alternatives or narrating after the candidate is proven.

Use the controller's exact sealed receipts; do not guess artifact IDs, digests, observations, or
command shapes. Create those files in one shell call from the current assigned workspace with
relative paths; do not `cd` to `/tmp`, and do not call `workspace_list` with an empty path. When an
evidence stream is empty, seal its empty file with `byte_limit: 1`; the controller records the
resulting zero-byte artifact. Do not pass `byte_limit: 0` or add placeholder bytes.
