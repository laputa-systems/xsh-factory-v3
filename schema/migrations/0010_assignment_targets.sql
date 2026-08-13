-- Assignment target identity is durable authority, not prompt prose.  Product
-- work has no downstream target, Engineering is bound to one exact ticket
-- attempt, and Quality is bound to both that attempt and one candidate from
-- it.  These nullable FKs keep the fixed twenty-table MVP shape intact.

ALTER TABLE factory.assignments
    ADD COLUMN ticket_attempt_id BIGINT REFERENCES factory.ticket_attempts (id),
    ADD COLUMN candidate_id BIGINT REFERENCES factory.candidates (id);

ALTER TABLE factory.assignments
    ADD CONSTRAINT assignments_office_target_shape
    CHECK (
        (office = 0 AND ticket_attempt_id IS NULL AND candidate_id IS NULL)
        OR (office = 1 AND ticket_attempt_id IS NOT NULL AND candidate_id IS NULL)
        OR (office = 2 AND ticket_attempt_id IS NOT NULL AND candidate_id IS NOT NULL)
    );

-- A foreign key alone cannot prove that the Quality candidate belongs to the
-- assignment's attempt.  Keep that relationship at the SQL boundary so a
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

CREATE OR REPLACE FUNCTION factory.reject_assignment_identity_update()
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

COMMENT ON SCHEMA factory IS 'factory-v3-schema:initial-authority-v10';
