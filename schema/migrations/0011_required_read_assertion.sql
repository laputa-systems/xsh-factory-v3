-- Session terminal evidence is an actor-read assertion, not the immutable
-- required-read manifest carried by its assignment. The original name made
-- two intentionally distinct artifact identities look interchangeable.
ALTER TABLE factory.sessions
    RENAME COLUMN required_read_manifest_artifact_id
    TO required_read_assertion_artifact_id;

COMMENT ON SCHEMA factory IS 'factory-v3-schema:initial-authority-v11';
