DENO_VERSION := 2.9.4

.PHONY: cache check rust-check deno-check deno-version postgres-test ticket-test decision-test sqlx-check

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

# Requires a disposable PostgreSQL 18 database and an externally installed
# sqlx-cli matching the pinned project crate. It verifies committed `.sqlx`
# query metadata; ordinary `make check` compiles from that metadata offline.
sqlx-check:
	test -n "$$DATABASE_URL"
	cargo sqlx prepare --workspace --check -- --all-targets
