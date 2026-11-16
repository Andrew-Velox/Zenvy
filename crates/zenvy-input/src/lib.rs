//! Input injection via `/dev/uinput` (primary backend) with pluggable text entry.
//!
//! Safety properties (all unit-tested against a recording sink):
//! * modifiers are always released, in reverse order, after a shortcut;
//! * a pointer button held by a phone is released when that phone disconnects;
//! * only validated [`InputCommand`]s reach the kernel.

mod ascii;
mod text;

pub use text::TextBackend;

use evdev::{
    uinput::VirtualDevice, AttributeSet, EventType, InputEvent, KeyCode, RelativeAxisCode,
};
use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;
use zenvy_core::{Error, Input, Result};
use zenvy_proto::{keycode, Button, ButtonState, InputCommand, Modifier};

const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
const REL_X: u16 = 0;
const REL_Y: u16 = 1;
const REL_HWHEEL: u16 = 6;
const REL_WHEEL: u16 = 8;

fn btn_code(b: Button) -> u16 {
    match b {
        Button::Left => 0x110,
        Button::Right => 0x111,
        Button::Middle => 0x112,
    }
}

/// Destination for raw input events (the kernel device in production, a recorder in tests).
pub trait Sink: Send {
    /// Emits events followed by a sync report, atomically from the reader's point of view.
    fn emit(&mut self, events: &[(u16, u16, i32)]) -> std::io::Result<()>;
}

pub struct UinputSink(VirtualDevice);

impl Sink for UinputSink {
    fn emit(&mut self, events: &[(u16, u16, i32)]) -> std::io::Result<()> {
        let evs: Vec<InputEvent> = events
            .iter()
            .map(|(t, c, v)| InputEvent::new(*t, *c, *v))
            .collect();
        self.0.emit(&evs)
    }
}

pub struct InputAdapter<S: Sink> {
    pointer: Mutex<S>,
    keyboard: Mutex<S>,
    text: TextBackend,
    held: Mutex<HashSet<Button>>,
}

fn io(e: std::io::Error) -> Error {
    Error::Io(e)
}

impl<S: Sink> InputAdapter<S> {
    pub fn with_sinks(pointer: S, keyboard: S, text: TextBackend) -> Self {
        Self {
            pointer: Mutex::new(pointer),
            keyboard: Mutex::new(keyboard),
            text,
            held: Mutex::new(HashSet::new()),
        }
    }

    fn pointer_emit(&self, ev: &[(u16, u16, i32)]) -> Result<()> {
        self.pointer.lock().unwrap().emit(ev).map_err(io)
    }

    fn key_emit(&self, code: u16, value: i32) -> Result<()> {
        self.keyboard
            .lock()
            .unwrap()
            .emit(&[(EV_KEY, code, value)])
            .map_err(io)
    }

    fn button(&self, b: Button, state: ButtonState) -> Result<()> {
        let c = btn_code(b);
        match state {
            ButtonState::Down => {
                self.held.lock().unwrap().insert(b);
                self.pointer_emit(&[(EV_KEY, c, 1)])
            }
            ButtonState::Up => {
                self.held.lock().unwrap().remove(&b);
                self.pointer_emit(&[(EV_KEY, c, 0)])
            }
            ButtonState::Click => {
                self.pointer_emit(&[(EV_KEY, c, 1)])?;
                self.pointer_emit(&[(EV_KEY, c, 0)])
            }
        }
    }

