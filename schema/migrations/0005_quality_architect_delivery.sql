-- T8 is the final purpose-specific authority slice. These five tables bring
-- the Factory-owned application relation count to the hard MVP maximum of 20.
-- They retain immutable candidate/validation/review/decision/delivery facts;
-- mutable progress remains only on the candidate and ticket-attempt aggregates.

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
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
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
           NEW.candidate_tree, NEW.changed_paths_artifact_id, NEW.patch_artifact_id,
           NEW.engineering_session_id, NEW.engineering_report_artifact_id,
           NEW.commit_subject, NEW.commit_body, NEW.regression_test_identity,
           NEW.risks_artifact_id, NEW.created_at)
       IS DISTINCT FROM
       ROW(OLD.ticket_attempt_id, OLD.base_commit, OLD.base_tree, OLD.regression_tree,
           OLD.candidate_tree, OLD.changed_paths_artifact_id, OLD.patch_artifact_id,
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

COMMENT ON SCHEMA factory IS 'factory-v3-schema:initial-authority-v5';
