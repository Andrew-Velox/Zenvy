//! Input commands a phone may send. Everything is bounded and validated here, so the agent
//! never forwards free-form codes or unbounded values to the OS.

use serde::{Deserialize, Serialize};

/// Largest single relative pointer step accepted (pixels before acceleration).
pub const MAX_POINTER_STEP: i32 = 2000;
pub const MAX_SCROLL_STEP: i32 = 120;
pub const MAX_TEXT_CHARS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ButtonState {
    Down,
    Up,
    Click,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modifier {
    Ctrl,
    Alt,
    Shift,
    Super,
}

impl Modifier {
    pub fn keycode(self) -> u16 {
        match self {
            Self::Ctrl => 29,
            Self::Shift => 42,
            Self::Alt => 56,
            Self::Super => 125,
        }
    }
}

/// Allowed key names → Linux input event codes (`linux/input-event-codes.h`).
/// Names denote the physical key on a US-QWERTY keyboard; on other layouts the *character*
/// produced by `key: "c"` follows the PC's layout (documented limitation, PLAN.md Appendix A).
const KEYS: &[(&str, u16)] = &[
    ("escape", 1),
    ("1", 2),
    ("2", 3),
    ("3", 4),
    ("4", 5),
    ("5", 6),
    ("6", 7),
    ("7", 8),
    ("8", 9),
    ("9", 10),
    ("0", 11),
    ("minus", 12),
    ("equal", 13),
    ("leftbracket", 26),
    ("rightbracket", 27),
    ("semicolon", 39),
    ("apostrophe", 40),
    ("grave", 41),
    ("backslash", 43),
    ("backspace", 14),
    ("tab", 15),
    ("q", 16),
    ("w", 17),
    ("e", 18),
    ("r", 19),
    ("t", 20),
    ("y", 21),
    ("u", 22),
    ("i", 23),
    ("o", 24),
    ("p", 25),
    ("enter", 28),
    ("a", 30),
    ("s", 31),
    ("d", 32),
    ("f", 33),
    ("g", 34),
    ("h", 35),
    ("j", 36),
    ("k", 37),
    ("l", 38),
    ("z", 44),
    ("x", 45),
    ("c", 46),
    ("v", 47),
    ("b", 48),
    ("n", 49),
    ("m", 50),
    ("comma", 51),
    ("period", 52),
    ("slash", 53),
    ("space", 57),
    ("f1", 59),
    ("f2", 60),
    ("f3", 61),
    ("f4", 62),
    ("f5", 63),
    ("f6", 64),
    ("f7", 65),
    ("f8", 66),
    ("f9", 67),
    ("f10", 68),
    ("f11", 87),
    ("f12", 88),
    ("print", 99),
    ("home", 102),
    ("up", 103),
    ("page_up", 104),
    ("left", 105),
    ("right", 106),
    ("end", 107),
    ("down", 108),
    ("page_down", 109),
    ("insert", 110),
    ("delete", 111),
    ("volume_mute", 113),
    ("volume_down", 114),
    ("volume_up", 115),
    ("media_next", 163),
    ("media_play_pause", 164),
    ("media_prev", 165),
    ("brightness_down", 224),
    ("brightness_up", 225),
];

pub fn keycode(name: &str) -> Option<u16> {
    KEYS.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

pub fn key_names() -> impl Iterator<Item = &'static str> {
    KEYS.iter().map(|(n, _)| *n)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum InputCommand {
    PointerMove {
        dx: i32,
        dy: i32,
    },
    Button {
        button: Button,
        state: ButtonState,
    },
    Scroll {
        dx: i32,
        dy: i32,
    },
    /// Press `key` while holding `mods` (a shortcut), then release everything.
    Key {
        key: String,
        #[serde(default)]
        mods: Vec<Modifier>,
    },
    Text {
        text: String,
    },
}

impl InputCommand {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::PointerMove { dx, dy }
                if dx.abs() > MAX_POINTER_STEP || dy.abs() > MAX_POINTER_STEP =>
            {
                Err("pointer step too large")
            }
            Self::Scroll { dx, dy } if dx.abs() > MAX_SCROLL_STEP || dy.abs() > MAX_SCROLL_STEP => {
                Err("scroll step too large")
            }
            Self::Key { key, mods } => {
                if keycode(key).is_none() {
                    return Err("unknown key");
                }
                if mods.len() > 4 {
                    return Err("too many modifiers");
                }
                Ok(())
            }
            Self::Text { text } => {
                if text.is_empty() {
                    Err("empty text")
                } else if text.chars().count() > MAX_TEXT_CHARS {
                    Err("text too long")
                } else if text.chars().any(|c| c.is_control()) {
                    Err("control characters are not allowed in text (use key commands)")
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        }
    }

    /// High-rate commands use a more generous rate limit than discrete ones.
    pub fn is_pointer_stream(&self) -> bool {
        matches!(self, Self::PointerMove { .. } | Self::Scroll { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(j: &str) -> Result<InputCommand, serde_json::Error> {
        serde_json::from_str(j)
    }

    #[test]
    fn valid_commands_parse_and_validate() {
        for j in [
            r#"{"cmd":"pointer_move","dx":5,"dy":-3}"#,
            r#"{"cmd":"button","button":"left","state":"click"}"#,
            r#"{"cmd":"scroll","dx":0,"dy":-2}"#,
            r#"{"cmd":"key","key":"d","mods":["super"]}"#,
            r#"{"cmd":"key","key":"enter"}"#,
            r#"{"cmd":"text","text":"héllo 日本 😀"}"#,
        ] {
            parse(j)
                .unwrap()
                .validate()
                .unwrap_or_else(|e| panic!("{j}: {e}"));
        }
    }

    #[test]
    fn out_of_range_and_unknown_rejected() {
        assert!(parse(r#"{"cmd":"pointer_move","dx":99999,"dy":0}"#)
            .unwrap()
            .validate()
            .is_err());
        assert!(parse(r#"{"cmd":"scroll","dx":0,"dy":500}"#)
            .unwrap()
            .validate()
            .is_err());
        assert!(parse(r#"{"cmd":"key","key":"KEY_POWER"}"#)
            .unwrap()
            .validate()
            .is_err());
        assert!(
            parse(r#"{"cmd":"key","key":"power"}"#)
                .unwrap()
                .validate()
                .is_err(),
            "no power/sleep keys in the table"
        );
        assert!(
            parse(r#"{"cmd":"key","key":"c","mods":["ctrl","ctrl","alt","shift","super"]}"#)
                .unwrap()
                .validate()
                .is_err()
        );
        assert!(parse(r#"{"cmd":"raw","code":116}"#).is_err());
        assert!(parse(r#"{"cmd":"button","button":"back","state":"click"}"#).is_err());
        assert!(parse(r#"{"cmd":"pointer_move","dx":"1","dy":0}"#).is_err());
    }

    #[test]
    fn text_rules() {
        assert!(InputCommand::Text {
            text: String::new()
        }
        .validate()
        .is_err());
        assert!(
            InputCommand::Text {
                text: "a\nb".into()
            }
            .validate()
            .is_err(),
            "newline must be a key command"
        );
        assert!(
            InputCommand::Text {
                text: "a\u{1b}[31m".into()
            }
            .validate()
            .is_err(),
            "escape sequences rejected"
        );
        assert!(InputCommand::Text {
            text: "x".repeat(257)
        }
        .validate()
        .is_err());
        assert!(InputCommand::Text {
            text: "x".repeat(256)
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn table_has_unique_names_and_codes() {
        let mut n: Vec<_> = KEYS.iter().map(|k| k.0).collect();
        let mut c: Vec<_> = KEYS.iter().map(|k| k.1).collect();
        n.sort();
        c.sort();
        let (nl, cl) = (n.len(), c.len());
        n.dedup();
        c.dedup();
        assert_eq!((n.len(), c.len()), (nl, cl));
    }
}
