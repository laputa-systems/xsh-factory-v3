-- Institutional records are deliberately concrete relations.  This migration
-- does not create a generic object directory or copy custody facts into a
-- second graph.  Long bodies and evidence stay in the sealed CAS and are
-- addressed here by factory.artifacts.

-- Composite identities let every new relation prove application scope in the
-- foreign key itself.  The primary keys remain convenient public identities.
ALTER TABLE factory.tickets
    ADD CONSTRAINT tickets_id_application_revision_unique
        UNIQUE (id, application_revision_id);
ALTER TABLE factory.ticket_revisions
    ADD CONSTRAINT ticket_revisions_id_application_revision_unique
        UNIQUE (id, application_revision_id);
ALTER TABLE factory.ticket_revisions
    ADD CONSTRAINT ticket_revisions_ticket_application_revision_fkey
        FOREIGN KEY (ticket_id, application_revision_id)
        REFERENCES factory.tickets (id, application_revision_id)
        DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE factory.assignments
    ADD CONSTRAINT assignments_id_application_revision_unique
        UNIQUE (id, application_revision_id);
ALTER TABLE factory.sessions
    ADD CONSTRAINT sessions_id_application_revision_unique
        UNIQUE (id, application_revision_id);
ALTER TABLE factory.candidates
    ADD COLUMN application_revision_id BIGINT;
UPDATE factory.candidates AS candidate
   SET application_revision_id = application_revision.id
  FROM factory.ticket_attempts AS attempt
  JOIN factory.campaigns AS campaign ON campaign.id = attempt.campaign_id
  JOIN factory.application_revisions AS application_revision
    ON application_revision.id = campaign.application_revision_id
 WHERE attempt.id = candidate.ticket_attempt_id;
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM factory.candidates WHERE application_revision_id IS NULL) THEN
        RAISE EXCEPTION 'cannot scope historical candidate to an application revision';
    END IF;
END;
$$;
ALTER TABLE factory.candidates
    ALTER COLUMN application_revision_id SET NOT NULL,
    ADD CONSTRAINT candidates_id_application_revision_unique
        UNIQUE (id, application_revision_id),
    ADD CONSTRAINT candidates_application_revision_fkey
        FOREIGN KEY (application_revision_id)
        REFERENCES factory.application_revisions (id);

