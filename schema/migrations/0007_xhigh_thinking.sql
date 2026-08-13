-- Pi's closed thinking-level vocabulary includes `xhigh`.  The initial XSH
-- Engineering profile uses a model whose frozen catalog descriptor supports
-- `xhigh` (and `max`) but not `high`, so retain that exact effective setting
-- in assignment/session provenance rather than silently substituting one.

ALTER TABLE factory.assignments
    DROP CONSTRAINT assignments_thinking_level_check,
    ADD CONSTRAINT assignments_thinking_level_check
        CHECK (thinking_level BETWEEN 0 AND 4);

ALTER TABLE factory.sessions
    DROP CONSTRAINT sessions_thinking_level_check,
    ADD CONSTRAINT sessions_thinking_level_check
        CHECK (thinking_level BETWEEN 0 AND 4);

COMMENT ON SCHEMA factory IS 'factory-v3-schema:initial-authority-v7';
