function source(path: string): string {
  return Deno.readTextFileSync(new URL(`../${path}`, import.meta.url));
}

const sql = source("schema/migrations/0006_institutional_records.sql");

Deno.test("institutional schema uses concrete typed relations", () => {
  for (
    const table of [
      "projects",
      "rfcs",
      "rfc_revisions",
      "experiments",
      "experiment_runs",
      "claims",
      "claim_evidence",
      "decisions",
      "decision_targets",
      "project_ticket_links",
      "ticket_rfc_links",
    ]
  ) {
    if (!sql.includes(`CREATE TABLE factory.${table}`)) {
      throw new Error(`missing institutional table: ${table}`);
    }
  }
  if (/institutional_objects|institutional_edges|jsonb|hstore/i.test(sql)) {
    throw new Error("institutional schema must not become a generic metadata graph");
  }
});

Deno.test("institutional bodies and evidence are sealed artifacts", () => {
  for (
    const column of [
      "body_artifact_id",
      "evaluation_plan_artifact_id",
      "invocation_artifact_id",
      "rationale_artifact_id",
    ]
  ) {
    if (!sql.includes(`${column} BIGINT NOT NULL REFERENCES factory.artifacts`)) {
      throw new Error(`missing sealed artifact reference: ${column}`);
    }
  }
  for (const table of ["projects", "rfc_revisions", "experiments", "claims", "decisions"]) {
    if (!sql.includes(`CREATE INDEX ${table}_search_gin`)) {
      throw new Error(`missing bounded text search index: ${table}`);
    }
  }
  if ((sql.match(/GENERATED ALWAYS AS \(/g) ?? []).length < 5) {
    throw new Error("institutional searchable records need generated search vectors");
  }
});

Deno.test("institutional links prove application scope and closed targets", () => {
  for (
    const required of [
      "projects_owner_office_scope_fkey",
      "rfcs_owner_office_scope_fkey",
      "rfc_revisions_rfc_scope_fkey",
      "rfcs_current_revision_scope_fkey",
      "experiments_project_scope_fkey",
      "experiments_target_exactly_one_check",
      "experiment_runs_experiment_scope_fkey",
      "claim_evidence_claim_scope_fkey",
      "claim_evidence_run_scope_fkey",
      "decision_targets_rfc_scope_fkey",
      "decision_targets_ticket_scope_fkey",
      "decision_targets_experiment_scope_fkey",
      "decision_targets_exactly_one_kind_check",
    ]
  ) {
    if (!sql.includes(required)) throw new Error(`missing scope/target contract: ${required}`);
  }
  if (!sql.includes("DEFERRABLE INITIALLY DEFERRED")) {
    throw new Error("RFC current pointer must be transactionally safe");
  }
});

Deno.test("institutional rows and relations are append-only", () => {
  for (
    const trigger of [
      "projects_identity_immutable",
      "rfcs_identity_immutable",
      "rfc_revisions_immutable",
      "experiments_identity_immutable",
      "experiment_runs_immutable",
      "claims_immutable",
      "claim_evidence_immutable",
      "decisions_immutable",
      "decision_targets_immutable",
      "project_ticket_links_immutable",
      "ticket_rfc_links_immutable",
    ]
  ) {
    if (!sql.includes(trigger)) throw new Error(`missing immutability trigger: ${trigger}`);
  }
  if (!sql.includes("rfcs_current_revision_present")) {
    throw new Error("RFCs need a deferred non-null current revision invariant");
  }
});
