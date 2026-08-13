/**
 * One explicit, provider-free backup/restore qualification.
 *
 * The caller creates both databases and the empty restore runtime root before
 * this process starts. This script never creates or drops a database, starts
 * a daemon, contacts a provider, or changes the source runtime root. It pairs
 * a PostgreSQL custom dump with a byte-preserving copy of the append-only CAS
 * object tree, restores only into the exact blank clone, then invokes the
 * kernel's read-only restore-integrity judge.
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
  if (sourceRuntimeRoot === restoreRuntimeRoot) {
    fail("source and restore runtime roots must be distinct");
  }
  if (isWithin(dumpFile, sourceRuntimeRoot) || isWithin(dumpFile, restoreRuntimeRoot)) {
    fail("dump-file must be outside both runtime roots");
  }

  await requireDirectory(sourceRuntimeRoot, "source-runtime-root");
  await requireEmptyDirectory(restoreRuntimeRoot, "restore-runtime-root");
  await requireMissingPath(dumpFile, "dump-file");
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
    fail("restore CAS fingerprint changed during read-only integrity qualification");
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
  const output = await commandOutput(psqlPath, [
    "--dbname",
    databaseUrl,
    "--no-align",
    "--tuples-only",
    "--set",
    "ON_ERROR_STOP=1",
    "--command",
    `WITH factory_relations AS (
       SELECT relation.relname, relation.relkind, relation.relnatts, relation.relchecks
       FROM pg_class AS relation
       JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
       WHERE namespace.nspname = 'factory'
         AND relation.relkind IN ('r', 'p', 'v', 'm', 'S', 'f')
     ), factory_columns AS (
       SELECT relation.relname, attribute.attname, attribute.atttypid::REGTYPE::TEXT,
              attribute.attnotnull, attribute.attnum
       FROM pg_class AS relation
       JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
       JOIN pg_attribute AS attribute ON attribute.attrelid = relation.oid
       WHERE namespace.nspname = 'factory'
         AND relation.relkind IN ('r', 'p', 'v', 'm', 'S', 'f')
         AND attribute.attnum > 0
         AND NOT attribute.attisdropped
     )
     SELECT 'database', current_database(), 'logical-v1'
     UNION ALL
     SELECT 'factory_relation', relname || ':' || relkind::TEXT,
            relnatts::TEXT || ':' || relchecks::TEXT
     FROM factory_relations
     UNION ALL
     SELECT 'factory_column', relname || ':' || attname,
            atttypid || ':' || attnotnull::TEXT || ':' || attnum::TEXT
     FROM factory_columns
     UNION ALL
     SELECT 'audit_log', count(*)::TEXT,
            coalesce(min(id), 0)::TEXT || ':' || coalesce(max(id), 0)::TEXT || ':' ||
            coalesce(sum(id), 0)::TEXT || ':' || coalesce(sum(resulting_revision), 0)::TEXT
     FROM factory.audit_log
     UNION ALL
     SELECT 'artifacts', count(*)::TEXT,
            coalesce(min(id), 0)::TEXT || ':' || coalesce(max(id), 0)::TEXT || ':' ||
            coalesce(sum(id), 0)::TEXT || ':' || coalesce(sum(byte_length), 0)::TEXT || ':' ||
            coalesce(sum(octet_length(digest)), 0)::TEXT
     FROM factory.artifacts
     UNION ALL
     SELECT 'kernel_builds', count(*)::TEXT,
            coalesce(sum(revision), 0)::TEXT || ':' ||
            coalesce(sum(qualification_receipt_artifact_id), 0)::TEXT || ':' ||
            coalesce(sum(CASE WHEN is_current THEN 1 ELSE 0 END), 0)::TEXT
     FROM factory.kernel_builds
     UNION ALL
     SELECT 'application_revisions', count(*)::TEXT,
            coalesce(sum(aggregate_revision), 0)::TEXT || ':' ||
            coalesce(sum(bundle_artifact_id), 0)::TEXT || ':' ||
            coalesce(sum(CASE WHEN is_active THEN 1 ELSE 0 END), 0)::TEXT
     FROM factory.application_revisions
     UNION ALL
     SELECT 'factory_sequence', sequence_name,
            coalesce(last_value::TEXT, '') || ':' || start_value::TEXT || ':' || increment_by::TEXT
     FROM pg_sequences
     WHERE schemaname = 'factory'
     ORDER BY 1, 2, 3`,
  ]);
  return hex(await crypto.subtle.digest("SHA-256", output.stdout));
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
  if (!output.success) fail(`command ${path} failed with status ${output.code ?? "signal"}`);
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
