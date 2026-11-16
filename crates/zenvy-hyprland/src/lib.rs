//! Hyprland adapter: direct socket IPC (no `hyprctl` spawns), typed dispatchers, event stream.

mod client;
mod dispatch;
mod events;
mod probe;
mod state;

pub use client::{resolve_instance_dir, HyprClient};
pub use dispatch::{classify_dispatch_reply, DispatchMode};
pub use events::{parse_event, subscribe, Event, StreamItem};
pub use probe::probe_all;
pub use state::HyprlandAdapter;
