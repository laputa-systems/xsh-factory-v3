-- Factory V3's canonical fresh-schema authority.  This pre-release schema has
-- one migration: every durable relation, constraint, index, and trigger below
-- is the current MVP contract. No historical V3 database shape is supported.

CREATE SCHEMA factory;

COMMENT ON SCHEMA factory IS 'factory-v3-schema:initial-authority-v1';

CREATE TABLE factory.kernel_builds (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    build_digest BYTEA NOT NULL UNIQUE CHECK (octet_length(build_digest) = 32),
    source_digest BYTEA NOT NULL CHECK (octet_length(source_digest) = 32),
    binary_digest BYTEA NOT NULL CHECK (octet_length(binary_digest) = 32),
    schema_identity TEXT NOT NULL CHECK (octet_length(schema_identity) BETWEEN 1 AND 160),
    deno_executable_path TEXT NOT NULL CHECK (deno_executable_path LIKE '/%' AND octet_length(deno_executable_path) <= 4096),
    deno_version TEXT NOT NULL CHECK (octet_length(deno_version) BETWEEN 1 AND 240),
    deno_lock_digest BYTEA NOT NULL CHECK (octet_length(deno_lock_digest) = 32),
    -- The install transaction inserts the deferred build/artifact pair
    -- together, so this receipt is never durably absent.
    qualification_receipt_artifact_id BIGINT NOT NULL,
    installed_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    is_current BOOLEAN NOT NULL DEFAULT FALSE,
    revision BIGINT NOT NULL CHECK (revision > 0)
);

CREATE UNIQUE INDEX kernel_builds_one_current
    ON factory.kernel_builds (is_current)
    WHERE is_current;

