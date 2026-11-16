//! Randomized robustness test for the functions that clean text coming from phones and the browser extension.
use crate::files::{display_name, sanitize_filename};
use serde_json::{json, Value};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn noise(rng: &mut Rng, max: usize) -> String {
    let n = (rng.next() % max as u64) as usize;
    (0..n)
        .map(|_| {
            let r = rng.next();
            match r % 8 {
                0 => '/',
                1 => '\\',
                2 => '.',
                3 => '\0',
                4 => '\u{202e}', // right-to-left override
                5 => char::from_u32((r >> 8) as u32 % 0x11_0000).unwrap_or('x'),
                _ => (b'a' + (r >> 8) as u8 % 26) as char,
            }
        })
        .collect()
}

#[test]
fn file_names_are_always_safe_or_refused() {
    let mut rng = Rng(7);
    for raw in [
        "", ".", "..", "../x", "a/b", "a\\b", "CON", "x\0y", ".hidden", "ok.txt",
    ] {
        let _ = display_name(raw);
        if let Some(n) = sanitize_filename(raw) {
            assert!(
                !n.contains('/') && !n.contains('\\') && !n.contains('\0') && n != "." && n != "..",
                "{raw:?} -> {n:?}"
            );
        }
    }
    for _ in 0..30000 {
        let raw = noise(&mut rng, 400);
        let _ = display_name(&raw);
        if let Some(n) = sanitize_filename(&raw) {
            assert!(
                !n.is_empty()
                    && !n.contains(['/', '\\', '\0'])
                    && !n.starts_with('.')
                    && n != ".."
                    && n.len() <= 255
                    && !n.chars().any(|c| c.is_control() || c == '\u{202e}'),
                "{raw:?} -> {n:?}"
            );
        }
    }
}

#[test]
fn extension_data_is_sanitized_without_panicking() {
    use crate::browser::{sanitize_favicons, sanitize_tabs};
    let mut rng = Rng(99);
    for _ in 0..3000 {
        let v: Value = json!({
            "icons": { noise(&mut rng, 40): noise(&mut rng, 300), "a.example": "data:image/png;base64,AAAA" },
            "tabs": [ { "id": (rng.next() as i64), "title": noise(&mut rng, 300), "url": noise(&mut rng, 100),
                        "active": rng.next() % 2 == 0, "windowId": (rng.next() as i64) }, noise(&mut rng, 10), null, 5 ],
            "focused": noise(&mut rng, 5),
        });
        let _ = sanitize_favicons(&v);
        let _ = sanitize_tabs(&v["tabs"]);
        let _ = sanitize_tabs(&v);
        let _ = sanitize_favicons(&v["tabs"]);
    }
}

#[test]
fn names_cannot_disguise_their_extension_or_exceed_the_file_system_limit() {
    // the right-to-left override would make this read "photoexe.jpg"
    assert_eq!(
        sanitize_filename("photo\u{202e}gpj.exe").as_deref(),
        Some("photogpj.exe")
    );
    assert_eq!(
        sanitize_filename("a\u{200b}b\u{feff}c.txt").as_deref(),
        Some("abc.txt")
    );
    let long = format!("{}.pdf", "😀".repeat(100));
    let n = sanitize_filename(&long).unwrap();
    assert!(
        n.len() <= 200 && n.ends_with(".pdf") && n.chars().count() <= 120,
        "{} bytes",
        n.len()
    );
    assert!(sanitize_filename(&"é".repeat(500)).unwrap().len() <= 200);
}
