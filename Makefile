DENO_VERSION := 2.9.4

.PHONY: cache check rust-check deno-check deno-version factoryd-serve postgres-test ticket-test decision-test xsh-bundle-test provider-free-vertical backup-restore-test provider-free-acceptance sqlx-check

cache:
	deno task cache

rust-check:
	cargo fmt --check
	cargo check --workspace --all-targets
	cargo test --workspace

deno-version:
	test "$$(deno --version | sed -n '1s/^deno \([0-9.]*\).*/\1/p')" = "$(DENO_VERSION)"

deno-check: deno-version
	deno fmt --check
	deno lint
	deno task check
	deno task test

check: rust-check deno-check

# The credential is introduced only at the daemon process boundary. Callers
# must choose the dedicated database and runtime root explicitly; this target
# never starts a provider-backed actor on its own.
factoryd-serve:
	test -n "$$FACTORY_DATABASE_URL"
	test -n "$$FACTORY_RUNTIME_ROOT"
	vault OPENROUTER_API_KEY -- target/release/factoryd serve \
		--database-url "$$FACTORY_DATABASE_URL" \
		--runtime-root "$$FACTORY_RUNTIME_ROOT"

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
	$(MAKE) check
	FACTORY_TEST_DATABASE_URL="$$FACTORY_ACCEPTANCE_POSTGRES_URL" $(MAKE) postgres-test
	FACTORY_TEST_DATABASE_URL="$$FACTORY_ACCEPTANCE_DECISION_URL" $(MAKE) decision-test
	DATABASE_URL="$$FACTORY_ACCEPTANCE_POSTGRES_URL" $(MAKE) sqlx-check
	FACTORY_TEST_DATABASE_URL="$$FACTORY_ACCEPTANCE_XSH_BUNDLE_URL" $(MAKE) xsh-bundle-test
	FACTORY_TEST_DATABASE_URL="$$FACTORY_ACCEPTANCE_VERTICAL_URL" $(MAKE) provider-free-vertical
	$(MAKE) backup-restore-test

# Requires a disposable PostgreSQL 18 database and an externally installed
# sqlx-cli matching the pinned project crate. It verifies committed `.sqlx`
# query metadata; ordinary `make check` compiles from that metadata offline.
sqlx-check:
	test -n "$$DATABASE_URL"
	cargo sqlx prepare --workspace --check -- --all-targets
