//! Randomized robustness test: every command type a phone can send is fed thousands of mutated and random inputs.
//! Parsing and `validate` must never panic, and anything accepted must also pass `validate` or be refused by it.
//! (A deterministic generator, so a failure reproduces; it complements, not replaces, cargo-fuzz.)

use serde_json::Value;
use zenvy_proto::*;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const SEEDS: &[&str] = &[
    r#"{"cmd":"switch_workspace","id":3}"#,
    r#"{"cmd":"focus_window","address":"0x55d0c8a0"}"#,
    r#"{"cmd":"close_window","address":"0x1"}"#,
    r#"{"cmd":"move_window_to_workspace","address":"0x1","workspace":2}"#,
    r#"{"cmd":"pointer_move","dx":3.5,"dy":-2}"#,
    r#"{"cmd":"scroll","dx":0,"dy":12}"#,
    r#"{"cmd":"button","button":"left","state":"down"}"#,
    r#"{"cmd":"key","key":"Enter","state":"down"}"#,
    r#"{"cmd":"text","text":"héllo wörld 日本 😀"}"#,
    r#"{"cmd":"set_volume","percent":40}"#,
    r#"{"cmd":"set_default_output","name":"alsa_output.usb-x.analog-stereo"}"#,
    r#"{"cmd":"set","id":"ddc:2","percent":70}"#,
    r#"{"cmd":"play_pause"}"#,
    r#"{"cmd":"seek","player":"org.mpris.MediaPlayer2.spotify","position_ms":1000}"#,
    r#"{"cmd":"start","mute_pc":true}"#,
    r#"{"cmd":"request","action":"suspend"}"#,
    r#"{"cmd":"confirm","token":"0123456789abcdef01234567"}"#,
    r#"{"cmd":"start","output":"HDMI-A-1","preset":"medium"}"#,
    r#"{"cmd":"click","x":0.5,"y":0.25,"button":"right","count":2}"#,
    r#"{"password":"hunter2"}"#,
];

const WEIRD_NUMBERS: &[&str] = &[
    "0",
    "-1",
    "1e308",
    "-1e308",
    "1e-320",
    "9223372036854775807",
    "-9223372036854775808",
    "18446744073709551615",
    "0.0000001",
    "NaN",
    "null",
    "true",
    "\"\"",
    "[]",
    "{}",
];

fn mutate(rng: &mut Rng, s: &str) -> String {
    let mut b: Vec<u8> = s.as_bytes().to_vec();
    for _ in 0..1 + rng.below(4) {
        if b.is_empty() {
            break;
        }
        let i = rng.below(b.len());
        match rng.below(6) {
            0 => b[i] = rng.next() as u8,
            1 => {
                b.remove(i);
            }
            2 => b.insert(i, {
                const C: &[u8] = b"{}[]\":,\\0123456789-eE.\x00\xff";
                C[rng.below(C.len())]
            }),
            3 => {
                let w = WEIRD_NUMBERS[rng.below(WEIRD_NUMBERS.len())];
                b.splice(i..(i + 1).min(b.len()), w.bytes());
            }
            4 => b.truncate(i),
            _ => {
                let rep = "A".repeat(1 + rng.below(4000));
                b.splice(i..i, rep.bytes());
            }
        }
    }
    String::from_utf8_lossy(&b).into_owned()
}

fn nested(depth: usize) -> String {
    format!("{}{}", "[".repeat(depth), "]".repeat(depth))
}

fn run_one(input: &str) {
    macro_rules! try_type {
        ($t:ty) => {
            if let Ok(c) = serde_json::from_str::<$t>(input) {
                let _ = c.validate();
            }
        };
    }
    try_type!(DesktopCommand);
    try_type!(InputCommand);
    try_type!(AudioCommand);
    try_type!(MediaCommand);
    try_type!(BrightnessCommand);
    try_type!(PowerCommand);
    try_type!(UnlockCommand);
    try_type!(ScreenCommand);
    let _ = serde_json::from_str::<ListenCommand>(input);
    let _ = serde_json::from_str::<Value>(input);
}

#[test]
fn mutated_and_random_commands_never_panic() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for seed in SEEDS {
        run_one(seed);
        for _ in 0..4000 {
            run_one(&mutate(&mut rng, seed));
        }
    }
    // pure noise
    for _ in 0..20000 {
        let n = rng.below(120);
        let s: String = (0..n)
            .map(|_| char::from_u32(rng.next() as u32 % 0x250).unwrap_or('?'))
            .collect();
        run_one(&s);
    }
    // very deep and very long inputs are refused, not crashed on
    run_one(&nested(100));
    run_one(&format!(
        "{{\"cmd\":\"text\",\"text\":\"{}\"}}",
        "x".repeat(2_000_000)
    ));
}

#[test]
fn accepted_commands_respect_their_limits() {
    // anything the parser accepts that carries an absurd number must be refused by validate, never acted on
    let mut rng = Rng(42);
    for seed in SEEDS {
        for _ in 0..3000 {
            let m = mutate(&mut rng, seed);
            if let Ok(DesktopCommand::SwitchWorkspace { id }) = serde_json::from_str(&m) {
                let c = DesktopCommand::SwitchWorkspace { id };
                assert_eq!(
                    c.validate().is_ok(),
                    (1..=MAX_WORKSPACE_ID).contains(&id),
                    "{m}"
                );
            }
        }
    }
}
