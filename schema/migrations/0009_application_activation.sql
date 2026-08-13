-- An admitted application revision is inert until the Grand Architect
-- explicitly selects it between campaigns.  This stays a pointer on the
-- existing immutable application lineage instead of spending another table
-- from the fixed twenty-relation MVP budget.

ALTER TABLE factory.application_revisions
    ADD COLUMN is_active BOOLEAN NOT NULL DEFAULT FALSE;

CREATE UNIQUE INDEX application_revisions_one_active_per_application
    ON factory.application_revisions (application_key)
    WHERE is_active;

COMMENT ON SCHEMA factory IS 'factory-v3-schema:initial-authority-v9';
