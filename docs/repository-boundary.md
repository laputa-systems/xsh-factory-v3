# Repository boundary

`xsh-factory-v3` owns the generic Rust kernel and host, XSH application
declaration, migrations, tests, and ignored runtime roots. The local
`pi-agent-core-rs` checkout is a separately pinned source dependency.
It does not import Factory V1/V2 code or durable state.

`../xsh` owns product source, product documentation, tests, and Git history.
It is never the factory database, ticket buffer, transcript store, CAS, or
workflow state. Factory work reaches it only through kernel-owned isolated
worktrees and guarded local fast-forward delivery.

The pinned `pi-agent-core-rs` checkout is a runtime source dependency, not an
XSH assignment workspace. A future `factory-engineer` core lane must bind its
own application revision and isolated worktree to that repository; an XSH
campaign cannot edit or deliver core changes. Core delivery changes the
qualified runtime and therefore requires the daemon-stopped rebuild and
requalification boundary described in `V1.md`.

The dependency direction is deliberately one way:

```text
applications/xsh -> factory-protocol / factory-pi-host -> factoryd / Rust kernel -> ../xsh
```

Rust must compile and test without `applications/xsh`. The XSH application may
use only the public SDK authoring surface and declarative Markdown paths. It
may not connect to PostgreSQL, access runtime/CAS paths, construct a commit,
spawn a session, or inspect live kernel state. The actor host receives a sealed
assignment packet and does not import application source at runtime.
