//! Provider-free PostgreSQL/CAS backup and restore qualification.
//!
//! The caller creates both databases and the empty restore runtime root before
//! invoking [`run`].  This module never creates or drops a database, starts a
//! daemon, contacts a provider, or changes the source runtime root.  It pairs
//! a PostgreSQL custom dump with a byte-preserving copy of the append-only CAS
//! object tree, restores only into the exact blank clone, then invokes the
//! kernel's restore-integrity and repaired clone-corruption judge.
//!
//! This is deliberately self-contained so the command-line binary can wire it
//! in without making the factory kernel depend on operator concerns.  The
//! public [`parse_options`] and [`run`] functions are the integration seam for
//! `factoryctl`.
#![allow(dead_code)]

use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
    process::Command,
};

/// Parsed `--flag value` arguments.  Unknown flags are retained so the
/// required-option conversion can report missing options without silently
/// accepting malformed values.
pub type Arguments = BTreeMap<String, String>;

/// All inputs required for one explicit backup/restore qualification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackupRestoreArguments {
    pub source_database_url: String,
    pub source_runtime_root: PathBuf,
    pub restore_database_url: String,
    pub restore_runtime_root: PathBuf,
    pub dump_file: PathBuf,
    pub pg_dump: PathBuf,
    pub pg_restore: PathBuf,
    pub psql: PathBuf,
    pub cargo: PathBuf,
}

/// Errors raised by argument validation, filesystem guards, and child
/// commands. Messages retain stable operator-facing vocabulary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackupRestoreError {
    message: String,
}

impl BackupRestoreError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for BackupRestoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "backup/restore qualification: {}", self.message)
    }
}

impl std::error::Error for BackupRestoreError {}

type Result<T> = std::result::Result<T, BackupRestoreError>;

/// Parses the strict alternating `--flag value` command-line shape.
pub fn parse_arguments(values: &[String]) -> Result<Arguments> {
    if !values.len().is_multiple_of(2) {
        return Err(usage_error());
    }
    let mut result = BTreeMap::new();
    let (pairs, remainder) = values.as_chunks::<2>();
    debug_assert!(remainder.is_empty(), "pair count was checked");
    for pair in pairs {
        let flag = &pair[0];
        let value = &pair[1];
        if !flag.starts_with("--") || flag.len() <= 2 || value.starts_with("--") {
            return Err(usage_error());
        }
        let name = &flag[2..];
        if !valid_flag_name(name) || result.contains_key(name) {
            return Err(usage_error());
        }
        result.insert(name.to_owned(), value.clone());
    }
    Ok(result)
}

/// Parses command-line values and performs all scalar/path normalization
/// needed before [`run`] is called by the CLI.
pub fn parse_options(values: &[String]) -> Result<BackupRestoreArguments> {
    let arguments = parse_arguments(values)?;
    Ok(BackupRestoreArguments {
        source_database_url: required(&arguments, "source-database-url")?,
        source_runtime_root: absolute_path(&required(&arguments, "source-runtime-root")?)?,
        restore_database_url: required(&arguments, "restore-database-url")?,
        restore_runtime_root: absolute_path(&required(&arguments, "restore-runtime-root")?)?,
        dump_file: absolute_path(&required(&arguments, "dump-file")?)?,
        pg_dump: executable(&required(&arguments, "pg-dump")?)?,
        pg_restore: executable(&required(&arguments, "pg-restore")?)?,
        psql: executable(&required(&arguments, "psql")?)?,
        cargo: executable(&required(&arguments, "cargo")?)?,
    })
}

/// Alias with a descriptive name for callers that do not want to expose the
/// generic `parse_options` name in their own argument parser.
pub fn parse_backup_restore_arguments(values: &[String]) -> Result<BackupRestoreArguments> {
    parse_options(values)
}

