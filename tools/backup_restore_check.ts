/**
 * One explicit, provider-free backup/restore qualification.
 *
 * The caller creates both databases and the empty restore runtime root before
 * this process starts. This script never creates or drops a database, starts
 * a daemon, contacts a provider, or changes the source runtime root. It pairs
 * a PostgreSQL custom dump with a byte-preserving copy of the append-only CAS
 * object tree, restores only into the exact blank clone, then invokes the
 * kernel's restore-integrity and repaired clone-corruption judge.
 */

export type Arguments = Readonly<Record<string, string>>;

if (import.meta.main) {
  const argumentsByName = parseArguments(Deno.args);
  await main({
    sourceDatabaseUrl: required(argumentsByName, "source-database-url"),
    sourceRuntimeRoot: absolutePath(required(argumentsByName, "source-runtime-root")),
    restoreDatabaseUrl: required(argumentsByName, "restore-database-url"),
    restoreRuntimeRoot: absolutePath(required(argumentsByName, "restore-runtime-root")),
    dumpFile: absolutePath(required(argumentsByName, "dump-file")),
    pgDump: executable(required(argumentsByName, "pg-dump")),
    pgRestore: executable(required(argumentsByName, "pg-restore")),
    psql: executable(required(argumentsByName, "psql")),
    cargo: executable(required(argumentsByName, "cargo")),
  });
}

interface BackupRestoreArguments {
  readonly sourceDatabaseUrl: string;
  readonly sourceRuntimeRoot: string;
  readonly restoreDatabaseUrl: string;
  readonly restoreRuntimeRoot: string;
  readonly dumpFile: string;
  readonly pgDump: string;
  readonly pgRestore: string;
  readonly psql: string;
  readonly cargo: string;
}

