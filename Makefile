# The local pi-agent-core-rs checkout is intentionally explicit until its
# crates are published. Cargo does not expand `~` in a dependency path.
PI_AGENT_CORE_ROOT ?= /Users/josh/d/pi-agent-core-rs
PI_AGENT_CORE_MANIFEST := $(PI_AGENT_CORE_ROOT)/Cargo.toml
FACTORY_OPERATION_DEADLINE_MS ?= 900000
FACTORYCTL ?= $(CURDIR)/target/release/factoryctl
FACTORY_PAID_CYCLE_PRINCIPAL ?= grand-architect
FACTORY_START_PRINCIPAL ?= grand-architect
FACTORY_START_WAIT_SECONDS ?= 30
FACTORY_START_SOCKET ?= $(FACTORY_RUNTIME_ROOT)/factoryd.operator.sock
# Status is read-only and can discover the socket from the daemon command line
# when the caller has not exported the runtime root.  Keep this override empty
# by default so an empty FACTORY_RUNTIME_ROOT does not become the bogus path
# `/factoryd.operator.sock`.
FACTORY_STATUS_SOCKET ?=
FACTORY_START_PID_FILE ?= $(FACTORY_RUNTIME_ROOT)/factoryd.pid
FACTORY_START_LOG_FILE ?= $(FACTORY_RUNTIME_ROOT)/factoryd.log

# The factory keeps Clippy's correctness and default quality groups strict.
# Pedantic documentation/style heuristics and these boundary-shape
# warnings are reviewed policy, not pre-commit failures for this codebase.
CLIPPY_GATE_FLAGS := --deny warnings \
	--allow clippy::pedantic \
	--allow clippy::large_enum_variant \
	--allow clippy::result_large_err \
	--allow clippy::type_complexity \
	--allow clippy::too_many_arguments

.PHONY: cache lint release-build status factory-database-guard paid-cycle-preflight pi-agent-core-rs-test factoryd-serve factory-start factory-stop factory-reset paid-cycle paid-cycle-verify postgres-test ticket-test decision-test application-contract-test provider-free-host provider-free-vertical backup-restore-test provider-free-acceptance pi-agent-core-rs-acceptance sqlx-check

# Build metadata and dependencies for both Rust workspaces. The external
# checkout is tested independently because it is a direct local dependency
# while the local core source remains the explicit dependency.
cache:
	test -f "$(PI_AGENT_CORE_MANIFEST)"
	cargo fetch --locked --manifest-path "$(PI_AGENT_CORE_MANIFEST)"
	cargo fetch --locked --manifest-path "$(CURDIR)/Cargo.toml"

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

release-build: cache
	cargo build --locked --release --workspace

