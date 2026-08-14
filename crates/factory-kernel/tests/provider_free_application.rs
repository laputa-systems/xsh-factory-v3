//! Provider-free admission coverage for the sealed Rust application contract.
//!
//! The application compiler is a Rust build artifact.  This test reads the
//! checked-in XSH bundle produced by that compiler, parses it through the
//! exact kernel-facing V2 boundary, and verifies that repeated admission is
//! byte-for-byte deterministic without constructing a provider or contacting
//! a network service.

use std::fs;

use factory_protocol::{ContentDigest, parse_application_bundle_v2};

fn xsh_bundle() -> Vec<u8> {
    let source = fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../applications/xsh/bundle.v2.json"
    ))
    .expect("Rust-compiled XSH bundle is present");
    // The static source accepts one conventional trailing LF; the Rust
    // compiler removes it before producing the canonical admission bytes.
    source
        .strip_suffix(b"\n")
        .unwrap_or(source.as_slice())
        .to_vec()
}

#[test]
fn rust_compiled_xsh_bundle_is_canonical_and_provider_free() {
    let first = xsh_bundle();
    let second = xsh_bundle();
    assert_eq!(
        first, second,
        "sealed application bytes must be deterministic"
    );

    let bundle = parse_application_bundle_v2(&first)
        .expect("the Rust application bundle must satisfy the closed V2 contract");
    assert_eq!(bundle.application_key.as_str(), "xsh");
    assert_eq!(bundle.assignment_role_profiles.len(), 3);
    assert!(
        bundle.assignment_role_profiles.iter().all(|profile| profile
            .policy
            .source_path
            .as_str()
            .starts_with("policies/")),
        "every role must carry a sealed Rust-host policy artifact"
    );

    let digest = ContentDigest::of_bytes(&first);
    assert_eq!(digest, ContentDigest::of_bytes(&second));
}

#[test]
fn rust_compiled_xsh_bundle_rejects_noncanonical_bytes() {
    let mut noncanonical = xsh_bundle();
    noncanonical.push(b'\n');
    assert!(
        parse_application_bundle_v2(&noncanonical).is_err(),
        "the authority must reject whitespace or alternate serialization"
    );
}
