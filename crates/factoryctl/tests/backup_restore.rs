#[path = "../src/backup_restore.rs"]
mod backup_restore;

use backup_restore::{
    absolute_path, database_target, parse_arguments, parse_options, quote_identifier,
    same_database_target,
};
use std::path::PathBuf;

#[test]
fn argument_parser_accepts_only_unique_flag_value_pairs() {
    let values = [
        "--source-runtime-root",
        "/source",
        "--restore-runtime-root",
        "/restore",
    ]
    .map(str::to_owned);
    assert_eq!(
        parse_arguments(&values).unwrap(),
        [
            ("restore-runtime-root".to_owned(), "/restore".to_owned()),
            ("source-runtime-root".to_owned(), "/source".to_owned()),
        ]
        .into_iter()
        .collect()
    );

    let duplicate = [
        "--source-runtime-root",
        "/source",
        "--source-runtime-root",
        "/other",
    ]
    .map(str::to_owned);
    assert!(parse_arguments(&duplicate).is_err());
    assert!(parse_arguments(&["--source-runtime-root".to_owned()]).is_err());
    assert!(parse_arguments(&["source-runtime-root".to_owned(), "/source".to_owned()]).is_err());
}

#[test]
fn path_guard_requires_normalized_absolute_distinct_roots() {
    assert_eq!(
        absolute_path("/restore///").unwrap(),
        PathBuf::from("/restore")
    );
    assert!(absolute_path("relative/restore").is_err());
    assert!(absolute_path("/restore/../source").is_err());
    assert!(absolute_path("/restore/./clone").is_err());
}

#[test]
fn database_target_guard_compares_host_port_and_name() {
    assert!(
        same_database_target(
            "postgresql://josh@localhost/factory_restore_v3_1",
            "postgres://other@LOCALHOST:5432/factory_restore_v3_1",
        )
        .unwrap()
    );
    assert!(
        !same_database_target(
            "postgresql://josh@%2Ftmp/factory_restore_v3_1",
            "postgresql://josh@%2Ftmp/factory_restore_v3_2",
        )
        .unwrap()
    );
    assert!(
        !same_database_target(
            "postgresql://josh@localhost:5433/factory_restore_v3_1",
            "postgresql://josh@localhost/factory_restore_v3_1",
        )
        .unwrap()
    );
    assert_eq!(
        database_target(
            "postgresql://josh@localhost/factory_restore_v3_1",
            "source-database-url",
        )
        .unwrap(),
        concat!("localhost\0", "5432\0factory_restore_v3_1")
    );
}

#[test]
fn logical_database_fingerprint_quotes_catalog_owned_identifiers() {
    assert_eq!(quote_identifier("factory").unwrap(), "\"factory\"");
    assert_eq!(
        quote_identifier("relation\"name").unwrap(),
        "\"relation\"\"name\""
    );
    assert!(quote_identifier("").is_err());
    assert!(quote_identifier("relation\0name").is_err());
}

#[test]
fn full_option_parser_requires_all_operational_inputs() {
    let values = [
        "--source-database-url",
        "postgresql://localhost/factory",
        "--source-runtime-root",
        "/source",
        "--restore-database-url",
        "postgresql://localhost/factory_restore_v3_1",
        "--restore-runtime-root",
        "/restore",
        "--dump-file",
        "/tmp/factory.dump",
        "--pg-dump",
        "/usr/bin/pg_dump",
        "--pg-restore",
        "/usr/bin/pg_restore",
        "--psql",
        "/usr/bin/psql",
        "--cargo",
        "/usr/bin/cargo",
    ]
    .map(str::to_owned);
    let parsed = parse_options(&values).unwrap();
    assert_eq!(parsed.source_runtime_root, PathBuf::from("/source"));
    assert_eq!(parsed.restore_runtime_root, PathBuf::from("/restore"));
    assert!(parse_options(&values[..values.len() - 2]).is_err());
}
