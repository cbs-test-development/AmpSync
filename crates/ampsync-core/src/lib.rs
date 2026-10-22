//! AmpSync core: everything except the user interface.
//!
//! The public API of this crate is specified in `docs/ARCHITECTURE.md`; change the document before
//! changing a signature here.

pub mod appdirs;
pub mod auth;
pub mod engine;
pub mod errors;
pub mod hash;
pub mod link;
pub mod names;
pub mod plan;
pub mod purge;
pub mod remote;
pub mod settings;
pub mod state;

pub use auth::{SessionStatus, check_session};
pub use errors::Error;
pub use purge::{PurgeReport, purge_all};
