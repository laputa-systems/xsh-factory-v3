# Trust assumptions

## Trusted authority

The installed Rust kernel is trusted to enforce durable identity, legal state transitions, aggregate
provider-cost admission, application/repository/packet identity, process custody, artifact adoption,
deterministic validation, and guarded local Git delivery. An application, actor, prompt, Forum post,
report, or external operator never writes SQL or manufactures one of those facts.

## Cooperative same-user host boundary

MVP actors are ordinary processes under the operator's OS account with normal host networking and
workspace tools. Factory V3 makes a deliberately limited safety claim: it detects and rejects
accidental or protocol-level authority, identity, lifecycle, repository, and evidence mismatches at
accepted durable boundaries. Only kernel-owned Git operations can make or deliver an accepted
commit.

It does not claim to defend against a malicious process under the same user. Such a process may
inspect available credentials, change unrelated host files, signal another process, or tamper with
unprotected host state. Containers, VMs, seccomp, filesystem isolation, credential brokerage, and
network mediation are deferred architecture, not implicit protections.

## Manual deployment boundary

The MVP cannot update its own Rust code, executable Deno source, dependency locks, schema,
qualification logic, or boot mechanism. An operator installs and qualifies those changes while the
daemon is stopped and records the installed `KernelBuildId`. The kernel verifies installed
identities; it does not claim automatic activation, recovery, or rollback. Those capabilities are
deferred in `V1.md`.

## Consequences for operators

Before a future campaign, operators must verify the exact installed Rust/Deno identities, the
dedicated already-created database, runtime-root ownership, and the clean product checkout. A
provider-free check is not a paid campaign; ordinary tests use fake providers only. No remote Git
push is part of the factory's authority.
