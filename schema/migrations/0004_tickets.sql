-- T6 ticket authority. Ticket contracts are immutable sealed artifacts; these
-- rows retain only the state, exact artifact references, snapshots, and small
-- bounded reasons needed to admit work and explain buffer pressure.

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

COMMENT ON SCHEMA factory IS 'factory-v3-schema:initial-authority-v4';