-- Candidate custody remains authoritative.  The added application scope is
-- an immutable derived fact, so a later update cannot move a candidate into
-- another institutional graph merely by changing the composite FK column.
CREATE OR REPLACE FUNCTION factory.reject_candidate_identity_update()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.ticket_attempt_id, NEW.application_revision_id,
           NEW.base_commit, NEW.base_tree, NEW.regression_tree,
           NEW.candidate_tree, NEW.changed_paths_artifact_id,
           NEW.regression_patch_artifact_id, NEW.regression_command_set_artifact_id,
           NEW.regression_log_artifact_id, NEW.patch_artifact_id,
           NEW.engineering_session_id, NEW.engineering_report_artifact_id,
           NEW.commit_subject, NEW.commit_body, NEW.regression_test_identity,
           NEW.risks_artifact_id, NEW.created_at)
       IS DISTINCT FROM
       ROW(OLD.ticket_attempt_id, OLD.application_revision_id,
           OLD.base_commit, OLD.base_tree, OLD.regression_tree,
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

-- Migration 0005 renamed the session role column and added its durable office
-- identity. Recreate this inherited immutability function under the new
-- column names before any later session transition can invoke a stale body.
CREATE OR REPLACE FUNCTION factory.reject_session_identity_update()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.assignment_id, NEW.campaign_id, NEW.kernel_build_id,
           NEW.application_revision_id, NEW.office_id, NEW.assignment_role,
           NEW.model_provider, NEW.model_id, NEW.thinking_level,
           NEW.input_price_micro_usd_per_million,
           NEW.output_price_micro_usd_per_million,
           NEW.cache_read_price_micro_usd_per_million,
           NEW.cache_write_price_micro_usd_per_million, NEW.pid, NEW.pgid,
           NEW.process_started_at_unix_millis)
       IS DISTINCT FROM
       ROW(OLD.assignment_id, OLD.campaign_id, OLD.kernel_build_id,
           OLD.application_revision_id, OLD.office_id, OLD.assignment_role,
           OLD.model_provider, OLD.model_id, OLD.thinking_level,
           OLD.input_price_micro_usd_per_million,
           OLD.output_price_micro_usd_per_million,
           OLD.cache_read_price_micro_usd_per_million,
           OLD.cache_write_price_micro_usd_per_million, OLD.pid, OLD.pgid,
           OLD.process_started_at_unix_millis) THEN
        RAISE EXCEPTION 'session process identity is immutable' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TABLE factory.projects (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    application_revision_id BIGINT NOT NULL
        REFERENCES factory.application_revisions (id),
    owner_office_id BIGINT NOT NULL,
    title TEXT NOT NULL CHECK (
        octet_length(title) BETWEEN 1 AND 240 AND title !~ E'[\\n\\r\\000]'
    ),
    summary TEXT NOT NULL CHECK (
        octet_length(summary) BETWEEN 1 AND 4096 AND summary !~ E'[\\n\\r\\000]'
    ),
    body_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    lifecycle SMALLINT NOT NULL DEFAULT 0 CHECK (lifecycle BETWEEN 0 AND 4),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    search_vector TSVECTOR GENERATED ALWAYS AS (
        to_tsvector('simple', coalesce(title, '') || ' ' || coalesce(summary, ''))
    ) STORED,
    UNIQUE (id, application_revision_id),
    CONSTRAINT projects_owner_office_scope_fkey
        FOREIGN KEY (owner_office_id, application_revision_id)
        REFERENCES factory.offices (id, application_revision_id)
);

CREATE INDEX projects_application_lifecycle_index
    ON factory.projects (application_revision_id, lifecycle, id);
CREATE INDEX projects_search_gin ON factory.projects USING GIN (search_vector);

CREATE TABLE factory.rfcs (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    application_revision_id BIGINT NOT NULL
        REFERENCES factory.application_revisions (id),
    owner_office_id BIGINT NOT NULL,
    project_id BIGINT,
    title TEXT NOT NULL CHECK (
        octet_length(title) BETWEEN 1 AND 240 AND title !~ E'[\\n\\r\\000]'
    ),
    summary TEXT NOT NULL CHECK (
        octet_length(summary) BETWEEN 1 AND 4096 AND summary !~ E'[\\n\\r\\000]'
    ),
    current_rfc_revision_id BIGINT,
    lifecycle SMALLINT NOT NULL DEFAULT 0 CHECK (lifecycle BETWEEN 0 AND 5),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    search_vector TSVECTOR GENERATED ALWAYS AS (
        to_tsvector('simple', coalesce(title, '') || ' ' || coalesce(summary, ''))
    ) STORED,
    UNIQUE (id, application_revision_id)
);

CREATE INDEX rfcs_application_lifecycle_index
    ON factory.rfcs (application_revision_id, lifecycle, id);

ALTER TABLE factory.rfcs
    ADD CONSTRAINT rfcs_owner_office_scope_fkey
        FOREIGN KEY (owner_office_id, application_revision_id)
        REFERENCES factory.offices (id, application_revision_id),
    ADD CONSTRAINT rfcs_project_scope_fkey
        FOREIGN KEY (project_id, application_revision_id)
        REFERENCES factory.projects (id, application_revision_id);

CREATE INDEX rfcs_search_gin ON factory.rfcs USING GIN (search_vector);