/// Runs one complete provider-free backup/restore qualification.
pub fn run(arguments: BackupRestoreArguments) -> Result<()> {
    let source_database_name =
        database_name(&arguments.source_database_url, "source-database-url")?;
    let restore_database_name =
        database_name(&arguments.restore_database_url, "restore-database-url")?;
    if !is_restore_database_name(&restore_database_name) {
        return Err(BackupRestoreError::new(
            "restore-database-url must name exactly factory_restore_v3_<digits>",
        ));
    }
    if same_database_target(
        &arguments.source_database_url,
        &arguments.restore_database_url,
    )? {
        return Err(BackupRestoreError::new(
            "source and restore databases must use distinct PostgreSQL host/port/name targets",
        ));
    }

    require_directory(&arguments.source_runtime_root, "source-runtime-root")?;
    require_empty_directory(&arguments.restore_runtime_root, "restore-runtime-root")?;
    require_missing_path(&arguments.dump_file, "dump-file")?;

    let canonical_source_runtime_root = canonical_path(&arguments.source_runtime_root)?;
    let canonical_restore_runtime_root = canonical_path(&arguments.restore_runtime_root)?;
    if is_within(
        &canonical_source_runtime_root,
        &canonical_restore_runtime_root,
    ) || is_within(
        &canonical_restore_runtime_root,
        &canonical_source_runtime_root,
    ) {
        return Err(BackupRestoreError::new(
            "source and restore runtime roots must be distinct and non-overlapping",
        ));
    }

    let canonical_dump_parent = arguments
        .dump_file
        .parent()
        .ok_or_else(|| BackupRestoreError::new("dump-file must name a final component"))
        .and_then(canonical_path)?;
    let canonical_dump_file =
        canonical_dump_parent.join(arguments.dump_file.file_name().ok_or_else(|| {
            BackupRestoreError::new("filesystem path must name a final component")
        })?);
    if is_within(&canonical_dump_file, &canonical_source_runtime_root)
        || is_within(&canonical_dump_file, &canonical_restore_runtime_root)
    {
        return Err(BackupRestoreError::new(
            "dump-file must be outside both runtime roots",
        ));
    }

    let source_objects = arguments.source_runtime_root.join("objects");
    require_directory(&source_objects, "source runtime CAS objects")?;
    require_blank_restore_database(&arguments.psql, &arguments.restore_database_url)?;

    let source_database_before =
        database_fingerprint(&arguments.psql, &arguments.source_database_url)?;
    let source_cas_before = tree_fingerprint(&source_objects)?;
    run_command(
        &arguments.pg_dump,
        &[
            "--format".to_owned(),
            "custom".to_owned(),
            "--file".to_owned(),
            arguments.dump_file.to_string_lossy().into_owned(),
            "--dbname".to_owned(),
            arguments.source_database_url.clone(),
        ],
        &[],
    )?;
    copy_tree(
        &source_objects,
        &arguments.restore_runtime_root.join("objects"),
    )?;
    let restore_objects = arguments.restore_runtime_root.join("objects");
    let clone_before_integrity = tree_fingerprint(&restore_objects)?;
    if clone_before_integrity != source_cas_before {
        return Err(BackupRestoreError::new(
            "restored CAS fingerprint differs from the source CAS fingerprint",
        ));
    }

    run_command(
        &arguments.pg_restore,
        &[
            "--exit-on-error".to_owned(),
            "--no-owner".to_owned(),
            "--no-privileges".to_owned(),
            "--dbname".to_owned(),
            arguments.restore_database_url.clone(),
            arguments.dump_file.to_string_lossy().into_owned(),
        ],
        &[],
    )?;
    let clone_database_before_integrity =
        database_fingerprint(&arguments.psql, &arguments.restore_database_url)?;
    if clone_database_before_integrity != source_database_before {
        return Err(BackupRestoreError::new(
            "restored database logical fingerprint differs from the source database",
        ));
    }

    run_command(
        &arguments.cargo,
        &[
            "test".to_owned(),
            "-p".to_owned(),
            "factory-kernel".to_owned(),
            "--test".to_owned(),
            "backup_restore".to_owned(),
            "restored_database_and_cas_are_integrity_qualified".to_owned(),
            "--".to_owned(),
            "--ignored".to_owned(),
            "--test-threads=1".to_owned(),
        ],
        &[
            ("DATABASE_URL", arguments.restore_database_url.clone()),
            (
                "FACTORY_RESTORE_DATABASE_URL",
                arguments.restore_database_url.clone(),
            ),
            (
                "FACTORY_RESTORE_RUNTIME_ROOT",
                arguments
                    .restore_runtime_root
                    .to_string_lossy()
                    .into_owned(),
            ),
        ],
    )?;

    let source_database_after =
        database_fingerprint(&arguments.psql, &arguments.source_database_url)?;
    let source_cas_after = tree_fingerprint(&source_objects)?;
    let clone_database_after_integrity =
        database_fingerprint(&arguments.psql, &arguments.restore_database_url)?;
    let clone_after_integrity = tree_fingerprint(&restore_objects)?;
    if source_database_after != source_database_before {
        return Err(BackupRestoreError::new(
            "source database fingerprint changed during backup/restore qualification",
        ));
    }
    if source_cas_after != source_cas_before {
        return Err(BackupRestoreError::new(
            "source CAS fingerprint changed during backup/restore qualification",
        ));
    }
    if clone_database_after_integrity != clone_database_before_integrity {
        return Err(BackupRestoreError::new(
            "restore database logical fingerprint changed during integrity qualification",
        ));
    }
    if clone_after_integrity != source_cas_before {
        return Err(BackupRestoreError::new(
            "restore CAS fingerprint changed during integrity corruption probes",
        ));
    }

    println!(
        "backup/restore qualification passed for source {source_database_name} and clone {restore_database_name}"
    );
    Ok(())
}