async function main({
  sourceDatabaseUrl,
  sourceRuntimeRoot,
  restoreDatabaseUrl,
  restoreRuntimeRoot,
  dumpFile,
  pgDump,
  pgRestore,
  psql,
  cargo,
}: BackupRestoreArguments): Promise<void> {
  const sourceDatabaseName = databaseName(sourceDatabaseUrl, "source-database-url");
  const restoreDatabaseName = databaseName(restoreDatabaseUrl, "restore-database-url");
  if (!/^factory_restore_v3_[0-9]+$/.test(restoreDatabaseName)) {
    fail("restore-database-url must name exactly factory_restore_v3_<digits>");
  }
  if (sameDatabaseTarget(sourceDatabaseUrl, restoreDatabaseUrl)) {
    fail("source and restore databases must use distinct PostgreSQL host/port/name targets");
  }
  await requireDirectory(sourceRuntimeRoot, "source-runtime-root");
  await requireEmptyDirectory(restoreRuntimeRoot, "restore-runtime-root");
  await requireMissingPath(dumpFile, "dump-file");
  const canonicalSourceRuntimeRoot = await Deno.realPath(sourceRuntimeRoot);
  const canonicalRestoreRuntimeRoot = await Deno.realPath(restoreRuntimeRoot);
  if (
    isWithin(canonicalSourceRuntimeRoot, canonicalRestoreRuntimeRoot) ||
    isWithin(canonicalRestoreRuntimeRoot, canonicalSourceRuntimeRoot)
  ) {
    fail("source and restore runtime roots must be distinct and non-overlapping");
  }
  const canonicalDumpParent = await Deno.realPath(parentPath(dumpFile));
  const canonicalDumpFile = `${canonicalDumpParent}/${baseName(dumpFile)}`;
  if (
    isWithin(canonicalDumpFile, canonicalSourceRuntimeRoot) ||
    isWithin(canonicalDumpFile, canonicalRestoreRuntimeRoot)
  ) {
    fail("dump-file must be outside both runtime roots");
  }
  const sourceObjects = `${sourceRuntimeRoot}/objects`;
  await requireDirectory(sourceObjects, "source runtime CAS objects");
  await requireBlankRestoreDatabase(psql, restoreDatabaseUrl);

  const sourceDatabaseBefore = await databaseFingerprint(psql, sourceDatabaseUrl);
  const sourceCasBefore = await treeFingerprint(sourceObjects);
  await run(pgDump, ["--format=custom", "--file", dumpFile, "--dbname", sourceDatabaseUrl]);
  await copyTree(sourceObjects, `${restoreRuntimeRoot}/objects`);
  const restoreObjects = `${restoreRuntimeRoot}/objects`;
  const cloneBeforeIntegrity = await treeFingerprint(restoreObjects);
  if (cloneBeforeIntegrity !== sourceCasBefore) {
    fail("restored CAS fingerprint differs from the source CAS fingerprint");
  }

  await run(pgRestore, [
    "--exit-on-error",
    "--no-owner",
    "--no-privileges",
    "--dbname",
    restoreDatabaseUrl,
    dumpFile,
  ]);
  const cloneDatabaseBeforeIntegrity = await databaseFingerprint(psql, restoreDatabaseUrl);
  if (cloneDatabaseBeforeIntegrity !== sourceDatabaseBefore) {
    fail("restored database logical fingerprint differs from the source database");
  }
  await run(cargo, [
    "test",
    "-p",
    "factory-kernel",
    "--test",
    "backup_restore",
    "restored_database_and_cas_are_integrity_qualified",
    "--",
    "--ignored",
    "--test-threads=1",
  ], {
    DATABASE_URL: restoreDatabaseUrl,
    FACTORY_RESTORE_DATABASE_URL: restoreDatabaseUrl,
    FACTORY_RESTORE_RUNTIME_ROOT: restoreRuntimeRoot,
  });

  const sourceDatabaseAfter = await databaseFingerprint(psql, sourceDatabaseUrl);
  const sourceCasAfter = await treeFingerprint(sourceObjects);
  const cloneDatabaseAfterIntegrity = await databaseFingerprint(psql, restoreDatabaseUrl);
  const cloneAfterIntegrity = await treeFingerprint(restoreObjects);
  if (sourceDatabaseAfter !== sourceDatabaseBefore) {
    fail("source database fingerprint changed during backup/restore qualification");
  }
  if (sourceCasAfter !== sourceCasBefore) {
    fail("source CAS fingerprint changed during backup/restore qualification");
  }
  if (cloneDatabaseAfterIntegrity !== cloneDatabaseBeforeIntegrity) {
    fail("restore database logical fingerprint changed during integrity qualification");
  }
  if (cloneAfterIntegrity !== sourceCasBefore) {
    fail("restore CAS fingerprint changed during integrity corruption probes");
  }
  console.log(
    `backup/restore qualification passed for source ${sourceDatabaseName} and clone ${restoreDatabaseName}`,
  );
}

export function parseArguments(values: readonly string[]): Arguments {
  const result: Record<string, string> = {};
  for (let index = 0; index < values.length; index += 2) {
    const flag = values[index];
    const value = values[index + 1];
    if (
      flag === undefined || value === undefined || !flag.startsWith("--") || value.startsWith("--")
    ) {
      usage();
    }
    const name = flag.slice(2);
    if (!/^[a-z]+(?:-[a-z]+)*$/.test(name) || name in result) usage();
    result[name] = value;
  }
  return result;
}

function required(values: Arguments, name: string): string {
  const value = values[name];
  if (value === undefined || value.length === 0 || value.includes("\0")) {
    fail(`--${name} is required`);
  }
  return value;
}

export function absolutePath(value: string): string {
  if (!value.startsWith("/")) fail("filesystem paths must be absolute");
  if (value.split("/").some((component) => component === "." || component === "..")) {
    fail("filesystem paths must not contain dot components");
  }
  return value === "/" ? value : value.replace(/\/+$/, "");
}

function executable(value: string): string {
  return absolutePath(value);
}

