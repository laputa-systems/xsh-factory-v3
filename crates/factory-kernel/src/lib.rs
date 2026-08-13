//! Factory kernel authority and custody boundary.
//!
//! The kernel owns PostgreSQL durable transitions and CAS custody. Process
//! adapters invoke its typed commands; application and actor code never see a
//! raw database pool or arbitrary write surface.

use factory_protocol::{ApplicationBundleV1, ContractError};

/// Authenticated local-operator application registration, inspection, and
/// activation. This module is never linked from an actor route.
pub mod application_rpc;
pub mod assignment_runtime;
pub mod campaign_driver;
pub mod candidate_runtime;
pub mod cas;
pub mod command_supervision;
pub mod decision_store;
pub mod durable_authority;
mod forum_rpc;
pub mod forum_store;
pub mod git;
pub mod installed_runtime;
pub mod local_transport;
/// Authenticated operator adoption of a bounded regular source file into the
/// daemon-owned CAS, followed by ordinary immutable artifact registration.
pub mod operator_artifact_rpc;
/// Grand Architect Forum adapter. It reuses the permanent Forum authority and
/// derives Grand Architect attribution from the local operator socket.
pub mod operator_forum_rpc;
/// Fixed, bounded ticket/candidate/audit navigation on the local operator
/// socket. This is not a generic query surface.
pub mod operator_navigation;
/// Trusted socket-only Grand Architect command routing and the narrow daemon
/// composition seam for exact transition context.
pub mod operator_rpc;
pub mod process;
pub mod process_custody;
pub mod product_runtime;
pub mod restart_recovery;
pub mod scheduler;
pub mod session_runtime;
pub mod storage;
pub mod ticket_store;
pub mod workspace_read;

#[cfg(test)]
#[path = "forum_store_database_tests.rs"]
mod forum_store_database_tests;

/// Validates the generic portion of an application bundle before kernel
/// admission adds artifact custody and durable identity.
pub fn validate_bundle_contract(bundle: &ApplicationBundleV1) -> Result<(), ContractError> {
    bundle.validate()
}
