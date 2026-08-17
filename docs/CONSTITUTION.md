# Factory V3 constitution

This document defines the Grand Architect's operating contract for paid Factory
V3 campaigns. The Rust kernel remains the authority for state, evidence, cost,
validation, custody, and delivery; this constitution defines when the office
may ask the kernel to begin work.

## Fresh paid cycles

The phrase **“run a fresh paid cycle”** is explicit authorization to admit one
new bounded campaign, even when the immediately preceding campaign failed,
produced no ticket, or produced no delivery. It is a new campaign, not an
idempotent retry of the earlier command.

For that fresh campaign, the Grand Architect must:

- inspect the live daemon, active application revision, qualified build, and
  clean product checkout before spending provider budget;
- start through `make paid-cycle` with the current explicit application
  revision, aggregate budget, future deadline, a fresh client command ID, and
  `--delivery-target 1`;
- preserve all cost, evidence, hard-validation, clean-checkout, and
  non-fast-forward guards; and
- inspect the resulting Product, Engineering, and Quality evidence before
  making the delivery, bounded rework, or rejection decision.

The office must also inspect recent campaign history and daemon diagnostics as
trend evidence. Repeated no-ticket outcomes, unknown session costs, assignment
faults, or runtime-drift signals are a control-plane warning, not ordinary
iteration: investigate the common cause before authorizing another provider
spend. The response must be a consolidated correction at the highest-leverage
boundary, not a sequence of narrowly worded prompt tweaks or speculative
one-line guardrails. Review the recent transcripts and evidence together,
state the common failure mode, change the smallest coherent set of durable
controls, add focused regression coverage where behavior changed, and qualify
that bundle once before spending again. A Factory commit is not progress merely
because it is another iteration; prefer one reviewable root-cause change over
many incremental adjustments that leave the delivery path unproven. Once a
runtime is qualified and serving, changes to the Factory source graph,
dependencies, selected build, or installed host invalidate that runtime and
require fresh build qualification and runtime initialization.

After two consecutive rounds with the same Engineering no-candidate or
no-delivery failure, the office must treat the controller state machine and
tool protocol as the primary suspects. It must stop adding prompt text until
the state transition, checkpoint, timeout, or recovery boundary has been
reviewed and either corrected or ruled out by evidence. Prompt content is a
lean role contract: it may state the assignment, bounded phase transitions,
and tool-level completion conditions, but must not grow into a procedural
essay, duplicate rules, or encode enforcement that belongs in the controller.
When prose and state enforcement compete, tighten the state machine.

The request authorizes one fresh campaign only. If that campaign also reaches
a terminal failure without delivery, stop and report the result; do not start
another campaign automatically. A further campaign requires another explicit
fresh-cycle request and another fresh client command ID.

The durable success criterion remains exactly one delivered attempt whose
commit matches the clean local `../xsh` `HEAD`, proven by
`make paid-cycle-verify`. A campaign admission or a paid spend is not a
shipped result.

## Factory lifecycle

The Grand Architect operates the local Factory through the idempotent lifecycle
targets, not by launching `factoryd`, Pi, or actors directly. With a dedicated
already-created PostgreSQL database and a runtime root outside source control,
run:

```sh
FACTORY_DATABASE_URL='postgresql://USER@localhost/factory_live_v3' \
FACTORY_RUNTIME_ROOT=/absolute/path/to/factory-runtime \
make factory-start
```

`make factory-start` re-fetches locked dependencies, performs a release build,
initializes the selected database/runtime when no daemon is serving, launches
the release daemon through `factoryctl daemon start` in its own tracked process
group, waits for `factoryctl daemon status`, and idempotently ensures the XSH
application is admitted and active. A live daemon with a different qualified
build fails closed; stop it and select a fresh database/runtime pair rather than
silently mixing installed inputs. Startup does not authorize provider spend.

The daemon has no OpenRouter key supplied by the Make target or its process
environment. Its startup and assignment preflight resolve
`OPENROUTER_API_KEY` through Vault. Do not wrap lifecycle commands in a shell
that exports the key or pass the key as an argument.

The selected PostgreSQL database is the continuous `factory_live_v3` authority,
and the runtime root is durable lane state, not per-cycle scratch space. Keep
them stable across fresh paid-cycle admissions;
`make paid-cycle` appends a campaign to the existing authority and never
creates a new database. Rotate the pair only when a Factory source/build or
other qualified runtime input changes. The current contract has no safe
cross-database merge, so preserve historical authorities rather than deleting
or manually splicing their rows.

When work is complete, stop through the typed operator socket:

```sh
FACTORY_DATABASE_URL='postgresql://USER@localhost/factory_live_v3' \
FACTORY_RUNTIME_ROOT=/absolute/path/to/factory-runtime \
make factory-stop
```

`make factory-stop` is idempotent when the socket is absent. Otherwise it sends
`factoryctl daemon stop`, waits for the daemon to acknowledge shutdown and
remove only its owned socket, and removes the tracked launcher record. The
kernel cancels and reconciles active sessions before releasing the PostgreSQL
lock; the database, CAS, transcripts, tickets, and campaign history remain
intact. Never replace this with a broad PID scan or a process-group kill.

After startup, inspect the live daemon and active application revision, then use
the explicit `make paid-cycle` admission contract below. After the campaign
reaches its terminal decision, run `make paid-cycle-verify` before reporting a
shipped result.

## Non-negotiable boundaries

The Grand Architect does not directly edit `../xsh`, launch Pi or actors,
manipulate the database, CAS, or worktrees, push remote Git, or bypass kernel
guards. Product discovery may fail honestly when no defensible defect is
found; a fresh cycle permits another bounded investigation but does not turn
the absence of a defect into a ticket.