CREATE TABLE factory.rfc_revisions (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    rfc_id BIGINT NOT NULL,
    application_revision_id BIGINT NOT NULL
    REFERENCES factory.application_revisions (id),
    revision_ordinal INTEGER NOT NULL CHECK (revision_ordinal > 0),
    summary TEXT NOT NULL CHECK (
        octet_length(summary) BETWEEN 1 AND 4096 AND summary !~ E'[\\n\\r\\000]'
    ),
    body_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    author_office_id BIGINT NOT NULL,
    lifecycle SMALLINT NOT NULL DEFAULT 0 CHECK (lifecycle BETWEEN 0 AND 3),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    supersedes_rfc_revision_id BIGINT,
    search_vector TSVECTOR GENERATED ALWAYS AS (
        to_tsvector('simple', coalesce(summary, ''))
    ) STORED,
    UNIQUE (rfc_id, revision_ordinal),
    UNIQUE (id, application_revision_id),
    UNIQUE (id, rfc_id, application_revision_id),
    CONSTRAINT rfc_revisions_rfc_scope_fkey
        FOREIGN KEY (rfc_id, application_revision_id)
        REFERENCES factory.rfcs (id, application_revision_id)
        DEFERRABLE INITIALLY DEFERRED,
    CONSTRAINT rfc_revisions_author_office_scope_fkey
        FOREIGN KEY (author_office_id, application_revision_id)
        REFERENCES factory.offices (id, application_revision_id),
    CONSTRAINT rfc_revisions_supersedes_scope_fkey
        FOREIGN KEY (supersedes_rfc_revision_id, rfc_id, application_revision_id)
        REFERENCES factory.rfc_revisions (id, rfc_id, application_revision_id)
        DEFERRABLE INITIALLY DEFERRED
);

CREATE INDEX rfc_revisions_application_index
    ON factory.rfc_revisions (application_revision_id, rfc_id, revision_ordinal DESC);
CREATE INDEX rfc_revisions_search_gin ON factory.rfc_revisions USING GIN (search_vector);

ALTER TABLE factory.rfcs
    ADD CONSTRAINT rfcs_current_revision_scope_fkey
        FOREIGN KEY (current_rfc_revision_id, id, application_revision_id)
        REFERENCES factory.rfc_revisions (id, rfc_id, application_revision_id)
        DEFERRABLE INITIALLY DEFERRED;

CREATE TABLE factory.experiments (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    application_revision_id BIGINT NOT NULL
        REFERENCES factory.application_revisions (id),
    owner_office_id BIGINT NOT NULL,
    project_id BIGINT,
    question TEXT NOT NULL CHECK (
        octet_length(question) BETWEEN 1 AND 4096 AND question !~ E'[\\n\\r\\000]'
    ),
    summary TEXT NOT NULL CHECK (
        octet_length(summary) BETWEEN 1 AND 4096 AND summary !~ E'[\\n\\r\\000]'
    ),
    evaluation_plan_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    intended_base_commit TEXT CHECK (
        intended_base_commit IS NULL OR (
            octet_length(intended_base_commit) BETWEEN 40 AND 64
            AND intended_base_commit ~ '^[0-9a-f]{40}([0-9a-f]{24})?$'
        )
    ),
    intended_base_tree TEXT CHECK (
        intended_base_tree IS NULL OR (
            octet_length(intended_base_tree) BETWEEN 40 AND 64
            AND intended_base_tree ~ '^[0-9a-f]{40}([0-9a-f]{24})?$'
        )
    ),
    target_claim_id BIGINT,
    target_rfc_revision_id BIGINT,
    budget_micro_usd BIGINT NOT NULL CHECK (budget_micro_usd >= 0),
    lifecycle SMALLINT NOT NULL DEFAULT 0 CHECK (lifecycle BETWEEN 0 AND 6),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    search_vector TSVECTOR GENERATED ALWAYS AS (
        to_tsvector('simple', coalesce(question, '') || ' ' || coalesce(summary, ''))
    ) STORED,
    UNIQUE (id, application_revision_id),
    CONSTRAINT experiments_owner_office_scope_fkey
        FOREIGN KEY (owner_office_id, application_revision_id)
        REFERENCES factory.offices (id, application_revision_id),
    CONSTRAINT experiments_project_scope_fkey
        FOREIGN KEY (project_id, application_revision_id)
        REFERENCES factory.projects (id, application_revision_id),
    CONSTRAINT experiments_target_exactly_one_check CHECK (
        num_nonnulls(target_claim_id, target_rfc_revision_id) = 1
    ),
    CONSTRAINT experiments_intended_base_pair_check CHECK (
        (intended_base_commit IS NULL AND intended_base_tree IS NULL)
        OR (intended_base_commit IS NOT NULL AND intended_base_tree IS NOT NULL)
    ));

