//! Audio and media types (Phase 5). Every command is typed and bounded: the phone can change volume,
//! mute, pick an output and control a media player, nothing else.

use serde::{Deserialize, Serialize};

pub const MAX_VOLUME_PERCENT: u8 = 100;
pub const MAX_SEEK_MS: i64 = 10 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDevice {
    pub name: String,
    pub description: String,
    /// Average channel volume, 0–100+ (PipeWire allows boost above 100).
    pub volume: u32,
    pub muted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputChoice {
    pub name: String,
    pub description: String,
    pub default: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioState {
    pub output: Option<AudioDevice>,
    pub mic: Option<AudioDevice>,
    pub outputs: Vec<OutputChoice>,
}

/// A PulseAudio/PipeWire device name. Restricted so it can never carry anything but a name.
pub fn valid_device_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:+@-".contains(&b))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum AudioCommand {
    SetVolume { percent: u8 },
    SetMute { muted: bool },
    SetMicMute { muted: bool },
    SetDefaultOutput { name: String },
}

impl AudioCommand {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::SetVolume { percent } if *percent > MAX_VOLUME_PERCENT => {
                Err("volume out of range")
            }
            Self::SetDefaultOutput { name } if !valid_device_name(name) => {
                Err("invalid device name")
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaPlayer {
    /// MPRIS bus name, e.g. `org.mpris.MediaPlayer2.spotify`.
    pub id: String,
    pub identity: String,
    pub status: PlaybackStatus,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub length_ms: Option<u64>,
    pub position_ms: Option<u64>,
    pub can_next: bool,
    pub can_prev: bool,
    pub can_seek: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaState {
    pub players: Vec<MediaPlayer>,
    /// The player the UI should show first (a playing one if any).
    pub current: Option<String>,
}

pub const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";

/// Must look like an MPRIS player bus name. (The agent additionally requires that it is a currently
/// running player, so a phone can never aim a command at an arbitrary D-Bus service.)
pub fn valid_player_id(s: &str) -> bool {
    s.len() <= 150
        && s.strip_prefix(MPRIS_PREFIX).is_some_and(|r| {
            !r.is_empty()
                && r.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum MediaCommand {
    PlayPause { player: String },
    Next { player: String },
    Previous { player: String },
    SeekBy { player: String, offset_ms: i64 },
}

impl MediaCommand {
    pub fn player(&self) -> &str {
        match self {
            Self::PlayPause { player }
            | Self::Next { player }
            | Self::Previous { player }
            | Self::SeekBy { player, .. } => player,
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if !valid_player_id(self.player()) {
            return Err("invalid player");
        }
        match self {
            Self::SeekBy { offset_ms, .. } if offset_ms.abs() > MAX_SEEK_MS => {
                Err("seek too large")
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse<T: serde::de::DeserializeOwned>(j: &str) -> Result<T, serde_json::Error> {
        serde_json::from_str(j)
    }

    #[test]
    fn audio_commands_validate() {
        for j in [
            r#"{"cmd":"set_volume","percent":40}"#,
            r#"{"cmd":"set_mute","muted":true}"#,
            r#"{"cmd":"set_mic_mute","muted":false}"#,
            r#"{"cmd":"set_default_output","name":"alsa_output.usb-24ae_Rapoo_Gaming_Headset-00.analog-stereo"}"#,
        ] {
            parse::<AudioCommand>(j)
                .unwrap()
                .validate()
                .unwrap_or_else(|e| panic!("{j}: {e}"));
        }
        assert!(
            parse::<AudioCommand>(r#"{"cmd":"set_volume","percent":101}"#)
                .unwrap()
                .validate()
                .is_err(),
            "no volume boost from the phone"
        );
        assert!(parse::<AudioCommand>(r#"{"cmd":"set_volume","percent":-5}"#).is_err());
        assert!(
            parse::<AudioCommand>(r#"{"cmd":"set_volume","percent":300}"#).is_err(),
            "u8 overflow is a parse error"
        );
        assert!(parse::<AudioCommand>(r#"{"cmd":"exec","argv":["x"]}"#).is_err());
        for bad in [
            "",
            "a b",
            "x;rm -rf /",
            "$(id)",
            "../../etc",
            "n\nl",
            &"a".repeat(201),
        ] {
            assert!(
                AudioCommand::SetDefaultOutput { name: bad.into() }
                    .validate()
                    .is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn media_commands_validate() {
        let p = "org.mpris.MediaPlayer2.spotify";
        assert!(MediaCommand::PlayPause { player: p.into() }
            .validate()
            .is_ok());
        assert!(MediaCommand::SeekBy {
            player: p.into(),
            offset_ms: -10_000
        }
        .validate()
        .is_ok());
        assert!(MediaCommand::SeekBy {
            player: p.into(),
            offset_ms: MAX_SEEK_MS + 1
        }
        .validate()
        .is_err());
        for bad in [
            "org.freedesktop.systemd1",
            "org.mpris.MediaPlayer2.",
            "org.mpris.MediaPlayer2.a b",
            "org.mpris.MediaPlayer2.x/y",
            "spotify",
            "",
        ] {
            assert!(
                MediaCommand::Next { player: bad.into() }
                    .validate()
                    .is_err(),
                "{bad:?}"
            );
        }
        assert!(parse::<MediaCommand>(r#"{"cmd":"call","method":"Quit"}"#).is_err());
    }
}

// ---------------------------------------------------------------- brightness

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrightnessKind {
    /// External monitor over DDC/CI (I²C).
    Ddc,
    /// Built-in panel backlight.
    Backlight,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrightnessDisplay {
    /// Stable id: `ddc:<i2c bus>` or `bl:<device>`.
    pub id: String,
    /// Hyprland connector for monitors (`HDMI-A-1`), device name for backlights.
    pub name: String,
    /// Model only. Serial numbers are never read into this type.
    pub model: String,
    pub kind: BrightnessKind,
    pub percent: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrightnessState {
    pub displays: Vec<BrightnessDisplay>,
}

pub fn valid_display_id(s: &str) -> bool {
    if let Some(n) = s.strip_prefix("ddc:") {
        return !n.is_empty() && n.len() <= 3 && n.bytes().all(|b| b.is_ascii_digit());
    }
    s.strip_prefix("bl:").is_some_and(|n| {
        !n.is_empty()
            && n.len() <= 64
            && n.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum BrightnessCommand {
    Set {
        id: String,
        percent: u8,
    },
    /// Re-read the real values (a monitor's own buttons change brightness without telling anyone).
    Refresh,
}

impl BrightnessCommand {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Set { id, .. } if !valid_display_id(id) => Err("invalid display id"),
            Self::Set { percent, .. } if *percent > 100 => Err("brightness out of range"),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod brightness_tests {
    use super::*;

    #[test]
    fn commands_validate() {
        for ok in [
            r#"{"cmd":"set","id":"ddc:2","percent":50}"#,
            r#"{"cmd":"set","id":"bl:intel_backlight","percent":0}"#,
            r#"{"cmd":"refresh"}"#,
        ] {
            serde_json::from_str::<BrightnessCommand>(ok)
                .unwrap()
                .validate()
                .unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        assert!(serde_json::from_str::<BrightnessCommand>(
            r#"{"cmd":"set","id":"ddc:2","percent":101}"#
        )
        .unwrap()
        .validate()
        .is_err());
        assert!(serde_json::from_str::<BrightnessCommand>(
            r#"{"cmd":"set","id":"ddc:2","percent":-1}"#
        )
        .is_err());
        for bad in [
            "",
            "ddc:",
            "ddc:x",
            "ddc:1234",
            "ddc:2; rm",
            "bl:",
            "bl:a b",
            "bl:../x",
            "hdmi",
            "ddc:-1",
        ] {
            assert!(!valid_display_id(bad), "{bad:?}");
        }
        assert!(serde_json::from_str::<BrightnessCommand>(r#"{"cmd":"exec"}"#).is_err());
    }
}

// ---------------------------------------------------------------- listening (PC audio to the phone)

/// Every scope a device can hold. `listen` (hearing the PC's audio) is never part of the default scopes.
pub const KNOWN_SCOPES: &[&str] = &[
    "view",
    "input",
    "windows",
    "system",
    "files",
    "clipboard",
    "listen",
    "power",
    "unlock",
    "screen",
    "exec",
];

pub fn valid_scope(s: &str) -> bool {
    KNOWN_SCOPES.contains(&s)
}

/// Binary WebSocket audio frame: `[AUDIO_FRAME_TAG][u32 little-endian sequence][one Opus packet]`.
pub const AUDIO_FRAME_TAG: u8 = 0x01;
pub const AUDIO_RATE: u32 = 48_000;
pub const AUDIO_CHANNELS: u8 = 2;
pub const AUDIO_FRAME_MS: u32 = 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum ListenCommand {
    /// Start streaming the PC's audio to this phone. `mute_pc` silences the PC speakers while listening.
    Start {
        #[serde(default)]
        mute_pc: bool,
    },
    Stop,
}

#[cfg(test)]
mod listen_tests {
    use super::*;

    #[test]
    fn scopes_and_commands() {
        for s in KNOWN_SCOPES {
            assert!(valid_scope(s));
        }
        for bad in ["", "root", "LISTEN", "view ", "listen;x", "*"] {
            assert!(!valid_scope(bad), "{bad:?}");
        }
        assert!(
            !["view", "input", "windows", "system"].contains(&"listen"),
            "listen must not be a default scope"
        );
        assert_eq!(
            serde_json::from_str::<ListenCommand>(r#"{"cmd":"start"}"#).unwrap(),
            ListenCommand::Start { mute_pc: false }
        );
        assert_eq!(
            serde_json::from_str::<ListenCommand>(r#"{"cmd":"start","mute_pc":true}"#).unwrap(),
            ListenCommand::Start { mute_pc: true }
        );
        assert!(
            serde_json::from_str::<ListenCommand>(r#"{"cmd":"record","path":"/tmp/x"}"#).is_err()
        );
    }
}

// ---------------------------------------------------------------- power and lock

/// What a phone may ask the PC to do with its power state. Nothing else is ever executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerAction {
    Lock,
    Suspend,
    Reboot,
    Poweroff,
}

impl PowerAction {
    /// Locking is harmless and instant; the rest interrupt work and need an explicit confirmation.
    pub fn needs_confirm(self) -> bool {
        !matches!(self, PowerAction::Lock)
    }
    pub fn name(self) -> &'static str {
        match self {
            PowerAction::Lock => "lock",
            PowerAction::Suspend => "suspend",
            PowerAction::Reboot => "reboot",
            PowerAction::Poweroff => "poweroff",
        }
    }
}

/// `request` starts an action (lock runs at once, the others return a confirmation token);
/// `confirm` schedules a requested action; `cancel` stops a scheduled one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum PowerCommand {
    Request { action: PowerAction },
    Confirm { token: String },
    Cancel { token: String },
}

impl PowerCommand {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            PowerCommand::Request { .. } => Ok(()),
            PowerCommand::Confirm { token } | PowerCommand::Cancel { token } => {
                if !token.is_empty()
                    && token.len() <= 64
                    && token.chars().all(|c| c.is_ascii_alphanumeric())
                {
                    Ok(())
                } else {
                    Err("bad token")
                }
            }
        }
    }
}

pub const MAX_PASSWORD_LEN: usize = 128;

/// The password a phone types for the PC's lock screen. It is typed, never stored, logged or echoed back:
/// `Debug` is redacted so it cannot end up in a log line by accident.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct UnlockCommand {
    pub password: String,
}

impl std::fmt::Debug for UnlockCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UnlockCommand { password: <redacted> }")
    }
}

impl UnlockCommand {
    /// Printable ASCII only: the lock screen is typed through the keyboard, which is the US layout.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.password.is_empty() {
            return Err("empty password");
        }
        if self.password.len() > MAX_PASSWORD_LEN {
            return Err("password too long");
        }
        if !self.password.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
            return Err("only printable ASCII characters can be typed on the lock screen");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LockState {
    Locked,
    Unlocked,
    #[default]
    Unknown,
}

/// What the phone needs to draw the power sheet and the "PC is locked" banner.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerState {
    pub lock: LockState,
    pub can_lock: bool,
    pub can_suspend: bool,
    pub can_reboot: bool,
    pub can_poweroff: bool,
    /// The PC can be unlocked by typing the password from the phone.
    pub can_unlock: bool,
}

#[cfg(test)]
mod power_tests {
    use super::*;

    #[test]
    fn actions_and_tokens() {
        assert_eq!(
            serde_json::from_str::<PowerCommand>(r#"{"cmd":"request","action":"poweroff"}"#)
                .unwrap(),
            PowerCommand::Request {
                action: PowerAction::Poweroff
            }
        );
        for bad in [
            r#"{"cmd":"request","action":"hibernate"}"#,
            r#"{"cmd":"request","action":"rm -rf /"}"#,
            r#"{"cmd":"exec","argv":["id"]}"#,
            r#"{"cmd":"request"}"#,
        ] {
            assert!(serde_json::from_str::<PowerCommand>(bad).is_err(), "{bad}");
        }
        assert!(
            !PowerAction::Lock.needs_confirm()
                && PowerAction::Suspend.needs_confirm()
                && PowerAction::Reboot.needs_confirm()
                && PowerAction::Poweroff.needs_confirm()
        );
        assert!(PowerCommand::Confirm {
            token: "ab12".into()
        }
        .validate()
        .is_ok());
        for t in ["", "a b", "x;y", &"a".repeat(65), "../x"] {
            assert!(
                PowerCommand::Confirm { token: t.into() }
                    .validate()
                    .is_err(),
                "{t:?}"
            );
        }
    }

    #[test]
    fn the_password_is_validated_and_never_printed() {
        let u = UnlockCommand {
            password: "Pa$$w0rd! ~`[]{}|;:'\",.<>/?".into(),
        };
        assert!(u.validate().is_ok());
        assert!(
            !format!("{u:?}").contains("Pa$$"),
            "Debug must not leak the password"
        );
        for bad in [
            "",
            "caf\u{e9}",
            "tab\there",
            "new\nline",
            "\u{7f}",
            &"x".repeat(129),
        ] {
            assert!(
                UnlockCommand {
                    password: bad.into()
                }
                .validate()
                .is_err(),
                "{bad:?}"
            );
        }
    }
}

// ---------------------------------------------------------------- screen preview

/// Binary WebSocket screen frame: `[SCREEN_FRAME_TAG][u32 seq LE][u16 width LE][u16 height LE][JPEG]`.
pub const SCREEN_FRAME_TAG: u8 = 0x02;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScreenPreset {
    Low,
    #[default]
    Medium,
    High,
}

impl ScreenPreset {
    /// Longest image side in pixels, JPEG quality and target frames per second.
    pub fn max_width(self) -> u32 {
        match self {
            ScreenPreset::Low => 640,
            ScreenPreset::Medium => 960,
            ScreenPreset::High => 1280,
        }
    }
    pub fn quality(self) -> u8 {
        match self {
            ScreenPreset::Low => 45,
            ScreenPreset::Medium => 55,
            ScreenPreset::High => 65,
        }
    }
    pub fn fps(self) -> u32 {
        match self {
            ScreenPreset::Low => 4,
            ScreenPreset::Medium => 8,
            ScreenPreset::High => 12,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ClickKind {
    #[default]
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum ScreenCommand {
    /// Start viewing a monitor (default: the focused one).
    Start {
        #[serde(default)]
        output: Option<String>,
        #[serde(default)]
        preset: Option<ScreenPreset>,
    },
    Stop,
    Preset {
        preset: ScreenPreset,
    },
    /// A tap on the preview: `x` and `y` are fractions (0..1) of the monitor as it is shown.
    Click {
        x: f64,
        y: f64,
        #[serde(default)]
        button: ClickKind,
        #[serde(default)]
        count: Option<u8>,
    },
}

/// A monitor connector name as Hyprland reports it (`HDMI-A-1`).
pub fn valid_output(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 40
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._:-".contains(c))
}

impl ScreenCommand {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            ScreenCommand::Start {
                output: Some(o), ..
            } if !valid_output(o) => Err("bad monitor name"),
            ScreenCommand::Click { x, y, count, .. } => {
                if !(x.is_finite()
                    && y.is_finite()
                    && (0.0..=1.0).contains(x)
                    && (0.0..=1.0).contains(y))
                {
                    Err("the point must be inside the screen")
                } else if count.is_some_and(|c| !(1..=2).contains(&c)) {
                    Err("a click is one or two taps")
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod screen_tests {
    use super::*;

    #[test]
    fn screen_commands_are_validated() {
        let c = |j: &str| serde_json::from_str::<ScreenCommand>(j);
        assert!(c(r#"{"cmd":"start"}"#).unwrap().validate().is_ok());
        assert_eq!(
            c(r#"{"cmd":"start","preset":"high","output":"HDMI-A-1"}"#).unwrap(),
            ScreenCommand::Start {
                output: Some("HDMI-A-1".into()),
                preset: Some(ScreenPreset::High)
            }
        );
        assert!(
            c(r#"{"cmd":"start","output":"a;b"}"#)
                .unwrap()
                .validate()
                .is_err()
                && c(r#"{"cmd":"start","output":"../x"}"#)
                    .unwrap()
                    .validate()
                    .is_err()
        );
        assert!(c(r#"{"cmd":"click","x":0.5,"y":0.25}"#)
            .unwrap()
            .validate()
            .is_ok());
        for bad in [
            r#"{"cmd":"click","x":1.5,"y":0.2}"#,
            r#"{"cmd":"click","x":-0.1,"y":0.2}"#,
            r#"{"cmd":"click","x":0.1,"y":0.2,"count":3}"#,
            r#"{"cmd":"click","x":0.1,"y":0.2,"count":0}"#,
        ] {
            assert!(c(bad).unwrap().validate().is_err(), "{bad}");
        }
        for bad in [
            r#"{"cmd":"start","preset":"ultra"}"#,
            r#"{"cmd":"record"}"#,
            r#"{"cmd":"click","x":"a","y":1}"#,
        ] {
            assert!(c(bad).is_err(), "{bad}");
        }
        assert!(
            ScreenPreset::Low.fps() < ScreenPreset::High.fps()
                && ScreenPreset::Low.max_width() < ScreenPreset::High.max_width()
        );
    }
}
