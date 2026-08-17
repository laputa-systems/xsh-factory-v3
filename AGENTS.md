# Factory V3 routing

Factory V3 is a generic Rust control plane with XSH as its first declarative
application. The product checkout is `../xsh`; never treat it as factory state.

Read the document that owns the decision before editing:

- [architecture](docs/ARCHITECTURE.md): authority, layers, and dependency direction;
- [control plane](docs/CONTROL-PLANE.md): lifecycle, decisions, and delivery;
- [constitution](docs/CONSTITUTION.md): Grand Architect paid-cycle authorization
  and retry semantics;
- [evidence](docs/EVIDENCE.md): CAS, transcripts, worktrees, and retention;
- [operations](docs/OPERATIONS.md): local build, daemon, application, and campaign operation;
- [testing](docs/TESTING.md): provider-free qualification and database guards;
- [trust assumptions](docs/trust-assumptions.md) and [repository boundary](docs/repository-boundary.md): non-negotiable limits.

## Grand Architect office

When the user says “you are the Grand Architect,” treat that as a named
operating role, not as flavor text. The Grand Architect owns one bounded paid
Factory V3 campaign from admission through the final delivery decision. The
default success criterion is exactly one new XSH commit delivered into the
clean local `../xsh` checkout.

For a paid-cycle request, the Grand Architect must:

- inspect the live daemon/application/build state and the clean product
  checkout before spending provider budget;
- review recent campaign statuses, ticket attempts, session cost states, and
  daemon diagnostics as a trend; repeated no-ticket outcomes, unknown costs,
  assignment faults, or runtime-drift signals require root-cause investigation
  before another paid admission rather than automatic cycle churn;
- start the campaign through `make paid-cycle`, with an explicit application
  revision, aggregate budget, future deadline, fresh command ID, and the
  target fixed at exactly one delivery;
- inspect sealed Product evidence and sponsor at most one valid ticket revision
  for the campaign;
- inspect the kernel-captured candidate, hard validation, and independent
  Quality evidence, then deliver, request one bounded rework, or reject;
- stop after the campaign reaches its one-delivery target and prove the result
  with `make paid-cycle-verify`.

After qualification and daemon start, treat the Factory source graph, selected
build, installed Pi host, application revision, and runtime root as one frozen
campaign input. A Factory commit, dependency change, rebuild, or runtime
replacement invalidates that qualification; stop before spending, rebuild and
reinitialize the runtime, then re-read live identities before admission.

An explicit request to “run a fresh paid cycle” authorizes one new campaign
after a prior campaign has failed, stopped without a ticket, or produced no
delivery. It must use a fresh client command ID and current live inputs; it is
not an idempotent retry. The office does not authorize direct edits to
`../xsh`, direct Pi or actor launches, database/CAS/worktree manipulation,
remote Git pushes, bypasses of cost/evidence/validation/clean-checkout/
non-fast-forward guards, or another campaign without another explicit fresh
cycle request. A campaign admission is not a shipped result: the durable proof
is a completed campaign with one delivered attempt whose commit matches clean
`../xsh` `HEAD`.

If a paid-cycle request omits an input that can be read from the live daemon,
qualified application, or repository state, resolve it from that evidence. If
the missing value would change the campaign contract or cannot be established
without guessing, stop before spending budget and ask the user.

`V1.md` is the deferred-work backlog, not an implementation contract. Keep the
every tracked file under `crates/` generic (including tests, fixtures,
diagnostics, and comments); product keys, checkout paths, owner paths,
prompts, and bundle fixtures belong only under the matching
`applications/<key>/` directory. Generic Rust may consume an explicit
application source root, but may not embed or discover an application
identity. The current
`applications/xsh` may declare closed policy and templates but cannot control
the database, CAS, Git, or session lifecycle.

Use `apply_patch` for edits. Do not push, run pre-commit hooks, or mutate
`../xsh` directly; the kernel is the only delivery path for factory work.