CREATE TABLE factory.artifacts (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    digest BYTEA NOT NULL UNIQUE CHECK (octet_length(digest) = 32),
    byte_length BIGINT NOT NULL CHECK (byte_length >= 0),
    cas_relative_path TEXT NOT NULL UNIQUE CHECK (
        octet_length(cas_relative_path) BETWEEN 1 AND 4096
        AND cas_relative_path NOT LIKE '/%'
        AND cas_relative_path !~ E'\\\\'
        AND cas_relative_path !~ '(^|/)(\\.|\\.\\.)(/|$)'
        AND cas_relative_path !~ '//'
    ),
    creating_kernel_build_id BIGINT NOT NULL
        REFERENCES factory.kernel_builds (id) DEFERRABLE INITIALLY DEFERRED,
    sealed_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

ALTER TABLE factory.kernel_builds
    ADD CONSTRAINT kernel_builds_qualification_receipt_artifact_id_fkey
    FOREIGN KEY (qualification_receipt_artifact_id)
    REFERENCES factory.artifacts (id) DEFERRABLE INITIALLY DEFERRED;

CREATE TABLE factory.repositories (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repository_key TEXT NOT NULL UNIQUE CHECK (octet_length(repository_key) BETWEEN 1 AND 160),
    canonical_local_path TEXT NOT NULL UNIQUE CHECK (canonical_local_path LIKE '/%' AND octet_length(canonical_local_path) <= 4096),
    default_branch TEXT NOT NULL CHECK (octet_length(default_branch) BETWEEN 1 AND 240),
    delivery_mode SMALLINT NOT NULL CHECK (delivery_mode = 0),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE factory.application_revisions (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    application_key TEXT NOT NULL CHECK (octet_length(application_key) BETWEEN 1 AND 80),
    aggregate_revision BIGINT NOT NULL CHECK (aggregate_revision > 0),
    predecessor_application_revision_id BIGINT REFERENCES factory.application_revisions (id),
    bundle_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    mission_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    product_research_system_template_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    product_research_assignment_template_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    engineering_system_template_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    engineering_assignment_template_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    quality_system_template_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    quality_assignment_template_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    repository_id BIGINT NOT NULL REFERENCES factory.repositories (id),
    ticket_low_water INTEGER NOT NULL CHECK (ticket_low_water > 0),
    ticket_target INTEGER NOT NULL CHECK (ticket_target >= ticket_low_water),
    ticket_maximum INTEGER NOT NULL CHECK (ticket_maximum >= ticket_target),
    proposal_maximum INTEGER NOT NULL CHECK (proposal_maximum > 0),
    ticket_narrative_byte_limit INTEGER NOT NULL CHECK (ticket_narrative_byte_limit > 0),
    ticket_acceptance_criteria_limit INTEGER NOT NULL CHECK (ticket_acceptance_criteria_limit > 0),
    ticket_contract_read_limit INTEGER NOT NULL CHECK (ticket_contract_read_limit > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    is_active BOOLEAN NOT NULL DEFAULT FALSE,
    UNIQUE (application_key, aggregate_revision)
);

CREATE UNIQUE INDEX application_revisions_one_active_per_application
    ON factory.application_revisions (application_key)
    WHERE is_active;

CREATE TABLE factory.campaigns (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kernel_build_id BIGINT NOT NULL REFERENCES factory.kernel_builds (id),
    application_revision_id BIGINT NOT NULL REFERENCES factory.application_revisions (id),
    repository_id BIGINT NOT NULL REFERENCES factory.repositories (id),
    lifecycle SMALLINT NOT NULL CHECK (lifecycle BETWEEN 0 AND 3),
    aggregate_budget_micro_usd BIGINT NOT NULL CHECK (aggregate_budget_micro_usd >= 0),
    deadline TIMESTAMPTZ NOT NULL,
    delivery_target INTEGER NOT NULL CHECK (delivery_target > 0),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    measured_cost_micro_usd BIGINT NOT NULL DEFAULT 0
        CHECK (measured_cost_micro_usd >= 0),
    cost_state SMALLINT NOT NULL DEFAULT 0
        CHECK (cost_state BETWEEN 0 AND 2),
    failure_reason TEXT
        CHECK (failure_reason IS NULL OR octet_length(failure_reason) BETWEEN 1 AND 240),
    CONSTRAINT campaigns_failure_reason_matches_lifecycle CHECK (
        (lifecycle = 2 AND failure_reason IS NOT NULL)
        OR (lifecycle <> 2 AND failure_reason IS NULL)
    )
);

CREATE UNIQUE INDEX campaigns_one_running
    ON factory.campaigns (lifecycle)
    WHERE lifecycle = 0;

CREATE TABLE factory.audit_log (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    principal TEXT NOT NULL CHECK (octet_length(principal) BETWEEN 1 AND 160),
    command_id TEXT NOT NULL CHECK (octet_length(command_id) BETWEEN 1 AND 160),
    operation TEXT NOT NULL CHECK (octet_length(operation) BETWEEN 1 AND 160),
    command_fingerprint BYTEA NOT NULL CHECK (octet_length(command_fingerprint) = 32),
    subject_kind SMALLINT NOT NULL CHECK (subject_kind BETWEEN 0 AND 255),
    subject_id BIGINT NOT NULL CHECK (subject_id > 0),
    resulting_revision BIGINT NOT NULL CHECK (resulting_revision >= 0),
    accepted_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (principal, command_id)
);

CREATE INDEX audit_log_subject_index ON factory.audit_log (subject_kind, subject_id, id);

-- Forum rows are permanent, immutable application communication facts.  The
-- typed kernel writes one audit receipt for each mutation; Forum reads do not
-- write.
CREATE TABLE factory.forum_topics (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    author_kind SMALLINT NOT NULL CHECK (author_kind IN (0, 1)),
    author_session_id BIGINT,
    author_office SMALLINT,
    -- The framed protocol rejects NUL before SQL; PostgreSQL text itself
    -- cannot represent NUL in a UTF-8 server encoding.
    name TEXT NOT NULL CHECK (octet_length(name) BETWEEN 1 AND 160),
    description TEXT NOT NULL CHECK (octet_length(description) <= 4096),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    supersedes_topic_id BIGINT REFERENCES factory.forum_topics (id),
    search_vector TSVECTOR GENERATED ALWAYS AS (
        to_tsvector('simple', coalesce(name, '') || ' ' || coalesce(description, ''))
    ) STORED,
    CHECK (
        (author_kind = 0 AND author_session_id IS NOT NULL AND author_office IS NOT NULL)
        OR (author_kind = 1 AND author_session_id IS NULL AND author_office IS NULL)
    ),
    CHECK (supersedes_topic_id IS NULL OR supersedes_topic_id <> id)
);

CREATE INDEX forum_topics_search_gin ON factory.forum_topics USING GIN (search_vector);
CREATE INDEX forum_topics_recent_index ON factory.forum_topics (created_at DESC, id DESC);

CREATE TABLE factory.forum_threads (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    topic_id BIGINT NOT NULL REFERENCES factory.forum_topics (id),
    author_kind SMALLINT NOT NULL CHECK (author_kind IN (0, 1)),
    author_session_id BIGINT,
    author_office SMALLINT,
    title TEXT NOT NULL CHECK (octet_length(title) BETWEEN 1 AND 240),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    supersedes_thread_id BIGINT REFERENCES factory.forum_threads (id),
    search_vector TSVECTOR GENERATED ALWAYS AS (
        to_tsvector('simple', coalesce(title, ''))
    ) STORED,
    CHECK (
        (author_kind = 0 AND author_session_id IS NOT NULL AND author_office IS NOT NULL)
        OR (author_kind = 1 AND author_session_id IS NULL AND author_office IS NULL)
    ),
    CHECK (supersedes_thread_id IS NULL OR supersedes_thread_id <> id)
);

CREATE INDEX forum_threads_search_gin ON factory.forum_threads USING GIN (search_vector);
CREATE INDEX forum_threads_topic_index ON factory.forum_threads (topic_id, id);
CREATE INDEX forum_threads_recent_index ON factory.forum_threads (created_at DESC, id DESC);

CREATE TABLE factory.forum_posts (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    thread_id BIGINT NOT NULL REFERENCES factory.forum_threads (id),
    author_kind SMALLINT NOT NULL CHECK (author_kind IN (0, 1)),
    author_session_id BIGINT,
    author_office SMALLINT,
    body TEXT NOT NULL CHECK (octet_length(body) <= 16384),
    kind SMALLINT NOT NULL CHECK (kind BETWEEN 0 AND 6),
    reply_to_post_id BIGINT REFERENCES factory.forum_posts (id),
    supersedes_post_id BIGINT REFERENCES factory.forum_posts (id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    search_vector TSVECTOR GENERATED ALWAYS AS (
        to_tsvector('simple', coalesce(body, ''))
    ) STORED,
    CHECK (
        (author_kind = 0 AND author_session_id IS NOT NULL AND author_office IS NOT NULL)
        OR (author_kind = 1 AND author_session_id IS NULL AND author_office IS NULL)
    ),
    CHECK (
        reply_to_post_id IS NULL OR supersedes_post_id IS NULL
        OR reply_to_post_id <> supersedes_post_id
    )
);

CREATE INDEX forum_posts_search_gin ON factory.forum_posts USING GIN (search_vector);
CREATE INDEX forum_posts_thread_order_index ON factory.forum_posts (thread_id, id);
CREATE INDEX forum_posts_author_index ON factory.forum_posts (author_office, id);
CREATE INDEX forum_posts_created_index ON factory.forum_posts (created_at, id);

CREATE TABLE factory.forum_attachments (
    post_id BIGINT NOT NULL REFERENCES factory.forum_posts (id),
    artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    label TEXT NOT NULL CHECK (octet_length(label) <= 160),
    PRIMARY KEY (post_id, artifact_id)
);

CREATE INDEX forum_attachments_artifact_index ON factory.forum_attachments (artifact_id, post_id);

CREATE FUNCTION factory.forum_enforce_attachment_quota()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF (SELECT count(*) FROM factory.forum_attachments WHERE post_id = NEW.post_id) >= 8 THEN
        RAISE EXCEPTION 'Forum post % exceeds the eight-attachment quota', NEW.post_id
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER forum_attachments_count_check
    BEFORE INSERT ON factory.forum_attachments
    FOR EACH ROW EXECUTE FUNCTION factory.forum_enforce_attachment_quota();

-- PostgreSQL CHECK constraints cannot inspect a target row. These triggers
-- enforce cross-row immutability and relation contracts atomically.
CREATE FUNCTION factory.forum_reject_immutable_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'Forum rows are immutable: % %', TG_TABLE_NAME, OLD.id
        USING ERRCODE = 'check_violation';
END;
$$;

CREATE TRIGGER forum_topics_immutable
    BEFORE UPDATE OR DELETE ON factory.forum_topics
    FOR EACH ROW EXECUTE FUNCTION factory.forum_reject_immutable_update();
CREATE TRIGGER forum_threads_immutable
    BEFORE UPDATE OR DELETE ON factory.forum_threads
    FOR EACH ROW EXECUTE FUNCTION factory.forum_reject_immutable_update();
CREATE TRIGGER forum_posts_immutable
    BEFORE UPDATE OR DELETE ON factory.forum_posts
    FOR EACH ROW EXECUTE FUNCTION factory.forum_reject_immutable_update();

CREATE FUNCTION factory.forum_reject_attachment_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'Forum attachment relations are immutable: post %, artifact %', OLD.post_id, OLD.artifact_id
        USING ERRCODE = 'check_violation';
END;
$$;

CREATE TRIGGER forum_attachments_immutable
    BEFORE UPDATE OR DELETE ON factory.forum_attachments
    FOR EACH ROW EXECUTE FUNCTION factory.forum_reject_attachment_update();

CREATE FUNCTION factory.forum_validate_post_relations()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    target_thread BIGINT;
BEGIN
    IF NEW.reply_to_post_id IS NOT NULL THEN
        SELECT thread_id INTO target_thread FROM factory.forum_posts WHERE id = NEW.reply_to_post_id;
        IF target_thread IS NULL THEN
            RAISE EXCEPTION 'reply target % does not exist', NEW.reply_to_post_id USING ERRCODE = 'foreign_key_violation';
        END IF;
        IF target_thread <> NEW.thread_id OR NEW.reply_to_post_id >= NEW.id THEN
            RAISE EXCEPTION 'reply target must be earlier and in the same thread' USING ERRCODE = 'check_violation';
        END IF;
    END IF;
    IF NEW.supersedes_post_id IS NOT NULL THEN
        SELECT thread_id INTO target_thread FROM factory.forum_posts WHERE id = NEW.supersedes_post_id;
        IF target_thread IS NULL THEN
            RAISE EXCEPTION 'supersession target % does not exist', NEW.supersedes_post_id USING ERRCODE = 'foreign_key_violation';
        END IF;
        IF target_thread <> NEW.thread_id OR NEW.supersedes_post_id >= NEW.id THEN
            RAISE EXCEPTION 'supersession target must be earlier and in the same thread' USING ERRCODE = 'check_violation';
        END IF;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER forum_posts_relation_check
    BEFORE INSERT ON factory.forum_posts
    FOR EACH ROW EXECUTE FUNCTION factory.forum_validate_post_relations();

-- Process custody retains the sealed packet, eligibility profile, process
-- identity, and one terminal summary. SDK events and transcript chunks remain
-- assignment-local CAS evidence rather than PostgreSQL rows.
CREATE TABLE factory.assignments (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    campaign_id BIGINT NOT NULL REFERENCES factory.campaigns (id),
    kernel_build_id BIGINT NOT NULL REFERENCES factory.kernel_builds (id),
    application_revision_id BIGINT NOT NULL REFERENCES factory.application_revisions (id),
    office SMALLINT NOT NULL CHECK (office BETWEEN 0 AND 2),
    target TEXT NOT NULL CHECK (octet_length(target) BETWEEN 1 AND 4096),
    packet_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    packet_digest BYTEA NOT NULL CHECK (octet_length(packet_digest) = 32),
    system_prompt_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    assignment_prompt_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    required_read_manifest_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    model_provider TEXT NOT NULL CHECK (octet_length(model_provider) BETWEEN 1 AND 160),
    model_id TEXT NOT NULL CHECK (octet_length(model_id) BETWEEN 1 AND 240),
    thinking_level SMALLINT NOT NULL CHECK (thinking_level BETWEEN 0 AND 4),
    context_token_limit INTEGER NOT NULL CHECK (context_token_limit > 0),
    output_token_limit INTEGER NOT NULL CHECK (output_token_limit > 0),
    input_price_micro_usd_per_million BIGINT NOT NULL CHECK (input_price_micro_usd_per_million >= 0),
    output_price_micro_usd_per_million BIGINT NOT NULL CHECK (output_price_micro_usd_per_million >= 0),
    cache_read_price_micro_usd_per_million BIGINT NOT NULL CHECK (cache_read_price_micro_usd_per_million >= 0),
    cache_write_price_micro_usd_per_million BIGINT NOT NULL CHECK (cache_write_price_micro_usd_per_million >= 0),
    turn_limit INTEGER NOT NULL CHECK (turn_limit > 0),
    wall_limit_millis BIGINT NOT NULL CHECK (wall_limit_millis > 0),
    output_byte_limit INTEGER NOT NULL CHECK (output_byte_limit > 0),
    terminal_operations_mask BIGINT NOT NULL CHECK (terminal_operations_mask > 0),
    remaining_campaign_allowance_micro_usd BIGINT NOT NULL CHECK (remaining_campaign_allowance_micro_usd >= 0),
    attempt_ordinal INTEGER NOT NULL CHECK (attempt_ordinal > 0),
    lifecycle SMALLINT NOT NULL CHECK (lifecycle BETWEEN 0 AND 5),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    -- Target foreign keys are installed once both downstream tables exist.
    ticket_attempt_id BIGINT,
    candidate_id BIGINT,
    UNIQUE (campaign_id, attempt_ordinal)
);

CREATE INDEX assignments_campaign_lifecycle_index
    ON factory.assignments (campaign_id, lifecycle, id);

CREATE TABLE factory.sessions (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    assignment_id BIGINT NOT NULL UNIQUE REFERENCES factory.assignments (id),
    campaign_id BIGINT NOT NULL REFERENCES factory.campaigns (id),
    kernel_build_id BIGINT NOT NULL REFERENCES factory.kernel_builds (id),
    application_revision_id BIGINT NOT NULL REFERENCES factory.application_revisions (id),
    office SMALLINT NOT NULL CHECK (office BETWEEN 0 AND 2),
    model_provider TEXT NOT NULL CHECK (octet_length(model_provider) BETWEEN 1 AND 160),
    model_id TEXT NOT NULL CHECK (octet_length(model_id) BETWEEN 1 AND 240),
    thinking_level SMALLINT NOT NULL CHECK (thinking_level BETWEEN 0 AND 4),
    input_price_micro_usd_per_million BIGINT NOT NULL CHECK (input_price_micro_usd_per_million >= 0),
    output_price_micro_usd_per_million BIGINT NOT NULL CHECK (output_price_micro_usd_per_million >= 0),
    cache_read_price_micro_usd_per_million BIGINT NOT NULL CHECK (cache_read_price_micro_usd_per_million >= 0),
    cache_write_price_micro_usd_per_million BIGINT NOT NULL CHECK (cache_write_price_micro_usd_per_million >= 0),
    pid INTEGER NOT NULL CHECK (pid > 0),
    pgid INTEGER NOT NULL CHECK (pgid > 0),
    process_started_at_unix_millis BIGINT NOT NULL CHECK (process_started_at_unix_millis > 0),
    lifecycle SMALLINT NOT NULL CHECK (lifecycle BETWEEN 0 AND 5),
    transcript_artifact_id BIGINT REFERENCES factory.artifacts (id),
    stdout_artifact_id BIGINT REFERENCES factory.artifacts (id),
    stderr_artifact_id BIGINT REFERENCES factory.artifacts (id),
    partial_transcript_artifact_id BIGINT REFERENCES factory.artifacts (id),
    required_read_assertion_artifact_id BIGINT,
    required_read_expected_count INTEGER CHECK (required_read_expected_count >= 0),
    required_read_satisfied_count INTEGER CHECK (required_read_satisfied_count >= 0),
    input_tokens BIGINT CHECK (input_tokens >= 0),
    output_tokens BIGINT CHECK (output_tokens >= 0),
    cache_read_tokens BIGINT CHECK (cache_read_tokens >= 0),
    cache_write_tokens BIGINT CHECK (cache_write_tokens >= 0),
    reasoning_tokens BIGINT CHECK (reasoning_tokens >= 0),
    reported_cost_micro_usd BIGINT CHECK (reported_cost_micro_usd >= 0),
    cost_state SMALLINT CHECK (cost_state BETWEEN 0 AND 2),
    cost_micro_usd BIGINT CHECK (cost_micro_usd >= 0),
    stop_reason SMALLINT,
    terminal_operation SMALLINT,
    failure_class SMALLINT,
    started_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    terminal_at TIMESTAMPTZ,
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    CHECK (lifecycle < 2 OR (terminal_at IS NOT NULL AND transcript_artifact_id IS NOT NULL
        AND required_read_assertion_artifact_id IS NOT NULL AND cost_state IS NOT NULL
        AND stop_reason IS NOT NULL)),
    CHECK (cost_state <> 0 OR cost_micro_usd IS NOT NULL),
    CHECK (cost_state <> 1 OR cost_micro_usd IS NULL),
    CHECK (required_read_satisfied_count IS NULL OR required_read_expected_count IS NOT NULL),
    CHECK (required_read_satisfied_count IS NULL OR required_read_satisfied_count <= required_read_expected_count),
    CONSTRAINT sessions_required_read_manifest_artifact_id_fkey
        FOREIGN KEY (required_read_assertion_artifact_id) REFERENCES factory.artifacts (id)
);

CREATE UNIQUE INDEX sessions_one_running_paid ON factory.sessions ((TRUE))
    WHERE lifecycle = 1;
CREATE INDEX sessions_campaign_lifecycle_index
    ON factory.sessions (campaign_id, lifecycle, id);

CREATE FUNCTION factory.reject_session_identity_update()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.assignment_id, NEW.campaign_id, NEW.kernel_build_id,
           NEW.application_revision_id, NEW.office, NEW.model_provider, NEW.model_id,
           NEW.thinking_level, NEW.input_price_micro_usd_per_million,
           NEW.output_price_micro_usd_per_million, NEW.cache_read_price_micro_usd_per_million,
           NEW.cache_write_price_micro_usd_per_million, NEW.pid, NEW.pgid,
           NEW.process_started_at_unix_millis)
       IS DISTINCT FROM
       ROW(OLD.assignment_id, OLD.campaign_id, OLD.kernel_build_id,
           OLD.application_revision_id, OLD.office, OLD.model_provider, OLD.model_id,
           OLD.thinking_level, OLD.input_price_micro_usd_per_million,
           OLD.output_price_micro_usd_per_million, OLD.cache_read_price_micro_usd_per_million,
           OLD.cache_write_price_micro_usd_per_million, OLD.pid, OLD.pgid,
           OLD.process_started_at_unix_millis) THEN
        RAISE EXCEPTION 'session process identity is immutable' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER sessions_identity_immutable
    BEFORE UPDATE ON factory.sessions FOR EACH ROW
    EXECUTE FUNCTION factory.reject_session_identity_update();

-- Ticket contracts are immutable sealed artifacts. These rows retain only
-- lifecycle, exact artifact references, snapshots, and bounded explanations
-- required for admission and buffer pressure.
CREATE TABLE factory.tickets (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    application_revision_id BIGINT NOT NULL REFERENCES factory.application_revisions (id),
    lifecycle SMALLINT NOT NULL CHECK (lifecycle BETWEEN 0 AND 7),
    current_ticket_revision_id BIGINT,
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX tickets_application_lifecycle_index
    ON factory.tickets (application_revision_id, lifecycle, id);

CREATE TABLE factory.ticket_revisions (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    ticket_id BIGINT NOT NULL REFERENCES factory.tickets (id)
        DEFERRABLE INITIALLY DEFERRED,
    application_revision_id BIGINT NOT NULL REFERENCES factory.application_revisions (id),
    revision_ordinal INTEGER NOT NULL CHECK (revision_ordinal > 0),
    lifecycle SMALLINT NOT NULL CHECK (lifecycle BETWEEN 0 AND 7),
    proposal_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    reproducer_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    expected_observation_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    discovery_observation_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    discovery_commit TEXT NOT NULL CHECK (octet_length(discovery_commit) BETWEEN 1 AND 240),
    discovery_tree TEXT NOT NULL CHECK (octet_length(discovery_tree) BETWEEN 1 AND 240),
    sponsored_at TIMESTAMPTZ,
    sponsorship_reason TEXT CHECK (octet_length(sponsorship_reason) BETWEEN 1 AND 4096),
    last_requalification_outcome SMALLINT CHECK (last_requalification_outcome BETWEEN 0 AND 2),
    last_requalification_commit TEXT CHECK (octet_length(last_requalification_commit) BETWEEN 1 AND 240),
    last_requalification_tree TEXT CHECK (octet_length(last_requalification_tree) BETWEEN 1 AND 240),
    last_requalification_first_observation_artifact_id BIGINT REFERENCES factory.artifacts (id),
    last_requalification_second_observation_artifact_id BIGINT REFERENCES factory.artifacts (id),
    last_requalified_at TIMESTAMPTZ,
    blocked_reason TEXT CHECK (octet_length(blocked_reason) BETWEEN 1 AND 4096),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (ticket_id, revision_ordinal),
    UNIQUE (application_revision_id, reproducer_artifact_id),
    CHECK (
        (sponsored_at IS NULL AND sponsorship_reason IS NULL)
        OR (sponsored_at IS NOT NULL AND sponsorship_reason IS NOT NULL)
    ),
    CHECK (
        (last_requalification_outcome IS NULL
            AND last_requalification_commit IS NULL
            AND last_requalification_tree IS NULL
            AND last_requalification_first_observation_artifact_id IS NULL
            AND last_requalification_second_observation_artifact_id IS NULL
            AND last_requalified_at IS NULL)
        OR (last_requalification_outcome IS NOT NULL
            AND last_requalification_commit IS NOT NULL
            AND last_requalification_tree IS NOT NULL
            AND last_requalification_first_observation_artifact_id IS NOT NULL
            AND last_requalification_second_observation_artifact_id IS NOT NULL
            AND last_requalified_at IS NOT NULL)
    )
);

ALTER TABLE factory.tickets
    ADD CONSTRAINT tickets_current_ticket_revision_id_fkey
    FOREIGN KEY (current_ticket_revision_id)
    REFERENCES factory.ticket_revisions (id)
    DEFERRABLE INITIALLY DEFERRED;

ALTER TABLE factory.tickets
    ALTER COLUMN current_ticket_revision_id SET NOT NULL;

CREATE INDEX ticket_revisions_application_sponsorship_index
    ON factory.ticket_revisions (application_revision_id, sponsored_at, id)
    WHERE lifecycle = 1;
CREATE INDEX ticket_revisions_ticket_index
    ON factory.ticket_revisions (ticket_id, revision_ordinal DESC);

CREATE TABLE factory.ticket_attempts (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    ticket_revision_id BIGINT NOT NULL REFERENCES factory.ticket_revisions (id),
    campaign_id BIGINT NOT NULL REFERENCES factory.campaigns (id),
    claimed_commit TEXT NOT NULL CHECK (octet_length(claimed_commit) BETWEEN 1 AND 240),
    claimed_tree TEXT NOT NULL CHECK (octet_length(claimed_tree) BETWEEN 1 AND 240),
    stage SMALLINT NOT NULL CHECK (stage BETWEEN 0 AND 9),
    candidate_ordinal INTEGER NOT NULL DEFAULT 0 CHECK (candidate_ordinal >= 0),
    rework_ordinal INTEGER NOT NULL DEFAULT 0 CHECK (rework_ordinal >= 0),
    failed_at TIMESTAMPTZ,
    failure_reason TEXT CHECK (octet_length(failure_reason) BETWEEN 1 AND 4096),
    released_at TIMESTAMPTZ,
    release_reason TEXT CHECK (octet_length(release_reason) BETWEEN 1 AND 4096),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (
        (failed_at IS NULL AND failure_reason IS NULL)
        OR (failed_at IS NOT NULL AND failure_reason IS NOT NULL)
    ),
    CHECK (
        (released_at IS NULL AND release_reason IS NULL)
        OR (released_at IS NOT NULL AND release_reason IS NOT NULL)
    )
);

-- A failed attempt remains the sole owner of its revision until an explicit
-- successful release records fresh current-head reproduction evidence.
CREATE UNIQUE INDEX ticket_attempts_one_unreleased_revision
    ON factory.ticket_attempts (ticket_revision_id)
    WHERE released_at IS NULL;
CREATE INDEX ticket_attempts_campaign_stage_index
    ON factory.ticket_attempts (campaign_id, stage, id);
CREATE INDEX ticket_attempts_revision_stage_index
    ON factory.ticket_attempts (ticket_revision_id, stage, id);

CREATE FUNCTION factory.reject_ticket_identity_update()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.application_revision_id, NEW.created_at)
       IS DISTINCT FROM ROW(OLD.application_revision_id, OLD.created_at) THEN
        RAISE EXCEPTION 'ticket identity is immutable' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER tickets_identity_immutable
    BEFORE UPDATE ON factory.tickets FOR EACH ROW
    EXECUTE FUNCTION factory.reject_ticket_identity_update();

CREATE FUNCTION factory.reject_ticket_revision_contract_update()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.ticket_id, NEW.application_revision_id, NEW.revision_ordinal,
           NEW.proposal_artifact_id, NEW.reproducer_artifact_id,
           NEW.expected_observation_artifact_id, NEW.discovery_observation_artifact_id,
           NEW.discovery_commit, NEW.discovery_tree, NEW.created_at)
       IS DISTINCT FROM
       ROW(OLD.ticket_id, OLD.application_revision_id, OLD.revision_ordinal,
           OLD.proposal_artifact_id, OLD.reproducer_artifact_id,
           OLD.expected_observation_artifact_id, OLD.discovery_observation_artifact_id,
           OLD.discovery_commit, OLD.discovery_tree, OLD.created_at) THEN
        RAISE EXCEPTION 'ticket revision contract is immutable' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER ticket_revisions_contract_immutable
    BEFORE UPDATE ON factory.ticket_revisions FOR EACH ROW
    EXECUTE FUNCTION factory.reject_ticket_revision_contract_update();

CREATE FUNCTION factory.reject_ticket_attempt_identity_update()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.ticket_revision_id, NEW.campaign_id, NEW.claimed_commit,
           NEW.claimed_tree, NEW.created_at)
       IS DISTINCT FROM
       ROW(OLD.ticket_revision_id, OLD.campaign_id, OLD.claimed_commit,
           OLD.claimed_tree, OLD.created_at) THEN
        RAISE EXCEPTION 'ticket attempt claim identity is immutable' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER ticket_attempts_identity_immutable
    BEFORE UPDATE ON factory.ticket_attempts FOR EACH ROW
    EXECUTE FUNCTION factory.reject_ticket_attempt_identity_update();

-- Candidate, validation, review, decision, and delivery facts complete the
-- fixed twenty-table MVP schema. Mutable progress remains only on candidate
-- and ticket-attempt aggregates.
CREATE TABLE factory.candidates (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    ticket_attempt_id BIGINT NOT NULL REFERENCES factory.ticket_attempts (id),
    base_commit TEXT NOT NULL CHECK (octet_length(base_commit) BETWEEN 40 AND 64),
    base_tree TEXT NOT NULL CHECK (octet_length(base_tree) BETWEEN 40 AND 64),
    regression_tree TEXT NOT NULL CHECK (octet_length(regression_tree) BETWEEN 40 AND 64),
    candidate_tree TEXT NOT NULL CHECK (octet_length(candidate_tree) BETWEEN 40 AND 64),
    changed_paths_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    patch_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    engineering_session_id BIGINT NOT NULL UNIQUE REFERENCES factory.sessions (id),
    engineering_report_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    commit_subject TEXT NOT NULL CHECK (octet_length(commit_subject) BETWEEN 1 AND 120 AND commit_subject !~ E'[\\n\\r\\000]'),
    commit_body TEXT NOT NULL CHECK (octet_length(commit_body) <= 8192 AND commit_body !~ E'\\000'),
    regression_test_identity TEXT NOT NULL CHECK (octet_length(regression_test_identity) BETWEEN 1 AND 4096 AND regression_test_identity !~ E'\\000'),
    risks_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    candidate_commit TEXT CHECK (octet_length(candidate_commit) BETWEEN 40 AND 64),
    candidate_ref TEXT CHECK (candidate_ref LIKE 'refs/heads/factory/%' AND octet_length(candidate_ref) <= 512),
    lifecycle SMALLINT NOT NULL CHECK (lifecycle BETWEEN 0 AND 4),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    regression_patch_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    regression_command_set_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    regression_log_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    UNIQUE (ticket_attempt_id, candidate_tree),
    CHECK (base_commit ~ '^[0-9a-f]{40}([0-9a-f]{24})?$'),
    CHECK (base_tree ~ '^[0-9a-f]{40}([0-9a-f]{24})?$'),
    CHECK (regression_tree ~ '^[0-9a-f]{40}([0-9a-f]{24})?$'),
    CHECK (candidate_tree ~ '^[0-9a-f]{40}([0-9a-f]{24})?$'),
    CHECK (candidate_commit IS NULL OR candidate_commit ~ '^[0-9a-f]{40}([0-9a-f]{24})?$'),
    CHECK ((candidate_commit IS NULL AND candidate_ref IS NULL)
        OR (candidate_commit IS NOT NULL AND candidate_ref IS NOT NULL)),
    CHECK (base_tree <> regression_tree AND base_tree <> candidate_tree AND regression_tree <> candidate_tree)
);

CREATE INDEX candidates_attempt_lifecycle_index
    ON factory.candidates (ticket_attempt_id, lifecycle, id);

CREATE TABLE factory.validations (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    candidate_id BIGINT NOT NULL REFERENCES factory.candidates (id),
    kernel_build_id BIGINT NOT NULL REFERENCES factory.kernel_builds (id),
    performed_by_session_id BIGINT NOT NULL REFERENCES factory.sessions (id),
    validation_scope SMALLINT NOT NULL CHECK (validation_scope BETWEEN 0 AND 1),
    validation_profile TEXT NOT NULL CHECK (octet_length(validation_profile) BETWEEN 1 AND 160),
    pristine_tree TEXT NOT NULL CHECK (octet_length(pristine_tree) BETWEEN 40 AND 64 AND pristine_tree ~ '^[0-9a-f]{40}([0-9a-f]{24})?$'),
    command_set_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    lifecycle SMALLINT NOT NULL CHECK (lifecycle BETWEEN 1 AND 3),
    duration_millis BIGINT NOT NULL CHECK (duration_millis >= 0),
    log_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (candidate_id, validation_scope)
);

CREATE INDEX validations_candidate_scope_index
    ON factory.validations (candidate_id, validation_scope, id);

CREATE TABLE factory.reviews (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    candidate_id BIGINT NOT NULL UNIQUE REFERENCES factory.candidates (id),
    quality_session_id BIGINT NOT NULL UNIQUE REFERENCES factory.sessions (id),
    full_suite_validation_id BIGINT NOT NULL UNIQUE REFERENCES factory.validations (id),
    verdict SMALLINT NOT NULL CHECK (verdict BETWEEN 0 AND 1),
    rationale_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    risks_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    additional_probes_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id)
);