function databaseName(value: string, argumentName: string): string {
  let parsed: URL;
  try {
    parsed = new URL(value);
  } catch {
    fail(`--${argumentName} must be a PostgreSQL URL`);
  }
  if (parsed.protocol !== "postgresql:" && parsed.protocol !== "postgres:") {
    fail(`--${argumentName} must use postgresql:// or postgres://`);
  }
  const name = decodeURIComponent(parsed.pathname).replace(/^\//, "");
  if (name.length === 0 || name.includes("/")) fail(`--${argumentName} must name one database`);
  return name;
}

export function sameDatabaseTarget(left: string, right: string): boolean {
  return databaseTarget(left, "source-database-url") ===
    databaseTarget(right, "restore-database-url");
}

export function databaseTarget(value: string, argumentName: string): string {
  let parsed: URL;
  try {
    parsed = new URL(value);
  } catch {
    fail(`--${argumentName} must be a PostgreSQL URL`);
  }
  if (parsed.protocol !== "postgresql:" && parsed.protocol !== "postgres:") {
    fail(`--${argumentName} must use postgresql:// or postgres://`);
  }
  const host = decodeURIComponent(parsed.hostname).toLowerCase();
  if (host.length === 0) fail(`--${argumentName} must include a PostgreSQL host`);
  const port = parsed.port.length === 0 ? "5432" : parsed.port;
  return `${host}\0${port}\0${databaseName(value, argumentName)}`;
}

async function requireDirectory(path: string, label: string): Promise<void> {
  let information: Deno.FileInfo;
  try {
    information = await Deno.lstat(path);
  } catch {
    fail(`${label} must be an existing directory`);
  }
  if (!information.isDirectory || information.isSymlink) fail(`${label} must be a real directory`);
}

async function requireEmptyDirectory(path: string, label: string): Promise<void> {
  await requireDirectory(path, label);
  for await (const _entry of Deno.readDir(path)) {
    fail(`${label} must be empty before restore`);
  }
}

async function requireMissingPath(path: string, label: string): Promise<void> {
  try {
    await Deno.lstat(path);
  } catch (error) {
    if (error instanceof Deno.errors.NotFound) return;
    throw error;
  }
  fail(`${label} must not already exist`);
}

async function requireBlankRestoreDatabase(psqlPath: string, databaseUrl: string): Promise<void> {
  const output = await commandOutput(psqlPath, [
    "--dbname",
    databaseUrl,
    "--no-psqlrc",
    "--no-align",
    "--tuples-only",
    "--set",
    "ON_ERROR_STOP=1",
    "--command",
    `SELECT NOT EXISTS (
       SELECT 1
       FROM pg_namespace AS namespace
       WHERE namespace.nspname NOT IN ('pg_catalog', 'information_schema', 'public')
         AND namespace.nspname !~ '^pg_'
       UNION ALL
       SELECT 1
       FROM pg_class AS relation
       JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
       WHERE namespace.nspname NOT IN ('pg_catalog', 'information_schema')
         AND namespace.nspname !~ '^pg_'
         AND relation.relkind IN ('r', 'p', 'v', 'm', 'S', 'f')
       UNION ALL
       SELECT 1
       FROM pg_type AS type
       JOIN pg_namespace AS namespace ON namespace.oid = type.typnamespace
       WHERE namespace.nspname NOT IN ('pg_catalog', 'information_schema')
         AND namespace.nspname !~ '^pg_'
         AND type.typtype IN ('b', 'c', 'd', 'e', 'm', 'r')
       UNION ALL
       SELECT 1
       FROM pg_proc AS procedure
       JOIN pg_namespace AS namespace ON namespace.oid = procedure.pronamespace
       WHERE namespace.nspname NOT IN ('pg_catalog', 'information_schema')
         AND namespace.nspname !~ '^pg_'
     ) AS blank`,
  ]);
  if (new TextDecoder().decode(output.stdout).trim() !== "t") {
    fail("restore database must be blank: user schema objects are present");
  }
}

async function databaseFingerprint(psqlPath: string, databaseUrl: string): Promise<string> {
  const parts: Uint8Array[] = [];
  appendFingerprintPart(parts, "format", new TextEncoder().encode("logical-database-v2"));

  // This deliberately records logical catalog shape, not deparsed CHECK/index
  // expressions: PostgreSQL may remove redundant parse-tree parentheses while
  // restoring semantically identical DDL. The authoritative schema identity
  // and SQLx migration rows below retain the exact admitted migration lineage.
  appendFingerprintPart(
    parts,
    "schema",
    await psqlQuery(psqlPath, databaseUrl, stableSchemaShapeQuery()),
  );

  const relations = await catalogObjects(
    psqlPath,
    databaseUrl,
    `SELECT json_build_array(namespace.nspname, relation.relname)::TEXT
       FROM pg_class AS relation
       JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
      WHERE namespace.nspname NOT IN ('pg_catalog', 'information_schema')
        AND namespace.nspname !~ '^pg_toast'
        AND relation.relkind IN ('r', 'p', 'm')
      ORDER BY namespace.nspname COLLATE "C", relation.relname COLLATE "C"`,
  );
  for (const [schema, relation] of relations) {
    const qualifiedRelation = `${quoteIdentifier(schema)}.${quoteIdentifier(relation)}`;
    appendFingerprintPart(
      parts,
      `rows:${schema}.${relation}`,
      await psqlQuery(
        psqlPath,
        databaseUrl,
        `SELECT to_jsonb(logical_row)::TEXT
           FROM ${qualifiedRelation} AS logical_row
          ORDER BY to_jsonb(logical_row)::TEXT COLLATE "C"`,
      ),
    );
  }

  const sequences = await catalogObjects(
    psqlPath,
    databaseUrl,
    `SELECT json_build_array(namespace.nspname, relation.relname)::TEXT
       FROM pg_class AS relation
       JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
      WHERE namespace.nspname NOT IN ('pg_catalog', 'information_schema')
        AND namespace.nspname !~ '^pg_toast'
        AND relation.relkind = 'S'
      ORDER BY namespace.nspname COLLATE "C", relation.relname COLLATE "C"`,
  );
  for (const [schema, sequence] of sequences) {
    appendFingerprintPart(
      parts,
      `sequence:${schema}.${sequence}`,
      await psqlQuery(
        psqlPath,
        databaseUrl,
        `SELECT json_build_array(last_value, is_called)::TEXT
           FROM ${quoteIdentifier(schema)}.${quoteIdentifier(sequence)}`,
      ),
    );
  }

  const length = parts.reduce((sum, part) => sum + part.length, 0);
  const logicalBytes = new Uint8Array(length);
  let offset = 0;
  for (const part of parts) {
    logicalBytes.set(part, offset);
    offset += part.length;
  }
  return hex(await crypto.subtle.digest("SHA-256", logicalBytes));
}

function stableSchemaShapeQuery(): string {
  return `WITH user_namespaces AS (
  SELECT oid, nspname
    FROM pg_namespace
   WHERE nspname NOT IN ('pg_catalog', 'information_schema')
     AND nspname !~ '^pg_toast'
), schema_items AS (
  SELECT 'namespace'::TEXT AS kind, namespace.nspname AS namespace,
         ''::TEXT AS owner_name, ''::TEXT AS item_name,
         coalesce(obj_description(namespace.oid, 'pg_namespace'), '') AS shape
    FROM user_namespaces AS namespace
  UNION ALL
  SELECT 'relation', namespace.nspname, relation.relname, '',
         concat_ws(':', relation.relkind::TEXT, relation.relpersistence::TEXT,
                   relation.relreplident::TEXT, relation.relrowsecurity::TEXT,
                   relation.relforcerowsecurity::TEXT, relation.relnatts::TEXT,
                   relation.relchecks::TEXT)
    FROM pg_class AS relation
    JOIN user_namespaces AS namespace ON namespace.oid = relation.relnamespace
   WHERE relation.relkind IN ('r', 'p', 'v', 'm', 'S', 'f')
  UNION ALL
  SELECT 'column', namespace.nspname, relation.relname,
         attribute.attnum::TEXT || ':' || attribute.attname,
         concat_ws(':', format_type(attribute.atttypid, attribute.atttypmod),
                   attribute.attnotnull::TEXT, attribute.attidentity::TEXT,
                   attribute.attgenerated::TEXT,
                   coalesce(collation_namespace.nspname || '.' || collation_row.collname, ''),
                   (default_value.oid IS NOT NULL)::TEXT)
    FROM pg_class AS relation
    JOIN user_namespaces AS namespace ON namespace.oid = relation.relnamespace
    JOIN pg_attribute AS attribute ON attribute.attrelid = relation.oid
    LEFT JOIN pg_attrdef AS default_value
      ON default_value.adrelid = relation.oid AND default_value.adnum = attribute.attnum
    LEFT JOIN pg_collation AS collation_row ON collation_row.oid = attribute.attcollation
    LEFT JOIN pg_namespace AS collation_namespace
      ON collation_namespace.oid = collation_row.collnamespace
   WHERE relation.relkind IN ('r', 'p', 'v', 'm', 'f')
     AND attribute.attnum > 0
     AND NOT attribute.attisdropped
  UNION ALL
  SELECT 'constraint', namespace.nspname, relation.relname, constraint_row.conname,
         concat_ws(':', constraint_row.contype::TEXT, constraint_row.condeferrable::TEXT,
                   constraint_row.condeferred::TEXT, constraint_row.convalidated::TEXT,
                   constraint_row.connoinherit::TEXT, constraint_row.conkey::TEXT,
                   constraint_row.confkey::TEXT,
                   coalesce(referenced_namespace.nspname || '.' || referenced.relname, ''))
    FROM pg_constraint AS constraint_row
    JOIN pg_class AS relation ON relation.oid = constraint_row.conrelid
    JOIN user_namespaces AS namespace ON namespace.oid = relation.relnamespace
    LEFT JOIN pg_class AS referenced ON referenced.oid = constraint_row.confrelid
    LEFT JOIN pg_namespace AS referenced_namespace
      ON referenced_namespace.oid = referenced.relnamespace
  UNION ALL
  SELECT 'index', namespace.nspname, indexed_relation.relname, index_relation.relname,
         concat_ws(':', index_row.indisunique::TEXT, index_row.indisprimary::TEXT,
                   index_row.indisexclusion::TEXT, index_row.indimmediate::TEXT,
                   index_row.indisclustered::TEXT, index_row.indisvalid::TEXT,
                   index_row.indisready::TEXT, index_row.indislive::TEXT,
                   index_row.indisreplident::TEXT, index_row.indnullsnotdistinct::TEXT,
                   index_row.indnatts::TEXT, index_row.indnkeyatts::TEXT,
                   index_row.indkey::TEXT, index_row.indoption::TEXT,
                   (index_row.indexprs IS NOT NULL)::TEXT,
                   (index_row.indpred IS NOT NULL)::TEXT)
    FROM pg_index AS index_row
    JOIN pg_class AS index_relation ON index_relation.oid = index_row.indexrelid
    JOIN pg_class AS indexed_relation ON indexed_relation.oid = index_row.indrelid
    JOIN user_namespaces AS namespace ON namespace.oid = indexed_relation.relnamespace
  UNION ALL
  SELECT 'function', namespace.nspname, procedure.proname,
         pg_get_function_identity_arguments(procedure.oid),
         concat_ws(':', procedure.prokind::TEXT, language.lanname,
                   procedure.provolatile::TEXT, procedure.proparallel::TEXT,
                   procedure.proisstrict::TEXT, procedure.prosecdef::TEXT,
                   procedure.proleakproof::TEXT, procedure.prosrc)
    FROM pg_proc AS procedure
    JOIN user_namespaces AS namespace ON namespace.oid = procedure.pronamespace
    JOIN pg_language AS language ON language.oid = procedure.prolang
  UNION ALL
  SELECT 'trigger', namespace.nspname, relation.relname, trigger.tgname,
         concat_ws(':', trigger.tgtype::TEXT, trigger.tgenabled::TEXT,
                   function_namespace.nspname, procedure.proname,
                   pg_get_function_identity_arguments(procedure.oid),
                   encode(trigger.tgargs, 'hex'), (trigger.tgqual IS NOT NULL)::TEXT)
    FROM pg_trigger AS trigger
    JOIN pg_class AS relation ON relation.oid = trigger.tgrelid
    JOIN user_namespaces AS namespace ON namespace.oid = relation.relnamespace
    JOIN pg_proc AS procedure ON procedure.oid = trigger.tgfoid
    JOIN pg_namespace AS function_namespace ON function_namespace.oid = procedure.pronamespace
   WHERE NOT trigger.tgisinternal
  UNION ALL
  SELECT 'sequence', namespace.nspname, relation.relname, '',
         concat_ws(':', format_type(sequence.seqtypid, NULL), sequence.seqstart::TEXT,
                   sequence.seqincrement::TEXT, sequence.seqmax::TEXT,
                   sequence.seqmin::TEXT, sequence.seqcache::TEXT,
                   sequence.seqcycle::TEXT)
    FROM pg_sequence AS sequence
    JOIN pg_class AS relation ON relation.oid = sequence.seqrelid
    JOIN user_namespaces AS namespace ON namespace.oid = relation.relnamespace
)
SELECT json_build_array(kind, namespace, owner_name, item_name, shape)::TEXT
  FROM schema_items
 ORDER BY kind COLLATE "C", namespace COLLATE "C", owner_name COLLATE "C",
          item_name COLLATE "C", shape COLLATE "C"`;
}

async function catalogObjects(
  psqlPath: string,
  databaseUrl: string,
  query: string,
): Promise<readonly [string, string][]> {
  const output = new TextDecoder().decode(await psqlQuery(psqlPath, databaseUrl, query)).trim();
  if (output.length === 0) return [];
  return output.split("\n").map((line) => {
    const value: unknown = JSON.parse(line);
    if (
      !Array.isArray(value) || value.length !== 2 ||
      value.some((part) => typeof part !== "string")
    ) {
      fail("PostgreSQL catalog returned an invalid logical object identity");
    }
    return [value[0] as string, value[1] as string] as const;
  });
}

async function psqlQuery(
  psqlPath: string,
  databaseUrl: string,
  query: string,
): Promise<Uint8Array> {
  const output = await commandOutput(psqlPath, [
    "--dbname",
    databaseUrl,
    "--no-psqlrc",
    "--quiet",
    "--no-align",
    "--tuples-only",
    "--set",
    "ON_ERROR_STOP=1",
    "--command",
    `SET search_path = pg_catalog;
     SET timezone = 'UTC';
     SET datestyle = 'ISO, YMD';
     SET intervalstyle = 'iso_8601';
     SET bytea_output = 'hex';
     SET extra_float_digits = 3;
     ${query}`,
  ]);
  return output.stdout;
}

function appendFingerprintPart(parts: Uint8Array[], label: string, bytes: Uint8Array): void {
  parts.push(new TextEncoder().encode(`${label.length}:${label}:${bytes.length}:`), bytes);
}

export function quoteIdentifier(value: string): string {
  if (value.length === 0 || value.includes("\0")) fail("invalid empty PostgreSQL identifier");
  return `"${value.replaceAll('"', '""')}"`;
}

async function copyTree(source: string, target: string): Promise<void> {
  const information = await Deno.lstat(source);
  if (information.isSymlink || !information.isDirectory) {
    fail("source CAS tree must contain only real directories and regular files");
  }
  await Deno.mkdir(target);
  await copyPermissions(source, target);
  const entries: Deno.DirEntry[] = [];
  for await (const entry of Deno.readDir(source)) entries.push(entry);
  entries.sort((left, right) => left.name.localeCompare(right.name));
  for (const entry of entries) {
    const childSource = `${source}/${entry.name}`;
    const childTarget = `${target}/${entry.name}`;
    const child = await Deno.lstat(childSource);
    if (child.isSymlink) fail(`source CAS tree contains a symlink at ${childSource}`);
    if (child.isDirectory) {
      await copyTree(childSource, childTarget);
    } else if (child.isFile) {
      await Deno.copyFile(childSource, childTarget);
      await copyPermissions(childSource, childTarget);
    } else {
      fail(`source CAS tree contains a non-regular entry at ${childSource}`);
    }
  }
}

async function copyPermissions(source: string, target: string): Promise<void> {
  const mode = (await Deno.stat(source)).mode;
  if (mode !== null) await Deno.chmod(target, mode);
}

async function treeFingerprint(root: string): Promise<string> {
  const entries: string[] = [];
  await appendTreeFingerprintEntries(root, root, entries);
  entries.sort();
  const bytes = new TextEncoder().encode(entries.join("\n"));
  return hex(await crypto.subtle.digest("SHA-256", bytes));
}

async function appendTreeFingerprintEntries(
  root: string,
  directory: string,
  entries: string[],
): Promise<void> {
  const children: Deno.DirEntry[] = [];
  for await (const entry of Deno.readDir(directory)) children.push(entry);
  children.sort((left, right) => left.name.localeCompare(right.name));
  for (const child of children) {
    const path = `${directory}/${child.name}`;
    const relative = path.slice(root.length + 1);
    const information = await Deno.lstat(path);
    if (information.isSymlink) fail(`CAS fingerprint refuses symlink ${relative}`);
    if (information.isDirectory) {
      entries.push(`d\0${relative}`);
      await appendTreeFingerprintEntries(root, path, entries);
    } else if (information.isFile) {
      entries.push(`f\0${relative}\0${await fileDigest(path)}\0${information.size}`);
    } else {
      fail(`CAS fingerprint refuses non-regular entry ${relative}`);
    }
  }
}

async function fileDigest(path: string): Promise<string> {
  return hex(await crypto.subtle.digest("SHA-256", await Deno.readFile(path)));
}

function hex(buffer: ArrayBuffer): string {
  return [...new Uint8Array(buffer)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

function isWithin(path: string, root: string): boolean {
  return path === root || path.startsWith(`${root}/`);
}

function parentPath(path: string): string {
  const separator = path.lastIndexOf("/");
  return separator === 0 ? "/" : path.slice(0, separator);
}

function baseName(path: string): string {
  const name = path.slice(path.lastIndexOf("/") + 1);
  if (name.length === 0) fail("filesystem path must name a final component");
  return name;
}

async function run(
  path: string,
  args: readonly string[],
  env?: Record<string, string>,
): Promise<void> {
  const output = await commandOutput(path, args, env);
  if (!output.success) fail(`command ${path} failed with status ${output.code ?? "signal"}`);
}

async function commandOutput(
  path: string,
  args: readonly string[],
  env?: Record<string, string>,
): Promise<Deno.CommandOutput> {
  const output = await new Deno.Command(path, {
    args: [...args],
    env,
    stdout: "piped",
    stderr: "piped",
  }).output();
  if (!output.success) {
    const stdout = new TextDecoder().decode(output.stdout).trim();
    const stderr = new TextDecoder().decode(output.stderr).trim();
    fail(
      `command ${path} failed with status ${output.code ?? "signal"}` +
        (stdout.length === 0 ? "" : `\nstdout:\n${stdout}`) +
        (stderr.length === 0 ? "" : `\nstderr:\n${stderr}`),
    );
  }
  return output;
}

function usage(): never {
  fail(
    "usage: backup_restore_check.ts --source-database-url <url> --source-runtime-root <absolute-path> --restore-database-url <url> --restore-runtime-root <empty-absolute-path> --dump-file <new-absolute-path> --pg-dump <absolute-executable> --pg-restore <absolute-executable> --psql <absolute-executable> --cargo <absolute-executable>",
  );
}

function fail(message: string): never {
  throw new Error(`backup/restore qualification: ${message}`);
}
