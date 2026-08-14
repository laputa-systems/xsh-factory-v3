-- Durable offices are the institutional owners of fungible assignment
-- invocations.  The closed assignment role remains a packet capability; an
-- assignment now carries both identities and SQL proves that they agree.

CREATE TABLE factory.offices (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    application_revision_id BIGINT NOT NULL
        REFERENCES factory.application_revisions (id),
    -- Root offices carry one of the currently closed assignment roles.  A
    -- child office can be durable governance structure without pretending it
    -- is directly runnable under one of those three roles.
    assignment_role SMALLINT CHECK (assignment_role BETWEEN 0 AND 2),
    parent_office_id BIGINT,
    charter_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    authority_mask BIGINT NOT NULL CHECK (authority_mask BETWEEN 1 AND 7),
    budget_ceiling_micro_usd BIGINT
        CHECK (budget_ceiling_micro_usd IS NULL OR budget_ceiling_micro_usd >= 0),
    lifecycle SMALLINT NOT NULL DEFAULT 0 CHECK (lifecycle BETWEEN 0 AND 2),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (application_revision_id, assignment_role),
    UNIQUE (id, application_revision_id),
    UNIQUE (id, application_revision_id, assignment_role),
    CONSTRAINT offices_parent_same_application_fkey
        FOREIGN KEY (parent_office_id, application_revision_id)
        REFERENCES factory.offices (id, application_revision_id)
        DEFERRABLE INITIALLY DEFERRED
);

CREATE INDEX offices_application_lifecycle_index
    ON factory.offices (application_revision_id, lifecycle, id);

CREATE FUNCTION factory.assert_office_parent_acyclic()
RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    cursor_id BIGINT := NEW.parent_office_id;
    hops INTEGER := 0;
BEGIN
    WHILE cursor_id IS NOT NULL LOOP
        IF cursor_id = NEW.id THEN
            RAISE EXCEPTION 'office parent relation must be acyclic'
                USING ERRCODE = 'check_violation';
        END IF;
        SELECT parent_office_id
          INTO cursor_id
          FROM factory.offices
         WHERE id = cursor_id
           AND application_revision_id = NEW.application_revision_id;
        hops := hops + 1;
        IF hops > 64 THEN
            RAISE EXCEPTION 'office parent relation exceeds bounded depth'
                USING ERRCODE = 'check_violation';
        END IF;
    END LOOP;
    RETURN NEW;
END;
$$;

CREATE CONSTRAINT TRIGGER offices_parent_acyclic
    AFTER INSERT OR UPDATE OF parent_office_id, application_revision_id
    ON factory.offices
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION factory.assert_office_parent_acyclic();

-- Existing application revisions need the same fixed roots before their
-- assignment rows can be made office-bound.  The template artifacts are the
-- admitted, immutable charters for this initial root set.
INSERT INTO factory.offices (
    application_revision_id, assignment_role, charter_artifact_id, authority_mask
)
SELECT ar.id, roots.assignment_role, roots.charter_artifact_id, roots.authority_mask
  FROM factory.application_revisions ar
 CROSS JOIN LATERAL (
    VALUES
        (0, ar.product_research_system_template_artifact_id, 1::BIGINT),
        (1, ar.engineering_system_template_artifact_id, 2::BIGINT),
        (2, ar.quality_system_template_artifact_id, 4::BIGINT)
 ) AS roots(assignment_role, charter_artifact_id, authority_mask)
ON CONFLICT (application_revision_id, assignment_role) DO NOTHING;

ALTER TABLE factory.assignments
    RENAME COLUMN office TO assignment_role;

ALTER TABLE factory.sessions
    RENAME COLUMN office TO assignment_role;

ALTER TABLE factory.assignments
    ADD COLUMN office_id BIGINT;

UPDATE factory.assignments AS a
   SET office_id = o.id
  FROM factory.offices AS o
 WHERE o.application_revision_id = a.application_revision_id
   AND o.assignment_role = a.assignment_role;

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM factory.assignments WHERE office_id IS NULL) THEN
        RAISE EXCEPTION 'cannot bind historical assignment to a durable office';
    END IF;
END;
$$;

ALTER TABLE factory.assignments
    ALTER COLUMN office_id SET NOT NULL,
    ADD CONSTRAINT assignments_office_application_role_fkey
        FOREIGN KEY (office_id, application_revision_id, assignment_role)
        REFERENCES factory.offices (id, application_revision_id, assignment_role);

ALTER TABLE factory.sessions
    ADD COLUMN office_id BIGINT;

UPDATE factory.sessions AS s
   SET office_id = a.office_id
  FROM factory.assignments AS a
 WHERE a.id = s.assignment_id;

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM factory.sessions WHERE office_id IS NULL) THEN
        RAISE EXCEPTION 'cannot bind historical session to a durable office';
    END IF;
END;
$$;

ALTER TABLE factory.sessions
    ALTER COLUMN office_id SET NOT NULL,
    ADD CONSTRAINT sessions_office_application_role_fkey
        FOREIGN KEY (office_id, application_revision_id, assignment_role)
        REFERENCES factory.offices (id, application_revision_id, assignment_role);

-- The baseline trigger body is recreated because its role column was renamed.
CREATE OR REPLACE FUNCTION factory.assert_assignment_target_relation()
RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    candidate_attempt_id BIGINT;
BEGIN
    IF NEW.assignment_role = 0 THEN
        IF NEW.ticket_attempt_id IS NOT NULL OR NEW.candidate_id IS NOT NULL THEN
            RAISE EXCEPTION 'Product assignment must not name a ticket attempt or candidate'
                USING ERRCODE = 'check_violation';
        END IF;
    ELSIF NEW.assignment_role = 1 THEN
        IF NEW.ticket_attempt_id IS NULL OR NEW.candidate_id IS NOT NULL THEN
            RAISE EXCEPTION 'Engineering assignment must name exactly one ticket attempt'
                USING ERRCODE = 'check_violation';
        END IF;
    ELSIF NEW.assignment_role = 2 THEN
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
        RAISE EXCEPTION 'assignment role is invalid' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION factory.reject_assignment_identity_update()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.campaign_id, NEW.kernel_build_id, NEW.application_revision_id,
           NEW.office_id, NEW.assignment_role, NEW.target,
           NEW.ticket_attempt_id, NEW.candidate_id,
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
           OLD.office_id, OLD.assignment_role, OLD.target,
           OLD.ticket_attempt_id, OLD.candidate_id,
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

COMMENT ON TABLE factory.offices IS
    'Durable institutional offices scoped to an admitted application revision';
COMMENT ON COLUMN factory.assignments.assignment_role IS
    'Closed packet role; SQL foreign key binds it to the selected office';