CREATE INDEX experiments_application_lifecycle_index
    ON factory.experiments (application_revision_id, lifecycle, id);
CREATE INDEX experiments_project_index
    ON factory.experiments (project_id, id) WHERE project_id IS NOT NULL;
CREATE INDEX experiments_search_gin ON factory.experiments USING GIN (search_vector);

CREATE TABLE factory.experiment_runs (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    experiment_id BIGINT NOT NULL,
    application_revision_id BIGINT NOT NULL
        REFERENCES factory.application_revisions (id),
    owner_office_id BIGINT NOT NULL,
    run_ordinal INTEGER NOT NULL CHECK (run_ordinal > 0),
    base_commit TEXT NOT NULL CHECK (
        octet_length(base_commit) BETWEEN 40 AND 64
            AND base_commit ~ '^[0-9a-f]{40}([0-9a-f]{24})?$'
    ),
    base_tree TEXT NOT NULL CHECK (
        octet_length(base_tree) BETWEEN 40 AND 64
            AND base_tree ~ '^[0-9a-f]{40}([0-9a-f]{24})?$'
    ),
    invocation_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    evaluation_plan_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    candidate_id BIGINT,
    assignment_id BIGINT,
    session_id BIGINT,
    result_artifact_id BIGINT REFERENCES factory.artifacts (id),
    evaluator_receipt_artifact_id BIGINT REFERENCES factory.artifacts (id),
    produced_candidate_tree TEXT CHECK (
        produced_candidate_tree IS NULL OR (
            octet_length(produced_candidate_tree) BETWEEN 40 AND 64
            AND produced_candidate_tree ~ '^[0-9a-f]{40}([0-9a-f]{24})?$'
        )
    ),
    lifecycle SMALLINT NOT NULL DEFAULT 0 CHECK (lifecycle BETWEEN 0 AND 4),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    search_vector TSVECTOR GENERATED ALWAYS AS (
        to_tsvector('simple', coalesce(base_commit, '') || ' ' || coalesce(base_tree, ''))
    ) STORED,
    UNIQUE (experiment_id, run_ordinal),
    UNIQUE (id, application_revision_id),
    UNIQUE (id, experiment_id, application_revision_id),
    CONSTRAINT experiment_runs_experiment_scope_fkey
        FOREIGN KEY (experiment_id, application_revision_id)
        REFERENCES factory.experiments (id, application_revision_id)
        DEFERRABLE INITIALLY DEFERRED,
    CONSTRAINT experiment_runs_owner_office_scope_fkey
        FOREIGN KEY (owner_office_id, application_revision_id)
        REFERENCES factory.offices (id, application_revision_id),
    CONSTRAINT experiment_runs_candidate_scope_fkey
        FOREIGN KEY (candidate_id, application_revision_id)
        REFERENCES factory.candidates (id, application_revision_id),
    CONSTRAINT experiment_runs_assignment_scope_fkey
        FOREIGN KEY (assignment_id, application_revision_id)
        REFERENCES factory.assignments (id, application_revision_id),
    CONSTRAINT experiment_runs_session_scope_fkey
        FOREIGN KEY (session_id, application_revision_id)
        REFERENCES factory.sessions (id, application_revision_id)
);

