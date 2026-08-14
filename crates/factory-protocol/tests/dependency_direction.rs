use std::{fs, path::Path};

#[test]
fn generic_rust_source_has_no_product_vocabulary_or_application_source_dependency() {
    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crate is nested below the repository root");
    let crate_root = repository_root.join("crates");
    let sources = fs::read_dir(crate_root)
        .expect("crate directory is readable")
        .flat_map(|entry| {
            let entry = entry.expect("directory entry is readable");
            let name = entry.file_name();
            // The protocol, kernel, and actor host are the generic boundary.
            // `factoryctl` is the explicitly product-facing application compiler
            // entrypoint, so it is intentionally outside this direction check.
            if matches!(
                name.to_str(),
                Some("factory-protocol" | "factory-kernel" | "factory-pi-host")
            ) {
                collect_rust_sources(&entry.path().join("src"))
            } else {
                Vec::new()
            }
        })
        .collect::<Vec<_>>();

    for source in sources {
        let contents = fs::read_to_string(&source).expect("Rust source is UTF-8");
        let lower = contents.to_ascii_lowercase();
        assert!(
            !lower.contains("xsh"),
            "generic Rust source must not name product vocabulary: {}",
            source.display()
        );
        assert!(
            !contents.contains("applications/"),
            "generic Rust source must not depend on application source: {}",
            source.display()
        );
    }
}

fn collect_rust_sources(directory: &Path) -> Vec<std::path::PathBuf> {
    let mut sources = Vec::new();
    if !directory.exists() {
        return sources;
    }
    for entry in fs::read_dir(directory).expect("crate directory is readable") {
        let entry = entry.expect("directory entry is readable");
        let path = entry.path();
        if path.is_dir() {
            sources.extend(collect_rust_sources(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            sources.push(path);
        }
    }
    sources
}
