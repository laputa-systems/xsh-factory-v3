function source(path: string): string {
  return Deno.readTextFileSync(new URL(`../${path}`, import.meta.url));
}

const sql = source("schema/migrations/0008_harness_compilations.sql");

Deno.test("harness compilation is an immutable assignment-owned receipt", () => {
  for (
    const required of [
      "CREATE TABLE factory.harness_compilations",
      "assignment_id BIGINT NOT NULL UNIQUE",
      "compiler_version SMALLINT NOT NULL CHECK (compiler_version = 1)",
      "harness_compilations_assignment_scope_fkey",
      "harness_compilations_office_scope_fkey",
      "harness_compilations_immutable",
    ]
  ) {
    if (!sql.includes(required)) throw new Error(`missing harness receipt contract: ${required}`);
  }
  if (/jsonb|hstore|metadata/i.test(sql)) {
    throw new Error("harness storage must remain typed rather than generic metadata");
  }
});

Deno.test("harness context is bounded, typed, scoped, and append-only", () => {
  for (
    const required of [
      "CREATE TABLE factory.harness_context_items",
      "ordinal SMALLINT NOT NULL CHECK (ordinal BETWEEN 0 AND 31)",
      "inclusion_class SMALLINT NOT NULL CHECK (inclusion_class BETWEEN 0 AND 3)",
      "harness_context_items_exactly_one_reference_check",
      "harness_context_items_compilation_scope_fkey",
      "harness_context_items_ticket_scope_fkey",
      "harness_context_items_office_scope_fkey",
      "harness_context_items_immutable",
    ]
  ) {
    if (!sql.includes(required)) throw new Error(`missing harness context contract: ${required}`);
  }
});
