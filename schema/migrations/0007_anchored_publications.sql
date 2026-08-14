-- Anchored publications are the new durable discourse boundary.  Forum rows
-- remain the legacy, read-only discussion projection; this migration does not
-- reinterpret or rewrite them.
--
-- The anchor columns are intentionally concrete nullable foreign keys.  The
-- exactly-one check below is the closed polymorphic boundary: no generic
-- object directory, JSON metadata, or free-form topic identity is admitted.

-- A session provenance reference is only valid when it names the same durable
-- office that authored the publication.  The existing session primary key is
-- sufficient for ordinary reads, while this composite key lets SQL prove the
-- office relationship without a trigger or an untyped lookup.
ALTER TABLE factory.sessions
    ADD CONSTRAINT sessions_id_application_office_unique
        UNIQUE (id, application_revision_id, office_id);

CREATE TABLE factory.publications (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    application_revision_id BIGINT NOT NULL
        REFERENCES factory.application_revisions (id),
    authoring_office_id BIGINT NOT NULL,
    originating_session_id BIGINT,
    publication_kind SMALLINT NOT NULL CHECK (publication_kind BETWEEN 0 AND 5),
    summary TEXT NOT NULL CHECK (
        octet_length(summary) BETWEEN 1 AND 4096
        AND summary !~ E'[\\n\\r\\000]'
    ),
    body_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    project_id BIGINT,
    rfc_id BIGINT,
    rfc_revision_id BIGINT,
    ticket_id BIGINT,
    ticket_revision_id BIGINT,
    experiment_id BIGINT,
    claim_id BIGINT,
    decision_id BIGINT,
    office_id BIGINT,
    reply_to_publication_id BIGINT,
    supersedes_publication_id BIGINT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    search_vector TSVECTOR GENERATED ALWAYS AS (
        to_tsvector('simple', coalesce(summary, ''))
    ) STORED,
    UNIQUE (id, application_revision_id),
    CONSTRAINT publications_authoring_office_scope_fkey
        FOREIGN KEY (authoring_office_id, application_revision_id)
        REFERENCES factory.offices (id, application_revision_id),
    CONSTRAINT publications_originating_session_scope_fkey
        FOREIGN KEY (
            originating_session_id, application_revision_id, authoring_office_id
        )
        REFERENCES factory.sessions (id, application_revision_id, office_id),
    CONSTRAINT publications_project_anchor_scope_fkey
        FOREIGN KEY (project_id, application_revision_id)
        REFERENCES factory.projects (id, application_revision_id),
    CONSTRAINT publications_rfc_anchor_scope_fkey
        FOREIGN KEY (rfc_id, application_revision_id)
        REFERENCES factory.rfcs (id, application_revision_id),
    CONSTRAINT publications_rfc_revision_anchor_scope_fkey
        FOREIGN KEY (rfc_revision_id, application_revision_id)
        REFERENCES factory.rfc_revisions (id, application_revision_id),
    CONSTRAINT publications_ticket_anchor_scope_fkey
        FOREIGN KEY (ticket_id, application_revision_id)
        REFERENCES factory.tickets (id, application_revision_id),
    CONSTRAINT publications_ticket_revision_anchor_scope_fkey
        FOREIGN KEY (ticket_revision_id, application_revision_id)
        REFERENCES factory.ticket_revisions (id, application_revision_id),
    CONSTRAINT publications_experiment_anchor_scope_fkey
        FOREIGN KEY (experiment_id, application_revision_id)
        REFERENCES factory.experiments (id, application_revision_id),
    CONSTRAINT publications_claim_anchor_scope_fkey
        FOREIGN KEY (claim_id, application_revision_id)
        REFERENCES factory.claims (id, application_revision_id),
    CONSTRAINT publications_decision_anchor_scope_fkey
        FOREIGN KEY (decision_id, application_revision_id)
        REFERENCES factory.decisions (id, application_revision_id),
    CONSTRAINT publications_office_anchor_scope_fkey
        FOREIGN KEY (office_id, application_revision_id)
        REFERENCES factory.offices (id, application_revision_id),
    CONSTRAINT publications_reply_scope_fkey
        FOREIGN KEY (reply_to_publication_id, application_revision_id)
        REFERENCES factory.publications (id, application_revision_id),
    CONSTRAINT publications_supersedes_scope_fkey
        FOREIGN KEY (supersedes_publication_id, application_revision_id)
        REFERENCES factory.publications (id, application_revision_id),
    CONSTRAINT publications_exactly_one_anchor_check CHECK (
        num_nonnulls(
            project_id,
            rfc_id,
            rfc_revision_id,
            ticket_id,
            ticket_revision_id,
            experiment_id,
            claim_id,
            decision_id,
            office_id
        ) = 1
    ),
    CONSTRAINT publications_no_self_link_check CHECK (
        (reply_to_publication_id IS NULL OR reply_to_publication_id <> id)
        AND (supersedes_publication_id IS NULL OR supersedes_publication_id <> id)
    )
);