    /// Press `key` while holding `mods`; release in reverse order. Even if the key press fails
    /// part-way, modifiers are still released so none can be left stuck.
    fn shortcut(&self, key: &str, mods: &[Modifier]) -> Result<()> {
        let code = keycode(key).ok_or(Error::InvalidCommand("unknown key"))?;
        let mut uniq: Vec<Modifier> = Vec::new();
        for m in mods {
            if !uniq.contains(m) {
                uniq.push(*m);
            }
        }
        let mut pressed: Vec<u16> = Vec::new();
        let mut result = Ok(());
        for m in &uniq {
            match self.key_emit(m.keycode(), 1) {
                Ok(()) => pressed.push(m.keycode()),
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
        }
        if result.is_ok() {
            result = self.key_emit(code, 1).and_then(|_| self.key_emit(code, 0));
        }
        for m in pressed.into_iter().rev() {
            let r = self.key_emit(m, 0);
            if result.is_ok() {
                result = r;
            }
        }
        result
    }

    /// Types a secret (a lock-screen password) followed by Enter through the virtual keyboard.
    /// Everything is validated before the first key is pressed, nothing is logged, and `pause`
    /// separates keystrokes so a slow lock screen does not drop characters.
    /// US layout only, which the caller must have checked.
    pub fn type_secret(&self, secret: &str, pause: Duration) -> Result<()> {
        let mut strokes = Vec::with_capacity(secret.len());
        for ch in secret.chars() {
            strokes.push(
                ascii::keystroke(ch).ok_or(Error::InvalidCommand("character cannot be typed"))?,
            );
        }
        // a lone Shift tap wakes a sleeping lock screen without typing a character
        let shift_key = Modifier::Shift.keycode();
        self.key_emit(shift_key, 1)?;
        self.key_emit(shift_key, 0)?;
        std::thread::sleep(Duration::from_millis(250));
        for (code, shift) in strokes {
            let mods: &[Modifier] = if shift { &[Modifier::Shift] } else { &[] };
            self.shortcut(code, mods)?;
            std::thread::sleep(pause);
        }
        self.shortcut("enter", &[])
    }

    /// Releases buttons that a (now disconnected) session still holds.
    pub fn release_buttons(&self, buttons: impl IntoIterator<Item = Button>) {
        for b in buttons {
            let was_held = self.held.lock().unwrap().remove(&b);
            if was_held {
                let _ = self.pointer_emit(&[(EV_KEY, btn_code(b), 0)]);
            }
        }
    }

    pub fn execute_sync(&self, cmd: &InputCommand) -> Result<()> {
        cmd.validate().map_err(Error::InvalidCommand)?;
        match cmd {
            InputCommand::PointerMove { dx, dy } => {
                let mut ev = Vec::with_capacity(2);
                if *dx != 0 {
                    ev.push((EV_REL, REL_X, *dx));
                }
                if *dy != 0 {
                    ev.push((EV_REL, REL_Y, *dy));
                }
                if ev.is_empty() {
                    return Ok(());
                }
                self.pointer_emit(&ev)
            }
            // Protocol: dy > 0 scrolls the content down (finger moves up). evdev's wheel is the opposite.
            InputCommand::Scroll { dx, dy } => {
                let mut ev = Vec::with_capacity(2);
                if *dy != 0 {
                    ev.push((EV_REL, REL_WHEEL, -*dy));
                }
                if *dx != 0 {
                    ev.push((EV_REL, REL_HWHEEL, *dx));
                }
                if ev.is_empty() {
                    return Ok(());
                }
                self.pointer_emit(&ev)
            }
            InputCommand::Button { button, state } => self.button(*button, *state),
            InputCommand::Key { key, mods } => self.shortcut(key, mods),
            InputCommand::Text { .. } => Err(Error::InvalidCommand("text is asynchronous")),
        }
    }
}

impl<S: Sink> Input for InputAdapter<S> {
    fn name(&self) -> &'static str {
        "uinput"
    }

    async fn execute(&self, cmd: &InputCommand) -> Result<()> {
        match cmd {
            InputCommand::Text { text } => {
                cmd.validate().map_err(Error::InvalidCommand)?;
                match self.text {
                    TextBackend::Wtype => text::wtype(text).await,
                    TextBackend::UinputAscii => {
                        for ch in text.chars() {
                            let (code, shift) = ascii::keystroke(ch).ok_or_else(|| {
                                Error::Rejected(format!(
                                    "'{ch}' cannot be typed by the ASCII fallback"
                                ))
                            })?;
                            let mods: &[Modifier] = if shift { &[Modifier::Shift] } else { &[] };
                            self.shortcut(code, mods)?;
                        }
                        Ok(())
                    }
                    TextBackend::None => Err(Error::Rejected(
                        "text input is not available on this system".into(),
                    )),
                }
            }
            other => self.execute_sync(other),
        }
    }
}

