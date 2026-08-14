# The local pi-agent-core-rs checkout is intentionally explicit until its
# crates are published. Cargo does not expand `~` in a dependency path.
PI_AGENT_CORE_ROOT ?= /Users/josh/d/pi-agent-core-rs
PI_AGENT_CORE_MANIFEST := $(PI_AGENT_CORE_ROOT)/Cargo.toml
FACTORY_OPERATION_DEADLINE_MS ?= 900000
FACTORYCTL ?= $(CURDIR)/target/release/factoryctl
FACTORY_PAID_CYCLE_PRINCIPAL ?= grand-architect

# The factory keeps Clippy's correctness and default quality groups strict.
# Pedantic documentation/style heuristics and these boundary-shape
# warnings are reviewed policy, not pre-commit failures for this codebase.
CLIPPY_GATE_FLAGS := --deny warnings \
	--allow clippy::pedantic \
	--allow clippy::large_enum_variant \
	--allow clippy::result_large_err \
	--allow clippy::type_complexity \
	--allow clippy::too_many_arguments

.PHONY: cache lint release-build pi-agent-core-rs-test factoryd-serve paid-cycle paid-cycle-verify postgres-test ticket-test decision-test xsh-bundle-test provider-free-host provider-free-vertical backup-restore-test provider-free-acceptance pi-agent-core-rs-acceptance sqlx-check

# Build metadata and dependencies for both Rust workspaces. The external
# checkout is tested independently because it is a direct local dependency
# while the local core source remains the explicit dependency.
cache:
	test -f "$(PI_AGENT_CORE_MANIFEST)"
	cargo fetch --manifest-path "$(PI_AGENT_CORE_MANIFEST)"
	cargo fetch --workspace

pi-agent-core-rs-test:
	test -f "$(PI_AGENT_CORE_MANIFEST)"
	cargo fmt --all --manifest-path "$(PI_AGENT_CORE_MANIFEST)" -- --check
	cargo test --manifest-path "$(PI_AGENT_CORE_MANIFEST)" -p pi-agent-core --all-targets --features trace
	cargo test --manifest-path "$(PI_AGENT_CORE_MANIFEST)" -p pi-agent-trace
	cargo check --manifest-path "$(PI_AGENT_CORE_MANIFEST)" -p pi-agent-core --features parity-runner --bin pi-agent-parity

lint: pi-agent-core-rs-test
	cargo fmt --all -- --check
	cargo clippy --all-targets --all-features -- $(CLIPPY_GATE_FLAGS)
	cargo check --workspace --all-targets
	cargo test --workspace

release-build:
	cargo build --release --workspace

# The credential is introduced only at the daemon process boundary. Callers
# must choose the dedicated database and runtime root explicitly; this target
# never starts a provider-backed actor on its own.
factoryd-serve:
	test -n "$$FACTORY_DATABASE_URL"
	test -n "$$FACTORY_RUNTIME_ROOT"
	vault OPENROUTER_API_KEY -- target/release/factoryd serve \
		--database-url "$$FACTORY_DATABASE_URL" \
		--runtime-root "$$FACTORY_RUNTIME_ROOT" \
		--operation-deadline-ms "$(FACTORY_OPERATION_DEADLINE_MS)"

