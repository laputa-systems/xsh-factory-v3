-- Factory V3's first and only migration lineage begins with the authority
-- facts needed before paid work is admitted. Later tables arrive with the
-- first transition that needs them; this is intentionally not a placeholder
-- dump of the complete MVP schema.

CREATE SCHEMA factory;

COMMENT ON SCHEMA factory IS 'factory-v3-schema:initial-authority-v4';

CREATE TABLE factory.kernel_builds (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    build_digest BYTEA NOT NULL UNIQUE CHECK (octet_length(build_digest) = 32),
    source_digest BYTEA NOT NULL CHECK (octet_length(source_digest) = 32),
    binary_digest BYTEA NOT NULL CHECK (octet_length(binary_digest) = 32),
    schema_identity TEXT NOT NULL CHECK (octet_length(schema_identity) BETWEEN 1 AND 160),
    deno_executable_path TEXT NOT NULL CHECK (deno_executable_path LIKE '/%' AND octet_length(deno_executable_path) <= 4096),
    deno_version TEXT NOT NULL CHECK (octet_length(deno_version) BETWEEN 1 AND 240),
    deno_lock_digest BYTEA NOT NULL CHECK (octet_length(deno_lock_digest) = 32),
    -- The install transaction inserts the deferred build/artifact pair
    -- together, so this receipt is never durably absent.
    qualification_receipt_artifact_id BIGINT NOT NULL,
    installed_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    is_current BOOLEAN NOT NULL DEFAULT FALSE,
    revision BIGINT NOT NULL CHECK (revision > 0)
);

CREATE UNIQUE INDEX kernel_builds_one_current
    ON factory.kernel_builds (is_current)
    WHERE is_current;

CREATE TABLE factory.artifacts (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    digest BYTEA NOT NULL UNIQUE CHECK (octet_length(digest) = 32),
    byte_length BIGINT NOT NULL CHECK (byte_length >= 0),
    cas_relative_path TEXT NOT NULL UNIQUE CHECK (
        octet_length(cas_relative_path) BETWEEN 1 AND 4096
        AND cas_relative_path NOT LIKE '/%'
        AND cas_relative_path !~ E'\\\\'
        AND cas_relative_path !~ '(^|/)(\\.|\\.\\.)(/|$)'
        AND cas_relative_path !~ '//'
    ),
    creating_kernel_build_id BIGINT NOT NULL
        REFERENCES factory.kernel_builds (id) DEFERRABLE INITIALLY DEFERRED,
    sealed_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

ALTER TABLE factory.kernel_builds
    ADD CONSTRAINT kernel_builds_qualification_receipt_artifact_id_fkey
    FOREIGN KEY (qualification_receipt_artifact_id)
    REFERENCES factory.artifacts (id) DEFERRABLE INITIALLY DEFERRED;

CREATE TABLE factory.repositories (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repository_key TEXT NOT NULL UNIQUE CHECK (octet_length(repository_key) BETWEEN 1 AND 160),
    canonical_local_path TEXT NOT NULL UNIQUE CHECK (canonical_local_path LIKE '/%' AND octet_length(canonical_local_path) <= 4096),
    default_branch TEXT NOT NULL CHECK (octet_length(default_branch) BETWEEN 1 AND 240),
    delivery_mode SMALLINT NOT NULL CHECK (delivery_mode = 0),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE factory.application_revisions (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    application_key TEXT NOT NULL CHECK (octet_length(application_key) BETWEEN 1 AND 80),
    aggregate_revision BIGINT NOT NULL CHECK (aggregate_revision > 0),
    predecessor_application_revision_id BIGINT REFERENCES factory.application_revisions (id),
    bundle_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    mission_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    product_research_system_template_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    product_research_assignment_template_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    engineering_system_template_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    engineering_assignment_template_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    quality_system_template_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    quality_assignment_template_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts (id),
    repository_id BIGINT NOT NULL REFERENCES factory.repositories (id),
    ticket_low_water INTEGER NOT NULL CHECK (ticket_low_water > 0),
    ticket_target INTEGER NOT NULL CHECK (ticket_target >= ticket_low_water),
    ticket_maximum INTEGER NOT NULL CHECK (ticket_maximum >= ticket_target),
    proposal_maximum INTEGER NOT NULL CHECK (proposal_maximum > 0),
    ticket_narrative_byte_limit INTEGER NOT NULL CHECK (ticket_narrative_byte_limit > 0),
    ticket_acceptance_criteria_limit INTEGER NOT NULL CHECK (ticket_acceptance_criteria_limit > 0),
    ticket_contract_read_limit INTEGER NOT NULL CHECK (ticket_contract_read_limit > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (application_key, aggregate_revision)
);

CREATE TABLE factory.campaigns (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kernel_build_id BIGINT NOT NULL REFERENCES factory.kernel_builds (id),
    application_revision_id BIGINT NOT NULL REFERENCES factory.application_revisions (id),
    repository_id BIGINT NOT NULL REFERENCES factory.repositories (id),
    lifecycle SMALLINT NOT NULL CHECK (lifecycle BETWEEN 0 AND 3),
    aggregate_budget_micro_usd BIGINT NOT NULL CHECK (aggregate_budget_micro_usd >= 0),
    deadline TIMESTAMPTZ NOT NULL,
    delivery_target INTEGER NOT NULL CHECK (delivery_target > 0),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX campaigns_one_running
    ON factory.campaigns (lifecycle)
    WHERE lifecycle = 0;

CREATE TABLE factory.audit_log (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    principal TEXT NOT NULL CHECK (octet_length(principal) BETWEEN 1 AND 160),
    command_id TEXT NOT NULL CHECK (octet_length(command_id) BETWEEN 1 AND 160),
    operation TEXT NOT NULL CHECK (octet_length(operation) BETWEEN 1 AND 160),
    command_fingerprint BYTEA NOT NULL CHECK (octet_length(command_fingerprint) = 32),
    subject_kind SMALLINT NOT NULL CHECK (subject_kind BETWEEN 0 AND 255),
    subject_id BIGINT NOT NULL CHECK (subject_id > 0),
    resulting_revision BIGINT NOT NULL CHECK (resulting_revision >= 0),
    accepted_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (principal, command_id)
);

CREATE INDEX audit_log_subject_index ON factory.audit_log (subject_kind, subject_id, id);
