//! Shared Factory runtime settings.
//!
//! This crate is deliberately dependency-free.  It owns bounded runtime policy
//! knobs that must agree across the daemon, kernel, and actor host.  Wire
//! protocol versions, operation names, persisted state codes, and evidence
//! identity strings remain in their owning modules because those values are
//! contract identities rather than operator settings.

#![forbid(unsafe_code)]
#![allow(missing_docs)]

pub mod settings;

pub use settings::*;