CREATE INDEX publications_application_created_index
    ON factory.publications (application_revision_id, created_at DESC, id DESC);
CREATE INDEX publications_search_gin ON factory.publications USING GIN (search_vector);
CREATE INDEX publications_project_anchor_index
    ON factory.publications (application_revision_id, project_id, id)
    WHERE project_id IS NOT NULL;
CREATE INDEX publications_rfc_anchor_index
    ON factory.publications (application_revision_id, rfc_id, id)
    WHERE rfc_id IS NOT NULL;
CREATE INDEX publications_rfc_revision_anchor_index
    ON factory.publications (application_revision_id, rfc_revision_id, id)
    WHERE rfc_revision_id IS NOT NULL;
CREATE INDEX publications_ticket_anchor_index
    ON factory.publications (application_revision_id, ticket_id, id)
    WHERE ticket_id IS NOT NULL;
CREATE INDEX publications_ticket_revision_anchor_index
    ON factory.publications (application_revision_id, ticket_revision_id, id)
    WHERE ticket_revision_id IS NOT NULL;
CREATE INDEX publications_experiment_anchor_index
    ON factory.publications (application_revision_id, experiment_id, id)
    WHERE experiment_id IS NOT NULL;
CREATE INDEX publications_claim_anchor_index
    ON factory.publications (application_revision_id, claim_id, id)
    WHERE claim_id IS NOT NULL;
CREATE INDEX publications_decision_anchor_index
    ON factory.publications (application_revision_id, decision_id, id)
    WHERE decision_id IS NOT NULL;
CREATE INDEX publications_office_anchor_index
    ON factory.publications (application_revision_id, office_id, id)
    WHERE office_id IS NOT NULL;

-- Attachments retain the Forum quota and label discipline, but now attach to
-- an immutable anchored publication rather than to an unanchored post.
CREATE TABLE factory.publication_attachments (
    publication_id BIGINT NOT NULL,
    application_revision_id BIGINT NOT NULL
        REFERENCES factory.application_revisions (id),
    artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    label TEXT NOT NULL CHECK (octet_length(label) BETWEEN 1 AND 160),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (publication_id, artifact_id),
    CONSTRAINT publication_attachments_publication_scope_fkey
        FOREIGN KEY (publication_id, application_revision_id)
        REFERENCES factory.publications (id, application_revision_id)
);

CREATE INDEX publication_attachments_artifact_index
    ON factory.publication_attachments (artifact_id, publication_id);

CREATE FUNCTION factory.enforce_publication_attachment_quota()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF (
        SELECT count(*)
          FROM factory.publication_attachments
         WHERE publication_id = NEW.publication_id
           AND application_revision_id = NEW.application_revision_id
    ) >= 8 THEN
        RAISE EXCEPTION
            'publication % exceeds the eight-attachment quota',
            NEW.publication_id
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER publication_attachments_count_check
    BEFORE INSERT ON factory.publication_attachments
    FOR EACH ROW EXECUTE FUNCTION factory.enforce_publication_attachment_quota();

CREATE FUNCTION factory.reject_publication_identity_mutation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'publication identities are immutable: %', OLD.id
        USING ERRCODE = 'check_violation';
END;
$$;

CREATE TRIGGER publications_immutable
    BEFORE UPDATE OR DELETE ON factory.publications
    FOR EACH ROW EXECUTE FUNCTION factory.reject_publication_identity_mutation();

CREATE TRIGGER publication_attachments_immutable
    BEFORE UPDATE OR DELETE ON factory.publication_attachments
    FOR EACH ROW EXECUTE FUNCTION factory.reject_institutional_link_mutation();

COMMENT ON TABLE factory.publications IS
    'Immutable anchored discourse identity; legacy Forum rows are not publications';
COMMENT ON TABLE factory.publication_attachments IS
    'Immutable sealed artifacts attached to an anchored publication';
