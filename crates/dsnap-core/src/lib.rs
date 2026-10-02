//! D-Snap core library: content-addressed snapshots, diffs and restores of a project folder.
//!
//! The public entry point is [`Dsnap`]. Each feature module adds its methods in its own
//! `impl Dsnap` block, so the owning chain edits only its own file. Module owners (Andare
//! chains) are noted in each module's docs.
#![warn(missing_docs)]

pub mod error;
pub mod paths;
pub mod types;

mod facade;

pub mod db;
pub mod diff;
pub mod ignore_rules;
pub mod lock;
pub mod store;
pub mod walk;

pub mod auto;
pub mod projects;
pub mod restore;
pub mod retention;
pub mod settings;
pub mod snapshot;
pub mod status;
pub mod versions;
pub mod watch;

pub use error::{Error, Result};
pub use facade::Dsnap;
pub use paths::Home;
pub use types::*;
