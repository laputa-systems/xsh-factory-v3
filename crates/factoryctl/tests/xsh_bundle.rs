//! Provider-free qualification of the Rust XSH application compiler seam.

#[path = "../src/xsh_bundle.rs"]
mod xsh_bundle;

use std::path::PathBuf;

use factory_protocol::ThinkingLevelV2;

#[test]
fn static_xsh_bundle_compiles_deterministically_with_all_sources() {
    let source_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../applications/xsh");
    let first = xsh_bundle::compile_xsh_bundle(&source_root).expect("compile XSH V2 source");
    let second = xsh_bundle::compile_xsh_bundle(&source_root).expect("compile XSH V2 source twice");

    assert_eq!(first, second);
    assert_eq!(
        first.application.files.len(),
        xsh_bundle::XSH_SOURCE_PATHS.len()
    );
    assert_eq!(first.application.bundle.format_version, 2);
    assert_eq!(first.application.bundle.application_key.as_str(), "xsh");
    assert!(
        first
            .application
            .file(
                &factory_protocol::ApplicationRelativePath::parse("policies/engineering.luau")
                    .expect("policy path")
            )
            .is_some()
    );
    assert!(
        first
            .application
            .bundle
            .assignment_role_profiles
            .iter()
            .all(|profile| profile.model.output_token_limit <= 32_768)
    );
    assert_eq!(
        first
            .application
            .bundle
            .assignment_role_profiles
            .first()
            .expect("Product profile")
            .model
            .thinking_level,
        ThinkingLevelV2::None
    );
    assert!(
        first
            .application
            .bundle
            .assignment_role_profiles
            .iter()
            .skip(1)
            .all(|profile| profile.model.thinking_level == ThinkingLevelV2::Low)
    );
    assert!(!first.canonical_bundle.contains(&b'\n'));
    assert!(!first.canonical_bundle.contains(&b'\r'));
}
