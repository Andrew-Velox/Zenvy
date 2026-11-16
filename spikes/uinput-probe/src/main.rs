//! Spike 2: create a virtual mouse via /dev/uinput as the normal user, nudge it 8px and back,
//! and measure how long each emit() takes. No root.
use evdev::{uinput::VirtualDevice, AttributeSet, EventType, InputEvent, KeyCode, RelativeAxisCode};
use std::{thread::sleep, time::{Duration, Instant}};
fn main() -> std::io::Result<()> {
    let mut keys = AttributeSet::<KeyCode>::new();
    keys.insert(KeyCode::BTN_LEFT); keys.insert(KeyCode::BTN_RIGHT); keys.insert(KeyCode::BTN_MIDDLE);
    let mut rel = AttributeSet::<RelativeAxisCode>::new();
    for a in [RelativeAxisCode::REL_X, RelativeAxisCode::REL_Y, RelativeAxisCode::REL_WHEEL, RelativeAxisCode::REL_HWHEEL] { rel.insert(a); }
    let mut dev = VirtualDevice::builder()?.name("zenvy-spike-pointer").with_keys(&keys)?.with_relative_axes(&rel)?.build()?;
    println!("virtual device created");
    sleep(Duration::from_millis(500));
    let mut total = Duration::ZERO;
    for (dx, n) in [(1, 8), (-1, 8)] {
        for _ in 0..n {
            let t = Instant::now();
            dev.emit(&[InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_X.0, dx)])?;
            total += t.elapsed();
            sleep(Duration::from_millis(8));
        }
    }
    println!("16 emits, avg {:?} each", total / 16);
    sleep(Duration::from_millis(300));
    Ok(())
}
