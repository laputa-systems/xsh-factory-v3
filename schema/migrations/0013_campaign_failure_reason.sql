-- A Product assignment can fail before a session exists.  Persist its one
-- bounded daemon fault on the terminal campaign, rather than leaving the
-- audit fingerprint as the only (and unqueryable) explanation.
ALTER TABLE factory.campaigns
    ADD COLUMN failure_reason TEXT
        CHECK (failure_reason IS NULL OR octet_length(failure_reason) BETWEEN 1 AND 240);

-- Earlier failed campaigns predate the column and their exact command payload
-- was intentionally never stored.  Preserve their terminal state honestly
-- with a fixed legacy explanation before making the invariant structural.
UPDATE factory.campaigns
   SET failure_reason = 'legacy campaign failure reason unavailable'
 WHERE lifecycle = 2
   AND failure_reason IS NULL;

ALTER TABLE factory.campaigns
    ADD CONSTRAINT campaigns_failure_reason_matches_lifecycle
    CHECK (
        (lifecycle = 2 AND failure_reason IS NOT NULL)
        OR (lifecycle <> 2 AND failure_reason IS NULL)
    );

COMMENT ON SCHEMA factory IS 'factory-v3-schema:initial-authority-v13';
