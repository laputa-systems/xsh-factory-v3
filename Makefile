DENO_VERSION := 2.9.4
PI_HEADLESS_ROOT := $(CURDIR)/vendor/pi-headless
PI_HEADLESS_BUILD := $(PI_HEADLESS_ROOT)/packages/coding-agent/dist
PI_HEADLESS_RUNTIME := $(CURDIR)/packages/factory-pi-host/headless
PI_HEADLESS_SDK := $(PI_HEADLESS_RUNTIME)/headless-sdk.mjs
PI_HEADLESS_AI := $(PI_HEADLESS_RUNTIME)/headless-ai.mjs
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

.PHONY: cache lint deno-version pi-headless-cache pi-headless-build factoryd-serve paid-cycle paid-cycle-verify postgres-test ticket-test decision-test xsh-bundle-test provider-free-vertical backup-restore-test provider-free-acceptance sqlx-check

pi-headless-cache:
	test -f "$(PI_HEADLESS_ROOT)/package-lock.json"
	cd "$(PI_HEADLESS_ROOT)" && npm ci --ignore-scripts

# The local Pi fork owns its frozen provider catalog as source. `cache` may
# install its locked Node build dependencies, while the lint gate only uses
# this offline build and never contacts a provider or the npm registry.
pi-headless-build:
	test -d "$(PI_HEADLESS_ROOT)/node_modules"
	test -d "$(PI_HEADLESS_ROOT)/packages/ai/src/providers/data"
	cd "$(PI_HEADLESS_ROOT)" && npm run build:offline
	mkdir -p "$(PI_HEADLESS_RUNTIME)"
	cp "$(PI_HEADLESS_BUILD)/headless-sdk.mjs" "$(PI_HEADLESS_RUNTIME)/headless-sdk.mjs"
	cp "$(PI_HEADLESS_BUILD)/headless-sdk.d.ts" "$(PI_HEADLESS_RUNTIME)/headless-sdk.d.ts"
	cp "$(PI_HEADLESS_BUILD)/headless-ai.mjs" "$(PI_HEADLESS_RUNTIME)/headless-ai.mjs"
	cp "$(PI_HEADLESS_BUILD)/headless-ai.d.ts" "$(PI_HEADLESS_RUNTIME)/headless-ai.d.ts"
	test -s "$(PI_HEADLESS_SDK)"
	test -s "$(PI_HEADLESS_AI)"

cache: pi-headless-cache pi-headless-build
	deno task cache

deno-version:
	test "$$(deno --version | sed -n '1s/^deno \([0-9.]*\).*/\1/p')" = "$(DENO_VERSION)"

lint: deno-version pi-headless-build
	cargo fmt --all
	cargo clippy --fix --allow-dirty --all-targets --all-features -- $(CLIPPY_GATE_FLAGS)
	cargo clippy --all-targets --all-features -- $(CLIPPY_GATE_FLAGS)
	cargo check --workspace --all-targets
	cargo test --workspace
	deno fmt
	deno lint
	deno task check
	deno task test

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
	delivery_proof="$$(printf '%s\n' "$$status_json" | deno eval 'const text = await new Response(Deno.stdin.readable).text(); const status = JSON.parse(text); if (status.state !== "completed" || status.delivery_target !== 1 || status.delivered_attempt_count !== 1 || typeof status.delivered_commit !== "string" || status.delivered_commit.length === 0 || !Number.isSafeInteger(status.delivered_factory_cost_micro_usd)) Deno.exit(1); console.log(status.delivered_commit + "|" + (status.delivered_factory_cost_micro_usd / 1000000).toFixed(6));')"; \
	delivered_commit="$${delivery_proof%%|*}"; \
	factory_cost_usd="$${delivery_proof#*|}"; \
	test "$$delivered_commit" = "$$(git -C "$(CURDIR)/../xsh" rev-parse HEAD)"; \
	test -z "$$(git -C "$(CURDIR)/../xsh" status --porcelain)"; \
	git -C "$(CURDIR)/../xsh" log -1 --format='%B' | awk -v expected="$$factory_cost_usd" '($$1 == "Factory-Cost:" && $$2 == sprintf("%c%s", 36, expected)) { found = 1 } END { exit !found }'; \
	printf 'paid cycle %s delivered XSH commit %s; Factory-Cost $$%s\n' "$(FACTORY_PAID_CYCLE_ID)" "$$delivered_commit" "$$factory_cost_usd"

