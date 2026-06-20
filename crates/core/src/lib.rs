//! session-manager core: supervisor + backend trait + OS layer.
//!
//! The crate is platform-agnostic above the [`os`] module. Everything that
//! differs per OS lives behind the [`os::OsLayer`] trait.

pub mod backend;
pub mod config;
pub mod error;
pub mod events;
#[cfg(unix)]
pub mod ipc;
pub mod os;
pub mod paths;
pub mod state;
pub mod supervisor;

pub use config::{PermissionMode, ResumeMode, SessionConfig, SessionDefaults, SessionsFile};
pub use error::{Error, Result};
pub use events::{CoreEvent, SessionStatus};
pub use state::{RuntimeState, SessionRuntime};
pub use supervisor::Supervisor;
