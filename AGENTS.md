# Factory V3 working contract

Factory V3 is a cleanroom generic factory kernel with one initial XSH application. `PLAN.md` is the
controlling implementation contract. `V1.md` records deferred work and does not authorize
placeholders for it.

## Read before deciding

Read `PLAN.md` and `V1.md` in full before making a substantive architectural decision. Then read the
nearest crate, package, application, or test contract. Keep the product checkout (`../xsh`) separate
from this repository.

## Boundaries

- The Rust workspace is the trusted generic authority. It must contain no XSH product vocabulary,
  application callback, application import, or product policy decision.
- `applications/xsh` is a one-way consumer of `@factory/sdk`. It declares closed data and Markdown
  paths only. It must not connect to PostgreSQL, access kernel-owned runtime/CAS paths, construct
  Git commits, or launch an actor session.
- The product checkout is never factory state, a database, or an artifact store. Do not put factory
  workflow state in `../xsh`.
- The root `deno.json` is the only TypeScript dependency authority. Do not add Node tooling,
  `package.json`, a third-party TypeScript runner, or another resolver.
- The initial Rust workspace has exactly four crates. Do not add a crate, database table, resident
  process, or dependency without the explicit plan revision required by `PLAN.md`.

## Implementation and evidence

- Keep authority in typed Rust contracts; keep application policy declarative; keep qualitative
  judgment in prompts and actor reports.
- Use no opaque metadata maps, generic policy/workflow engines, callback hooks, or executable
  application plugins. Add a named protocol field only when a selected plan change needs it.
- Use `apply_patch` for edits. Preserve unrelated user changes. Never reset, commit, push, or run
  pre-commit hooks unless expressly asked.
- Tests must be provider-free. Do not make model-provider calls outside an explicitly authorized
  paid campaign.
- Start with focused provider-free checks. The standard local qualification entrypoint is
  `make check`; populate the locked Deno cache first with `make cache` when necessary.

## Trust statement

Actors are cooperative same-user host processes, not adversarially isolated tenants. The kernel
protects protocol and durable-boundary authority, but it does not claim to protect secrets or
unrelated host files from a malicious same-user process. Keep that limit explicit in operator-facing
text.
