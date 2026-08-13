-- A blocked ticket preserves durable diagnosis but has no live Engineering
-- attempt. It must not permanently reserve a reproducer after a kernel-side
-- correction changes the canonical observation identity.
ALTER TABLE factory.ticket_revisions
    DROP CONSTRAINT ticket_revisions_application_revision_id_reproducer_artifac_key;

CREATE UNIQUE INDEX ticket_revisions_live_reproducer_identity
    ON factory.ticket_revisions (application_revision_id, reproducer_artifact_id)
    WHERE lifecycle <> 4;

COMMENT ON SCHEMA factory IS 'factory-v3-schema:authority-v3';
