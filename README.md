# Factory V3

Factory V3 turns a reproducible product defect into a reviewed, locally
delivered XSH commit. Rust owns durable state, custody, validation, commit
construction, and delivery. The XSH application contributes only versioned
closed policy and worker templates.

Start with the [architecture](docs/ARCHITECTURE.md), then the
[control plane](docs/CONTROL-PLANE.md) and the
[Grand Architect constitution](docs/CONSTITUTION.md). Operators should read
[operations](docs/OPERATIONS.md), [evidence](docs/EVIDENCE.md), and
[testing](docs/TESTING.md) before starting a campaign.

The actor boundary is cooperative, same-user host execution; it is not a
sandbox. See [trust assumptions](docs/trust-assumptions.md).

## Local setup

Factory links directly to the frozen local Tea checkout at
`/Users/josh/d/tea-copy`. Factory never uses a package-registry runtime or
discovers agent code from the environment.
Populate both Cargo workspaces once:

```sh
make cache
```

Run the Rust-only qualification before committing factory changes. The Grand
Architect owns this gate:

```sh
make lint
```

The first full provider-free gate is:

```sh
make tea-acceptance
```

`make factoryd-serve` keeps the OpenRouter credential out of the daemon
environment. The daemon checks `vault OPENROUTER_API_KEY -- ...` before
binding its socket and resolves the credential from Vault for each assignment.
The live lane must use the continuous `factory_live_v3` database through
`FACTORY_DATABASE_URL`; lifecycle targets reject cycle-specific live names.
`FACTORY_RUNTIME_ROOT` remains explicit, and the daemon does not itself start
paid work.

## Application bundles

The checked-in XSH V2 bundle is inert application data. Register it directly
before activating an application revision:

```sh
factoryctl application register xsh \
  --source-root applications/xsh \
  --bundle-relative-path bundle.v2.json
```

Registration re-reads the declared templates and policy artifacts beneath
`applications/xsh`, verifies their BLAKE3 identities, and adopts the immutable
bytes into CAS. A campaign pins one active application revision; its Product,
Engineering, and Quality sessions cannot see mutable application source.

Detailed command sequences, guards, and recovery rules are in
[operations](docs/OPERATIONS.md).
