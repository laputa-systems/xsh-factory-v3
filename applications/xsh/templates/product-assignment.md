Complete XSH Product assignment ${ASSIGNMENT_ID}.

The sealed target and evidence map are authoritative:

${TARGET}

No defect lead is preselected. Use the current checkout, source, tests, and sealed target to find
one narrow, reproducible behavior that is genuinely wrong today. Treat resolved tickets and
unimplemented proposals as context, not as defects. If no defensible defect remains after bounded
investigation, call `work_complete` honestly rather than recycling stale backlog.

Begin by invoking `workspace_read` for the first required path; do not narrate an intended read
before making that call. Prose analysis is allowed after tool results, but every action-oriented
response must include the next admitted tool call or terminal call. Never spend a turn only
repeating an intended read, search, or probe.

Bound the search: after the required reads, read `LANG.md` first and select the first open ticket
with a concrete reproducer. Run that exact admitted command twice before reading unrelated history,
the SPEC, or exploring another owner. When the two observations contradict the contract, create
the evidence and submit immediately; do not keep investigating alternatives or narrating after the
candidate is proven. If both observations conform, perform one bounded proposal promotion: read
the `spawn` stderr-redirection proposal in `LANG.md`, run a minimal `spawn(..., stderr: /dev/null)`
program twice with the exact admitted profile, and submit it when the promised syntax is still
rejected. If that proposal also conforms, perform one alternative proposal check for the integer
bitset/radix helpers: run a minimal `let value = 4.has_bit(2)` program twice with the exact
admitted profile and submit it when the promised helper is rejected. A proposal promotion is
valid only when current behavior contradicts its concrete user-visible contract; do not turn a
design preference into a ticket. If the bounded checks conform, call `work_complete` as the
immediate next tool call: do not write another prose turn or reopen the result. Do not open a third
investigation.

Concise prose analysis is allowed, but keep it action-linked: after each read or tool result, make
the next relevant tool call or terminal call. Do not repeat intended actions in prose, simulate
tool output, or spend a turn narrating an unbounded investigation. If no defensible defect remains,
call `work_complete` honestly. After a proposal's second conforming run, the next assistant
message must be that terminal tool call; repeated prose-only turns are host-detected as a stalled
assignment.

Use the controller's exact sealed receipts; do not guess artifact IDs, digests, observations, or
command shapes. Create those files in one shell call from the current assigned workspace with
relative paths; do not `cd` to `/tmp`, and do not call `workspace_list` with an empty path. When an
evidence stream is empty, seal its empty file with `byte_limit: 1`; the controller records the
resulting zero-byte artifact. Do not pass `byte_limit: 0` or add placeholder bytes.