CREATE TABLE factory.architect_decisions (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    decision_kind SMALLINT NOT NULL CHECK (decision_kind BETWEEN 0 AND 4),
    ticket_revision_id BIGINT REFERENCES factory.ticket_revisions (id),
    ticket_attempt_id BIGINT REFERENCES factory.ticket_attempts (id),
    candidate_id BIGINT REFERENCES factory.candidates (id),
    review_id BIGINT REFERENCES factory.reviews (id),
    rationale_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    principal TEXT NOT NULL CHECK (octet_length(principal) BETWEEN 1 AND 160),
    overrides_quality_rejection BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (
        (decision_kind = 0 AND ticket_revision_id IS NOT NULL AND ticket_attempt_id IS NULL
            AND candidate_id IS NULL AND review_id IS NULL)
        OR (decision_kind = 1 AND ticket_revision_id IS NULL AND ticket_attempt_id IS NOT NULL
            AND candidate_id IS NULL AND review_id IS NULL)
        OR (decision_kind BETWEEN 2 AND 4 AND ticket_revision_id IS NULL AND ticket_attempt_id IS NULL
            AND candidate_id IS NOT NULL AND review_id IS NOT NULL)
    ),
    CHECK (overrides_quality_rejection = FALSE OR decision_kind = 2)
);

