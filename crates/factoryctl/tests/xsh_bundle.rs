//! Provider-free qualification of the Rust XSH application compiler seam.

#[path = "../src/xsh_bundle.rs"]
mod xsh_bundle;

use std::path::PathBuf;

use factory_protocol::{AssignmentRole, ThinkingLevelV2};

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
    assert_eq!(first.application.bundle.ticket_policy.maximum, None);
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
            .all(|profile| profile.model.output_token_limit == 32_768)
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
    let engineering = first
        .application
        .bundle
        .assignment_role_profiles
        .iter()
        .find(|profile| profile.assignment_role == AssignmentRole::Engineering)
        .expect("Engineering profile");
    assert_eq!(engineering.model.thinking_level, ThinkingLevelV2::Low);
    assert_eq!(
        engineering.model.model_id,
        "deepseek/deepseek-v4-flash-0731"
    );
    assert_eq!(engineering.model.context_token_limit, 1_048_576);
    assert_eq!(
        engineering
            .model
            .price_input_micro_usd_per_million_tokens
            .get(),
        90_000
    );
    assert_eq!(
        engineering
            .model
            .price_output_micro_usd_per_million_tokens
            .get(),
        180_000
    );
    assert_eq!(
        engineering
            .model
            .price_cache_read_micro_usd_per_million_tokens
            .get(),
        18_000
    );
    assert_eq!(
        engineering
            .model
            .price_cache_write_micro_usd_per_million_tokens
            .get(),
        0
    );
    assert_eq!(
        first
            .application
            .bundle
            .assignment_role_profiles
            .iter()
            .find(|profile| profile.assignment_role == AssignmentRole::Quality)
            .expect("Quality profile")
            .model
            .thinking_level,
        ThinkingLevelV2::Low
    );
    assert!(!first.canonical_bundle.contains(&b'\n'));
    assert!(!first.canonical_bundle.contains(&b'\r'));
}
