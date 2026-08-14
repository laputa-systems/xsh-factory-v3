//! Closed, product-neutral values shared at Factory V3 process boundaries.
//!
//! This crate deliberately contains no storage, process, Git, or application
//! implementation. Its values make the kernel/application boundary explicit;
//! application-specific source is compiled outside this crate into
//! [`ApplicationBundleV2`].

mod application;
mod candidate;
mod decision;
mod error;
mod forum;
mod harness;
mod identifier;
mod institutional;
mod path;
mod process;
mod revision;
mod state;
mod ticket;
mod value;
mod wire;

pub use application::*;
pub use candidate::*;
pub use decision::*;
pub use error::*;
pub use forum::*;
pub use harness::*;
pub use identifier::*;
pub use institutional::*;
pub use path::*;
pub use process::*;
pub use revision::*;
pub use state::*;
pub use ticket::*;
pub use value::*;
pub use wire::*;
