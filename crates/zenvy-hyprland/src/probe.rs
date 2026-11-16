use crate::client::HyprClient;
use crate::dispatch::DispatchMode;
use crate::state::HyprlandAdapter;
use tokio::net::UnixStream;
use zenvy_core::Compositor;
use zenvy_proto::{CapState, CapabilityRecord};

/// Runs the Hyprland probes (PLAN.md §7.2). Side-effect free: the dispatch probe sends a
/// deliberately invalid dispatcher. Returns `(capability id, record)` pairs.
pub async fn probe_all() -> Vec<(String, CapabilityRecord)> {
    let mut out = Vec::new();
    let mut push = |id: &str, r: CapabilityRecord| out.push((id.to_string(), r));

    let client = match HyprClient::from_env() {
        Ok(c) => c,
        Err(e) => {
            let r = CapabilityRecord::not(CapState::Unavailable, "hyprland-socket", e.to_string());
            for id in [
                "hyprland.ipc",
                "hyprland.events",
                "hyprland.dispatch",
                "monitor.info",
                "workspace.manage",
                "window.manage",
            ] {
                push(id, r.clone());
            }
            return out;
        }
    };

    push(
        "hyprland.ipc",
        match client.request("j/version").await {
            Ok(v) => {
                let ver = serde_json::from_str::<serde_json::Value>(&v)
                    .ok()
                    .and_then(|j| j["version"].as_str().map(String::from));
                CapabilityRecord::available(format!(
                    "hyprland-socket {}",
                    ver.unwrap_or_else(|| "?".into())
                ))
            }
            Err(e) => CapabilityRecord::not(CapState::Error, "hyprland-socket", e.to_string()),
        },
    );

    push(
        "hyprland.events",
        match UnixStream::connect(client.dir().join(".socket2.sock")).await {
            Ok(_) => CapabilityRecord::available("socket2"),
            Err(e) => CapabilityRecord::not(CapState::Error, "socket2", e.to_string()),
        },
    );

    let adapter = HyprlandAdapter::connect(client).await;
    push(
        "hyprland.dispatch",
        match &adapter {
            Ok(a) => CapabilityRecord::available(match a.mode() {
                DispatchMode::Lua => "dispatch-lua",
                DispatchMode::Legacy => "dispatch-legacy",
            }),
            Err(e) => CapabilityRecord::not(CapState::Error, "dispatch", e.to_string()),
        },
    );

    match adapter {
        Ok(a) => {
            push(
                "monitor.info",
                match a.monitors().await {
                    Ok(m) if !m.is_empty() => CapabilityRecord::available("hyprland"),
                    Ok(_) => CapabilityRecord::not(
                        CapState::Unavailable,
                        "hyprland",
                        "no monitors reported",
                    ),
                    Err(e) => CapabilityRecord::not(CapState::Error, "hyprland", e.to_string()),
                },
            );
            push(
                "workspace.manage",
                match a.workspaces().await {
                    Ok(_) => CapabilityRecord::available("hyprland"),
                    Err(e) => CapabilityRecord::not(CapState::Error, "hyprland", e.to_string()),
                },
            );
            push(
                "window.manage",
                match a.windows().await {
                    Ok(_) => CapabilityRecord::available("hyprland"),
                    Err(e) => CapabilityRecord::not(CapState::Error, "hyprland", e.to_string()),
                },
            );
        }
        Err(e) => {
            let r = CapabilityRecord::not(
                CapState::Error,
                "hyprland",
                format!("dispatch dialect unknown: {e}"),
            );
            for id in ["monitor.info", "workspace.manage", "window.manage"] {
                push(id, r.clone());
            }
        }
    }
    out
}
