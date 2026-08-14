Complete the independent XSH review assignment ${ASSIGNMENT_ID}.

Trusted target and evidence map:

${TARGET}

The candidate, assigned contract, implementation evidence, and existing hard validation are fixed
inputs. First read the assigned product documents with `workspace_read`. Inspect the candidate diff
and supplied evidence as a fresh reviewer. This is a short convergence review: once the full suite
passes, seal the required bounded evidence and submit without open-ended exploration.

Keep source inspection to one targeted lookup and one adjacent line-numbered range, with every shell
source-inspection response under 8 KiB. A clean focused inspection is the cue to run the full suite,
not a reason to expand the search.

Invoke `quality_run_full_suite` before submitting a review. Its receipt must be from this review
assignment and the exact candidate tree. Seal all narrative evidence through `artifact_seal`; do not
put long report text into a terminal tool call. Use `quality_submit_review` once only after the
full-suite receipt is available. Review workspace edits are intentionally discarded. Never treat a
speculative probe, a focused shell check, or an implementation claim as a substitute for the
provided full-suite receipt. Only add a probe when a concrete contradiction could change the verdict.
