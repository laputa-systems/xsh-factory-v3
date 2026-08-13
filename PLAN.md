# Remaining architectural gaps

The durable design is documented in `docs/`. This file records only work that
is still materially incomplete. It is not authority for speculative framework
work.

1. **Kernel-owned Engineering recovery.** A useful Engineering worktree can
   currently be lost when the actor fails before `candidate_submit`, even
   though the kernel can independently capture its tree, binary patch, changed
   paths, and validation. After a successful regression checkpoint, terminal
   reconciliation must preserve a changed worktree as controller-owned
   recovery evidence and offer an explicit, bounded continuation or
   validation path. Candidate completion records and risk records are already
   kernel-sealed; the remaining gap is recovery before `candidate_submit`.
2. **Terminal-cost recovery.** A host protocol failure after paid work can
   leave cost `unknown` and fail a campaign even when the provider response and
   product changes are otherwise usable. The host/kernel boundary needs a
   crash-safe, independently verifiable terminal usage handoff that remains
   fail-closed when no measurement exists.
3. **Scheduler throughput correctness.** Rejection and infrastructure failure
   must never cause repeated paid Product discovery for the same unmet demand.
   Scheduler transitions need a focused real-driver test covering proposal,
   sponsorship, Engineering failure, rework/recovery, and one-delivery stop.
4. **Retention and garbage collection.** Worktrees and staging directories are
   transient and are removed by the controller, but CAS objects currently have
   no lifecycle collector. Design a reference-safe, operator-invoked CAS and
   stale-staging collector before long-running operation; it must never remove
   evidence reachable from PostgreSQL.
5. **Application authoring ergonomics.** Template BLAKE3 pins and predecessor
   bundle lineage are correct but manually copied today. The compiler should
   derive and verify them from one declared source of truth so a harmless
   prompt edit does not require hand-editing opaque digests.
6. **First accepted delivery.** The generic provider-free vertical is complete,
   but the paid XSH Product → Engineering → Quality → Architect delivery path
   has not yet produced its first durable XSH commit. Complete it only through
   the controller, preserve the failed-session evidence, and use its measured
   failures to close the gaps above.

Deferred scope such as self-upgrade, concurrent campaigns, remote workers, and
stronger sandboxing remains in `V1.md` and is not implicitly approved.