/// Normalizes and validates an absolute filesystem path.
pub fn absolute_path(value: &str) -> Result<PathBuf> {
    if value.is_empty() || value.contains('\0') || !value.starts_with('/') {
        return Err(BackupRestoreError::new("filesystem paths must be absolute"));
    }
    let components = value.split('/');
    if components
        .clone()
        .any(|component| component == "." || component == "..")
    {
        return Err(BackupRestoreError::new(
            "filesystem paths must not contain dot components",
        ));
    }
    let normalized = if value == "/" {
        "/".to_owned()
    } else {
        value.trim_end_matches('/').to_owned()
    };
    if normalized.is_empty() {
        Ok(PathBuf::from("/"))
    } else {
        Ok(PathBuf::from(normalized))
    }
}

/// Compares PostgreSQL host/port/database targets, ignoring credentials and
/// URL scheme in the same way as the former operator script.
pub fn same_database_target(left: &str, right: &str) -> Result<bool> {
    Ok(database_target(left, "source-database-url")?
        == database_target(right, "restore-database-url")?)
}

/// Returns the canonical target tuple used by [`same_database_target`].
pub fn database_target(value: &str, argument_name: &str) -> Result<String> {
    let parsed = parse_database_url(value, argument_name)?;
    Ok(format!(
        "{}\0{}\0{}",
        parsed.host.to_ascii_lowercase(),
        parsed.port,
        parsed.name
    ))
}

/// Quotes one catalog-owned PostgreSQL identifier for a generated query.
pub fn quote_identifier(value: &str) -> Result<String> {
    if value.is_empty() || value.contains('\0') {
        return Err(BackupRestoreError::new(
            "invalid empty PostgreSQL identifier",
        ));
    }
    Ok(format!("\"{}\"", value.replace('"', "\"\"")))
}

fn valid_flag_name(value: &str) -> bool {
    let mut components = value.split('-');
    components
        .next()
        .is_some_and(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_lowercase()))
        && components
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_lowercase()))
}

fn required(values: &Arguments, name: &str) -> Result<String> {
    let value = values
        .get(name)
        .filter(|value| !value.is_empty() && !value.contains('\0'))
        .cloned()
        .ok_or_else(|| BackupRestoreError::new(format!("--{name} is required")))?;
    Ok(value)
}

fn executable(value: &str) -> Result<PathBuf> {
    absolute_path(value)
}

fn is_restore_database_name(value: &str) -> bool {
    let prefix = "factory_restore_v3_";
    value.starts_with(prefix)
        && !value[prefix.len()..].is_empty()
        && value[prefix.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_digit())
}

#[derive(Debug)]
struct ParsedDatabaseUrl {
    host: String,
    port: String,
    name: String,
}

fn database_name(value: &str, argument_name: &str) -> Result<String> {
    Ok(parse_database_url(value, argument_name)?.name)
}

