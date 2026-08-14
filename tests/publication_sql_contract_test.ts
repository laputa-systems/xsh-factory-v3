function source(path: string): string {
  return Deno.readTextFileSync(new URL(`../${path}`, import.meta.url));
}

const sql = source("schema/migrations/0007_anchored_publications.sql");

Deno.test("publications have one concrete typed anchor", () => {
  if (!sql.includes("CREATE TABLE factory.publications")) {
    throw new Error("missing anchored publication identity relation");
  }
  for (
    const anchor of [
      "project_id",
      "rfc_id",
      "rfc_revision_id",
      "ticket_id",
      "ticket_revision_id",
      "experiment_id",
      "claim_id",
      "decision_id",
      "office_id",
    ]
  ) {
    if (!sql.includes(`publications_${anchor.replace("_id", "")}_anchor_scope_fkey`)) {
      throw new Error(`missing typed publication anchor: ${anchor}`);
    }
  }
  if (!sql.includes("publications_exactly_one_anchor_check")) {
    throw new Error("publication anchor must be exactly one typed relation");
  }
  if (/institutional_objects|institutional_edges|jsonb|hstore/i.test(sql)) {
    throw new Error("publication storage must not become a generic metadata graph");
  }
});

Deno.test("publication bodies, search, and attachments remain bounded", () => {
  for (
    const required of [
      "CREATE TABLE factory.publications",
      "body_artifact_id BIGINT NOT NULL REFERENCES factory.artifacts",
      "publications_search_gin",
      "publication_attachments",
      "publication_attachments_count_check",
      "publication % exceeds the eight-attachment quota",
    ]
  ) {
    if (!sql.includes(required)) throw new Error(`missing publication contract: ${required}`);
  }
  if (
    !sql.includes("publication_kind SMALLINT NOT NULL CHECK (publication_kind BETWEEN 0 AND 5)")
  ) {
    throw new Error("publication kind must remain a closed protocol value");
  }
  if (!sql.includes("summary TEXT NOT NULL CHECK")) {
    throw new Error("publication revisions need a bounded searchable summary");
  }
});

Deno.test("publication provenance and links prove application scope", () => {
  for (
    const required of [
      "publications_authoring_office_scope_fkey",
      "publications_originating_session_scope_fkey",
      "publications_reply_scope_fkey",
      "publications_supersedes_scope_fkey",
      "publication_attachments_publication_scope_fkey",
    ]
  ) {
    if (!sql.includes(required)) throw new Error(`missing publication scope guard: ${required}`);
  }
  if (!sql.includes("sessions_id_application_office_unique")) {
    throw new Error("session provenance must prove its authoring office");
  }
});

Deno.test("publication identities and attachments are append-only", () => {
  for (const trigger of ["publications_immutable", "publication_attachments_immutable"]) {
    if (!sql.includes(trigger)) {
      throw new Error(`missing publication immutability trigger: ${trigger}`);
    }
  }
  if (
    sql.includes("forum_posts") || sql.includes("forum_topics") || sql.includes("forum_threads")
  ) {
    throw new Error("anchored publication migration must preserve Forum without rewriting it");
  }
});
