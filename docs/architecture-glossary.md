# Architecture glossary

## Kernel

The installed Rust authority. The kernel alone will admit durable state, manage PostgreSQL, custody
artifacts and processes, validate candidates, construct commits, and guardedly deliver local product
commits. `factoryd` and `factoryctl` are its daemon and operator executable shells; they do not
create additional authority layers.

## Protocol

The closed Rust values and wire shapes shared by the daemon and CLI through `factory-protocol`.
Protocol data names exact fields and legal enum values. It does not include arbitrary metadata,
callbacks, opaque application data, or a workflow language.

## Application bundle

`ApplicationBundleV1` is the canonical, bounded data the kernel can admit as an application
revision. It binds a generic application key and repository to mission/template artifacts, fixed
offices, profiles, required reads, ticket limits, reproducer and validation commands, path/Git
policy, and commit policy. The Deno authoring equivalent is `defineApplicationV1(...)` in
`packages/factory-sdk`.

## Application revision

An immutable kernel-admitted instance of an application bundle. An application can propose source
data, but only the future kernel admission path makes that data an authoritative revision. A running
campaign pins exactly one revision.

## Aggregate revision

The monotonically increasing revision of one durable aggregate. A mutating command will supply the
revision it observed; a different current value is an explicit conflict, not an implicit overwrite.

## Artifact and digest

An artifact is a bounded byte sequence adopted by the kernel into its append-only content-addressed
store. Its BLAKE3 digest identifies bytes, while the owning domain relation gives those bytes their
role and provenance.

## Assignment and session

An assignment is one immutable packet authorizing one office to perform one exact task. A session is
one fresh, kernel-custodied actor process for that assignment. Neither is a reusable agent identity
or an authority source.

## Required read

An exact canonical file path and BLAKE3 digest that an actor must read through the wrapped read
tool. A shell command, search result, prompt quotation, or assertion does not satisfy it.

## Candidate and validation

A candidate is an Engineering-submitted tree captured from its exact assigned base. Validation is a
kernel-owned deterministic command receipt against a pristine exact tree. A later Quality review is
independent qualitative evidence, not a substitute for validation.

## Campaign

A bounded execution and accounting envelope. It pins a kernel build, application revision, aggregate
provider-cost cap, wall deadline, and delivery target. Application ticket inventory and Forum
history outlive campaigns.

## Forum

Permanent shared, non-authoritative discussion. A Forum post cannot sponsor a ticket, complete an
assignment, grant authority, or certify a candidate.