CREATE INDEX experiment_runs_application_lifecycle_index
    ON factory.experiment_runs (application_revision_id, lifecycle, id);
CREATE INDEX experiment_runs_experiment_index
    ON factory.experiment_runs (experiment_id, run_ordinal DESC);
CREATE INDEX experiment_runs_search_gin ON factory.experiment_runs USING GIN (search_vector);

CREATE TABLE factory.claims (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    application_revision_id BIGINT NOT NULL
        REFERENCES factory.application_revisions (id),
    owner_office_id BIGINT NOT NULL,
    proposition TEXT NOT NULL CHECK (
        octet_length(proposition) BETWEEN 1 AND 4096 AND proposition !~ E'[\\n\\r\\000]'
    ),
    body_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    lifecycle SMALLINT NOT NULL DEFAULT 0 CHECK (lifecycle BETWEEN 0 AND 3),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    search_vector TSVECTOR GENERATED ALWAYS AS (
        to_tsvector('simple', coalesce(proposition, ''))
    ) STORED,
    UNIQUE (id, application_revision_id),
    CONSTRAINT claims_owner_office_scope_fkey
        FOREIGN KEY (owner_office_id, application_revision_id)
        REFERENCES factory.offices (id, application_revision_id)
);

CREATE INDEX claims_application_lifecycle_index
    ON factory.claims (application_revision_id, lifecycle, id);
CREATE INDEX claims_search_gin ON factory.claims USING GIN (search_vector);

ALTER TABLE factory.experiments
    ADD CONSTRAINT experiments_target_claim_scope_fkey
        FOREIGN KEY (target_claim_id, application_revision_id)
        REFERENCES factory.claims (id, application_revision_id),
    ADD CONSTRAINT experiments_target_rfc_revision_scope_fkey
        FOREIGN KEY (target_rfc_revision_id, application_revision_id)
        REFERENCES factory.rfc_revisions (id, application_revision_id);

CREATE TABLE factory.claim_evidence (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    claim_id BIGINT NOT NULL,
    application_revision_id BIGINT NOT NULL
        REFERENCES factory.application_revisions (id),
    relation_kind SMALLINT NOT NULL CHECK (relation_kind BETWEEN 0 AND 1),
    experiment_run_id BIGINT,
    evidence_artifact_id BIGINT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT claim_evidence_claim_scope_fkey
        FOREIGN KEY (claim_id, application_revision_id)
        REFERENCES factory.claims (id, application_revision_id),
    CONSTRAINT claim_evidence_run_scope_fkey
        FOREIGN KEY (experiment_run_id, application_revision_id)
        REFERENCES factory.experiment_runs (id, application_revision_id),
    CONSTRAINT claim_evidence_artifact_fkey
        FOREIGN KEY (evidence_artifact_id) REFERENCES factory.artifacts (id),
    CONSTRAINT claim_evidence_one_source_check CHECK (
        (experiment_run_id IS NOT NULL AND evidence_artifact_id IS NULL)
        OR (experiment_run_id IS NULL AND evidence_artifact_id IS NOT NULL)
    )
);

CREATE INDEX claim_evidence_claim_index
    ON factory.claim_evidence (claim_id, relation_kind, id);
CREATE INDEX claim_evidence_run_index
    ON factory.claim_evidence (experiment_run_id, id)
    WHERE experiment_run_id IS NOT NULL;