postgres-test:
	test -n "$$FACTORY_TEST_DATABASE_URL"
	factory_test_database="$${FACTORY_TEST_DATABASE_URL##*/}"; factory_test_database="$${factory_test_database%%\?*}"; printf '%s\n' "$$factory_test_database" | grep -Eq '^factory_test_v3_[0-9]+$$'
	cargo test -p factory-kernel --test storage --test forum_store --test process --test process_lifecycle --test session_runtime -- --ignored --test-threads=1
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
	cargo test -p factory-kernel decision_store::tests::postgres_final_authority_schema_has_exactly_twenty_named_tables --lib -- --ignored --test-threads=1

# The real XSH bundle is independently compiled twice and admitted through the
# typed Rust/CAS/activation boundary. It migrates and populates its own schema,
# so it must never share an ordinary or generic-vertical fixture database.
xsh-bundle-test:
	test -n "$$FACTORY_TEST_DATABASE_URL"
	factory_test_database="$${FACTORY_TEST_DATABASE_URL##*/}"; factory_test_database="$${factory_test_database%%\?*}"; printf '%s\n' "$$factory_test_database" | grep -Eq '^factory_test_v3_[0-9]+$$'
	cargo test -p factory-kernel --test xsh_bundle_admission -- --ignored --test-threads=1

# The generic resident-composition judge needs a fresh disposable schema: it
# starts the one unseeded Product proposal and proves exactly three sessions.
# It is deliberately separate from `postgres-test`, whose other fixtures leave
# state behind that would invalidate that acceptance fact.
provider-free-vertical:
	test -n "$$FACTORY_TEST_DATABASE_URL"
	factory_test_database="$${FACTORY_TEST_DATABASE_URL##*/}"; factory_test_database="$${factory_test_database%%\?*}"; printf '%s\n' "$$factory_test_database" | grep -Eq '^factory_test_v3_[0-9]+$$'
	cargo test -p factory-kernel --test full_vertical -- --ignored --test-threads=1

# This operator qualification never creates or drops a database. The restore
# database must already be blank and named factory_restore_v3_<digits>; the
# restore runtime root must already exist and be empty. See the dry-run record.
backup-restore-test:
	test -n "$$FACTORY_BACKUP_SOURCE_DATABASE_URL"
	test -n "$$FACTORY_BACKUP_SOURCE_RUNTIME_ROOT"
	test -n "$$FACTORY_BACKUP_RESTORE_DATABASE_URL"
	test -n "$$FACTORY_BACKUP_RESTORE_RUNTIME_ROOT"
	test -n "$$FACTORY_BACKUP_DUMP_FILE"
	deno run --allow-read --allow-write --allow-run --no-prompt --frozen tools/backup_restore_check.ts \
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
	FACTORY_TEST_DATABASE_URL="$$FACTORY_ACCEPTANCE_POSTGRES_URL" $(MAKE) postgres-test
	FACTORY_TEST_DATABASE_URL="$$FACTORY_ACCEPTANCE_DECISION_URL" $(MAKE) decision-test
	DATABASE_URL="$$FACTORY_ACCEPTANCE_POSTGRES_URL" $(MAKE) sqlx-check
	FACTORY_TEST_DATABASE_URL="$$FACTORY_ACCEPTANCE_XSH_BUNDLE_URL" $(MAKE) xsh-bundle-test
	FACTORY_TEST_DATABASE_URL="$$FACTORY_ACCEPTANCE_VERTICAL_URL" $(MAKE) provider-free-vertical
	$(MAKE) backup-restore-test

# Requires a disposable PostgreSQL 18 database and an externally installed
# sqlx-cli matching the pinned project crate. It verifies committed `.sqlx`
# query metadata; `make lint` compiles from that metadata offline.
sqlx-check:
	test -n "$$DATABASE_URL"
	cargo sqlx prepare --workspace --check -- --all-targets
