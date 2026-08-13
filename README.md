# Factory V3

Factory V3 turns a reproducible product defect into a reviewed, locally
delivered XSH commit. Rust owns durable state, custody, validation, commit
construction, and delivery. The XSH application contributes only versioned
closed policy and worker templates.

Start with the [architecture](docs/ARCHITECTURE.md), then the
[control plane](docs/CONTROL-PLANE.md). Operators should read
[operations](docs/OPERATIONS.md), [evidence](docs/EVIDENCE.md), and
[testing](docs/TESTING.md) before starting a campaign.

The actor boundary is cooperative, same-user host execution; it is not a
sandbox. See [trust assumptions](docs/trust-assumptions.md).

## Local setup

The vendored [`pi-headless`](vendor/pi-headless) submodule is the only Pi
implementation. Factory never uses a system Pi executable or a Pi SDK resolved
from a registry. Populate its locked build inputs and the Deno cache once:

```sh
make cache
```

Build the local headless ESM artifacts and run the ordinary provider-free
qualification:

```sh
make check
```

`make factoryd-serve` deliberately introduces the OpenRouter credential only
at the daemon process boundary through `vault OPENROUTER_API_KEY -- ...`.
It requires explicit `FACTORY_DATABASE_URL` and `FACTORY_RUNTIME_ROOT`; it
does not itself start paid work.

## Application bundles

Compile the inert XSH bundle before registering an application revision:

```sh
deno run --allow-read --no-prompt --frozen --cached-only applications/xsh/mod.ts \
  > applications/xsh/bundle.v1.json
```

The generated bundle is ignored. Registration re-reads it and every named
template beneath `applications/xsh`, verifies their BLAKE3 identities, and
adopts the immutable bytes into CAS. A campaign pins one active application
revision; its Product, Engineering, and Quality sessions cannot see mutable
application source.

Detailed command sequences, guards, and recovery rules are in
[operations](docs/OPERATIONS.md).
