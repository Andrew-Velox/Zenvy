//! Network layer: TLS, HTTPS pairing endpoint, authenticated WebSocket, local admin socket.

mod actions;
mod admin;
mod app;
mod browser;
mod listen;
mod power;
mod server;
mod state;
pub mod tls;
mod ws;

pub use actions::ActionDef;
pub use admin::serve_admin;
pub use app::{App, ApprovalRequest, Hub, NetConfig};
pub use browser::{load_or_create_token, serve_browser, BrowserCommand, BrowserHub};
pub use listen::ListenMute;
pub use power::PowerRuntime;
pub use server::serve;
pub use state::spawn_publisher;
