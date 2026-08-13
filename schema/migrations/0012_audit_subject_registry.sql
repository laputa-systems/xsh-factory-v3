-- Audit subject kinds are a closed global registry.  Forum originally reused
-- the process-custody family (4 campaign, 5 assignment, 6 session), making a
-- historical audit receipt ambiguous.  Move only the known Forum operations
-- to their permanent, otherwise-unused family before future writes use it.
UPDATE factory.audit_log
   SET subject_kind = 10
 WHERE subject_kind = 4
   AND operation IN ('forum.topic.create', 'forum.topic.supersede');

UPDATE factory.audit_log
   SET subject_kind = 11
 WHERE subject_kind = 5
   AND operation IN ('forum.thread.create', 'forum.thread.supersede');

UPDATE factory.audit_log
   SET subject_kind = 12
 WHERE subject_kind = 6
   AND operation = 'forum.post.append';

COMMENT ON SCHEMA factory IS 'factory-v3-schema:initial-authority-v12';