CREATE INDEX architect_decisions_candidate_index
    ON factory.architect_decisions (candidate_id, id)
    WHERE candidate_id IS NOT NULL;
CREATE INDEX architect_decisions_ticket_revision_index
    ON factory.architect_decisions (ticket_revision_id, id)
    WHERE ticket_revision_id IS NOT NULL;
CREATE INDEX architect_decisions_ticket_attempt_index
    ON factory.architect_decisions (ticket_attempt_id, id)
    WHERE ticket_attempt_id IS NOT NULL;

CREATE TABLE factory.deliveries (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    candidate_id BIGINT NOT NULL UNIQUE REFERENCES factory.candidates (id),
    candidate_commit TEXT NOT NULL CHECK (octet_length(candidate_commit) BETWEEN 40 AND 64 AND candidate_commit ~ '^[0-9a-f]{40}([0-9a-f]{24})?$'),
    expected_old_commit TEXT NOT NULL CHECK (octet_length(expected_old_commit) BETWEEN 40 AND 64 AND expected_old_commit ~ '^[0-9a-f]{40}([0-9a-f]{24})?$'),
    resulting_commit TEXT NOT NULL CHECK (octet_length(resulting_commit) BETWEEN 40 AND 64 AND resulting_commit ~ '^[0-9a-f]{40}([0-9a-f]{24})?$'),
    resulting_tree TEXT NOT NULL CHECK (octet_length(resulting_tree) BETWEEN 40 AND 64 AND resulting_tree ~ '^[0-9a-f]{40}([0-9a-f]{24})?$'),
    method SMALLINT NOT NULL CHECK (method = 0),
    lifecycle SMALLINT NOT NULL CHECK (lifecycle = 1),
    recovery_status SMALLINT NOT NULL CHECK (recovery_status = 0),
    receipt_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE FUNCTION factory.reject_candidate_identity_update()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.ticket_attempt_id, NEW.base_commit, NEW.base_tree, NEW.regression_tree,
           NEW.candidate_tree, NEW.changed_paths_artifact_id,
           NEW.regression_patch_artifact_id, NEW.regression_command_set_artifact_id,
           NEW.regression_log_artifact_id, NEW.patch_artifact_id,
           NEW.engineering_session_id, NEW.engineering_report_artifact_id,
           NEW.commit_subject, NEW.commit_body, NEW.regression_test_identity,
           NEW.risks_artifact_id, NEW.created_at)
       IS DISTINCT FROM
       ROW(OLD.ticket_attempt_id, OLD.base_commit, OLD.base_tree, OLD.regression_tree,
           OLD.candidate_tree, OLD.changed_paths_artifact_id,
           OLD.regression_patch_artifact_id, OLD.regression_command_set_artifact_id,
           OLD.regression_log_artifact_id, OLD.patch_artifact_id,
           OLD.engineering_session_id, OLD.engineering_report_artifact_id,
           OLD.commit_subject, OLD.commit_body, OLD.regression_test_identity,
           OLD.risks_artifact_id, OLD.created_at) THEN
        RAISE EXCEPTION 'candidate evidence identity is immutable' USING ERRCODE = 'check_violation';
    END IF;
    IF OLD.candidate_commit IS NOT NULL
       AND ROW(NEW.candidate_commit, NEW.candidate_ref)
           IS DISTINCT FROM ROW(OLD.candidate_commit, OLD.candidate_ref) THEN
        RAISE EXCEPTION 'candidate commit identity is immutable once attached' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER candidates_identity_immutable
    BEFORE UPDATE ON factory.candidates FOR EACH ROW
    EXECUTE FUNCTION factory.reject_candidate_identity_update();