CREATE TABLE factory.decisions (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    application_revision_id BIGINT NOT NULL
        REFERENCES factory.application_revisions (id),
    deciding_office_id BIGINT NOT NULL,
    decision_kind SMALLINT NOT NULL CHECK (decision_kind BETWEEN 0 AND 3),
    title TEXT NOT NULL CHECK (
        octet_length(title) BETWEEN 1 AND 240 AND title !~ E'[\\n\\r\\000]'
    ),
    summary TEXT NOT NULL CHECK (
        octet_length(summary) BETWEEN 1 AND 4096 AND summary !~ E'[\\n\\r\\000]'
    ),
    rationale_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    lifecycle SMALLINT NOT NULL DEFAULT 0 CHECK (lifecycle BETWEEN 0 AND 2),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    search_vector TSVECTOR GENERATED ALWAYS AS (
        to_tsvector('simple', coalesce(title, '') || ' ' || coalesce(summary, ''))
    ) STORED,
    UNIQUE (id, application_revision_id),
    CONSTRAINT decisions_office_scope_fkey
        FOREIGN KEY (deciding_office_id, application_revision_id)
        REFERENCES factory.offices (id, application_revision_id)
);

CREATE INDEX decisions_application_lifecycle_index
    ON factory.decisions (application_revision_id, lifecycle, id);
CREATE INDEX decisions_search_gin ON factory.decisions USING GIN (search_vector);

CREATE TABLE factory.decision_targets (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    decision_id BIGINT NOT NULL,
    application_revision_id BIGINT NOT NULL
        REFERENCES factory.application_revisions (id),
    rfc_revision_id BIGINT,
    ticket_revision_id BIGINT,
    experiment_id BIGINT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT decision_targets_decision_scope_fkey
        FOREIGN KEY (decision_id, application_revision_id)
        REFERENCES factory.decisions (id, application_revision_id),
    CONSTRAINT decision_targets_rfc_scope_fkey
        FOREIGN KEY (rfc_revision_id, application_revision_id)
        REFERENCES factory.rfc_revisions (id, application_revision_id),
    CONSTRAINT decision_targets_ticket_scope_fkey
        FOREIGN KEY (ticket_revision_id, application_revision_id)
        REFERENCES factory.ticket_revisions (id, application_revision_id),
    CONSTRAINT decision_targets_experiment_scope_fkey
        FOREIGN KEY (experiment_id, application_revision_id)
        REFERENCES factory.experiments (id, application_revision_id),
    CONSTRAINT decision_targets_exactly_one_kind_check CHECK (
        (rfc_revision_id IS NOT NULL)::INTEGER
        + (ticket_revision_id IS NOT NULL)::INTEGER
        + (experiment_id IS NOT NULL)::INTEGER = 1
    )
);

CREATE INDEX decision_targets_decision_index
    ON factory.decision_targets (decision_id, id);
CREATE INDEX decision_targets_rfc_index
    ON factory.decision_targets (rfc_revision_id, id)
    WHERE rfc_revision_id IS NOT NULL;
CREATE INDEX decision_targets_ticket_index
    ON factory.decision_targets (ticket_revision_id, id)
    WHERE ticket_revision_id IS NOT NULL;
CREATE INDEX decision_targets_experiment_index
    ON factory.decision_targets (experiment_id, id)
    WHERE experiment_id IS NOT NULL;