fn parse_database_url(value: &str, argument_name: &str) -> Result<ParsedDatabaseUrl> {
    let rest = if let Some(rest) = value.strip_prefix("postgresql://") {
        rest
    } else if let Some(rest) = value.strip_prefix("postgres://") {
        rest
    } else {
        return Err(BackupRestoreError::new(format!(
            "--{argument_name} must use postgresql:// or postgres://"
        )));
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let path_and_suffix = &rest[authority_end..];
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let (host, port) = parse_host_port(authority, argument_name)?;
    let path = path_and_suffix.split(['?', '#']).next().unwrap_or_default();
    if !path.starts_with('/') {
        return Err(BackupRestoreError::new(format!(
            "--{argument_name} must name one database"
        )));
    }
    let decoded_path = percent_decode(&path[1..]).map_err(|()| {
        BackupRestoreError::new(format!("--{argument_name} must name one database"))
    })?;
    if decoded_path.is_empty() || decoded_path.contains('/') || decoded_path.contains('\0') {
        return Err(BackupRestoreError::new(format!(
            "--{argument_name} must name one database"
        )));
    }
    Ok(ParsedDatabaseUrl {
        host,
        port,
        name: decoded_path,
    })
}

fn parse_host_port(authority: &str, argument_name: &str) -> Result<(String, String)> {
    if authority.is_empty() {
        return Err(BackupRestoreError::new(format!(
            "--{argument_name} must include a PostgreSQL host"
        )));
    }
    let (host, port) = if authority.starts_with('[') {
        let close = authority.find(']').ok_or_else(|| {
            BackupRestoreError::new(format!("--{argument_name} must be a PostgreSQL URL"))
        })?;
        let host = &authority[1..close];
        let suffix = &authority[close + 1..];
        let port = if suffix.is_empty() {
            "5432".to_owned()
        } else if let Some(port) = suffix.strip_prefix(':') {
            valid_port(port, argument_name)?
        } else {
            return Err(BackupRestoreError::new(format!(
                "--{argument_name} must be a PostgreSQL URL"
            )));
        };
        (host.to_owned(), port)
    } else {
        let colon_count = authority.bytes().filter(|byte| *byte == b':').count();
        if colon_count > 1 {
            return Err(BackupRestoreError::new(format!(
                "--{argument_name} must be a PostgreSQL URL"
            )));
        }
        if let Some((host, port)) = authority.rsplit_once(':') {
            (host.to_owned(), valid_port(port, argument_name)?)
        } else {
            (authority.to_owned(), "5432".to_owned())
        }
    };
    let host = percent_decode(&host).map_err(|()| {
        BackupRestoreError::new(format!("--{argument_name} must be a PostgreSQL URL"))
    })?;
    if host.is_empty() || host.contains('\0') {
        return Err(BackupRestoreError::new(format!(
            "--{argument_name} must include a PostgreSQL host"
        )));
    }
    Ok((host, port))
}

fn valid_port(value: &str, argument_name: &str) -> Result<String> {
    // WHATWG URL parsing treats an
    // explicitly empty `:port` as the scheme default.
    if value.is_empty() {
        return Ok("5432".to_owned());
    }
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(BackupRestoreError::new(format!(
            "--{argument_name} must be a PostgreSQL URL"
        )));
    }
    let parsed = value.parse::<u32>().map_err(|_| {
        BackupRestoreError::new(format!("--{argument_name} must be a PostgreSQL URL"))
    })?;
    if parsed > u32::from(u16::MAX) {
        return Err(BackupRestoreError::new(format!(
            "--{argument_name} must be a PostgreSQL URL"
        )));
    }
    Ok(parsed.to_string())
}

fn percent_decode(value: &str) -> std::result::Result<String, ()> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut chars = value.as_bytes().iter().copied();
    while let Some(byte) = chars.next() {
        if byte != b'%' {
            bytes.push(byte);
            continue;
        }
        let high = chars.next().ok_or(())?;
        let low = chars.next().ok_or(())?;
        let high = (high as char).to_digit(16).ok_or(())? as u8;
        let low = (low as char).to_digit(16).ok_or(())? as u8;
        bytes.push((high << 4) | low);
    }
    String::from_utf8(bytes).map_err(|_| ())
}

fn require_directory(path: &Path, label: &str) -> Result<()> {
    let information = fs::symlink_metadata(path)
        .map_err(|_| BackupRestoreError::new(format!("{label} must be an existing directory")))?;
    if information.file_type().is_symlink() || !information.is_dir() {
        return Err(BackupRestoreError::new(format!(
            "{label} must be a real directory"
        )));
    }
    Ok(())
}

fn require_empty_directory(path: &Path, label: &str) -> Result<()> {
    require_directory(path, label)?;
    let mut entries =
        fs::read_dir(path).map_err(|error| io_error("read restore directory", path, error))?;
    if entries
        .next()
        .transpose()
        .map_err(|error| io_error("read restore directory", path, error))?
        .is_some()
    {
        return Err(BackupRestoreError::new(format!(
            "{label} must be empty before restore"
        )));
    }
    Ok(())
}

fn require_missing_path(path: &Path, label: &str) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(BackupRestoreError::new(format!(
            "{label} must not already exist"
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error("inspect path", path, error)),
    }
}

fn canonical_path(path: &Path) -> Result<PathBuf> {
    fs::canonicalize(path).map_err(|error| io_error("canonicalize path", path, error))
}

fn is_within(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

fn require_blank_restore_database(psql_path: &Path, database_url: &str) -> Result<()> {
    let output = psql_query(
        psql_path,
        database_url,
        r"SELECT NOT EXISTS (
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
     ) AS blank",
    )?;
    if String::from_utf8_lossy(&output).trim() != "t" {
        return Err(BackupRestoreError::new(
            "restore database must be blank: user schema objects are present",
        ));
    }
    Ok(())
}