CREATE FUNCTION factory.reject_validation_update()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'validation receipts are immutable' USING ERRCODE = 'check_violation';
END;
$$;
CREATE TRIGGER validations_immutable
    BEFORE UPDATE ON factory.validations FOR EACH ROW
    EXECUTE FUNCTION factory.reject_validation_update();

CREATE FUNCTION factory.reject_review_update()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'quality reviews are immutable' USING ERRCODE = 'check_violation';
END;
$$;
CREATE TRIGGER reviews_immutable
    BEFORE UPDATE ON factory.reviews FOR EACH ROW
    EXECUTE FUNCTION factory.reject_review_update();

CREATE FUNCTION factory.reject_architect_decision_update()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'architect decisions are immutable' USING ERRCODE = 'check_violation';
END;
$$;
CREATE TRIGGER architect_decisions_immutable
    BEFORE UPDATE ON factory.architect_decisions FOR EACH ROW
    EXECUTE FUNCTION factory.reject_architect_decision_update();

CREATE FUNCTION factory.reject_delivery_update()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'delivery receipts are immutable' USING ERRCODE = 'check_violation';
END;
$$;
CREATE TRIGGER deliveries_immutable
    BEFORE UPDATE ON factory.deliveries FOR EACH ROW
    EXECUTE FUNCTION factory.reject_delivery_update();

