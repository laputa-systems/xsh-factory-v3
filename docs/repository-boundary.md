# Repository boundary

`xsh-factory-v3` owns the factory kernel source, Deno SDK/host source, application declarations,
migrations, tests, and future runtime evidence under its own ignored `var/` root. It is a cleanroom
repository and must not import Factory V1 or V2 code or durable state.

`../xsh` is the product repository. It owns XSH source, documentation, tests, and its own Git
history. It must never become the factory's database, artifact store, ticket buffer, Forum store,
session transcript store, or workflow-state location.

The boundary is one-way:

```text
applications/xsh -> @factory/sdk -> factoryd / Rust kernel -> local product checkout
```

Generic Rust must compile and test without `applications/xsh`. The XSH application may import only
the public `@factory/sdk` authoring surface; it may not import Rust source, open a PostgreSQL
connection, access a kernel-owned CAS path, construct a Git commit, or start a Pi session. The live
generic host will receive a sealed assignment packet and must not import application source.

The checked dependency-direction tests enforce the source-level portion of this boundary. Future
kernel admission will enforce it at runtime by accepting only the canonical closed bundle.
