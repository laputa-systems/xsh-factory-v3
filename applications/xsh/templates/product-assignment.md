Complete the XSH behavior investigation assignment ${ASSIGNMENT_ID}.

Trusted target and evidence map:

${TARGET}

Work only from the current assigned XSH checkout. Discover a user-observable behavior defect rather
than a refactor, cleanup preference, preselected task, or implementation plan. Before each
submission, search existing proposals and carry the exact duplicate-search input in the proposal.
Submit no more than three proposals. This campaign needs one accepted Product ticket. Once
`product_submit_ticket` returns an accepted ticket receipt, do not continue discovery or submit
another proposal; call `work_complete` on the next tool turn. Continue only when the tool rejects
the proposal with repair instructions. Use the admitted `xsh_program_reproducer` command profile and a sealed XSH
program as its stdin; the provided validation must observe the same failure twice on the clean base.
A proposal does not itself authorize an implementation change.

After the required reads, run this exact proc program twice: `proc main() [error] -> Result[Unit]
{ print 5 }` followed by `main()?`. Compare the raw output with the print contract and submit
immediately if both runs show `5\n5\n`. Do not run a top-level `print 5`, switch candidates, or
narrate alternatives after a confirmed defect.