-- Assignment target identity is durable authority rather than prompt prose.
-- Product work has no downstream target, Engineering is bound to one exact
-- ticket attempt, and Quality is bound to that attempt and one of its
-- candidates.
ALTER TABLE factory.assignments
    ADD CONSTRAINT assignments_ticket_attempt_id_fkey
    FOREIGN KEY (ticket_attempt_id) REFERENCES factory.ticket_attempts (id),
    ADD CONSTRAINT assignments_candidate_id_fkey
    FOREIGN KEY (candidate_id) REFERENCES factory.candidates (id),
    ADD CONSTRAINT assignments_office_target_shape
    CHECK (
        (office = 0 AND ticket_attempt_id IS NULL AND candidate_id IS NULL)
        OR (office = 1 AND ticket_attempt_id IS NOT NULL AND candidate_id IS NULL)
        OR (office = 2 AND ticket_attempt_id IS NOT NULL AND candidate_id IS NOT NULL)
    );

-- A foreign key alone cannot prove that the Quality candidate belongs to the
-- assignment's attempt. Keep that relationship at the SQL boundary so a
-- caller cannot combine otherwise valid identities from unrelated work.
CREATE FUNCTION factory.assert_assignment_target_relation()
RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    candidate_attempt_id BIGINT;
BEGIN
    IF NEW.office = 0 THEN
        IF NEW.ticket_attempt_id IS NOT NULL OR NEW.candidate_id IS NOT NULL THEN
            RAISE EXCEPTION 'Product assignment must not name a ticket attempt or candidate'
                USING ERRCODE = 'check_violation';
        END IF;
    ELSIF NEW.office = 1 THEN
        IF NEW.ticket_attempt_id IS NULL OR NEW.candidate_id IS NOT NULL THEN
            RAISE EXCEPTION 'Engineering assignment must name exactly one ticket attempt'
                USING ERRCODE = 'check_violation';
        END IF;
    ELSIF NEW.office = 2 THEN
        IF NEW.ticket_attempt_id IS NULL OR NEW.candidate_id IS NULL THEN
            RAISE EXCEPTION 'Quality assignment must name an attempt and candidate'
                USING ERRCODE = 'check_violation';
        END IF;
        SELECT ticket_attempt_id INTO candidate_attempt_id
          FROM factory.candidates
         WHERE id = NEW.candidate_id;
        IF NOT FOUND OR candidate_attempt_id <> NEW.ticket_attempt_id THEN
            RAISE EXCEPTION 'Quality assignment candidate does not belong to its ticket attempt'
                USING ERRCODE = 'check_violation';
        END IF;
    ELSE
        RAISE EXCEPTION 'assignment office is invalid' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER assignments_target_relation
    BEFORE INSERT OR UPDATE OF office, ticket_attempt_id, candidate_id ON factory.assignments
    FOR EACH ROW EXECUTE FUNCTION factory.assert_assignment_target_relation();

