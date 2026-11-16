//! Capability registry + backend traits. Adapters implement the traits; the core never
//! shells out or touches the OS itself.

mod engine;
mod manifest;
mod registry;
mod traits;

pub use engine::{override_for, resolve};
pub use manifest::{ui_manifest, Module};
pub use registry::{CapabilityChange, Registry};
pub use traits::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("compositor not reachable: {0}")]
    NotReachable(String),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("timed out after {0:?}")]
    Timeout(std::time::Duration),
    #[error("unexpected reply: {0}")]
    BadReply(String),
    #[error("rejected by compositor: {0}")]
    Rejected(String),
    #[error("invalid command: {0}")]
    InvalidCommand(&'static str),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