fn database_fingerprint(psql_path: &Path, database_url: &str) -> Result<String> {
    let mut parts = Vec::new();
    append_fingerprint_part(&mut parts, "format", b"logical-database-v2");
    append_fingerprint_part(
        &mut parts,
        "schema",
        &psql_query(psql_path, database_url, stable_schema_shape_query())?,
    );

    let relations = catalog_objects(
        psql_path,
        database_url,
        r#"SELECT json_build_array(namespace.nspname, relation.relname)::TEXT
       FROM pg_class AS relation
       JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
      WHERE namespace.nspname NOT IN ('pg_catalog', 'information_schema')
        AND namespace.nspname !~ '^pg_toast'
        AND relation.relkind IN ('r', 'p', 'm')
      ORDER BY namespace.nspname COLLATE "C", relation.relname COLLATE "C""#,
    )?;
    for (schema, relation) in relations {
        let qualified_relation = format!(
            "{}.{}",
            quote_identifier(&schema)?,
            quote_identifier(&relation)?
        );
        append_fingerprint_part(
            &mut parts,
            &format!("rows:{schema}.{relation}"),
            &psql_query(
                psql_path,
                database_url,
                &format!(
                    r#"SELECT to_jsonb(logical_row)::TEXT
           FROM {qualified_relation} AS logical_row
          ORDER BY to_jsonb(logical_row)::TEXT COLLATE "C""#
                ),
            )?,
        );
    }

    let sequences = catalog_objects(
        psql_path,
        database_url,
        r#"SELECT json_build_array(namespace.nspname, relation.relname)::TEXT
       FROM pg_class AS relation
       JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
      WHERE namespace.nspname NOT IN ('pg_catalog', 'information_schema')
        AND namespace.nspname !~ '^pg_toast'
        AND relation.relkind = 'S'
      ORDER BY namespace.nspname COLLATE "C", relation.relname COLLATE "C""#,
    )?;
    for (schema, sequence) in sequences {
        append_fingerprint_part(
            &mut parts,
            &format!("sequence:{schema}.{sequence}"),
            &psql_query(
                psql_path,
                database_url,
                &format!(
                    r"SELECT json_build_array(last_value, is_called)::TEXT
           FROM {}.{}",
                    quote_identifier(&schema)?,
                    quote_identifier(&sequence)?
                ),
            )?,
        );
    }
    Ok(sha256_hex(&parts.concat()))
}

fn stable_schema_shape_query() -> &'static str {
    r#"WITH user_namespaces AS (
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
          item_name COLLATE "C", shape COLLATE "C""#
}

fn catalog_objects(
    psql_path: &Path,
    database_url: &str,
    query: &str,
) -> Result<Vec<(String, String)>> {
    let output = String::from_utf8_lossy(&psql_query(psql_path, database_url, query)?).to_string();
    let output = output.trim();
    if output.is_empty() {
        return Ok(Vec::new());
    }
    output
        .lines()
        .map(parse_catalog_object)
        .collect::<Result<Vec<_>>>()
}

fn parse_catalog_object(line: &str) -> Result<(String, String)> {
    let mut parser = JsonStringParser::new(line.trim());
    parser.expect_byte(b'[')?;
    let first = parser.string()?;
    parser.expect_byte(b',')?;
    let second = parser.string()?;
    parser.expect_byte(b']')?;
    if parser.remaining().trim().is_empty() {
        Ok((first, second))
    } else {
        Err(BackupRestoreError::new(
            "PostgreSQL catalog returned an invalid logical object identity",
        ))
    }
}

struct JsonStringParser<'a> {
    input: &'a [u8],
    offset: usize,
}

