-- Forum records are immutable application communication facts. This first
-- Forum transition stages its tables after the authority migration has already
-- supplied artifacts and audit receipts. Mutating handlers append exactly one
-- audit receipt in their transaction; read paths do not write.

CREATE TABLE factory.forum_topics (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    author_kind SMALLINT NOT NULL CHECK (author_kind IN (0, 1)),
    author_session_id BIGINT,
    author_office SMALLINT,
    -- The framed protocol rejects NUL before SQL; PostgreSQL text itself
    -- cannot represent NUL in a UTF-8 server encoding.
    name TEXT NOT NULL CHECK (octet_length(name) BETWEEN 1 AND 160),
    description TEXT NOT NULL CHECK (octet_length(description) <= 4096),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    supersedes_topic_id BIGINT REFERENCES factory.forum_topics (id),
    search_vector TSVECTOR GENERATED ALWAYS AS (
        to_tsvector('simple', coalesce(name, '') || ' ' || coalesce(description, ''))
    ) STORED,
    CHECK (
        (author_kind = 0 AND author_session_id IS NOT NULL AND author_office IS NOT NULL)
        OR (author_kind = 1 AND author_session_id IS NULL AND author_office IS NULL)
    ),
    CHECK (supersedes_topic_id IS NULL OR supersedes_topic_id <> id)
);

CREATE INDEX forum_topics_search_gin ON factory.forum_topics USING GIN (search_vector);
CREATE INDEX forum_topics_recent_index ON factory.forum_topics (created_at DESC, id DESC);

CREATE TABLE factory.forum_threads (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    topic_id BIGINT NOT NULL REFERENCES factory.forum_topics (id),
    author_kind SMALLINT NOT NULL CHECK (author_kind IN (0, 1)),
    author_session_id BIGINT,
    author_office SMALLINT,
    title TEXT NOT NULL CHECK (octet_length(title) BETWEEN 1 AND 240),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    supersedes_thread_id BIGINT REFERENCES factory.forum_threads (id),
    search_vector TSVECTOR GENERATED ALWAYS AS (
        to_tsvector('simple', coalesce(title, ''))
    ) STORED,
    CHECK (
        (author_kind = 0 AND author_session_id IS NOT NULL AND author_office IS NOT NULL)
        OR (author_kind = 1 AND author_session_id IS NULL AND author_office IS NULL)
    ),
    CHECK (supersedes_thread_id IS NULL OR supersedes_thread_id <> id)
);

CREATE INDEX forum_threads_search_gin ON factory.forum_threads USING GIN (search_vector);
CREATE INDEX forum_threads_topic_index ON factory.forum_threads (topic_id, id);
CREATE INDEX forum_threads_recent_index ON factory.forum_threads (created_at DESC, id DESC);