CREATE FUNCTION factory.reject_assignment_identity_update()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.campaign_id, NEW.kernel_build_id, NEW.application_revision_id,
           NEW.office, NEW.target, NEW.ticket_attempt_id, NEW.candidate_id,
           NEW.packet_artifact_id, NEW.packet_digest,
           NEW.system_prompt_artifact_id, NEW.assignment_prompt_artifact_id,
           NEW.required_read_manifest_artifact_id, NEW.model_provider, NEW.model_id,
           NEW.thinking_level, NEW.context_token_limit, NEW.output_token_limit,
           NEW.input_price_micro_usd_per_million, NEW.output_price_micro_usd_per_million,
           NEW.cache_read_price_micro_usd_per_million, NEW.cache_write_price_micro_usd_per_million,
           NEW.turn_limit, NEW.wall_limit_millis, NEW.output_byte_limit,
           NEW.terminal_operations_mask, NEW.remaining_campaign_allowance_micro_usd,
           NEW.attempt_ordinal)
       IS DISTINCT FROM
       ROW(OLD.campaign_id, OLD.kernel_build_id, OLD.application_revision_id,
           OLD.office, OLD.target, OLD.ticket_attempt_id, OLD.candidate_id,
           OLD.packet_artifact_id, OLD.packet_digest,
           OLD.system_prompt_artifact_id, OLD.assignment_prompt_artifact_id,
           OLD.required_read_manifest_artifact_id, OLD.model_provider, OLD.model_id,
           OLD.thinking_level, OLD.context_token_limit, OLD.output_token_limit,
           OLD.input_price_micro_usd_per_million, OLD.output_price_micro_usd_per_million,
           OLD.cache_read_price_micro_usd_per_million, OLD.cache_write_price_micro_usd_per_million,
           OLD.turn_limit, OLD.wall_limit_millis, OLD.output_byte_limit,
           OLD.terminal_operations_mask, OLD.remaining_campaign_allowance_micro_usd,
           OLD.attempt_ordinal) THEN
        RAISE EXCEPTION 'assignment packet identity is immutable' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER assignments_identity_immutable
    BEFORE UPDATE ON factory.assignments FOR EACH ROW
    EXECUTE FUNCTION factory.reject_assignment_identity_update();
