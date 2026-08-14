# Control-plane lifecycle

## Product to delivery

1. The operator builds and qualifies a kernel, then registers and activates an
   immutable XSH application revision.
2. A campaign pins that revision, the qualified product snapshot, one aggregate
   budget, one deadline, and one delivery target.
3. Product supplies a bounded defect proposal. The kernel runs the admitted
   reproducer twice and seals the observations. Product discovery compares a
   canonical status-only manifest so host-specific stdout/stderr cannot make a
   repeat irreproducible; the raw streams remain sealed diagnostic artifacts.
4. The external Architect sponsors one proposed ticket revision. This is the
   first human judgement gate.
5. Engineering receives that ticket and an isolated worktree. It must obtain a
   kernel-captured failing regression checkpoint before the implementation fix.
   A ticket's already-sealed Product reproducer may validly checkpoint the
   pristine tree, so the checkpoint patch can be empty; its failed command log,
   the nonempty final candidate patch, and hard validation remain mandatory.
6. Engineering submits its candidate using only a normalized commit message
   and checkpoint identity. The kernel captures the final tree, changed paths,
   binary patch, completion record, and risk record; runs the reproducer and
   full suite on a pristine materialization; and records the result. If an
   unrelated validation test is proven flaky, Engineering has a ten-minute,
   two-rerun remediation budget; a narrowly named reversible disable may be
   used only after an isolated pass, with the original test body retained and
   the rationale recorded. This is a candidate change, not a validation
   waiver. A passing candidate is not yet delivered.
7. Quality receives a fresh worktree from the exact candidate tree, reruns the
   independent full suite through the kernel, and submits a qualitative review.
8. The Architect delivers, requests one bounded rework, or rejects. Delivery
   constructs provenance-bearing local Git history, appends the final known
   Factory-Cost trailer to the resulting commit, and guardedly fast-forwards
   `../xsh`; it never pushes.

## What an actor cannot decide

No actor can create a repository binding, pick a ticket outside its packet,
assert a tree/commit/validation result, change the product default branch,
deliver a commit, or waive a required read, cost, or hard-validation failure.
Quality prose cannot turn a failed full suite into a pass. Forum content is
never a lifecycle command.

## What the Architect decides

The Architect chooses whether to sponsor a valid Product ticket and whether to
deliver/rework/reject an independently reviewed candidate. A written override
may address a qualitative Quality rejection only; it cannot bypass a hard
validation failure, unknown/exceeded cost, bad evidence, dirty checkout, or
non-fast-forward delivery.

## Failure and recovery

The daemon owns the process group, deadline, output cap, cancellation, direct
wait, and terminal reconciliation. A provider cost that cannot be measured is
fail-closed for campaign admission. A released ticket requalifies again under
an idempotency key bound to its campaign, immutable ticket-revision row, and
current revision, so a retry cannot collide with sealed evidence from its
earlier attempt. Assignment launch keys additionally bind the Engineering
attempt (and Quality candidate when applicable), so a release retry cannot
recover a prior packet/session pair. Ticket-claim keys likewise bind that
sponsored revision, so the claim transition cannot recover an earlier attempt.
The raw session archive and typed terminal evidence remain available for
diagnosis. If a daemon stops after sealing a
complete current-head replay pair but before claiming the ticket, recovery
verifies and reuses that exact pair; it never mixes a partial pair with a fresh
observation.

A recovered assignment packet retains the campaign's admitted kernel identity
even when the resident controller was rebuilt. The installed build is recorded
separately by its qualification/install receipt; recovery does not silently
rewrite in-flight campaign lineage.

A new campaign after a terminal failure is not recovery of the failed campaign.
An explicit fresh paid-cycle request is required; admission rereads the live
daemon and application state and uses a fresh client command ID. The earlier
campaign and its sealed evidence remain immutable, and one fresh request does
not authorize an automatic campaign after that new attempt also fails.

Artifact bytes are globally content-addressed. Their row records the first
physical sealer, not exclusive campaign provenance, so final authority checks
the exact sealed digest and byte length. Packet and validation operations still
independently bind their kernel identity to the campaign; a deduplicated or
recovery-produced artifact cannot be rejected merely because another build
sealed identical bytes first.

Validation commands run with a closed minimal environment. Cargo profiles add
only the directory of the already-qualified Cargo executable, plus their exact
`rustc` and `rustdoc` siblings, so a product integration test may invoke Cargo
without inheriting an ambient operator `PATH`.

The current remaining weakness is intentional and explicit: Engineering work
is only admitted as a candidate after its actor calls `candidate_submit`. A
tool/protocol failure after a useful checkout can therefore lose an otherwise
recoverable patch. The next architecture change moves that recovery boundary
into controller-owned terminal reconciliation; see the [V1 backlog](../V1.md).
