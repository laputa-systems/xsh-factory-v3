# Factory V3 routing

Factory V3 is a generic Rust control plane with XSH as its first declarative
application. The product checkout is `../xsh`; never treat it as factory state.

Read the document that owns the decision before editing:

- [architecture](docs/ARCHITECTURE.md): authority, layers, and dependency direction;
- [control plane](docs/CONTROL-PLANE.md): lifecycle, decisions, and delivery;
- [evidence](docs/EVIDENCE.md): CAS, transcripts, worktrees, and retention;
- [operations](docs/OPERATIONS.md): local build, daemon, application, and campaign operation;
- [testing](docs/TESTING.md): provider-free qualification and database guards;
- [trust assumptions](docs/trust-assumptions.md) and [repository boundary](docs/repository-boundary.md): non-negotiable limits.

`PLAN.md` is a short list of known architectural gaps, not an implementation
contract. `V1.md` is deferred work only. Keep the Rust kernel generic;
`applications/xsh` may declare closed policy and templates but cannot control
the database, CAS, Git, or session lifecycle.

Use `apply_patch` for edits. Do not push, run pre-commit hooks, or mutate
`../xsh` directly; the kernel is the only delivery path for factory work.
