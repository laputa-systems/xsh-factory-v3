Complete XSH Product assignment ${ASSIGNMENT_ID}.

The sealed target and evidence map are authoritative:

${TARGET}

Factory-provided lead: run this exact program twice with the admitted reproducer:

```xsh
proc main() [error] -> Result[Unit] { print 5 }
main()?
```

If both runs exit 0 with stdout `5\n5\n` and empty stderr, the lead is a confirmed defect:
the expected stream is the assignment's stated `5\n`, and the actual stream is the observed
`5\n5\n`. Do not reinterpret this result, inspect more files, or run an alternative. Seal the
required evidence and submit that one defect immediately. Only if the lead does not match may you
perform one bounded alternative from the target, then submit one confirmed ticket or call
`work_complete` with the honest no-ticket result. Use the controller's exact sealed receipts; do
not guess artifact IDs, digests, observations, or command shapes. Create those files in one shell
call from the current assigned workspace with relative paths; do not `cd` to `/tmp`, and do not call
`workspace_list` with an empty path. When an evidence stream is empty, seal its empty file with
`byte_limit: 1`; the controller records the resulting zero-byte artifact. Do not pass
`byte_limit: 0` or add placeholder bytes.
