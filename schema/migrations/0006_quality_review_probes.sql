-- Quality's extra probes are durable review evidence, not an actor claim to
-- be validated then discarded. This extends the existing purpose-specific
-- review fact without increasing the fixed twenty-table MVP budget.

ALTER TABLE factory.reviews
    ADD COLUMN additional_probes_artifact_id BIGINT NOT NULL
        REFERENCES factory.artifacts (id);

COMMENT ON SCHEMA factory IS 'factory-v3-schema:initial-authority-v6';
