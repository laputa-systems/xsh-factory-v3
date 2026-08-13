# Repository boundary

`xsh-factory-v3` owns the generic kernel, Deno SDK/host, XSH application
declaration, migrations, tests, vendored Pi source, and ignored runtime roots.
It does not import Factory V1/V2 code or durable state.

`../xsh` owns product source, product documentation, tests, and Git history.
It is never the factory database, ticket buffer, transcript store, CAS, or
workflow state. Factory work reaches it only through kernel-owned isolated
worktrees and guarded local fast-forward delivery.

The dependency direction is deliberately one way:

```text
applications/xsh -> @factory/sdk -> factory-pi-host -> factoryd / Rust kernel -> ../xsh
```

Rust must compile and test without `applications/xsh`. The XSH application may
use only the public SDK authoring surface and declarative Markdown paths. It
may not connect to PostgreSQL, access runtime/CAS paths, construct a commit,
spawn a session, or inspect live kernel state. The actor host receives a sealed
assignment packet and does not import application source at runtime.

`vendor/pi-headless` is a pinned submodule used to build local headless ESM
artifacts. It is a supply-chain input, not a second factory control plane.