-- These are the only extra edge tables in the initial graph.  An RFC and an
-- experiment already carry a single scoped project FK, while a Decision and
-- an Experiment already carry their typed target FKs.  Repeating those facts
-- in link rows would make two sources of truth.  Tickets may relate to more
-- than one project or RFC without changing their delivery contract.
CREATE TABLE factory.project_ticket_links (
    project_id BIGINT NOT NULL,
    ticket_id BIGINT NOT NULL,
    application_revision_id BIGINT NOT NULL
        REFERENCES factory.application_revisions (id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (project_id, ticket_id),
    FOREIGN KEY (project_id, application_revision_id)
        REFERENCES factory.projects (id, application_revision_id),
    FOREIGN KEY (ticket_id, application_revision_id)
        REFERENCES factory.tickets (id, application_revision_id)
);

CREATE TABLE factory.ticket_rfc_links (
    ticket_id BIGINT NOT NULL,
    rfc_id BIGINT NOT NULL,
    application_revision_id BIGINT NOT NULL
        REFERENCES factory.application_revisions (id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (ticket_id, rfc_id),
    FOREIGN KEY (ticket_id, application_revision_id)
        REFERENCES factory.tickets (id, application_revision_id),
    FOREIGN KEY (rfc_id, application_revision_id)
        REFERENCES factory.rfcs (id, application_revision_id)
);

CREATE OR REPLACE FUNCTION factory.reject_institutional_row_mutation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION '% rows are immutable: %', TG_TABLE_NAME, OLD.id
        USING ERRCODE = 'check_violation';
END;
$$;

CREATE OR REPLACE FUNCTION factory.reject_institutional_link_mutation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION '% relations are immutable', TG_TABLE_NAME
        USING ERRCODE = 'check_violation';
END;
$$;

CREATE TRIGGER rfc_revisions_immutable
    BEFORE UPDATE OR DELETE ON factory.rfc_revisions
    FOR EACH ROW EXECUTE FUNCTION factory.reject_institutional_row_mutation();
CREATE TRIGGER experiment_runs_immutable
    BEFORE UPDATE OR DELETE ON factory.experiment_runs
    FOR EACH ROW EXECUTE FUNCTION factory.reject_institutional_row_mutation();
CREATE TRIGGER claim_evidence_immutable
    BEFORE UPDATE OR DELETE ON factory.claim_evidence
    FOR EACH ROW EXECUTE FUNCTION factory.reject_institutional_row_mutation();
CREATE TRIGGER decision_targets_immutable
    BEFORE UPDATE OR DELETE ON factory.decision_targets
    FOR EACH ROW EXECUTE FUNCTION factory.reject_institutional_row_mutation();

CREATE TRIGGER project_ticket_links_immutable
    BEFORE UPDATE OR DELETE ON factory.project_ticket_links
    FOR EACH ROW EXECUTE FUNCTION factory.reject_institutional_link_mutation();
CREATE TRIGGER ticket_rfc_links_immutable
    BEFORE UPDATE OR DELETE ON factory.ticket_rfc_links
    FOR EACH ROW EXECUTE FUNCTION factory.reject_institutional_link_mutation();

CREATE OR REPLACE FUNCTION factory.reject_project_identity_update()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.id, NEW.application_revision_id, NEW.owner_office_id,
           NEW.title, NEW.summary, NEW.body_artifact_id, NEW.created_at)
       IS DISTINCT FROM
       ROW(OLD.id, OLD.application_revision_id, OLD.owner_office_id,
           OLD.title, OLD.summary, OLD.body_artifact_id, OLD.created_at)
    OR NEW.revision <= OLD.revision THEN
        RAISE EXCEPTION 'project identity is immutable and revisions must increase'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION factory.reject_rfc_identity_update()
RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    selected_rfc BIGINT;
BEGIN
    IF ROW(NEW.id, NEW.application_revision_id, NEW.owner_office_id,
           NEW.project_id, NEW.title, NEW.summary, NEW.created_at)
       IS DISTINCT FROM ROW(OLD.id, OLD.application_revision_id, OLD.owner_office_id,
           OLD.project_id, OLD.title, OLD.summary, OLD.created_at)
    OR NEW.revision <= OLD.revision THEN
        RAISE EXCEPTION 'RFC identity is immutable and revisions must increase'
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.current_rfc_revision_id IS NOT NULL THEN
        SELECT rfc_id INTO selected_rfc
          FROM factory.rfc_revisions
         WHERE id = NEW.current_rfc_revision_id
           AND application_revision_id = NEW.application_revision_id;
        IF selected_rfc IS DISTINCT FROM NEW.id THEN
            RAISE EXCEPTION 'RFC current revision belongs to another RFC'
                USING ERRCODE = 'check_violation';
        END IF;
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION factory.reject_experiment_identity_update()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.id, NEW.application_revision_id, NEW.owner_office_id,
           NEW.project_id, NEW.question, NEW.summary,
           NEW.evaluation_plan_artifact_id,
           NEW.intended_base_commit, NEW.intended_base_tree,
           NEW.target_claim_id, NEW.target_rfc_revision_id,
           NEW.budget_micro_usd, NEW.created_at)
       IS DISTINCT FROM
       ROW(OLD.id, OLD.application_revision_id, OLD.owner_office_id,
           OLD.project_id, OLD.question, OLD.summary,
           OLD.evaluation_plan_artifact_id,
           OLD.intended_base_commit, OLD.intended_base_tree,
           OLD.target_claim_id, OLD.target_rfc_revision_id,
           OLD.budget_micro_usd, OLD.created_at)
    OR NEW.revision <= OLD.revision THEN
        RAISE EXCEPTION 'experiment identity is immutable and revisions must increase'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION factory.reject_claim_identity_update()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.id, NEW.application_revision_id, NEW.owner_office_id,
           NEW.proposition, NEW.body_artifact_id, NEW.created_at)
       IS DISTINCT FROM
       ROW(OLD.id, OLD.application_revision_id, OLD.owner_office_id,
           OLD.proposition, OLD.body_artifact_id, OLD.created_at)
    OR NEW.revision <= OLD.revision THEN
        RAISE EXCEPTION 'claim identity is immutable and revisions must increase'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION factory.reject_decision_identity_update()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'decisions are immutable' USING ERRCODE = 'check_violation';
