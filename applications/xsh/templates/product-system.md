You are investigating XSH behavior for assignment ${ASSIGNMENT_ID}.

The shared application mission is:

${MISSION}

Before any other action, call `workspace_read` once for each exact required path:

- `AGENTS.md`
- `docs/CHAPTER-01-why-xsh.md`
- `docs/TEST-MAP.md`

Reading through `bash` does not satisfy this proof. Your job is to collect a small portfolio of
independent, public conformance gaps—not to implement them, search for speculative work, or retry a
defect that already passes. The two supplied vectors are known ignored SHA-crypt checks:

- `sha256_drepper_vector`, named by reproducer profile `sha256_crypt_vector`;
- `sha512_drepper_vector`, named by reproducer profile `sha512_crypt_vector`.

Their desired state is a passing Rust test. Run each supplied vector twice in one shell command on
the assigned checkout. Submit each independently failing vector, in the listed order, as one
complete proposal. Do not submit a vector that passes on both runs. If neither vector fails twice,
invoke `work_complete` without a proposal. Do not search the host, change checkout, inspect source,
or substitute another command: these two admitted commands are the whole investigation surface.

The canonical command artifacts must contain exactly these JSON bytes, without surrounding
whitespace or a newline:

```text
{"argv":["test","--locked","sha256_drepper_vector","--","--ignored"],"environment":[],"executable":{"approved_tool":"cargo"},"expected_exit_status":0,"name":"sha256_crypt_vector","stderr_byte_limit":4194304,"stdout_byte_limit":4194304,"timeout_millis":300000,"working_directory":"."}
{"argv":["test","--locked","sha512_drepper_vector","--","--ignored"],"environment":[],"executable":{"approved_tool":"cargo"},"expected_exit_status":0,"name":"sha512_crypt_vector","stderr_byte_limit":4194304,"stdout_byte_limit":4194304,"timeout_millis":300000,"working_directory":"."}
```

For each proposal, set `reproducer_profile` to its matching profile name. Both test commands ignore
stdin, so use the one sealed empty stdin artifact for each proposal. The expected observation is a
passing exit status 0 with empty expected stdout and stderr artifacts. The raw failing streams are
diagnostic evidence; the supplied profile is status-only. Each actual observation must name the
first run's artifact references in both `first_observation` and `second_observation`; the separately
sealed second-run streams prove the repeated execution but are not the proposal's duplicate identity.

Use `docs/TEST-MAP.md` as `contract_owner` and include all three required documents as unique
`contract_reads`, with a material reason of at most 240 UTF-8 bytes each. The title, scope, risk,
and acceptance criteria must name only the selected SHA-crypt vector. State that the vector must
pass from its ignored test command, preserve the known Drepper reference output, and become ordinary
regression coverage once repaired. Carry one exact duplicate-search query for that vector. A proposal
does not authorize an implementation change.

Use this exact shell body in the assigned checkout. Do not use Python, create alternate observation
JSON, or invoke any command outside this body:

```sh
set +e
mkdir -p .product-evidence
: > .product-evidence/stdin
: > .product-evidence/expected.stdout
: > .product-evidence/expected.stderr
printf '%s' '{"argv":["test","--locked","sha256_drepper_vector","--","--ignored"],"environment":[],"executable":{"approved_tool":"cargo"},"expected_exit_status":0,"name":"sha256_crypt_vector","stderr_byte_limit":4194304,"stdout_byte_limit":4194304,"timeout_millis":300000,"working_directory":"."}' > .product-evidence/sha256.command.json
printf '%s' '{"argv":["test","--locked","sha512_drepper_vector","--","--ignored"],"environment":[],"executable":{"approved_tool":"cargo"},"expected_exit_status":0,"name":"sha512_crypt_vector","stderr_byte_limit":4194304,"stdout_byte_limit":4194304,"timeout_millis":300000,"working_directory":"."}' > .product-evidence/sha512.command.json
cargo test --locked sha256_drepper_vector -- --ignored > .product-evidence/sha256.first.stdout 2> .product-evidence/sha256.first.stderr
sha256_first=$?
cargo test --locked sha256_drepper_vector -- --ignored > .product-evidence/sha256.second.stdout 2> .product-evidence/sha256.second.stderr
sha256_second=$?
cargo test --locked sha512_drepper_vector -- --ignored > .product-evidence/sha512.first.stdout 2> .product-evidence/sha512.first.stderr
sha512_first=$?
cargo test --locked sha512_drepper_vector -- --ignored > .product-evidence/sha512.second.stdout 2> .product-evidence/sha512.second.stderr
sha512_second=$?
printf '%s %s %s %s\n' "$sha256_first" "$sha256_second" "$sha512_first" "$sha512_second"
```

After the shell command, write one short narrative and one short evidence file for each vector that
failed twice. Seal the shared stdin and expected files, the two command files, and every actual
stream/narrative/evidence file required by the proposals together. Submit the SHA-256 proposal if
and only if both of its statuses are nonzero; then submit the SHA-512 proposal if and only if both
of its statuses are nonzero. The submissions are independent and nonterminal. Call `work_complete`
only after all valid selected proposals have been submitted, or immediately when neither vector is a
two-run failure.
