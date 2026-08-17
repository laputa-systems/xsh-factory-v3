Complete XSH Product assignment ${ASSIGNMENT_ID}.

The sealed target and evidence map are authoritative:

${TARGET}

The Product office creates the ticket supply for the campaign. The campaign may begin with zero
existing tickets; an empty backlog is expected and is not a reason to call `work_complete`. Use the
current checkout, source, tests, and sealed target to find one narrow, reproducible behavior that is
genuinely wrong today. Treat existing tickets, when present, as context rather than as a required
lead. If no defensible defect remains after bounded investigation, call `work_complete` honestly,
but never do so merely because no ticket or backlog lead exists.

Treat `${TARGET}` as an assignment key, not a defect hint. After the required reads, form up to
three distinct hypotheses from promised contracts and their nearest implementation owners. Choose
the strongest hypothesis, run its smallest admitted reproducer twice, and then either submit that
defect or call `work_complete`; do not keep searching after the decision evidence is complete.

Begin by invoking `workspace_read` for the first required path; do not narrate an intended read
before making that call. Prose analysis is allowed after tool results, but every action-oriented
response must include the next admitted tool call or terminal call. Never spend a turn only
repeating an intended read, search, or probe.

Bound the search: after the required reads, inspect only the owners needed for those hypotheses. If
`LANG.md` exists, it may provide context, but its absence is not a blocker and an empty ticket
backlog is not a result. Run the exact admitted command twice before reading unrelated history or
opening another hypothesis. When the two observations contradict the contract, create the evidence
and submit immediately; do not keep investigating alternatives or narrating after the candidate is
proven. Do not promote an unpromised design preference. If no hypothesis survives, call
`work_complete` as the immediate next tool call; do not write another prose turn or reopen the
result. Do not open a fourth investigation.

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
