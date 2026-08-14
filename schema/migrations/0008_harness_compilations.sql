-- A harness compilation records the deterministic bridge from admitted policy
-- and durable context references to the exact actor packet. It is not agent
-- memory and it is not an executable application callback.

ALTER TABLE factory.assignments
    ADD CONSTRAINT assignments_id_application_office_role_unique
        UNIQUE (id, application_revision_id, office_id, assignment_role);

CREATE TABLE factory.harness_compilations (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    assignment_id BIGINT NOT NULL UNIQUE,
    application_revision_id BIGINT NOT NULL,
    office_id BIGINT NOT NULL,
    assignment_role SMALLINT NOT NULL CHECK (assignment_role BETWEEN 0 AND 2),
    compiler_version SMALLINT NOT NULL CHECK (compiler_version = 1),
    spec_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    system_prompt_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    assignment_prompt_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    packet_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    packet_digest BYTEA NOT NULL CHECK (octet_length(packet_digest) = 32),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (id, application_revision_id),
    CONSTRAINT harness_compilations_assignment_scope_fkey
        FOREIGN KEY (assignment_id, application_revision_id, office_id, assignment_role)
        REFERENCES factory.assignments (id, application_revision_id, office_id, assignment_role),
    CONSTRAINT harness_compilations_office_scope_fkey
        FOREIGN KEY (office_id, application_revision_id)
        REFERENCES factory.offices (id, application_revision_id)
);

CREATE INDEX harness_compilations_application_created_index
    ON factory.harness_compilations (application_revision_id, created_at DESC, id DESC);

CREATE TABLE factory.harness_context_items (
    compilation_id BIGINT NOT NULL,
    application_revision_id BIGINT NOT NULL,
    ordinal SMALLINT NOT NULL CHECK (ordinal BETWEEN 0 AND 31),
    inclusion_class SMALLINT NOT NULL CHECK (inclusion_class BETWEEN 0 AND 3),
    reason TEXT NOT NULL CHECK (
        octet_length(reason) BETWEEN 1 AND 512
        -- PostgreSQL TEXT cannot contain NUL; the expression guards the two
        -- remaining line breaks that would make an inclusion reason prompt text.
        AND reason !~ E'[\n\r]'
    ),
    artifact_id BIGINT REFERENCES factory.artifacts (id),
    project_id BIGINT,
    rfc_id BIGINT,
    rfc_revision_id BIGINT,
    ticket_id BIGINT,
    ticket_revision_id BIGINT,
    experiment_id BIGINT,
    claim_id BIGINT,
    decision_id BIGINT,
    office_id BIGINT,
    PRIMARY KEY (compilation_id, ordinal),
    CONSTRAINT harness_context_items_compilation_scope_fkey
        FOREIGN KEY (compilation_id, application_revision_id)
        REFERENCES factory.harness_compilations (id, application_revision_id),
    CONSTRAINT harness_context_items_project_scope_fkey
        FOREIGN KEY (project_id, application_revision_id)
        REFERENCES factory.projects (id, application_revision_id),
    CONSTRAINT harness_context_items_rfc_scope_fkey
        FOREIGN KEY (rfc_id, application_revision_id)
        REFERENCES factory.rfcs (id, application_revision_id),
    CONSTRAINT harness_context_items_rfc_revision_scope_fkey
        FOREIGN KEY (rfc_revision_id, application_revision_id)
        REFERENCES factory.rfc_revisions (id, application_revision_id),
    CONSTRAINT harness_context_items_ticket_scope_fkey
        FOREIGN KEY (ticket_id, application_revision_id)
        REFERENCES factory.tickets (id, application_revision_id),
    CONSTRAINT harness_context_items_ticket_revision_scope_fkey
        FOREIGN KEY (ticket_revision_id, application_revision_id)
        REFERENCES factory.ticket_revisions (id, application_revision_id),
    CONSTRAINT harness_context_items_experiment_scope_fkey
        FOREIGN KEY (experiment_id, application_revision_id)
        REFERENCES factory.experiments (id, application_revision_id),
    CONSTRAINT harness_context_items_claim_scope_fkey
        FOREIGN KEY (claim_id, application_revision_id)
        REFERENCES factory.claims (id, application_revision_id),
    CONSTRAINT harness_context_items_decision_scope_fkey
        FOREIGN KEY (decision_id, application_revision_id)
        REFERENCES factory.decisions (id, application_revision_id),
    CONSTRAINT harness_context_items_office_scope_fkey
        FOREIGN KEY (office_id, application_revision_id)
        REFERENCES factory.offices (id, application_revision_id),
    CONSTRAINT harness_context_items_exactly_one_reference_check CHECK (
        num_nonnulls(
            artifact_id, project_id, rfc_id, rfc_revision_id, ticket_id,
            ticket_revision_id, experiment_id, claim_id, decision_id, office_id
        ) = 1
    )
);

CREATE INDEX harness_context_items_artifact_index
    ON factory.harness_context_items (artifact_id, compilation_id)
    WHERE artifact_id IS NOT NULL;

CREATE TRIGGER harness_compilations_immutable
    BEFORE UPDATE OR DELETE ON factory.harness_compilations
    FOR EACH ROW EXECUTE FUNCTION factory.reject_institutional_row_mutation();

CREATE TRIGGER harness_context_items_immutable
    BEFORE UPDATE OR DELETE ON factory.harness_context_items
    FOR EACH ROW EXECUTE FUNCTION factory.reject_institutional_link_mutation();

COMMENT ON TABLE factory.harness_compilations IS
    'Immutable reproducible assignment harness inputs, outputs, and packet identity';
COMMENT ON TABLE factory.harness_context_items IS
    'Ordered typed durable references selected by one closed harness compiler';
