# XSH V2 application source

`bundle.v2.json` is the inert XSH application declaration. Rust reads the
declaration, resolves the seven Markdown templates and the three Luau policy
sources below this directory, verifies each BLAKE3 digest and byte limit, and
seals the resulting source bytes. A running actor never reads this directory.

The model price fields in the bundle are admitted accounting inputs. Keep their
source of truth in Pi's model catalog at
`/Users/josh/d/pi/packages/ai/src/providers/data/openrouter.json`; update the
bundle only after syncing the selected model's rates and limits from that
catalog.

Every role policy returns the `pi-agent-luau` `factory_policy` declaration:

- `system_prompt_append` is present and currently empty;
- each model-visible tool has a closed `schema_json`, sequential execution,
  and a coroutine `handler_source`; and
- every handler yields the single `factory` capability with an explicit
  operation method. The Rust host validates the parsed JSON and binds methods
  to the connection-owned operation for the admitted assignment.

The policy is not authority. Naming a tool, capability, or operation in Luau
does not grant it. The host checks the sealed policy against the packet's
exact tool allowlist and installs only the Rust bindings for that role.

## Role tool surface

| Role | Policy | Model-visible tools |
| --- | --- | --- |
| `product_research` | `policies/product_research.luau` | `workspace_read`, `workspace_search`, `workspace_list`, `shell`, `artifact_seal`, `product_submit_ticket`, `work_complete` |
| `engineering` | `policies/engineering.luau` | `workspace_read`, `workspace_write`, `workspace_edit`, `workspace_search`, `workspace_list`, `shell`, `forum_search`, `forum_list_topics`, `forum_list_threads`, `forum_read_thread`, `publication_create`, `artifact_read`, `candidate_checkpoint_regression`, `candidate_submit` |
| `quality` | `policies/quality.luau` | `workspace_read`, `workspace_write`, `workspace_edit`, `workspace_search`, `workspace_list`, `shell`, `forum_search`, `forum_list_topics`, `forum_list_threads`, `forum_read_thread`, `publication_create`, `artifact_seal`, `artifact_read`, `quality_run_full_suite`, `quality_submit_review` |

The lists are deliberately repeated in the static bundle and in the policy
declarations. The Rust compiler and host must reject a mismatch instead of
silently taking the intersection or union.

## Capability method contract

Policy handlers use these method names. Each method is a host-side dispatch
key, not an application-defined operation:

| Tool family | Rust method prefix or operation |
| --- | --- |
| Workspace and shell | `workspace.read`, `workspace.write`, `workspace.edit`, `workspace.search`, `workspace.list`, `workspace.shell` |
| Evidence | `artifact.seal_workspace_file`, `artifact.read` |
| Discussion/publication | `forum.search`, `forum.list_topics`, `forum.list_threads`, `forum.read_thread`, `publication.create` |
| Assignment lifecycle | `product.submit_ticket`, `candidate.checkpoint_regression`, `candidate.submit`, `quality.run_full_suite`, `quality.submit_review`, `work.complete` |

Rust remains responsible for path confinement, exact packet/session identity,
required-read accounting, command envelopes, terminal legality, evidence
sealing, cancellation, and model-visible result filtering. Luau cannot open a
file, run a process, inspect the environment, access a socket, access a
database, or choose a different assignment.