impl<'a> JsonStringParser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            offset: 0,
        }
    }

    fn remaining(&self) -> &str {
        std::str::from_utf8(&self.input[self.offset..]).unwrap_or_default()
    }

    fn expect_byte(&mut self, expected: u8) -> Result<()> {
        self.skip_whitespace();
        if self.input.get(self.offset).copied() == Some(expected) {
            self.offset += 1;
            Ok(())
        } else {
            Err(BackupRestoreError::new(
                "PostgreSQL catalog returned an invalid logical object identity",
            ))
        }
    }

    fn string(&mut self) -> Result<String> {
        self.skip_whitespace();
        if self.input.get(self.offset).copied() != Some(b'"') {
            return Err(BackupRestoreError::new(
                "PostgreSQL catalog returned an invalid logical object identity",
            ));
        }
        self.offset += 1;
        let mut result = String::new();
        while let Some(byte) = self.input.get(self.offset).copied() {
            self.offset += 1;
            match byte {
                b'"' => return Ok(result),
                b'\\' => {
                    let escaped = self.input.get(self.offset).copied().ok_or_else(|| {
                        BackupRestoreError::new(
                            "PostgreSQL catalog returned an invalid logical object identity",
                        )
                    })?;
                    self.offset += 1;
                    match escaped {
                        b'"' => result.push('"'),
                        b'\\' => result.push('\\'),
                        b'/' => result.push('/'),
                        b'b' => result.push('\u{0008}'),
                        b'f' => result.push('\u{000c}'),
                        b'n' => result.push('\n'),
                        b'r' => result.push('\r'),
                        b't' => result.push('\t'),
                        b'u' => {
                            let value = parse_unicode_escape(self.input, &mut self.offset)?;
                            if (0xD800..=0xDBFF).contains(&value) {
                                if self.input.get(self.offset..self.offset + 2) != Some(b"\\u") {
                                    return Err(BackupRestoreError::new(
                                        "PostgreSQL catalog returned an invalid logical object identity",
                                    ));
                                }
                                self.offset += 2;
                                let low = parse_unicode_escape(self.input, &mut self.offset)?;
                                if !(0xDC00..=0xDFFF).contains(&low) {
                                    return Err(BackupRestoreError::new(
                                        "PostgreSQL catalog returned an invalid logical object identity",
                                    ));
                                }
                                let scalar = 0x10000
                                    + (u32::from(value - 0xD800) << 10)
                                    + u32::from(low - 0xDC00);
                                result.push(char::from_u32(scalar).ok_or_else(|| {
                                    BackupRestoreError::new(
                                        "PostgreSQL catalog returned an invalid logical object identity",
                                    )
                                })?);
                            } else if (0xDC00..=0xDFFF).contains(&value) {
                                return Err(BackupRestoreError::new(
                                    "PostgreSQL catalog returned an invalid logical object identity",
                                ));
                            } else {
                                result.push(char::from_u32(u32::from(value)).ok_or_else(|| {
                                    BackupRestoreError::new(
                                        "PostgreSQL catalog returned an invalid logical object identity",
                                    )
                                })?);
                            }
                        }
                        _ => {
                            return Err(BackupRestoreError::new(
                                "PostgreSQL catalog returned an invalid logical object identity",
                            ));
                        }
                    }
                }
                byte if byte < 0x20 => {
                    return Err(BackupRestoreError::new(
                        "PostgreSQL catalog returned an invalid logical object identity",
                    ));
                }
                byte if byte.is_ascii() => result.push(byte as char),
                _ => {
                    let start = self.offset - 1;
                    let remaining = std::str::from_utf8(&self.input[start..]).map_err(|_| {
                        BackupRestoreError::new(
                            "PostgreSQL catalog returned an invalid logical object identity",
                        )
                    })?;
                    let character = remaining.chars().next().ok_or_else(|| {
                        BackupRestoreError::new(
                            "PostgreSQL catalog returned an invalid logical object identity",
                        )
                    })?;
                    self.offset = start + character.len_utf8();
                    result.push(character);
                }
            }
        }
        Err(BackupRestoreError::new(
            "PostgreSQL catalog returned an invalid logical object identity",
        ))
    }

    fn skip_whitespace(&mut self) {
        while self
            .input
            .get(self.offset)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.offset += 1;
        }
    }
}

fn parse_unicode_escape(input: &[u8], offset: &mut usize) -> Result<u16> {
    let end = offset.saturating_add(4);
    if end > input.len() {
        return Err(BackupRestoreError::new(
            "PostgreSQL catalog returned an invalid logical object identity",
        ));
    }
    let hex = std::str::from_utf8(&input[*offset..end]).map_err(|_| {
        BackupRestoreError::new("PostgreSQL catalog returned an invalid logical object identity")
    })?;
    let value = u16::from_str_radix(hex, 16).map_err(|_| {
        BackupRestoreError::new("PostgreSQL catalog returned an invalid logical object identity")
    })?;
    *offset = end;
    Ok(value)
}

fn psql_query(psql_path: &Path, database_url: &str, query: &str) -> Result<Vec<u8>> {
    let command = format!(
        "SET search_path = pg_catalog;\n         SET timezone = 'UTC';\n         SET datestyle = 'ISO, YMD';\n         SET intervalstyle = 'iso_8601';\n         SET bytea_output = 'hex';\n         SET extra_float_digits = 3;\n         {query}"
    );
    command_output(
        psql_path,
        &[
            "--dbname".to_owned(),
            database_url.to_owned(),
            "--no-psqlrc".to_owned(),
            "--quiet".to_owned(),
            "--no-align".to_owned(),
            "--tuples-only".to_owned(),
            "--set".to_owned(),
            "ON_ERROR_STOP=1".to_owned(),
            "--command".to_owned(),
            command,
        ],
        &[],
    )
}

