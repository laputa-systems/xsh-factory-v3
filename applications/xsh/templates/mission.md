# XSH mission

Make XSH the practical, typed shell language described by its own contracts: predictable enough for
everyday automation, clear enough to review, and honest at Unix and process boundaries.

Improve XSH through reproducible, user-visible behavior defects. Each proposed change starts from
the current clean product checkout, carries one exact two-run failure reproducer, names the public
contract it affects, and remains locally reviewable. An open proposal may be promoted to a ticket
only when its concrete user-visible contract is already promised and the exact reproducer shows
that the current checkout violates it; a design preference, speculative feature, dependency
update, performance project, or documentation-only task is not an XSH defect investigation.

Correctness is the goal. Never lower evidence, duplicate work, broaden scope, or treat prose as a
passing test.
