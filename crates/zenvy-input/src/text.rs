use std::process::Stdio;
use std::time::Duration;
use zenvy_core::{Error, Result};

/// How text (not shortcuts) is entered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextBackend {
    /// `wtype`: Wayland virtual-keyboard with a generated keymap, so any Unicode works (verified in Phase 0).
    Wtype,
    /// Raw uinput keycodes: ASCII only and keyboard-layout dependent. Explicitly partial.
    UinputAscii,
    None,
}

/// Runs `wtype -- <text>` with a fixed argv (no shell) and a hard timeout.
pub async fn wtype(text: &str) -> Result<()> {
    let child = tokio::process::Command::new("wtype")
        .arg("--") // so text starting with '-' is typed, never parsed as an option
        .arg(text)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(Error::Io)?;
    let t = Duration::from_secs(5);
    match tokio::time::timeout(t, child.wait_with_output()).await {
        Err(_) => Err(Error::Timeout(t)),
        Ok(Err(e)) => Err(Error::Io(e)),
        Ok(Ok(o)) if o.status.success() => Ok(()),
        Ok(Ok(o)) => Err(Error::Rejected(format!("wtype exited with {}", o.status))),
    }
}
