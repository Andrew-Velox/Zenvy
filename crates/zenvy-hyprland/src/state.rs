use crate::client::HyprClient;
use crate::dispatch::{self, classify_dispatch_reply, DispatchMode};
use serde::Deserialize;
use zenvy_core::{Compositor, Error, Result};
use zenvy_proto::{Address, DesktopCommand, Monitor, Window, Workspace};

// Raw Hyprland JSON. Every field is optional/defaulted: Hyprland's JSON changes between releases.
// `serial` is intentionally NOT declared, so a hardware serial can never be captured.
#[derive(Deserialize)]
struct RawWs {
    id: Option<i64>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMonitor {
    #[serde(default)]
    name: String,
    #[serde(default)]
    make: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
    #[serde(default)]
    refresh_rate: f64,
    #[serde(default)]
    scale: f64,
    #[serde(default)]
    transform: i32,
    #[serde(default)]
    x: i32,
    #[serde(default)]
    y: i32,
    #[serde(default)]
    focused: bool,
    active_workspace: Option<RawWs>,
    special_workspace: Option<RawWs>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawWorkspace {
    id: i64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    monitor: String,
    #[serde(default)]
    windows: u32,
}
#[derive(Deserialize)]
struct RawClient {
    address: String,
    #[serde(default)]
    class: String,
    #[serde(default)]
    title: String,
    workspace: Option<RawWs>,
    #[serde(default)]
    floating: bool,
    #[serde(default)]
    monitor: i64,
    #[serde(default)]
    mapped: bool,
}

impl From<RawMonitor> for Monitor {
    fn from(m: RawMonitor) -> Self {
        Monitor {
            connector: m.name,
            make: m.make,
            model: m.model,
            width: m.width,
            height: m.height,
            refresh_hz: m.refresh_rate,
            scale: if m.scale > 0.0 { m.scale } else { 1.0 },
            transform: m.transform,
            x: m.x,
            y: m.y,
            focused: m.focused,
            active_workspace: m.active_workspace.and_then(|w| w.id),
            // `{"id":0,"name":""}` means no special workspace is shown
            special_workspace: m.special_workspace.and_then(|w| w.id).filter(|id| *id != 0),
        }
    }
}

pub(crate) fn parse_monitors(json: &str) -> Result<Vec<Monitor>> {
    Ok(serde_json::from_str::<Vec<RawMonitor>>(json)?
        .into_iter()
        .map(Into::into)
        .collect())
}

/// Hyprland compositor backend.
pub struct HyprlandAdapter {
    client: HyprClient,
    mode: DispatchMode,
}

impl HyprlandAdapter {
    /// Detects the dispatch dialect by sending a deliberately invalid, non-mutating `dispatch`.
    pub async fn connect(client: HyprClient) -> Result<Self> {
        let reply = client.request("dispatch zenvy_probe_invalid").await?;
        let mode = classify_dispatch_reply(&reply).ok_or_else(|| {
            Error::BadReply(format!(
                "cannot tell dispatch dialect from: {}",
                reply.chars().take(100).collect::<String>()
            ))
        })?;
        Ok(Self { client, mode })
    }

    pub fn mode(&self) -> DispatchMode {
        self.mode
    }
    pub fn client(&self) -> &HyprClient {
        &self.client
    }
}

impl Compositor for HyprlandAdapter {
    fn name(&self) -> &'static str {
        "hyprland"
    }

    async fn monitors(&self) -> Result<Vec<Monitor>> {
        parse_monitors(&self.client.request("j/monitors").await?)
    }

    async fn workspaces(&self) -> Result<Vec<Workspace>> {
        let raw: Vec<RawWorkspace> = self.client.json("workspaces").await?;
        Ok(raw
            .into_iter()
            .map(|w| Workspace {
                id: w.id,
                name: w.name,
                monitor: w.monitor,
                windows: w.windows,
            })
            .collect())
    }

    async fn windows(&self) -> Result<Vec<Window>> {
        let raw: Vec<RawClient> = self.client.json("clients").await?;
        Ok(raw
            .into_iter()
            .filter_map(|c| {
                Some(Window {
                    address: Address::parse(&c.address).ok()?,
                    class: c.class,
                    title: c.title,
                    workspace: c.workspace.and_then(|w| w.id).unwrap_or(0),
                    floating: c.floating,
                    monitor: c.monitor,
                    mapped: c.mapped,
                })
            })
            .collect())
    }

    async fn execute(&self, cmd: &DesktopCommand) -> Result<()> {
        cmd.validate().map_err(Error::InvalidCommand)?;
        let reply = self
            .client
            .request(&dispatch::request(self.mode, cmd))
            .await?;
        if reply.trim() == "ok" {
            Ok(())
        } else {
            Err(Error::Rejected(reply.trim().chars().take(200).collect()))
        }
    }
}

impl HyprlandAdapter {
    /// Address of the focused window, if any.
    pub async fn active_window(&self) -> Result<Option<Address>> {
        let v: serde_json::Value = self.client.json("activewindow").await?;
        Ok(v.get("address")
            .and_then(|a| a.as_str())
            .and_then(|a| Address::parse(a).ok()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serial_is_never_captured() {
        let json = r#"[{"id":0,"name":"HDMI-A-1","make":"Microstep","model":"MSI MP225","serial":"TEST-SERIAL-0000",
            "width":1920,"height":1080,"refreshRate":60.0,"x":0,"y":0,"scale":1,"transform":0,"focused":true,
            "activeWorkspace":{"id":3,"name":"3"},"someFutureField":{"a":1}}]"#;
        let m = parse_monitors(json).unwrap();
        assert_eq!(m[0].connector, "HDMI-A-1");
        assert_eq!(m[0].active_workspace, Some(3));
        let out = serde_json::to_string(&m).unwrap();
        assert!(
            !out.contains("TEST-SERIAL-0000") && !out.contains("serial"),
            "{out}"
        );
    }

    #[test]
    fn a_shown_special_workspace_is_reported_and_an_absent_one_is_not() {
        let hidden = r#"[{"name":"HDMI-A-1","activeWorkspace":{"id":3,"name":"3"},"specialWorkspace":{"id":0,"name":""}}]"#;
        let shown = r#"[{"name":"HDMI-A-1","activeWorkspace":{"id":3,"name":"3"},"specialWorkspace":{"id":-99,"name":"special:scratchpad"}}]"#;
        assert_eq!(parse_monitors(hidden).unwrap()[0].special_workspace, None);
        assert_eq!(
            parse_monitors(shown).unwrap()[0].special_workspace,
            Some(-99)
        );
        assert_eq!(
            parse_monitors(r#"[{"name":"x"}]"#).unwrap()[0].special_workspace,
            None,
            "older Hyprland without the field"
        );
    }

    #[test]
    fn missing_fields_tolerated() {
        let m = parse_monitors(r#"[{"name":"eDP-1"}]"#).unwrap();
        assert_eq!(m[0].scale, 1.0);
        assert_eq!(m[0].active_workspace, None);
    }
}
