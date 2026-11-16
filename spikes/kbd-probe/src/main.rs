//! Spike 2b: uinput keyboard. `kbd-probe text "abc"` types ASCII via keycodes (US layout assumed);
//! `kbd-probe chord ctrl v` / `kbd-probe chord ctrl d` sends a chord. Measures nothing fancy.
use evdev::{uinput::VirtualDevice, AttributeSet, EventType, InputEvent, KeyCode};
use std::{thread::sleep, time::Duration};

fn key_for(c: char) -> Option<(KeyCode, bool)> {
    use KeyCode as K;
    let l = [K::KEY_A,K::KEY_B,K::KEY_C,K::KEY_D,K::KEY_E,K::KEY_F,K::KEY_G,K::KEY_H,K::KEY_I,K::KEY_J,K::KEY_K,K::KEY_L,K::KEY_M,K::KEY_N,K::KEY_O,K::KEY_P,K::KEY_Q,K::KEY_R,K::KEY_S,K::KEY_T,K::KEY_U,K::KEY_V,K::KEY_W,K::KEY_X,K::KEY_Y,K::KEY_Z];
    match c {
        'a'..='z' => Some((l[(c as u8 - b'a') as usize], false)),
        'A'..='Z' => Some((l[(c as u8 - b'A') as usize], true)),
        ' ' => Some((K::KEY_SPACE, false)), '\n' => Some((K::KEY_ENTER, false)),
        '0'..='9' => Some((KeyCode::new(K::KEY_1.0 + (if c=='0' {9} else {c as u16 - '1' as u16})), false)),
        _ => None,
    }
}
fn ev(k: KeyCode, v: i32) -> InputEvent { InputEvent::new(EventType::KEY.0, k.0, v) }
fn main() -> std::io::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let mut keys = AttributeSet::<KeyCode>::new();
    for c in 0..250u16 { keys.insert(KeyCode::new(c)); }
    let mut d = VirtualDevice::builder()?.name("zenvy-spike-kbd").with_keys(&keys)?.build()?;
    sleep(Duration::from_millis(600)); // compositor must see the new device first
    match a[1].as_str() {
        "text" => for c in a[2].chars() {
            match key_for(c) {
                Some((k, shift)) => {
                    if shift { d.emit(&[ev(KeyCode::KEY_LEFTSHIFT,1)])?; }
                    d.emit(&[ev(k,1)])?; d.emit(&[ev(k,0)])?;
                    if shift { d.emit(&[ev(KeyCode::KEY_LEFTSHIFT,0)])?; }
                }
                None => eprintln!("no keycode for {c:?} (non-ASCII: uinput cannot type this directly)"),
            }
            sleep(Duration::from_millis(8));
        },
        "chord" => {
            let m = match a[2].as_str() { "ctrl" => KeyCode::KEY_LEFTCTRL, _ => KeyCode::KEY_LEFTSHIFT };
            let (k, _) = key_for(a[3].chars().next().unwrap()).unwrap();
            d.emit(&[ev(m,1)])?; d.emit(&[ev(k,1)])?; d.emit(&[ev(k,0)])?; d.emit(&[ev(m,0)])?;
        }
        _ => {}
    }
    sleep(Duration::from_millis(300));
    Ok(())
}
