function source(path: string): string {
  return Deno.readTextFileSync(new URL(`../${path}`, import.meta.url));
}

Deno.test("Canonical schema has four immutable, indexed Forum text owners", () => {
  const sql = source("schema/migrations/0001_initial_authority.sql");
  for (const table of ["forum_topics", "forum_threads", "forum_posts", "forum_attachments"]) {
    if (!sql.includes(`CREATE TABLE factory.${table}`)) throw new Error(`missing ${table}`);
  }
  for (const table of ["topics", "threads", "posts"]) {
    if (!sql.includes(`forum_${table}_search_gin`)) throw new Error(`missing ${table} GIN index`);
  }
  if ((sql.match(/GENERATED ALWAYS AS \(/g) ?? []).length !== 3) {
    throw new Error("each text owner needs one generated search vector");
  }
  for (
    const trigger of [
      "forum_topics_immutable",
      "forum_threads_immutable",
      "forum_posts_immutable",
      "forum_attachments_immutable",
      "forum_posts_relation_check",
    ]
  ) {
    if (!sql.includes(trigger)) throw new Error(`missing ${trigger}`);
  }
  if (/last_activity/i.test(sql)) throw new Error("Forum schema copied a mutable activity column");
});

Deno.test("Forum SQLx search and reads are bounded, order-independent, and read-only", () => {
  const sql = source("crates/factory-kernel/src/forum_store.rs");
  for (
    const required of [
      "websearch_to_tsquery('simple'",
      "ts_headline(",
      "LIMIT $10",
      "best.rank < $8",
      "ORDER BY best.rank DESC, best.post_id ASC",
      "ORDER BY id ASC",
    ]
  ) {
    if (!sql.includes(required)) throw new Error(`missing search/read contract: ${required}`);
  }
  const readStart = sql.indexOf("pub async fn read_thread(");
  const readSection = sql.slice(readStart, sql.indexOf("/// Lists topics", readStart));
  const searchStart = sql.indexOf("pub async fn search(");
  const searchSection = sql.slice(
    searchStart,
    sql.indexOf("/// A bounded read-only diagnostic", searchStart),
  );
  if (
    /INSERT INTO factory\.forum|UPDATE factory\.forum|DELETE FROM factory\.forum/.test(
      readSection,
    ) ||
    /INSERT INTO factory\.forum|UPDATE factory\.forum|DELETE FROM factory\.forum/.test(
      searchSection,
    )
  ) {
    throw new Error("read/search authority contains a Forum write statement");
  }
});
