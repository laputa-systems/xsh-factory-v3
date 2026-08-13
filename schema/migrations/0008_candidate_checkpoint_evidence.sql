-- The accepted Engineering checkpoint is durable candidate identity.  These
-- columns extend the existing candidates fact instead of adding a checkpoint
-- table, preserving the hard twenty-table MVP ceiling.  All values are
-- kernel-sealed before `candidate.submit`; no actor request can name them.

ALTER TABLE factory.candidates
    ADD COLUMN regression_patch_artifact_id BIGINT NOT NULL
        REFERENCES factory.artifacts (id),
    ADD COLUMN regression_command_set_artifact_id BIGINT NOT NULL
        REFERENCES factory.artifacts (id),
    ADD COLUMN regression_log_artifact_id BIGINT NOT NULL
        REFERENCES factory.artifacts (id);

CREATE OR REPLACE FUNCTION factory.reject_candidate_identity_update()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.ticket_attempt_id, NEW.base_commit, NEW.base_tree, NEW.regression_tree,
           NEW.candidate_tree, NEW.changed_paths_artifact_id,
           NEW.regression_patch_artifact_id, NEW.regression_command_set_artifact_id,
           NEW.regression_log_artifact_id, NEW.patch_artifact_id,
           NEW.engineering_session_id, NEW.engineering_report_artifact_id,
           NEW.commit_subject, NEW.commit_body, NEW.regression_test_identity,
           NEW.risks_artifact_id, NEW.created_at)
       IS DISTINCT FROM
       ROW(OLD.ticket_attempt_id, OLD.base_commit, OLD.base_tree, OLD.regression_tree,
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

COMMENT ON SCHEMA factory IS 'factory-v3-schema:initial-authority-v8';