# Admit one provider-backed campaign whose terminal objective is exactly one
# locally delivered XSH commit. Product discovery, Architect sponsorship,
# Engineering, Quality, and the final Architect decision remain explicit
# daemon/operator lifecycle steps; this target only creates the campaign.
#
# Required inputs are deliberately explicit because the campaign pins the
# active application revision, aggregate budget, deadline, and idempotency
# identity at admission time. The delivery target is fixed at one here rather
# than exposed as a tunable variable.
paid-cycle:
	test -x "$(FACTORYCTL)"
	test -S "$(FACTORY_PAID_CYCLE_SOCKET)"
	test -d "$(CURDIR)/../xsh"
	test -z "$$(git -C "$(CURDIR)/../xsh" status --porcelain)"
	test -n "$(FACTORY_PAID_CYCLE_CLIENT_COMMAND_ID)"
	test -n "$(FACTORY_PAID_CYCLE_PRINCIPAL)"
	printf '%s\n' "$(FACTORY_PAID_CYCLE_EXPECTED_APPLICATION_REVISION)" | grep -Eq '^[0-9]+$$'
	printf '%s\n' "$(FACTORY_PAID_CYCLE_APPLICATION_REVISION_ID)" | grep -Eq '^[1-9][0-9]*$$'
	printf '%s\n' "$(FACTORY_PAID_CYCLE_BUDGET_MICRO_USD)" | grep -Eq '^[1-9][0-9]*$$'
	printf '%s\n' "$(FACTORY_PAID_CYCLE_DEADLINE_UNIX_MILLIS)" | grep -Eq '^[1-9][0-9]*$$'
	test "$(FACTORY_PAID_CYCLE_DEADLINE_UNIX_MILLIS)" -gt "$$(date +%s000)"
	"$(FACTORYCTL)" campaign start \
		--application-revision-id "$(FACTORY_PAID_CYCLE_APPLICATION_REVISION_ID)" \
		--expected-application-revision "$(FACTORY_PAID_CYCLE_EXPECTED_APPLICATION_REVISION)" \
		--aggregate-budget-micro-usd "$(FACTORY_PAID_CYCLE_BUDGET_MICRO_USD)" \
		--deadline-unix-millis "$(FACTORY_PAID_CYCLE_DEADLINE_UNIX_MILLIS)" \
		--delivery-target 1 \
		--socket "$(FACTORY_PAID_CYCLE_SOCKET)" \
		--client-command-id "$(FACTORY_PAID_CYCLE_CLIENT_COMMAND_ID)" \
		--principal "$(FACTORY_PAID_CYCLE_PRINCIPAL)" \
		--format json

