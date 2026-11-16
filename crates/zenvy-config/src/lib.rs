//! `~/.config/zenvy/config.toml` loading (PLAN.md §7.5). Unknown keys warn, they never crash;
//! values that would break the privacy stance (telemetry) are hard errors.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {0}: {1}")]
    Read(PathBuf, std::io::Error),
    #[error("{0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid config: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Server {
    pub bind: Vec<String>,
    pub port: u16,
    pub tls: String,
    pub allow_remote: bool,
}
impl Default for Server {
    fn default() -> Self {
        Self {
            bind: vec!["auto".into()],
            port: 7878,
            tls: "self-signed".into(),
            allow_remote: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Privacy {
    pub telemetry: bool,
    pub expose_monitor_models: bool,
}
impl Default for Privacy {
    fn default() -> Self {
        Self {
            telemetry: false,
            expose_monitor_models: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Security {
    pub pairing_ttl_seconds: u64,
    pub session_ttl_hours: u64,
    pub max_devices: usize,
    pub require_approval_for: Vec<String>,
}
impl Default for Security {
    fn default() -> Self {
        Self {
            pairing_ttl_seconds: 120,
            session_ttl_hours: 24,
            max_devices: 5,
            require_approval_for: ["power", "shortcut.custom", "exec", "file.download"]
                .map(String::from)
                .to_vec(),
        }
    }
}

/// A user-defined action: fixed argv, never a shell string.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Action {
    pub id: String,
    pub label: String,
    pub argv: Vec<String>,
    #[serde(default)]
    pub confirm: bool,
}

/// Lock screen detection and the power sheet.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Lock {
    /// Process names that only run while the screen is locked (matched against `/proc/<pid>/comm`).
    pub processes: Vec<String>,
    /// Optional authoritative detector: exit 0 = locked, exit 1 = unlocked, anything else = unknown.
    pub detect_cmd: Vec<String>,
    /// Ask Hyprland whether a session lock is active (the reliable signal; leave on).
    pub use_compositor: bool,
    /// Allow a phone with the `unlock` permission to type the password on the lock screen.
    pub unlock_from_phone: bool,
    /// Seconds between a confirmed suspend/reboot/power-off and its execution (cancellable meanwhile).
    pub power_delay_seconds: u64,
}
impl Default for Lock {
    fn default() -> Self {
        Self {
            processes: ["hyprlock", "swaylock", "gtklock", "waylock", "i3lock"]
                .map(String::from)
                .to_vec(),
            detect_cmd: Vec::new(),
            use_compositor: true,
            unlock_from_phone: true,
            power_delay_seconds: 3,
        }
    }
}

/// Optional browser-tab bridge (loopback only; needs the Zenvy browser extension).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Browser {
    pub enabled: bool,
    pub port: u16,
}
impl Default for Browser {
    fn default() -> Self {
        Self {
            enabled: true,
            port: 7879,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub schema_version: u32,
    pub server: Server,
    pub privacy: Privacy,
    /// capability id -> backend name (override of automatic selection)
    pub backends: BTreeMap<String, String>,
    /// module id -> enabled
    pub features: BTreeMap<String, bool>,
    pub security: Security,
    pub browser: Browser,
    pub lock: Lock,
    pub actions: Vec<Action>,
}

const KNOWN_TOP: &[&str] = &[
    "schema_version",
    "server",
    "privacy",
    "backends",
    "features",
    "security",
    "browser",
    "lock",
    "files",
    "actions",
];

pub struct Loaded {
    pub config: Config,
    pub warnings: Vec<String>,
}

pub fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".config")))?;
    Some(base.join("zenvy").join("config.toml"))
}

pub fn parse(text: &str) -> Result<Loaded, ConfigError> {
    let table: toml::Table = text.parse()?;
    let mut warnings: Vec<String> = table
        .keys()
        .filter(|k| !KNOWN_TOP.contains(&k.as_str()))
        .map(|k| format!("unknown config key '{k}' ignored"))
        .collect();
    let mut config: Config = toml::from_str(text)?;
    if config.schema_version == 0 {
        config.schema_version = 1;
    }
    if config.schema_version > 1 {
        warnings.push(format!(
            "config schema_version {} is newer than this build understands (1)",
            config.schema_version
        ));
    }
    if config.privacy.telemetry {
        return Err(ConfigError::Invalid(
            "privacy.telemetry = true: Zenvy has no telemetry; remove the key".into(),
        ));
    }
    if config.server.allow_remote
        && config
            .server
            .bind
            .iter()
            .any(|b| b == "auto" || b.starts_with("0.0.0.0") || b.starts_with("[::]"))
    {
        return Err(ConfigError::Invalid(
            "allow_remote = true requires explicit bind addresses (e.g. the tailscale0 address), not 'auto' or a wildcard".into(),
        ));
    }
    for a in &config.actions {
        if a.argv.is_empty() || a.id.is_empty() {
            return Err(ConfigError::Invalid(format!(
                "action '{}' needs an id and a non-empty argv",
                a.id
            )));
        }
    }
    Ok(Loaded { config, warnings })
}

/// Missing file => defaults (not an error).
pub fn load(path: &Path) -> Result<Loaded, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(t) => parse(&t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Loaded {
            config: Config {
                schema_version: 1,
                ..Default::default()
            },
            warnings: vec![],
        }),
        Err(e) => Err(ConfigError::Read(path.into(), e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_gives_defaults() {
        let d = tempfile::tempdir().unwrap();
        let l = load(&d.path().join("nope.toml")).unwrap();
        assert_eq!(l.config.server.port, 7878);
        assert!(!l.config.privacy.telemetry);
    }

    #[test]
    fn sample_from_plan_parses() {
        let l = parse(
            r#"schema_version = 1
[server]
bind = ["192.168.0.230"]
port = 7878
[backends]
"input.pointer" = "uinput"
[features]
touchpad = true
[[actions]]
id = "lock"
label = "Lock"
argv = ["loginctl", "lock-session"]
"#,
        )
        .unwrap();
        assert_eq!(l.config.backends["input.pointer"], "uinput");
        assert_eq!(l.config.actions[0].argv[0], "loginctl");
        assert!(l.warnings.is_empty());
    }

    #[test]
    fn unknown_keys_warn() {
        let l = parse("bogus = 1\n").unwrap();
        assert_eq!(l.warnings.len(), 1);
    }

    #[test]
    fn telemetry_is_refused() {
        assert!(matches!(
            parse("[privacy]\ntelemetry = true\n"),
            Err(ConfigError::Invalid(_))
        ));
    }

    #[test]
    fn remote_needs_explicit_bind() {
        assert!(parse("[server]\nallow_remote = true\n").is_err());
        assert!(parse("[server]\nallow_remote = true\nbind = [\"100.64.0.1\"]\n").is_ok());
    }

    #[test]
    fn browser_bridge_defaults_and_override() {
        let d = parse("").unwrap().config.browser;
        assert!(d.enabled && d.port == 7879);
        let c = parse("[browser]\nenabled = false\nport = 9000\n").unwrap();
        assert!(
            !c.config.browser.enabled && c.config.browser.port == 9000 && c.warnings.is_empty()
        );
    }

    #[test]
    fn actions_need_argv() {
        assert!(parse("[[actions]]\nid=\"x\"\nlabel=\"x\"\nargv=[]\n").is_err());
    }
}
