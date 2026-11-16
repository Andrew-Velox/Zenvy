use serde::{Deserialize, Serialize};

/// A Hyprland window address, validated so it can never smuggle text into a Lua/dispatch string.
/// Always stored in canonical `0x<hex>` form.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Address(String);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid window address")]
pub struct InvalidAddress;

impl Address {
    /// Accepts `0x1f2e` or the prefix-less `1f2e` that Hyprland's event socket uses.
    pub fn parse(s: &str) -> Result<Self, InvalidAddress> {
        let hex = s.strip_prefix("0x").unwrap_or(s);
        if hex.is_empty() || hex.len() > 16 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(InvalidAddress);
        }
        Ok(Self(format!("0x{}", hex.to_ascii_lowercase())))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Address {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Address::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// Highest workspace id the agent will accept from a client.
pub const MAX_WORKSPACE_ID: i64 = 1_000_000;

/// The only things a phone may ask the desktop to do. Typed on purpose: no free-form strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum DesktopCommand {
    SwitchWorkspace { id: i64 },
    FocusWindow { address: Address },
    CloseWindow { address: Address },
    ToggleFloating { address: Address },
    MoveWindowToWorkspace { address: Address, workspace: i64 },
}

impl DesktopCommand {
    /// Range checks that serde cannot express.
    pub fn validate(&self) -> Result<(), &'static str> {
        let ok = |id: &i64| (1..=MAX_WORKSPACE_ID).contains(id);
        match self {
            Self::SwitchWorkspace { id } if !ok(id) => Err("workspace id out of range"),
            Self::MoveWindowToWorkspace { workspace, .. } if !ok(workspace) => {
                Err("workspace id out of range")
            }
            _ => Ok(()),
        }
    }
}

/// Monitor info for the phone. There is deliberately **no serial field**: hardware identifiers
/// are never captured (PLAN.md §6.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Monitor {
    pub connector: String,
    pub make: String,
    pub model: String,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: f64,
    pub scale: f64,
    pub transform: i32,
    pub x: i32,
    pub y: i32,
    pub focused: bool,
    pub active_workspace: Option<i64>,
    /// The special workspace (scratchpad) currently shown over this monitor, if any. Special workspaces have
    /// negative ids; one that is not shown is hidden on purpose (for example Hyprland rules that park helper windows).
    #[serde(default)]
    pub special_workspace: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: i64,
    pub name: String,
    pub monitor: String,
    pub windows: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    pub address: Address,
    pub class: String,
    pub title: String,
    pub workspace: i64,
    pub floating: bool,
    pub monitor: i64,
    pub mapped: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_accepts_both_forms_and_canonicalises() {
        assert_eq!(
            Address::parse("55F131746E40").unwrap().as_str(),
            "0x55f131746e40"
        );
        assert_eq!(
            Address::parse("0x55f131746e40").unwrap().as_str(),
            "0x55f131746e40"
        );
    }

    #[test]
    fn address_rejects_injection_attempts() {
        for bad in [
            "",
            "0x",
            "0xzz",
            "0x1 })",
            "1f; os.execute('x')",
            "0x12345678901234567",
            "\"",
            "class:kitty",
        ] {
            assert!(Address::parse(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn address_validated_on_deserialize() {
        assert!(serde_json::from_str::<DesktopCommand>(
            r#"{"cmd":"close_window","address":"0x1\"})"}"#
        )
        .is_err());
        assert!(serde_json::from_str::<DesktopCommand>(
            r#"{"cmd":"close_window","address":"0xabc"}"#
        )
        .is_ok());
    }

    #[test]
    fn workspace_range_checked() {
        assert!(DesktopCommand::SwitchWorkspace { id: 0 }
            .validate()
            .is_err());
        assert!(DesktopCommand::SwitchWorkspace { id: -99 }
            .validate()
            .is_err());
        assert!(DesktopCommand::SwitchWorkspace { id: 3 }.validate().is_ok());
    }

    #[test]
    fn unknown_command_rejected() {
        assert!(serde_json::from_str::<DesktopCommand>(r#"{"cmd":"exec","argv":["x"]}"#).is_err());
    }
}
