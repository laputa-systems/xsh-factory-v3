-- T5 durable boundary. SDK events, transcript chunks, and usage updates stay
-- in assignment-local CAS. PostgreSQL stores the packet seal, eligibility
-- profile, process custody, one terminal summary, and one audit receipt.

ALTER TABLE factory.campaigns
    ADD COLUMN measured_cost_micro_usd BIGINT NOT NULL DEFAULT 0
        CHECK (measured_cost_micro_usd >= 0),
    ADD COLUMN cost_state SMALLINT NOT NULL DEFAULT 0
        CHECK (cost_state BETWEEN 0 AND 2);

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
    thinking_level SMALLINT NOT NULL CHECK (thinking_level BETWEEN 0 AND 3),
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
    thinking_level SMALLINT NOT NULL CHECK (thinking_level BETWEEN 0 AND 3),
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
    required_read_manifest_artifact_id BIGINT REFERENCES factory.artifacts (id),
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
        AND required_read_manifest_artifact_id IS NOT NULL AND cost_state IS NOT NULL
        AND stop_reason IS NOT NULL)),
    CHECK (cost_state <> 0 OR cost_micro_usd IS NOT NULL),
    CHECK (cost_state <> 1 OR cost_micro_usd IS NULL),
    CHECK (required_read_satisfied_count IS NULL OR required_read_expected_count IS NOT NULL),
    CHECK (required_read_satisfied_count IS NULL OR required_read_satisfied_count <= required_read_expected_count)
);

CREATE UNIQUE INDEX sessions_one_running_paid ON factory.sessions ((TRUE))
    WHERE lifecycle = 1;
CREATE INDEX sessions_campaign_lifecycle_index
    ON factory.sessions (campaign_id, lifecycle, id);

CREATE FUNCTION factory.reject_assignment_identity_update()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.campaign_id, NEW.kernel_build_id, NEW.application_revision_id,
           NEW.office, NEW.target, NEW.packet_artifact_id, NEW.packet_digest,
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
           OLD.office, OLD.target, OLD.packet_artifact_id, OLD.packet_digest,
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

COMMENT ON SCHEMA factory IS 'factory-v3-schema:initial-authority-v4';
