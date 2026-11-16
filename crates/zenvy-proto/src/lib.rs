//! Wire/domain types shared by the agent and (via generated TS types later) the phone UI.
//! No I/O here. Nothing in this crate may carry hardware identifiers (serials, machine ids).

mod capability;
mod desktop;
mod input;
mod profile;
mod system;

pub use capability::*;
pub use desktop::*;
pub use input::*;
pub use profile::*;
pub use system::*;

/// Protocol version carried in every envelope.
pub const PROTO_VERSION: u32 = 1;

/// WebSocket message envelope (see PLAN.md §10.2).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Envelope<T> {
    pub v: u32,
    pub t: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    pub data: T,
}