END;
$$;

CREATE TRIGGER projects_identity_immutable
    BEFORE UPDATE OR DELETE ON factory.projects
    FOR EACH ROW EXECUTE FUNCTION factory.reject_project_identity_update();
CREATE TRIGGER rfcs_identity_immutable
    BEFORE UPDATE OR DELETE ON factory.rfcs
    FOR EACH ROW EXECUTE FUNCTION factory.reject_rfc_identity_update();
CREATE TRIGGER experiments_identity_immutable
    BEFORE UPDATE OR DELETE ON factory.experiments
    FOR EACH ROW EXECUTE FUNCTION factory.reject_experiment_identity_update();
CREATE TRIGGER claims_immutable
    BEFORE UPDATE OR DELETE ON factory.claims
    FOR EACH ROW EXECUTE FUNCTION factory.reject_claim_identity_update();
CREATE TRIGGER decisions_immutable
    BEFORE UPDATE OR DELETE ON factory.decisions
    FOR EACH ROW EXECUTE FUNCTION factory.reject_decision_identity_update();

CREATE OR REPLACE FUNCTION factory.assert_rfc_current_revision_present()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.current_rfc_revision_id IS NULL THEN
        RAISE EXCEPTION 'RFC % must have a current revision', NEW.id
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE CONSTRAINT TRIGGER rfcs_current_revision_present
    AFTER INSERT OR UPDATE ON factory.rfcs
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION factory.assert_rfc_current_revision_present();

COMMENT ON TABLE factory.projects IS
    'Bounded institutional responsibility area; narrative is a sealed artifact';
COMMENT ON TABLE factory.rfcs IS
    'RFC identity and safe pointer to immutable RFC revisions';
COMMENT ON TABLE factory.rfc_revisions IS
    'Immutable RFC proposal body and searchable bounded summary';
COMMENT ON TABLE factory.experiments IS
    'Bounded institutional question and evaluation plan';
COMMENT ON TABLE factory.experiment_runs IS
    'Append-only execution evidence for one exact experiment plan and base tree';
COMMENT ON TABLE factory.claims IS
    'Immutable institutional proposition; support and challenge are separate facts';
COMMENT ON TABLE factory.decisions IS
    'Immutable authoritative disposition with typed decision targets';
COMMENT ON TABLE factory.project_ticket_links IS
    'Immutable project-to-ticket relation; ticket delivery contract remains separate';
COMMENT ON TABLE factory.ticket_rfc_links IS
    'Immutable ticket-to-RFC relation; an RFC does not rewrite a ticket revision';
