//! Derives which phone UI modules to show from the capability registry (PLAN.md §8.1).

use serde::Serialize;
use std::collections::BTreeMap;
use zenvy_proto::{CapState, RegistrySnapshot};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Module {
    pub id: &'static str,
    pub enabled: bool,
    /// Why it is disabled (shown as a hint instead of hiding silently, if the UI wants).
    pub reason: Option<String>,
}

fn ok(s: &RegistrySnapshot, id: &str) -> bool {
    s.capabilities
        .get(id)
        .is_some_and(|r| r.state == CapState::Available)
}

fn why(s: &RegistrySnapshot, ids: &[&str]) -> Option<String> {
    let parts: Vec<String> = ids
        .iter()
        .map(|id| match s.capabilities.get(*id) {
            Some(r) => format!(
                "{id}: {}{}",
                r.state.label(),
                r.reason
                    .as_ref()
                    .map(|x| format!(" ({x})"))
                    .unwrap_or_default()
            ),
            None => format!("{id}: not probed"),
        })
        .collect();
    Some(parts.join("; "))
}

/// `features` is the user's config override (`false` disables regardless of availability).
pub fn ui_manifest(s: &RegistrySnapshot, features: &BTreeMap<String, bool>) -> Vec<Module> {
    let user_on = |id: &str| features.get(id).copied().unwrap_or(true);
    // (module id, requires ALL of, or ANY-of group)
    let defs: [(&'static str, &[&str], bool); 12] = [
        ("dashboard", &[], false),
        ("touchpad", &["input.pointer"], false),
        ("keyboard", &["input.keyboard"], false),
        ("workspaces", &["workspace.manage"], false),
        ("windows", &["window.manage"], false),
        ("desktop", &["screen.capture", "window.manage"], true), // preview OR window list
        ("audio", &["audio.volume"], false),
        ("media", &["media.control"], false),
        ("clipboard", &["clipboard.sync"], false),
        ("brightness", &["brightness"], false),
        ("power", &["power.control"], false),
        ("battery", &["battery.monitor"], false),
    ];
    let mut out: Vec<Module> = defs
        .iter()
        .map(|(id, reqs, any)| {
            let cap_ok = if reqs.is_empty() {
                true
            } else if *any {
                reqs.iter().any(|r| ok(s, r))
            } else {
                reqs.iter().all(|r| ok(s, r))
            };
            let enabled = cap_ok && user_on(id);
            let reason = if enabled {
                None
            } else if !user_on(id) {
                Some("disabled in config".to_string())
            } else {
                why(s, reqs)
            };
            Module {
                id,
                enabled,
                reason,
            }
        })
        .collect();
    out.push(Module {
        id: "settings",
        enabled: true,
        reason: None,
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use zenvy_proto::CapabilityRecord;

    fn snap(items: &[(&str, CapabilityRecord)]) -> RegistrySnapshot {
        RegistrySnapshot {
            schema_version: 1,
            capabilities: items
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        }
    }

    #[test]
    fn modules_follow_capabilities() {
        let s = snap(&[
            ("input.pointer", CapabilityRecord::available("uinput")),
            (
                "battery.monitor",
                CapabilityRecord::not(CapState::Unsupported, "sysfs", "no battery present"),
            ),
        ]);
        let m = ui_manifest(&s, &BTreeMap::new());
        let get = |id| m.iter().find(|x| x.id == id).unwrap();
        assert!(get("touchpad").enabled);
        assert!(!get("battery").enabled);
        assert!(get("battery")
            .reason
            .as_ref()
            .unwrap()
            .contains("no battery present"));
        assert!(!get("keyboard").enabled); // never probed => not shown
        assert!(get("dashboard").enabled && get("settings").enabled);
    }

    #[test]
    fn config_can_disable_an_available_feature() {
        let s = snap(&[("input.pointer", CapabilityRecord::available("uinput"))]);
        let f = BTreeMap::from([("touchpad".to_string(), false)]);
        assert!(
            !ui_manifest(&s, &f)
                .iter()
                .find(|x| x.id == "touchpad")
                .unwrap()
                .enabled
        );
    }

    #[test]
    fn desktop_needs_preview_or_windows() {
        let s = snap(&[("window.manage", CapabilityRecord::available("hyprland"))]);
        assert!(
            ui_manifest(&s, &BTreeMap::new())
                .iter()
                .find(|x| x.id == "desktop")
                .unwrap()
                .enabled
        );
    }
}
