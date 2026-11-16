//! Spike 1: talk to Hyprland's sockets directly (no hyprctl), read-only.
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

fn dir() -> String {
    let rt = std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR");
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").expect("HYPRLAND_INSTANCE_SIGNATURE");
    format!("{rt}/hypr/{sig}")
}

fn request(cmd: &str) -> std::io::Result<String> {
    let mut s = UnixStream::connect(format!("{}/.socket.sock", dir()))?;
    s.set_read_timeout(Some(Duration::from_secs(2)))?;
    s.write_all(cmd.as_bytes())?;
    let mut out = String::new();
    s.read_to_string(&mut out)?;
    Ok(out)
}

fn main() {
    // Latency of a request, 200 samples.
    let t = Instant::now();
    for _ in 0..200 { request("j/activeworkspace").unwrap(); }
    println!("avg j/activeworkspace round trip: {:?}", t.elapsed() / 200);

    let v: serde_json::Value = serde_json::from_str(&request("j/version").unwrap()).unwrap();
    println!("version={} tag={}", v["version"], v["tag"]);

    for q in ["monitors", "workspaces", "clients"] {
        let raw = request(&format!("j/{q}")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
        let arr = v.as_array().unwrap();
        println!("{q}: {} items; keys of first: {:?}", arr.len(),
            arr.first().and_then(|x| x.as_object()).map(|o| o.keys().cloned().collect::<Vec<_>>()));
    }

    // Event stream for 8 seconds (switch workspace / focus windows now to see events).
    println!("listening on socket2 for 8s...");
    let s = UnixStream::connect(format!("{}/.socket2.sock", dir())).unwrap();
    s.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
    let mut r = BufReader::new(s);
    let end = Instant::now() + Duration::from_secs(8);
    let mut line = String::new();
    let mut n = 0;
    while Instant::now() < end {
        line.clear();
        match r.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => { n += 1; if n <= 15 { println!("  evt: {}", line.trim_end()); } }
            Err(_) => {}
        }
    }
    println!("events seen: {n}");
}
