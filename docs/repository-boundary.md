# Repository boundary

`xsh-factory-v3` owns the generic Rust kernel and host, XSH application
declaration, migrations, tests, and ignored runtime roots. The local
Tea checkout is a separately pinned source dependency.
It does not import Factory V1/V2 code or durable state.

### Crate genericity invariant

Every tracked file under `crates/` is application-neutral, including Rust
tests, fixtures, diagnostics, and comments. Crate code must not name a
product key, product checkout path, product executable, product owner path,
product prompt, or product-specific bundle fixture. Application-specific
policy, templates, bundle data, and qualification fixtures belong under the
matching `applications/<key>/` directory. Generic crate APIs may receive an
application source root and bundle path as explicit caller inputs, but they
must not embed an application identity or reach into an application directory.

The dependency-direction test scans every Rust source beneath `crates/` to
keep this invariant executable. A new product-specific requirement is a
boundary change: put it in its application directory or add a generic typed
interface before touching a crate.

`../xsh` owns product source, product documentation, tests, and Git history.
It is never the factory database, ticket buffer, transcript store, CAS, or
workflow state. Factory work reaches it only through kernel-owned isolated
worktrees and guarded local fast-forward delivery.

The pinned Tea checkout is a runtime source dependency, not an
XSH assignment workspace. A future `factory-engineer` core lane must bind its
own application revision and isolated worktree to that repository; an XSH
campaign cannot edit or deliver core changes. Core delivery changes the
qualified runtime and therefore requires the daemon-stopped rebuild and
requalification boundary described in `V1.md`.

The dependency direction is deliberately one way:

```text
applications/<key> -> factory-protocol / factory-tea-host -> factoryd / Rust kernel -> product checkout
```

Rust must compile and test without any application directory. The current XSH
application may use only the public SDK authoring surface and declarative
Markdown paths. It may not connect to PostgreSQL, access runtime/CAS paths,
construct a commit, spawn a session, or inspect live kernel state. The actor
host receives a sealed assignment packet and does not import application
source at runtime.
