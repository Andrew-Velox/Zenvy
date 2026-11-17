//! A fake MPRIS media player for end-to-end tests. Serves one player on the session bus named by
//! `$DBUS_SESSION_BUS_ADDRESS`, logs every method call to `$FAKE_PLAYER_LOG`, and emits the real
//! change signals. Run only against a *private* bus (see tests/run-e2e.sh).

use std::collections::HashMap;
use std::io::Write;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedValue, Value};

struct Root;
#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    #[zbus(property)]
    fn identity(&self) -> String {
        "Fake Player".into()
    }
}

struct Player {
    playing: bool,
    position_us: i64,
    volume: f64,
}

fn log(line: &str) {
    if let Ok(p) = std::env::var("FAKE_PLAYER_LOG") {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
        {
            let _ = writeln!(f, "{line}");
        }
    }
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    async fn play_pause(&mut self, #[zbus(signal_emitter)] ctx: SignalEmitter<'_>) {
        self.playing = !self.playing;
        log("PlayPause");
        let _ = self.playback_status_changed(&ctx).await;
    }
    async fn next(&self) {
        log("Next");
    }
    async fn previous(&self) {
        log("Previous");
    }
    async fn seek(&mut self, offset: i64, #[zbus(signal_emitter)] ctx: SignalEmitter<'_>) {
        self.position_us += offset;
        log(&format!("Seek {offset}"));
        let _ = Self::seeked(&ctx, self.position_us).await;
    }
    #[zbus(signal)]
    async fn seeked(ctx: &SignalEmitter<'_>, position: i64) -> zbus::Result<()>;
    #[zbus(property)]
    fn playback_status(&self) -> String {
        if self.playing { "Playing" } else { "Paused" }.into()
    }
    #[zbus(property)]
    fn position(&self) -> i64 {
        self.position_us
    }
    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        let mut m = HashMap::new();
        m.insert(
            "xesam:title".to_string(),
            OwnedValue::try_from(Value::from("Fake Track")).unwrap(),
        );
        m.insert(
            "xesam:artist".to_string(),
            OwnedValue::try_from(Value::from(vec!["Fake Artist"])).unwrap(),
        );
        m.insert(
            "mpris:length".to_string(),
            OwnedValue::try_from(Value::from(180_000_000i64)).unwrap(),
        );
        m
    }
    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_seek(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn volume(&self) -> f64 {
        self.volume
    }
    #[zbus(property)]
    fn set_volume(&mut self, v: f64) {
        self.volume = v;
        log(&format!("Volume {}", (v * 100.0).round() as i64));
    }
}

#[tokio::main]
async fn main() -> zbus::Result<()> {
    let _conn = zbus::connection::Builder::session()?
        .name("org.mpris.MediaPlayer2.zenvyfake")?
        .serve_at("/org/mpris/MediaPlayer2", Root)?
        .serve_at(
            "/org/mpris/MediaPlayer2",
            Player {
                playing: false,
                position_us: 30_000_000,
                volume: 0.5,
            },
        )?
        .build()
        .await?;
    eprintln!("fake player ready");
    std::future::pending::<()>().await;
    Ok(())
}