# Read-only high-level operator view. The daemon owns the aggregate counts;
# this target never opens PostgreSQL or mutates Factory state.  An explicit
# FACTORY_STATUS_SOCKET wins; otherwise use FACTORY_RUNTIME_ROOT when present,
# then recover runtime roots advertised by a running factoryd process.  This
# keeps `make status` useful from a fresh shell without making the status view
# guess a database or aggregate across independent authorities.
status:
	test -x "$(FACTORYCTL)"
	@set -eu; \
	 socket="$(FACTORY_STATUS_SOCKET)"; \
	 advertised_runtime_root=""; \
	 if test -z "$$socket" && test -n "$(FACTORY_RUNTIME_ROOT)"; then \
	  socket="$(FACTORY_RUNTIME_ROOT)/factoryd.operator.sock"; \
	 fi; \
	 if test -z "$$socket"; then \
	  for runtime_root in $$(ps -axo command= 2>/dev/null | awk '$$0 ~ /factoryd serve/ { for (i = 1; i < NF; i++) if ($$i == "--runtime-root") print $$(i + 1) }'); do \
	   advertised_runtime_root="$$runtime_root"; \
	   candidate="$$runtime_root/factoryd.operator.sock"; \
	   test -S "$$candidate" || continue; \
	   if test -n "$$socket" && test "$$socket" != "$$candidate"; then \
	    printf 'make status: multiple factoryd operator sockets found; set FACTORY_STATUS_SOCKET explicitly\n' >&2; \
	    exit 1; \
	   fi; \
	   socket="$$candidate"; \
	  done; \
	 fi; \
	 if test -z "$$socket" && test -n "$$advertised_runtime_root"; then \
	  printf 'make status: factoryd advertises runtime root %s, but its operator socket is missing; perform a controlled daemon restart\n' "$$advertised_runtime_root" >&2; \
	  exit 1; \
	 fi; \
	 test -n "$$socket" || { printf 'make status: no factoryd operator socket found; set FACTORY_STATUS_SOCKET or FACTORY_RUNTIME_ROOT\n' >&2; exit 1; }; \
	 test -S "$$socket" || { printf 'make status: operator socket is not live: %s\n' "$$socket" >&2; exit 1; }; \
	 live_json="$$($(FACTORYCTL) daemon status --socket "$$socket" --format json)"; \
	 live_build="$$(printf '%s\n' "$$live_json" | sed -n 's/.*"current_kernel_build_id":"\([^\"]*\)".*/\1/p')"; \
	 test -n "$$live_build" || { printf 'make status: daemon did not report a qualified build identity; perform a controlled daemon restart\n' >&2; exit 1; }; \
	 expected_json="$$($(FACTORYCTL) build identity \
	  --installation-root "$(CURDIR)" \
	  --factoryd "$(CURDIR)/target/release/factoryd" \
	  --format json)"; \
	 expected_build="$$(printf '%s\n' "$$expected_json" | sed -n 's/.*"kernel_build_id":"\([^\"]*\)".*/\1/p')"; \
	 test -n "$$expected_build" || { printf 'make status: could not compute the release build identity; run make release-build first\n' >&2; exit 1; }; \
	 if test "$$live_build" != "$$expected_build"; then \
	  printf 'make status: stale daemon build; live=%s expected=%s; restart with make factory-start\n' "$$live_build" "$$expected_build" >&2; \
	  exit 1; \
	 fi; \
	 "$(FACTORYCTL)" status --socket "$$socket"

# Requalify the complete release source graph before paid admission. The
# identity check deliberately compares the freshly built local graph with the
# build selected by the live daemon; a stale daemon fails closed before budget
# or provider credentials cross the campaign boundary.
paid-cycle-preflight:
	$(MAKE) release-build
	@set -eu; \
	 expected_json="$$($(FACTORYCTL) build identity \
	  --installation-root "$(CURDIR)" \
	  --factoryd "$(CURDIR)/target/release/factoryd" \
	  --format json)"; \
	 expected_build="$$(printf '%s\n' "$$expected_json" | sed -n 's/.*"kernel_build_id":"\([^"]*\)".*/\1/p')"; \
	 live_json="$$($(FACTORYCTL) daemon status \
	  --socket "$(FACTORY_PAID_CYCLE_SOCKET)" \
	  --format json)"; \
	 live_build="$$(printf '%s\n' "$$live_json" | sed -n 's/.*"current_kernel_build_id":"\([^"]*\)".*/\1/p')"; \
	 test -n "$$expected_build"; \
	 test -n "$$live_build"; \
	 if test "$$expected_build" != "$$live_build"; then \
	  printf 'paid-cycle: stale runtime build; live=%s expected=%s; stop, initialize a fresh runtime, and serve the release build\n' "$$live_build" "$$expected_build" >&2; \
	  exit 1; \
	 fi

# One live XSH lane owns one continuous PostgreSQL authority.  Cycle-specific
# database names are rejected before a daemon can initialize or serve, so a
# caller cannot silently strand tickets by choosing a new database per cycle.
factory-database-guard:
	test -n "$$FACTORY_DATABASE_URL"
	@set -eu; \
	 database_url="$$FACTORY_DATABASE_URL"; \
	 database_name="$${database_url##*/}"; \
	 database_name="$${database_name%%\?*}"; \
	 test "$$database_name" = factory_live_v3 || { \
	  printf 'factory: FACTORY_DATABASE_URL must name %s (got %s); cycle-specific live databases are forbidden\n' \
	   factory_live_v3 "$$database_name" >&2; \
	  exit 1; \
	 }

# The daemon has no provider credential in its environment. It invokes Vault
# for startup preflight and again at each provider-backed assignment launch.
# Callers must choose the dedicated database and runtime root explicitly.
factoryd-serve: factory-database-guard
	test -n "$$FACTORY_RUNTIME_ROOT"
	target/release/factoryd serve \
		--database-url "$$FACTORY_DATABASE_URL" \
		--runtime-root "$$FACTORY_RUNTIME_ROOT" \
		--operation-deadline-ms "$(FACTORY_OPERATION_DEADLINE_MS)"

factory-start: factory-database-guard
	test -x "$(FACTORYCTL)"
	test -n "$$FACTORY_RUNTIME_ROOT"
	test -d "$(CURDIR)/applications/xsh"
	test -f "$(CURDIR)/applications/xsh/bundle.v2.json"
	test -d "$(CURDIR)/../xsh"
	test -z "$$(git -C "$(CURDIR)/../xsh" status --porcelain)"
	$(MAKE) release-build
	@set -eu; \
	 runtime_root="$$FACTORY_RUNTIME_ROOT"; \
	 socket="$(FACTORY_START_SOCKET)"; \
	 expected_json="$$($(FACTORYCTL) build identity \
	  --installation-root "$(CURDIR)" \
	  --factoryd "$(CURDIR)/target/release/factoryd" \
	  --format json)"; \
	 expected_build="$$(printf '%s\n' "$$expected_json" | sed -n 's/.*"kernel_build_id":"\([^"]*\)".*/\1/p')"; \
	 test -n "$$expected_build"; \
	 daemon_ready=0; \
	 if test -S "$$socket"; then \
	  if live_json="$$($(FACTORYCTL) daemon status --socket "$$socket" --format json 2>/dev/null)"; then \
	   live_build="$$(printf '%s\n' "$$live_json" | sed -n 's/.*"current_kernel_build_id":"\([^"]*\)".*/\1/p')"; \
	   test "$$live_build" = "$$expected_build" || { printf 'factory-start: live daemon build %s differs from release build %s; stop it and choose a fresh runtime/database pair\n' "$$live_build" "$$expected_build" >&2; exit 1; }; \
	   daemon_ready=1; \
	  else \
	   printf 'factory-start: stale socket detected; daemon start will reclaim it under the runtime lock\n' >&2; \
	  fi; \
	 fi; \
	 if test "$$daemon_ready" != 1; then \
	  "$(FACTORYCTL)" init \
	   --installation-root "$(CURDIR)" \
	   --factoryd "$(CURDIR)/target/release/factoryd" \
	   --database-url "$$FACTORY_DATABASE_URL" \
	   --runtime-root "$$runtime_root" \
	   --provider-credential-environment openrouter=OPENROUTER_API_KEY; \
	  "$(FACTORYCTL)" daemon start \
	   --factoryd "$(CURDIR)/target/release/factoryd" \
	   --database-url "$$FACTORY_DATABASE_URL" \
	   --runtime-root "$$runtime_root" \
	   --pid-file "$(FACTORY_START_PID_FILE)" \
	   --log-file "$(FACTORY_START_LOG_FILE)" \
	   --operation-deadline-ms "$(FACTORY_OPERATION_DEADLINE_MS)"; \
	  ready=0; \
	  for attempt in $$(seq 1 "$(FACTORY_START_WAIT_SECONDS)"); do \
	   if test -S "$$socket" && "$(FACTORYCTL)" daemon status --socket "$$socket" >/dev/null 2>&1; then ready=1; break; fi; \
	   sleep 1; \
	  done; \
	  test "$$ready" = 1 || { printf 'factory-start: daemon did not become ready; inspect %s\n' "$(FACTORY_START_LOG_FILE)" >&2; exit 1; }; \
	 fi; \
	 status_json="$$($(FACTORYCTL) daemon status --socket "$$socket" --format json)"; \
	 kernel_revision="$$(printf '%s\n' "$$status_json" | sed -n 's/.*"aggregate_revision":\([0-9][0-9]*\).*/\1/p')"; \
	 test -n "$$kernel_revision"; \
	 if app_json="$$($(FACTORYCTL) application show xsh --socket "$$socket" --format json 2>/dev/null)"; then \
	  :; \
	 else \
	  app_json="$$($(FACTORYCTL) application register \
	   --socket "$$socket" --format json \
	   --client-command-id factory-start-register-xsh-$$expected_build \
	   --expected-revision 0 \
	   --expected-kernel-build-revision "$$kernel_revision" \
	   --kernel-build-id "$$expected_build" \
	   --source-root "$(CURDIR)/applications/xsh" \
	   --bundle-relative-path bundle.v2.json \
	   --principal "$(FACTORY_START_PRINCIPAL)")"; \
	  app_json="$$($(FACTORYCTL) application show xsh --socket "$$socket" --format json)"; \
	 fi; \
	 active="$$(printf '%s\n' "$$app_json" | sed -n 's/.*"is_active":\(true\|false\).*/\1/p')"; \
	 if test "$$active" != true; then \
	  app_revision="$$(printf '%s\n' "$$app_json" | sed -n 's/.*"application_revision_id":\([0-9][0-9]*\).*/\1/p')"; \
	  app_expected_revision="$$(printf '%s\n' "$$app_json" | sed -n 's/.*"aggregate_revision":\([0-9][0-9]*\).*/\1/p')"; \
	  rationale_root="$$runtime_root/operator"; rationale_file="$$rationale_root/factory-start-xsh.txt"; mkdir -p "$$rationale_root"; \
	  printf 'Grand Architect startup selected XSH application revision %s for the qualified release build %s.\n' "$$app_revision" "$$expected_build" >"$$rationale_file"; \
	  seal_json="$$($(FACTORYCTL) artifact seal \
	   --socket "$$socket" --format json \
	   --client-command-id factory-start-seal-xsh-rationale-$$expected_build \
	   --expected-kernel-build-revision "$$kernel_revision" \
	   --source-root "$$rationale_root" \
	   --source-relative-path factory-start-xsh.txt \
	   --principal "$(FACTORY_START_PRINCIPAL)")"; \
	  rationale_id="$$(printf '%s\n' "$$seal_json" | sed -n 's/.*"artifact_id":\([0-9][0-9]*\).*/\1/p')"; \
	  rationale_digest="$$(printf '%s\n' "$$seal_json" | sed -n 's/.*"digest":"\([^"]*\)".*/\1/p')"; \
	  rationale_bytes="$$(printf '%s\n' "$$seal_json" | sed -n 's/.*"byte_length":\([0-9][0-9]*\).*/\1/p')"; \
	  test -n "$$rationale_id"; test -n "$$rationale_digest"; test -n "$$rationale_bytes"; \
	  "$(FACTORYCTL)" application activate xsh "$$app_revision" \
	   --socket "$$socket" --format json \
	   --client-command-id factory-start-activate-xsh-$$expected_build \
	   --expected-revision "$$app_expected_revision" \
	   --rationale-artifact-id "$$rationale_id" \
	   --rationale-digest "$$rationale_digest" \
	   --rationale-byte-length "$$rationale_bytes" \
	   --principal "$(FACTORY_START_PRINCIPAL)" >/dev/null; \
	 fi; \
	 printf 'factory-start: ready on %s with qualified build %s and active XSH application\n' "$$socket" "$$expected_build"

factory-stop:
	test -n "$$FACTORY_RUNTIME_ROOT"
	@set -eu; \
	 socket="$(FACTORY_START_SOCKET)"; \
	 if test ! -S "$$socket"; then printf 'factory-stop: already stopped (%s)\n' "$$socket"; exit 0; fi; \
	 test -x "$(FACTORYCTL)"; \
	 "$(FACTORYCTL)" daemon stop --socket "$$socket" --format json; \
	 stopped=0; \
	 for attempt in $$(seq 1 "$(FACTORY_START_WAIT_SECONDS)"); do \
	  if test ! -e "$$socket"; then stopped=1; break; fi; \
	  sleep 1; \
	 done; \
	 test "$$stopped" = 1 || { printf 'factory-stop: daemon acknowledged shutdown but socket remains: %s\n' "$$socket" >&2; exit 1; }; \
	 test ! -e "$(FACTORY_START_PID_FILE)" || rm -f -- "$(FACTORY_START_PID_FILE)"; \
	 printf 'factory-stop: stopped cleanly\n'

factory-reset: factory-database-guard
	test "$(FACTORY_RESET_CONFIRM)" = "WIPE_FACTORY"
	test -n "$$FACTORY_RUNTIME_ROOT"
	command -v dropdb >/dev/null
	command -v createdb >/dev/null
	command -v psql >/dev/null
	@set -eu; \
	 database_url="$$FACTORY_DATABASE_URL"; \
	 database_name="$${database_url##*/}"; \
	 database_name="$${database_name%%\?*}"; \
	 case "$$database_name" in factory_*) ;; *) \
		printf '%s\n' 'factory-reset: database name must start with factory_' >&2; exit 1 ;; \
	 esac; \
	 if case "$$database_url" in postgresql://*) true ;; *) false ;; esac; then \
		connection_authority="$${database_url#postgresql://}"; \
		connection_authority="$${connection_authority%%/*}"; \
		connection_user=""; \
		case "$$connection_authority" in *@*) connection_user="$${connection_authority%@*}"; connection_authority="$${connection_authority#*@}" ;; esac; \
		connection_host="$${connection_authority%%:*}"; \
		connection_port="$${connection_authority##*:}"; \
		test -n "$$connection_host"; \
		if test "$$connection_port" = "$$connection_authority"; then connection_port=5432; fi; \
		PGHOST="$$connection_host" PGPORT="$$connection_port"; export PGHOST PGPORT; \
	 if test -n "$$connection_user"; then PGUSER="$$connection_user"; export PGUSER; fi; \
	 fi; \
	 runtime_root="$$FACTORY_RUNTIME_ROOT"; \
	 case "$$runtime_root" in /tmp/*|/private/tmp/*|"$(CURDIR)"/var/*) ;; *) \
		printf '%s\n' 'factory-reset: runtime root must be under /tmp, /private/tmp, or this checkout var/' >&2; exit 1 ;; \
	 esac; \
	 if ps -axo command= | grep -E '[/]factoryd serve|(^| )factoryd serve' >/dev/null 2>&1; then \
		printf '%s\n' 'factory-reset: stop all factoryd daemons before resetting factory state' >&2; exit 1; \
	 fi; \
	 for factory_database in $$(psql -d postgres -Atqc "select datname from pg_database where datname like 'factory_%' order by datname"); do \
		dropdb --if-exists --force "$$factory_database"; \
	 done; \
	 rm -rf -- "$$runtime_root" "$(CURDIR)/var"; \
	 for factory_runtime in /tmp/factory-* /private/tmp/factory-*; do \
		test -e "$$factory_runtime" || continue; \
		rm -rf -- "$$factory_runtime"; \
	 done; \
	createdb "$$database_name"; \
	mkdir -p "$(CURDIR)/var"; \
	printf 'factory-reset: wiped all factory_* databases and runtime state; recreated %s\n' "$$database_name"

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
	$(MAKE) paid-cycle-preflight
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

# The generic application contract is exercised without selecting a product
# or opening a database. Product bundle data remains under applications/<key>;
# daemon admission supplies the explicit source root at runtime.
application-contract-test:
	cargo test -p factory-protocol --test application_v2

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
# already-created disposable database for each database-backed judge plus the
# source/blank restore clone pair consumed by backup-restore-test. The generic
# application contract test is Rust-only and needs no database. No target here
# creates or drops a PostgreSQL database.
provider-free-acceptance:
	test -n "$$FACTORY_ACCEPTANCE_POSTGRES_URL"
	test -n "$$FACTORY_ACCEPTANCE_DECISION_URL"
	test -n "$$FACTORY_ACCEPTANCE_VERTICAL_URL"
	@set -eu; \
	acceptance_postgres_name="$${FACTORY_ACCEPTANCE_POSTGRES_URL##*/}"; acceptance_postgres_name="$${acceptance_postgres_name%%\?*}"; \
	acceptance_decision_name="$${FACTORY_ACCEPTANCE_DECISION_URL##*/}"; acceptance_decision_name="$${acceptance_decision_name%%\?*}"; \
	acceptance_vertical_name="$${FACTORY_ACCEPTANCE_VERTICAL_URL##*/}"; acceptance_vertical_name="$${acceptance_vertical_name%%\?*}"; \
	for database_name in "$$acceptance_postgres_name" "$$acceptance_decision_name" "$$acceptance_vertical_name"; do \
		printf '%s\n' "$$database_name" | grep -Eq '^factory_test_v3_[0-9]+$$'; \
	done; \
	test "$$acceptance_postgres_name" != "$$acceptance_decision_name"; \
	test "$$acceptance_postgres_name" != "$$acceptance_vertical_name"; \
	test "$$acceptance_decision_name" != "$$acceptance_vertical_name"
	$(MAKE) lint
	$(MAKE) provider-free-host
	FACTORY_TEST_DATABASE_URL="$$FACTORY_ACCEPTANCE_POSTGRES_URL" $(MAKE) postgres-test
	FACTORY_TEST_DATABASE_URL="$$FACTORY_ACCEPTANCE_DECISION_URL" $(MAKE) decision-test
	DATABASE_URL="$$FACTORY_ACCEPTANCE_POSTGRES_URL" $(MAKE) sqlx-check
	$(MAKE) application-contract-test
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