fn append_fingerprint_part(parts: &mut Vec<Vec<u8>>, label: &str, bytes: &[u8]) {
    parts.push(format!("{}:{}:{}:", label.len(), label, bytes.len()).into_bytes());
    parts.push(bytes.to_vec());
}

fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    let information = fs::symlink_metadata(source)
        .map_err(|error| io_error("inspect source CAS tree", source, error))?;
    if information.file_type().is_symlink() || !information.is_dir() {
        return Err(BackupRestoreError::new(
            "source CAS tree must contain only real directories and regular files",
        ));
    }
    fs::create_dir(target)
        .map_err(|error| io_error("create restore CAS directory", target, error))?;
    let mut entries = fs::read_dir(source)
        .map_err(|error| io_error("read source CAS directory", source, error))?
        .collect::<io::Result<Vec<_>>>()
        .map_err(|error| io_error("read source CAS directory", source, error))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let child_source = entry.path();
        let child_target = target.join(entry.file_name());
        let child = fs::symlink_metadata(&child_source)
            .map_err(|error| io_error("inspect source CAS entry", &child_source, error))?;
        if child.file_type().is_symlink() {
            return Err(BackupRestoreError::new(format!(
                "source CAS tree contains a symlink at {}",
                child_source.display()
            )));
        }
        if child.is_dir() {
            copy_tree(&child_source, &child_target)?;
        } else if child.is_file() {
            fs::copy(&child_source, &child_target)
                .map_err(|error| io_error("copy source CAS object", &child_source, error))?;
            copy_permissions(&child_source, &child_target)?;
        } else {
            return Err(BackupRestoreError::new(format!(
                "source CAS tree contains a non-regular entry at {}",
                child_source.display()
            )));
        }
    }
    copy_permissions(source, target)
}

fn copy_permissions(source: &Path, target: &Path) -> Result<()> {
    let permissions = fs::metadata(source)
        .map_err(|error| io_error("read source permissions", source, error))?
        .permissions();
    fs::set_permissions(target, permissions)
        .map_err(|error| io_error("copy source permissions", target, error))
}

fn tree_fingerprint(root: &Path) -> Result<String> {
    let mut entries = Vec::new();
    append_tree_fingerprint_entries(root, root, &mut entries)?;
    entries.sort();
    Ok(sha256_hex(entries.join("\n").as_bytes()))
}

fn append_tree_fingerprint_entries(
    root: &Path,
    directory: &Path,
    entries: &mut Vec<String>,
) -> Result<()> {
    let mut children = fs::read_dir(directory)
        .map_err(|error| io_error("read CAS fingerprint directory", directory, error))?
        .collect::<io::Result<Vec<_>>>()
        .map_err(|error| io_error("read CAS fingerprint directory", directory, error))?;
    children.sort_by_key(std::fs::DirEntry::file_name);
    for child in children {
        let path = child.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|_| BackupRestoreError::new("CAS fingerprint path escaped root"))?;
        let relative = relative.to_string_lossy();
        let information = fs::symlink_metadata(&path)
            .map_err(|error| io_error("inspect CAS fingerprint entry", &path, error))?;
        if information.file_type().is_symlink() {
            return Err(BackupRestoreError::new(format!(
                "CAS fingerprint refuses symlink {relative}"
            )));
        }
        if information.is_dir() {
            entries.push(format!("d\0{relative}"));
            append_tree_fingerprint_entries(root, &path, entries)?;
        } else if information.is_file() {
            entries.push(format!(
                "f\0{relative}\0{}\0{}",
                file_digest(&path)?,
                information.len()
            ));
        } else {
            return Err(BackupRestoreError::new(format!(
                "CAS fingerprint refuses non-regular entry {relative}"
            )));
        }
    }
    Ok(())
}

fn file_digest(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|error| io_error("read CAS object", path, error))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| io_error("read CAS object", path, error))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher.finalize_hex())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize_hex()
}

