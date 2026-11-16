use crate::Result;
use std::future::Future;
use zenvy_proto::{DesktopCommand, InputCommand, Monitor, Window, Workspace};

/// Compositor backend (Hyprland now; Sway etc. later). Static dispatch only for now.
#[allow(async_fn_in_trait)]
pub trait Compositor: Send + Sync {
    fn name(&self) -> &'static str;
    async fn monitors(&self) -> Result<Vec<Monitor>>;
    async fn workspaces(&self) -> Result<Vec<Workspace>>;
    async fn windows(&self) -> Result<Vec<Window>>;
    /// Executes a typed, already-validated command.
    fn execute(&self, cmd: &DesktopCommand) -> impl Future<Output = Result<()>> + Send;
}

/// Pointer/keyboard injection backend.
#[allow(async_fn_in_trait)]
pub trait Input: Send + Sync {
    fn name(&self) -> &'static str;
    /// Executes one validated command.
    fn execute(&self, cmd: &InputCommand) -> impl Future<Output = Result<()>> + Send;
}
