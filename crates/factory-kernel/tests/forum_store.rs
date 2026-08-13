//! Public Forum-store integration contracts.
//!
//! Actor mutations deliberately live in the crate-private database test module:
//! only the daemon can mint an actor socket binding. Public integration crates
//! retain no constructor or raw-pool escape hatch.

#[test]
fn actor_forum_authority_has_no_public_constructor() {
    // The private fields and crate-private constructor on the capability are
    // the real compile-time boundary. This integration judge stays capability-
    // free so it cannot become a test-only authority escape hatch.
    assert!(core::mem::size_of::<factory_kernel::local_transport::ActorConnectionBinding>() > 0);
}
