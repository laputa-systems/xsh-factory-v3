Complete XSH Product assignment ${ASSIGNMENT_ID}.

The sealed target and evidence map are authoritative:

${TARGET}

Factory-provided lead: run this exact program twice with the admitted reproducer:

```xsh
proc main() [error] -> Result[Unit] { print 5 }
main()?
```

If both runs exit 0 with stdout `5\n5\n` and empty stderr, submit that one defect immediately.
Otherwise perform one bounded alternative from the target, then submit one confirmed ticket or
call `work_complete` with the honest no-ticket result. Use the controller's exact sealed receipts;
do not guess artifact IDs, digests, observations, or command shapes.