# Verify the exact terminal proof for `paid-cycle`: the daemon must report a
# completed campaign with one delivery, and the delivered commit must be the
# current clean HEAD of the product checkout. This target never writes either
# repository; delivery itself is kernel-owned.
paid-cycle-verify:
	test -x "$(FACTORYCTL)"
	test -S "$(FACTORY_PAID_CYCLE_SOCKET)"
	test -d "$(CURDIR)/../xsh"
	printf '%s\n' "$(FACTORY_PAID_CYCLE_ID)" | grep -Eq '^[1-9][0-9]*$$'
	@set -eu; \
	status_json="$$("$(FACTORYCTL)" campaign status "$(FACTORY_PAID_CYCLE_ID)" --socket "$(FACTORY_PAID_CYCLE_SOCKET)" --format json)"; \
	printf '%s\n' "$$status_json" | grep -Eq '"state":"completed"'; \
	printf '%s\n' "$$status_json" | grep -Eq '"delivery_target":1'; \
	printf '%s\n' "$$status_json" | grep -Eq '"delivered_attempt_count":1'; \
	delivered_commit="$$(printf '%s\n' "$$status_json" | sed -n 's/.*"delivered_commit":"\([^"]*\)".*/\1/p')"; \
	test -n "$$delivered_commit"; \
	test "$$delivered_commit" = "$$(git -C "$(CURDIR)/../xsh" rev-parse HEAD)"; \
	test -z "$$(git -C "$(CURDIR)/../xsh" status --porcelain)"; \
	printf 'paid cycle %s delivered XSH commit %s\n' "$(FACTORY_PAID_CYCLE_ID)" "$$delivered_commit"

postgres-test:
	test -n "$$FACTORY_TEST_DATABASE_URL"
	factory_test_database="$${FACTORY_TEST_DATABASE_URL##*/}"; factory_test_database="$${factory_test_database%%\?*}"; printf '%s\n' "$$factory_test_database" | grep -Eq '^factory_test_v3_[0-9]+$$'
	# Cargo may schedule multiple integration-test binaries concurrently even
	# when each harness has one thread.  These authority judges deliberately
	# share one fresh database, so each binary is its own serial command.
	cargo test -p factory-kernel --test storage -- --ignored --test-threads=1
	cargo test -p factory-kernel --test forum_store -- --ignored --test-threads=1
	cargo test -p factory-kernel --test process -- --ignored --test-threads=1
	cargo test -p factory-kernel --test process_lifecycle -- --ignored --test-threads=1
	cargo test -p factory-kernel --lib -- --ignored --test-threads=1

# Focused provider-free authority judge for the fresh-schema application and
# ticket-buffer path. The same exact-name guard prevents accidental use of an
# operator database.
ticket-test:
	test -n "$$FACTORY_TEST_DATABASE_URL"
	factory_test_database="$${FACTORY_TEST_DATABASE_URL##*/}"; factory_test_database="$${factory_test_database%%\?*}"; printf '%s\n' "$$factory_test_database" | grep -Eq '^factory_test_v3_[0-9]+$$'
	cargo test -p factory-kernel --test storage application_admission_is_atomic_idempotent_and_revision_guarded -- --ignored --test-threads=1

# Focused Tranche 8 state-authority judge. The typed vertical proves the
# candidate-to-delivery transaction without a provider; the second command
# exercises the PostgreSQL 18 migration/table-budget assertion.
decision-test:
	test -n "$$FACTORY_TEST_DATABASE_URL"
	factory_test_database="$${FACTORY_TEST_DATABASE_URL##*/}"; factory_test_database="$${factory_test_database%%\?*}"; printf '%s\n' "$$factory_test_database" | grep -Eq '^factory_test_v3_[0-9]+$$'
	cargo test -p factory-kernel --test decision_store -- --ignored --test-threads=1
	cargo test -p factory-kernel decision_store::tests --lib
	cargo test -p factory-kernel decision_store::tests::postgres_authority_schema_has_exactly_thirty_six_named_tables --lib -- --ignored --test-threads=1

# The real XSH bundle is independently compiled twice by Rust and admitted
# through the typed Rust/CAS/activation boundary. This is a provider-free
# source qualification and does not need a database.
xsh-bundle-test:
	cargo test -p factoryctl --test xsh_bundle
	cargo test -p factory-kernel --test provider_free_application

# The direct Rust host is qualified with its real Agent, sealed policy bridge,
# frame contract, terminal gate, and transcript primitives without selecting a
# live provider. Provider-backed campaigns remain a separately authorized act.
provider-free-host:
	cargo test -p factory-pi-host

# The generic resident-composition judge needs a fresh disposable schema. It
# runs the typed candidate-to-delivery vertical without a provider.
provider-free-vertical:
	test -n "$$FACTORY_TEST_DATABASE_URL"
	factory_test_database="$${FACTORY_TEST_DATABASE_URL##*/}"; factory_test_database="$${factory_test_database%%\?*}"; printf '%s\n' "$$factory_test_database" | grep -Eq '^factory_test_v3_[0-9]+$$'
	cargo test -p factory-kernel --test decision_store typed_candidate_validation_review_decision_delivery_vertical -- --ignored --test-threads=1

# This operator qualification never creates or drops a database. The restore
# database must already be blank and named factory_restore_v3_<digits>; the
# restore runtime root must already exist and be empty. See the dry-run record.
backup-restore-test:
	test -n "$$FACTORY_BACKUP_SOURCE_DATABASE_URL"
	test -n "$$FACTORY_BACKUP_SOURCE_RUNTIME_ROOT"
	test -n "$$FACTORY_BACKUP_RESTORE_DATABASE_URL"
	test -n "$$FACTORY_BACKUP_RESTORE_RUNTIME_ROOT"
	test -n "$$FACTORY_BACKUP_DUMP_FILE"
	test -x "$(FACTORYCTL)"
	"$(FACTORYCTL)" backup-restore qualify \
		--source-database-url "$$FACTORY_BACKUP_SOURCE_DATABASE_URL" \
		--source-runtime-root "$$FACTORY_BACKUP_SOURCE_RUNTIME_ROOT" \
		--restore-database-url "$$FACTORY_BACKUP_RESTORE_DATABASE_URL" \
		--restore-runtime-root "$$FACTORY_BACKUP_RESTORE_RUNTIME_ROOT" \
		--dump-file "$$FACTORY_BACKUP_DUMP_FILE" \
		--pg-dump "$${FACTORY_BACKUP_PG_DUMP:?set FACTORY_BACKUP_PG_DUMP to an absolute pg_dump path}" \
		--pg-restore "$${FACTORY_BACKUP_PG_RESTORE:?set FACTORY_BACKUP_PG_RESTORE to an absolute pg_restore path}" \
		--psql "$${FACTORY_BACKUP_PSQL:?set FACTORY_BACKUP_PSQL to an absolute psql path}" \
		--cargo "$${FACTORY_BACKUP_CARGO:?set FACTORY_BACKUP_CARGO to an absolute cargo path}"

# Complete provider-free qualification. The caller, not Make, supplies an
# already-created disposable database for each of the ordinary, decision, XSH,
# and generic Product-to-delivery judges, plus the source/blank restore clone
# pair consumed by backup-restore-test. No target here creates or drops a
# PostgreSQL database.
provider-free-acceptance:
	test -n "$$FACTORY_ACCEPTANCE_POSTGRES_URL"
	test -n "$$FACTORY_ACCEPTANCE_DECISION_URL"
	test -n "$$FACTORY_ACCEPTANCE_XSH_BUNDLE_URL"
	test -n "$$FACTORY_ACCEPTANCE_VERTICAL_URL"
	@set -eu; \
	acceptance_postgres_name="$${FACTORY_ACCEPTANCE_POSTGRES_URL##*/}"; acceptance_postgres_name="$${acceptance_postgres_name%%\?*}"; \
	acceptance_decision_name="$${FACTORY_ACCEPTANCE_DECISION_URL##*/}"; acceptance_decision_name="$${acceptance_decision_name%%\?*}"; \
	acceptance_xsh_bundle_name="$${FACTORY_ACCEPTANCE_XSH_BUNDLE_URL##*/}"; acceptance_xsh_bundle_name="$${acceptance_xsh_bundle_name%%\?*}"; \
	acceptance_vertical_name="$${FACTORY_ACCEPTANCE_VERTICAL_URL##*/}"; acceptance_vertical_name="$${acceptance_vertical_name%%\?*}"; \
	for database_name in "$$acceptance_postgres_name" "$$acceptance_decision_name" "$$acceptance_xsh_bundle_name" "$$acceptance_vertical_name"; do \
		printf '%s\n' "$$database_name" | grep -Eq '^factory_test_v3_[0-9]+$$'; \
	done; \
	test "$$acceptance_postgres_name" != "$$acceptance_decision_name"; \
	test "$$acceptance_postgres_name" != "$$acceptance_xsh_bundle_name"; \
	test "$$acceptance_postgres_name" != "$$acceptance_vertical_name"; \
	test "$$acceptance_decision_name" != "$$acceptance_xsh_bundle_name"; \
	test "$$acceptance_decision_name" != "$$acceptance_vertical_name"; \
	test "$$acceptance_xsh_bundle_name" != "$$acceptance_vertical_name"
	$(MAKE) lint
	$(MAKE) provider-free-host
	FACTORY_TEST_DATABASE_URL="$$FACTORY_ACCEPTANCE_POSTGRES_URL" $(MAKE) postgres-test
	FACTORY_TEST_DATABASE_URL="$$FACTORY_ACCEPTANCE_DECISION_URL" $(MAKE) decision-test
	DATABASE_URL="$$FACTORY_ACCEPTANCE_POSTGRES_URL" $(MAKE) sqlx-check
	FACTORY_TEST_DATABASE_URL="$$FACTORY_ACCEPTANCE_XSH_BUNDLE_URL" $(MAKE) xsh-bundle-test
	FACTORY_TEST_DATABASE_URL="$$FACTORY_ACCEPTANCE_VERTICAL_URL" $(MAKE) provider-free-vertical
	$(MAKE) backup-restore-test

# Named first full gate from the Rust runtime qualification contract. The complete provider-free
# qualification is now the permanent Rust-only acceptance path.
pi-agent-core-rs-acceptance: provider-free-acceptance

# Requires a disposable PostgreSQL 18 database and an externally installed
# sqlx-cli matching the pinned project crate. It verifies committed `.sqlx`
# query metadata; `make lint` compiles from that metadata offline.
sqlx-check:
	test -n "$$DATABASE_URL"
	cargo sqlx prepare --workspace --check -- --all-targets
