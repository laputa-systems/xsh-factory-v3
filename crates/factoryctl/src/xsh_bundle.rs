//! Rust-only compiler for the checked-in XSH V2 application source.
//!
//! The CLI wiring is intentionally kept in `main.rs` for now. This module is
//! the narrow reusable seam that wiring must call: it reads one explicit static
//! source root, admits the canonical V2 declaration, and supplies exactly the
//! seven Markdown templates and three Luau role policies to
//! `ApplicationCompilerV2`. It performs no database, CAS, network, or daemon
//! operation.
#![allow(dead_code)]

use std::{error::Error, fs, path::Path};

use factory_protocol::{
    ApplicationCompilerV2, ApplicationRelativePath, ApplicationSourceFileV2, CompiledApplicationV2,
    parse_application_bundle_v2,
};

/// The static XSH declaration filename.
pub const XSH_BUNDLE_FILENAME: &str = "bundle.v2.json";

/// The complete and deliberately closed XSH source inventory.
///
/// The compiler does not recursively discover application files. A source
/// file must be one of these ten paths and must also be referenced by the V2
/// bundle. This prevents an unreviewed policy/template from entering the
/// sealed application identity.
pub const XSH_SOURCE_PATHS: [&str; 10] = [
    "templates/mission.md",
    "templates/product-system.md",
    "templates/product-assignment.md",
    "templates/engineering-system.md",
    "templates/engineering-assignment.md",
    "templates/quality-system.md",
    "templates/quality-assignment.md",
    "policies/product_research.luau",
    "policies/engineering.luau",
    "policies/quality.luau",
];

/// Output of one deterministic XSH compiler invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledXshBundle {
    /// Canonical V2 JSON bytes, without the source file's optional final LF.
    pub canonical_bundle: Vec<u8>,
    /// Sealed templates and policies with their deterministic source identity.
    pub application: CompiledApplicationV2,
}

/// Compile the checked-in XSH source beneath `source_root`.
///
/// The bundle file is required to already be canonical V2 JSON. A single final
/// LF is accepted as a source-file convention and removed before parsing and
/// returning `canonical_bundle`; any other whitespace remains rejected by the
/// protocol parser. All ten source artifacts are read explicitly and passed to
/// the Rust compiler, which verifies every declared digest and byte ceiling.
pub fn compile_xsh_bundle(source_root: &Path) -> Result<CompiledXshBundle, Box<dyn Error>> {
    let bundle_path = source_root.join(XSH_BUNDLE_FILENAME);
    let bundle_source = fs::read(&bundle_path)?;
    let canonical_bundle = bundle_source
        .strip_suffix(b"\n")
        .unwrap_or(bundle_source.as_slice());
    let bundle = parse_application_bundle_v2(canonical_bundle)?;

    let source_files = XSH_SOURCE_PATHS
        .iter()
        .map(|relative| {
            let path = ApplicationRelativePath::parse((*relative).to_owned())?;
            let bytes = fs::read(source_root.join(relative))?;
            Ok(ApplicationSourceFileV2::new(path, bytes)?)
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    let application = ApplicationCompilerV2::compile(bundle, source_files)?;

    Ok(CompiledXshBundle {
        canonical_bundle: canonical_bundle.to_vec(),
        application,
    })
}