CREATE TABLE factory.forum_posts (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    thread_id BIGINT NOT NULL REFERENCES factory.forum_threads (id),
    author_kind SMALLINT NOT NULL CHECK (author_kind IN (0, 1)),
    author_session_id BIGINT,
    author_office SMALLINT,
    body TEXT NOT NULL CHECK (octet_length(body) <= 16384),
    kind SMALLINT NOT NULL CHECK (kind BETWEEN 0 AND 6),
    reply_to_post_id BIGINT REFERENCES factory.forum_posts (id),
    supersedes_post_id BIGINT REFERENCES factory.forum_posts (id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    search_vector TSVECTOR GENERATED ALWAYS AS (
        to_tsvector('simple', coalesce(body, ''))
    ) STORED,
    CHECK (
        (author_kind = 0 AND author_session_id IS NOT NULL AND author_office IS NOT NULL)
        OR (author_kind = 1 AND author_session_id IS NULL AND author_office IS NULL)
    ),
    CHECK (
        reply_to_post_id IS NULL OR supersedes_post_id IS NULL
        OR reply_to_post_id <> supersedes_post_id
    )
);

CREATE INDEX forum_posts_search_gin ON factory.forum_posts USING GIN (search_vector);
CREATE INDEX forum_posts_thread_order_index ON factory.forum_posts (thread_id, id);
CREATE INDEX forum_posts_author_index ON factory.forum_posts (author_office, id);
CREATE INDEX forum_posts_created_index ON factory.forum_posts (created_at, id);

CREATE TABLE factory.forum_attachments (
    post_id BIGINT NOT NULL REFERENCES factory.forum_posts (id),
    artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    label TEXT NOT NULL CHECK (octet_length(label) <= 160),
    PRIMARY KEY (post_id, artifact_id)
);

CREATE INDEX forum_attachments_artifact_index ON factory.forum_attachments (artifact_id, post_id);

CREATE FUNCTION factory.forum_enforce_attachment_quota()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF (SELECT count(*) FROM factory.forum_attachments WHERE post_id = NEW.post_id) >= 8 THEN
        RAISE EXCEPTION 'Forum post % exceeds the eight-attachment quota', NEW.post_id
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER forum_attachments_count_check
    BEFORE INSERT ON factory.forum_attachments
    FOR EACH ROW EXECUTE FUNCTION factory.forum_enforce_attachment_quota();

-- PostgreSQL CHECK constraints cannot inspect a target row. These triggers
-- enforce cross-row immutability and relation contracts atomically.
CREATE FUNCTION factory.forum_reject_immutable_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'Forum rows are immutable: % %', TG_TABLE_NAME, OLD.id
        USING ERRCODE = 'check_violation';
END;
$$;

CREATE TRIGGER forum_topics_immutable
    BEFORE UPDATE OR DELETE ON factory.forum_topics
    FOR EACH ROW EXECUTE FUNCTION factory.forum_reject_immutable_update();
CREATE TRIGGER forum_threads_immutable
    BEFORE UPDATE OR DELETE ON factory.forum_threads
    FOR EACH ROW EXECUTE FUNCTION factory.forum_reject_immutable_update();
CREATE TRIGGER forum_posts_immutable
    BEFORE UPDATE OR DELETE ON factory.forum_posts
    FOR EACH ROW EXECUTE FUNCTION factory.forum_reject_immutable_update();

CREATE FUNCTION factory.forum_reject_attachment_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'Forum attachment relations are immutable: post %, artifact %', OLD.post_id, OLD.artifact_id
        USING ERRCODE = 'check_violation';
END;
$$;

CREATE TRIGGER forum_attachments_immutable
    BEFORE UPDATE OR DELETE ON factory.forum_attachments
    FOR EACH ROW EXECUTE FUNCTION factory.forum_reject_attachment_update();

CREATE FUNCTION factory.forum_validate_post_relations()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    target_thread BIGINT;
BEGIN
    IF NEW.reply_to_post_id IS NOT NULL THEN
        SELECT thread_id INTO target_thread FROM factory.forum_posts WHERE id = NEW.reply_to_post_id;
        IF target_thread IS NULL THEN
            RAISE EXCEPTION 'reply target % does not exist', NEW.reply_to_post_id USING ERRCODE = 'foreign_key_violation';
        END IF;
        IF target_thread <> NEW.thread_id OR NEW.reply_to_post_id >= NEW.id THEN
            RAISE EXCEPTION 'reply target must be earlier and in the same thread' USING ERRCODE = 'check_violation';
        END IF;
    END IF;
    IF NEW.supersedes_post_id IS NOT NULL THEN
        SELECT thread_id INTO target_thread FROM factory.forum_posts WHERE id = NEW.supersedes_post_id;
        IF target_thread IS NULL THEN
            RAISE EXCEPTION 'supersession target % does not exist', NEW.supersedes_post_id USING ERRCODE = 'foreign_key_violation';
        END IF;
        IF target_thread <> NEW.thread_id OR NEW.supersedes_post_id >= NEW.id THEN
            RAISE EXCEPTION 'supersession target must be earlier and in the same thread' USING ERRCODE = 'check_violation';
        END IF;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER forum_posts_relation_check
    BEFORE INSERT ON factory.forum_posts
    FOR EACH ROW EXECUTE FUNCTION factory.forum_validate_post_relations();

COMMENT ON SCHEMA factory IS 'factory-v3-schema:initial-authority-v4';