fn command_output(path: &Path, args: &[String], environment: &[(&str, String)]) -> Result<Vec<u8>> {
    let mut command = Command::new(path);
    command.args(args);
    for (name, value) in environment {
        command.env(name, value);
    }
    let output = command.output().map_err(|error| {
        BackupRestoreError::new(format!(
            "command {} failed to start: {error}",
            path.display()
        ))
    })?;
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let status = output
            .status
            .code()
            .map_or_else(|| "signal".to_owned(), |code| code.to_string());
        return Err(BackupRestoreError::new(format!(
            "command {} failed with status {status}{}{}",
            path.display(),
            if stdout.is_empty() {
                String::new()
            } else {
                format!("\nstdout:\n{stdout}")
            },
            if stderr.is_empty() {
                String::new()
            } else {
                format!("\nstderr:\n{stderr}")
            }
        )));
    }
    Ok(output.stdout)
}

fn run_command(path: &Path, args: &[String], environment: &[(&str, String)]) -> Result<()> {
    command_output(path, args, environment).map(|_| ())
}

fn usage_error() -> BackupRestoreError {
    BackupRestoreError::new(
        "usage: backup_restore_check --source-database-url <url> --source-runtime-root <absolute-path> --restore-database-url <url> --restore-runtime-root <empty-absolute-path> --dump-file <new-absolute-path> --pg-dump <absolute-executable> --pg-restore <absolute-executable> --psql <absolute-executable> --cargo <absolute-executable>",
    )
}

fn io_error(operation: &str, path: &Path, error: io::Error) -> BackupRestoreError {
    BackupRestoreError::new(format!(
        "I/O while {operation} at {}: {error}",
        path.display()
    ))
}

// A small dependency-free SHA-256 implementation keeps the Rust operator
// utility's logical and CAS fingerprints byte-compatible with WebCrypto's
// SHA-256 without adding a workspace dependency solely for this command.
struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffered: usize,
    length: u64,
}

impl Sha256 {
    fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buffer: [0; 64],
            buffered: 0,
            length: 0,
        }
    }

    fn update(&mut self, mut bytes: &[u8]) {
        self.length = self.length.wrapping_add(bytes.len() as u64);
        if self.buffered > 0 {
            let count = (64 - self.buffered).min(bytes.len());
            self.buffer[self.buffered..self.buffered + count].copy_from_slice(&bytes[..count]);
            self.buffered += count;
            bytes = &bytes[count..];
            if self.buffered == 64 {
                let block = self.buffer;
                self.compress(&block);
                self.buffered = 0;
            }
        }
        while bytes.len() >= 64 {
            self.compress(bytes[..64].try_into().expect("64-byte block"));
            bytes = &bytes[64..];
        }
        if !bytes.is_empty() {
            self.buffer[..bytes.len()].copy_from_slice(bytes);
            self.buffered = bytes.len();
        }
    }

    fn finalize_hex(mut self) -> String {
        let bit_length = self.length.wrapping_mul(8);
        self.buffer[self.buffered] = 0x80;
        self.buffered += 1;
        if self.buffered > 56 {
            self.buffer[self.buffered..].fill(0);
            let block = self.buffer;
            self.compress(&block);
            self.buffered = 0;
        }
        self.buffer[self.buffered..56].fill(0);
        self.buffer[56..].copy_from_slice(&bit_length.to_be_bytes());
        let block = self.buffer;
        self.compress(&block);
        let mut output = String::with_capacity(64);
        for word in self.state {
            output.push_str(&format!("{word:08x}"));
        }
        output
    }

    fn compress(&mut self, block: &[u8; 64]) {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];
        let mut words = [0_u32; 64];
        let (chunks, remainder) = block.as_chunks::<4>();
        debug_assert!(
            remainder.is_empty(),
            "SHA-256 block is a multiple of four bytes"
        );
        for (index, chunk) in chunks.iter().enumerate() {
            words[index] = u32::from_be_bytes(*chunk);
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }
        let mut working = self.state;
        for index in 0..64 {
            let [a, b, c, d, e, f, g, h] = working;
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(majority);
            working = [
                temp1.wrapping_add(temp2),
                a,
                b,
                c,
                d.wrapping_add(temp1),
                e,
                f,
                g,
            ];
        }
        for (state, value) in self.state.iter_mut().zip(working) {
            *state = state.wrapping_add(value);
        }
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn catalog_parser_handles_escaped_identifiers() {
        assert_eq!(
            parse_catalog_object(r#"["schema\\\"name","relation"]"#).unwrap(),
            ("schema\\\"name".to_owned(), "relation".to_owned())
        );
        assert_eq!(
            parse_catalog_object(r#"["schéma","😀"]"#).unwrap(),
            ("schéma".to_owned(), "😀".to_owned())
        );
        assert_eq!(
            parse_catalog_object(r#"["\uD83D\uDE00","relation"]"#).unwrap(),
            ("😀".to_owned(), "relation".to_owned())
        );
    }
}
