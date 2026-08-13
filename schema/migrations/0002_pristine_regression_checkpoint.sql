-- A regression checkpoint is deliberately captured before implementation.
-- Its exact tree may therefore equal the attempt's claimed base tree. The
-- candidate tree must still differ from both the base and the checkpoint.

ALTER TABLE factory.candidates
    DROP CONSTRAINT candidates_check1;

ALTER TABLE factory.candidates
    ADD CONSTRAINT candidates_regression_checkpoint_tree_check
    CHECK (base_tree <> candidate_tree AND regression_tree <> candidate_tree);

COMMENT ON SCHEMA factory IS 'factory-v3-schema:authority-v2';
