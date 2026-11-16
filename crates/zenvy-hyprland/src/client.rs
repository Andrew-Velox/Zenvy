use serde::de::DeserializeOwned;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use zenvy_core::{Error, Result};

const TIMEOUT: Duration = Duration::from_secs(2);
const MAX_REPLY: usize = 4 * 1024 * 1024;

/// Finds the live Hyprland instance directory.
///
/// `HYPRLAND_INSTANCE_SIGNATURE` goes stale when Hyprland restarts, so if that directory has no
/// socket we fall back to the most recently modified instance under `$XDG_RUNTIME_DIR/hypr/`.
pub fn resolve_instance_dir() -> Result<PathBuf> {
    let rt = std::env::var_os("XDG_RUNTIME_DIR")
        .ok_or_else(|| Error::NotReachable("XDG_RUNTIME_DIR not set".into()))?;
    let base = Path::new(&rt).join("hypr");
    if let Some(sig) = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE") {
        let dir = base.join(sig);
        if dir.join(".socket.sock").exists() {
            return Ok(dir);
        }
    }
    let newest = std::fs::read_dir(&base)
        .map_err(|e| Error::NotReachable(format!("{}: {e}", base.display())))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join(".socket.sock").exists())
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
    newest
        .map(|e| e.path())
        .ok_or_else(|| Error::NotReachable("no running Hyprland instance found".into()))
}

/// Request/response client. Hyprland's command socket is one-shot, so each request connects anew.
#[derive(Clone, Debug)]
pub struct HyprClient {
    dir: PathBuf,
}

impl HyprClient {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn from_env() -> Result<Self> {
        Ok(Self::new(resolve_instance_dir()?))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub async fn request(&self, cmd: &str) -> Result<String> {
        let fut = async {
            let mut s = UnixStream::connect(self.dir.join(".socket.sock")).await?;
            s.write_all(cmd.as_bytes()).await?;
            let mut out = Vec::new();
            (&mut s)
                .take(MAX_REPLY as u64)
                .read_to_end(&mut out)
                .await?;
            Ok::<_, std::io::Error>(String::from_utf8_lossy(&out).into_owned())
        };
        tokio::time::timeout(TIMEOUT, fut)
            .await
            .map_err(|_| Error::Timeout(TIMEOUT))?
            .map_err(Error::Io)
    }

    pub async fn json<T: DeserializeOwned>(&self, query: &str) -> Result<T> {
        let raw = self.request(&format!("j/{query}")).await?;
        serde_json::from_str(&raw).map_err(|_| Error::BadReply(raw.chars().take(120).collect()))
    }
}