impl InputAdapter<UinputSink> {
    /// Creates the two virtual devices (`zenvy-pointer`, `zenvy-keyboard`) through `/dev/uinput`.
    /// Blocks briefly so the compositor registers them; call from `spawn_blocking`.
    pub fn create(text: TextBackend) -> std::io::Result<Self> {
        let mut keys = AttributeSet::<KeyCode>::new();
        for n in zenvy_proto::key_names() {
            keys.insert(KeyCode::new(keycode(n).unwrap()));
        }
        for m in [
            Modifier::Ctrl,
            Modifier::Alt,
            Modifier::Shift,
            Modifier::Super,
        ] {
            keys.insert(KeyCode::new(m.keycode()));
        }
        let mut btns = AttributeSet::<KeyCode>::new();
        for b in [Button::Left, Button::Right, Button::Middle] {
            btns.insert(KeyCode::new(btn_code(b)));
        }
        let mut rel = AttributeSet::<RelativeAxisCode>::new();
        for a in [REL_X, REL_Y, REL_WHEEL, REL_HWHEEL] {
            rel.insert(RelativeAxisCode(a));
        }
        let pointer = VirtualDevice::builder()?
            .name("zenvy-pointer")
            .with_keys(&btns)?
            .with_relative_axes(&rel)?
            .build()?;
        let keyboard = VirtualDevice::builder()?
            .name("zenvy-keyboard")
            .with_keys(&keys)?
            .build()?;
        std::thread::sleep(Duration::from_millis(400));
        let _ = EventType::KEY; // keep the import meaningful for readers: devices emit KEY/REL events
        Ok(Self::with_sinks(
            UinputSink(pointer),
            UinputSink(keyboard),
            text,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[derive(Clone, Default)]
    struct Rec(Arc<Mutex<Vec<(u16, u16, i32)>>>);
    impl Sink for Rec {
        fn emit(&mut self, e: &[(u16, u16, i32)]) -> std::io::Result<()> {
            self.0.lock().unwrap().extend_from_slice(e);
            Ok(())
        }
    }

    fn adapter(text: TextBackend) -> (InputAdapter<Rec>, Rec, Rec) {
        let (p, k) = (Rec::default(), Rec::default());
        (InputAdapter::with_sinks(p.clone(), k.clone(), text), p, k)
    }
    fn log(r: &Rec) -> Vec<(u16, u16, i32)> {
        r.0.lock().unwrap().clone()
    }
    fn cmd(j: &str) -> InputCommand {
        serde_json::from_str(j).unwrap()
    }

    #[test]
    fn keycode_table_matches_linux_constants() {
        for (name, kc) in [
            ("a", KeyCode::KEY_A),
            ("z", KeyCode::KEY_Z),
            ("enter", KeyCode::KEY_ENTER),
            ("escape", KeyCode::KEY_ESC),
            ("f12", KeyCode::KEY_F12),
            ("0", KeyCode::KEY_0),
            ("1", KeyCode::KEY_1),
            ("space", KeyCode::KEY_SPACE),
            ("left", KeyCode::KEY_LEFT),
            ("page_down", KeyCode::KEY_PAGEDOWN),
            ("volume_up", KeyCode::KEY_VOLUMEUP),
            ("media_play_pause", KeyCode::KEY_PLAYPAUSE),
            ("media_next", KeyCode::KEY_NEXTSONG),
            ("media_prev", KeyCode::KEY_PREVIOUSSONG),
            ("brightness_up", KeyCode::KEY_BRIGHTNESSUP),
            ("delete", KeyCode::KEY_DELETE),
            ("tab", KeyCode::KEY_TAB),
            ("q", KeyCode::KEY_Q),
            ("m", KeyCode::KEY_M),
            ("slash", KeyCode::KEY_SLASH),
        ] {
            assert_eq!(keycode(name), Some(kc.0), "{name}");
        }
        assert_eq!(Modifier::Super.keycode(), KeyCode::KEY_LEFTMETA.0);
        assert_eq!(Modifier::Ctrl.keycode(), KeyCode::KEY_LEFTCTRL.0);
        assert_eq!(btn_code(Button::Left), KeyCode::BTN_LEFT.0);
        assert_eq!(btn_code(Button::Middle), KeyCode::BTN_MIDDLE.0);
    }

    #[test]
    fn pointer_and_scroll_events() {
        let (a, p, _) = adapter(TextBackend::None);
        a.execute_sync(&cmd(r#"{"cmd":"pointer_move","dx":5,"dy":-3}"#))
            .unwrap();
        a.execute_sync(&cmd(r#"{"cmd":"pointer_move","dx":0,"dy":0}"#))
            .unwrap();
        a.execute_sync(&cmd(r#"{"cmd":"scroll","dx":2,"dy":3}"#))
            .unwrap();
        assert_eq!(
            log(&p),
            vec![
                (EV_REL, REL_X, 5),
                (EV_REL, REL_Y, -3),
                (EV_REL, REL_WHEEL, -3),
                (EV_REL, REL_HWHEEL, 2)
            ]
        );
    }

    #[test]
    fn click_is_down_then_up() {
        let (a, p, _) = adapter(TextBackend::None);
        a.execute_sync(&cmd(r#"{"cmd":"button","button":"right","state":"click"}"#))
            .unwrap();
        assert_eq!(log(&p), vec![(EV_KEY, 0x111, 1), (EV_KEY, 0x111, 0)]);
    }

    #[test]
    fn type_secret_types_every_character_with_shift_where_needed_then_enter() {
        let (a, _, k) = adapter(TextBackend::None);
        a.type_secret("aB!", Duration::from_millis(0)).unwrap();
        let ev = log(&k);
        // wake: a Shift tap, then a, Shift+b, Shift+1, Enter
        assert_eq!(
            ev,
            vec![
                (EV_KEY, 42, 1),
                (EV_KEY, 42, 0),
                (EV_KEY, 30, 1),
                (EV_KEY, 30, 0),
                (EV_KEY, 42, 1),
                (EV_KEY, 48, 1),
                (EV_KEY, 48, 0),
                (EV_KEY, 42, 0),
                (EV_KEY, 42, 1),
                (EV_KEY, 2, 1),
                (EV_KEY, 2, 0),
                (EV_KEY, 42, 0),
                (EV_KEY, 28, 1),
                (EV_KEY, 28, 0)
            ]
        );
    }

    #[test]
    fn type_secret_refuses_untypable_characters_before_pressing_anything() {
        let (a, _, k) = adapter(TextBackend::None);
        assert!(a.type_secret("ok\u{e9}", Duration::from_millis(0)).is_err());
        assert!(a.type_secret("tab\t", Duration::from_millis(0)).is_err());
        assert!(
            log(&k).is_empty(),
            "nothing was typed: a half-typed password would be worse than none"
        );
    }

    #[test]
    fn type_secret_never_leaves_shift_down() {
        let (a, _, k) = adapter(TextBackend::None);
        a.type_secret("A~Z|", Duration::from_millis(0)).unwrap();
        let ev = log(&k);
        let downs = ev.iter().filter(|e| e.1 == 42 && e.2 == 1).count();
        let ups = ev.iter().filter(|e| e.1 == 42 && e.2 == 0).count();
        assert_eq!(downs, ups);
    }

    #[test]
    fn shortcut_presses_mods_in_order_and_releases_in_reverse() {
        let (a, _, k) = adapter(TextBackend::None);
        a.execute_sync(&cmd(r#"{"cmd":"key","key":"t","mods":["ctrl","shift"]}"#))
            .unwrap();
        assert_eq!(
            log(&k),
            vec![
                (EV_KEY, 29, 1),
                (EV_KEY, 42, 1),
                (EV_KEY, 20, 1),
                (EV_KEY, 20, 0),
                (EV_KEY, 42, 0),
                (EV_KEY, 29, 0)
            ]
        );
    }

    #[test]
    fn duplicate_modifiers_are_pressed_once() {
        let (a, _, k) = adapter(TextBackend::None);
        a.execute_sync(&cmd(r#"{"cmd":"key","key":"c","mods":["ctrl","ctrl"]}"#))
            .unwrap();
        assert_eq!(log(&k).iter().filter(|e| e.1 == 29 && e.2 == 1).count(), 1);
    }

    struct Failing(usize, Arc<Mutex<Vec<(u16, u16, i32)>>>);
    impl Sink for Failing {
        fn emit(&mut self, e: &[(u16, u16, i32)]) -> std::io::Result<()> {
            if self.0 == 0 {
                return Err(std::io::Error::other("boom"));
            }
            self.0 -= 1;
            self.1.lock().unwrap().extend_from_slice(e);
            Ok(())
        }
    }

    #[test]
    fn modifier_is_released_even_if_the_key_press_fails() {
        let rec = Arc::new(Mutex::new(vec![]));
        // allows: ctrl down, then fails on the key press; the release of ctrl must still be *attempted*
        let a = InputAdapter::with_sinks(
            Failing(100, Arc::new(Mutex::new(vec![]))),
            Failing(1, rec.clone()),
            TextBackend::None,
        );
        assert!(a
            .execute_sync(&cmd(r#"{"cmd":"key","key":"c","mods":["ctrl"]}"#))
            .is_err());
        // ctrl down was recorded; the release attempt happened (it fails on this sink, but is not skipped)
        assert_eq!(rec.lock().unwrap().first(), Some(&(EV_KEY, 29, 1)));
    }

    #[test]
    fn disconnect_releases_only_buttons_that_are_held() {
        let (a, p, _) = adapter(TextBackend::None);
        a.execute_sync(&cmd(r#"{"cmd":"button","button":"left","state":"down"}"#))
            .unwrap();
        a.release_buttons([Button::Left, Button::Right]);
        assert_eq!(
            log(&p),
            vec![(EV_KEY, 0x110, 1), (EV_KEY, 0x110, 0)],
            "no spurious release for the right button"
        );
        a.release_buttons([Button::Left]);
        assert_eq!(log(&p).len(), 2, "releasing twice is a no-op");
    }

    #[test]
    fn invalid_commands_never_reach_the_sink() {
        let (a, p, k) = adapter(TextBackend::None);
        assert!(a
            .execute_sync(&InputCommand::PointerMove { dx: 99999, dy: 0 })
            .is_err());
        assert!(a
            .execute_sync(&InputCommand::Key {
                key: "power".into(),
                mods: vec![]
            })
            .is_err());
        assert!(log(&p).is_empty() && log(&k).is_empty());
    }

    #[tokio::test]
    async fn ascii_fallback_types_with_shift_and_refuses_unicode() {
        let (a, _, k) = adapter(TextBackend::UinputAscii);
        a.execute(&InputCommand::Text { text: "Hi!".into() })
            .await
            .unwrap();
        let l = log(&k);
        // H = shift+h ; i ; ! = shift+1
        assert_eq!(
            &l[..4],
            &[
                (EV_KEY, 42, 1),
                (EV_KEY, 35, 1),
                (EV_KEY, 35, 0),
                (EV_KEY, 42, 0)
            ]
        );
        assert!(l.contains(&(EV_KEY, 2, 1)));
        let (a, _, k) = adapter(TextBackend::UinputAscii);
        assert!(a
            .execute(&InputCommand::Text {
                text: "日本".into()
            })
            .await
            .is_err());
        assert!(log(&k).is_empty());
        let (a, ..) = adapter(TextBackend::None);
        assert!(a
            .execute(&InputCommand::Text { text: "x".into() })
            .await
            .is_err());
    }
}
