//! Media control over MPRIS (the standard D-Bus interface media players expose).
//!
//! * Event driven: D-Bus signals (`PropertiesChanged`, `Seeked`, players appearing/disappearing) are
//!   debounced into one notification; nothing polls.
//! * A phone can only target a bus name that (a) looks like an MPRIS player and (b) is a *currently
//!   running* player, and can only call four fixed methods: PlayPause, Next, Previous, Seek.
//! * The `playerctld` proxy is skipped: it mirrors the real players and would show every track twice.

use futures_util::StreamExt;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;
use zbus::zvariant::{OwnedValue, Value};
use zbus::{Connection, MatchRule, MessageStream, Proxy};
use zenvy_core::{Error, Result};
use zenvy_proto::{MediaCommand, MediaPlayer, MediaState, PlaybackStatus, MPRIS_PREFIX};

const PLAYER_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
const ROOT_IFACE: &str = "org.mpris.MediaPlayer2";
const CALL_TIMEOUT: Duration = Duration::from_secs(2);

fn err(e: impl std::fmt::Display) -> Error {
    Error::Rejected(format!("d-bus: {e}"))
}

fn as_i64(v: &Value<'_>) -> Option<i64> {
    match v {
        Value::I64(n) => Some(*n),
        Value::U64(n) => i64::try_from(*n).ok(),
        Value::I32(n) => Some(i64::from(*n)),
        Value::U32(n) => Some(i64::from(*n)),
        Value::Value(inner) => as_i64(inner),
        _ => None,
    }
}

fn as_text(v: &Value<'_>) -> Option<String> {
    match v {
        Value::Str(s) => Some(s.to_string()),
        // `xesam:artist` is a list of strings; some players send a single string instead.
        Value::Array(a) => {
            let parts: Vec<String> = a.iter().filter_map(as_text).collect();
            (!parts.is_empty()).then(|| parts.join(", "))
        }
        Value::Value(inner) => as_text(inner),
        _ => None,
    }
}

fn clean(s: String) -> String {
    s.chars().filter(|c| !c.is_control()).take(200).collect()
}

/// Metadata dictionary -> (title, artist, album, length in ms).
pub fn parse_metadata(m: &HashMap<String, OwnedValue>) -> (String, String, String, Option<u64>) {
    let get = |k: &str| {
        m.get(k)
            .and_then(|v| as_text(v))
            .map(clean)
            .unwrap_or_default()
    };
    let length = m
        .get("mpris:length")
        .and_then(|v| as_i64(v))
        .filter(|n| *n > 0)
        .map(|us| (us / 1000) as u64);
    (
        get("xesam:title"),
        get("xesam:artist"),
        get("xesam:album"),
        length,
    )
}

pub fn parse_status(s: &str) -> PlaybackStatus {
    match s {
        "Playing" => PlaybackStatus::Playing,
        "Paused" => PlaybackStatus::Paused,
        _ => PlaybackStatus::Stopped,
    }
}

/// Browsers often appear twice (the browser's own MPRIS player plus a desktop-integration extension) for the
/// same tab. Two players of the same app with the same length and status are treated as one media item; the one
/// with the richer metadata wins. Players without a known length are never merged.
pub fn dedupe(players: Vec<MediaPlayer>) -> Vec<MediaPlayer> {
    let richness = |p: &MediaPlayer| {
        u8::from(!p.artist.is_empty()) * 2
            + u8::from(!p.album.is_empty())
            + u8::from(p.title.len() > 12)
    };
    let mut out: Vec<MediaPlayer> = Vec::new();
    for p in players {
        let same = |q: &MediaPlayer| {
            p.length_ms.is_some()
                && q.length_ms == p.length_ms
                && q.status == p.status
                && q.identity.eq_ignore_ascii_case(&p.identity)
        };
        match out.iter().position(same) {
            Some(i) if richness(&p) > richness(&out[i]) => out[i] = p,
            Some(_) => {}
            None => out.push(p),
        }
    }
    out
}

/// Which player to show first: a playing one, else a paused one, else the first.
pub fn pick_current(players: &[MediaPlayer]) -> Option<String> {
    [
        PlaybackStatus::Playing,
        PlaybackStatus::Paused,
        PlaybackStatus::Stopped,
    ]
    .iter()
    .find_map(|st| players.iter().find(|p| p.status == *st))
    .map(|p| p.id.clone())
}

#[derive(Clone)]
pub struct MediaAdapter {
    conn: Connection,
}

impl MediaAdapter {
    pub async fn connect() -> Result<Self> {
        Ok(Self {
            conn: Connection::session().await.map_err(err)?,
        })
    }

    async fn player_names(&self) -> Result<Vec<String>> {
        let dbus = zbus::fdo::DBusProxy::new(&self.conn).await.map_err(err)?;
        let mut names: Vec<String> = dbus
            .list_names()
            .await
            .map_err(err)?
            .into_iter()
            .map(|n| n.to_string())
            .filter(|n| {
                n.starts_with(MPRIS_PREFIX)
                    && n != "org.mpris.MediaPlayer2.playerctld"
                    && zenvy_proto::valid_player_id(n)
            })
            .collect();
        names.sort();
        Ok(names)
    }

    async fn proxy<'a>(&'a self, name: &'a str, iface: &'static str) -> Result<Proxy<'a>> {
        Proxy::new(&self.conn, name, PLAYER_PATH, iface)
            .await
            .map_err(err)
    }

    async fn read_player(&self, name: &str) -> Result<MediaPlayer> {
        let p = self.proxy(name, PLAYER_IFACE).await?;
        let root = self.proxy(name, ROOT_IFACE).await?;
        let status: String = p.get_property("PlaybackStatus").await.unwrap_or_default();
        let meta: HashMap<String, OwnedValue> =
            p.get_property("Metadata").await.unwrap_or_default();
        let (title, artist, album, length_ms) = parse_metadata(&meta);
        let position: Option<i64> = p.get_property("Position").await.ok();
        let identity: String = root
            .get_property("Identity")
            .await
            .unwrap_or_else(|_| name.trim_start_matches(MPRIS_PREFIX).to_string());
        Ok(MediaPlayer {
            id: name.to_string(),
            identity: clean(identity),
            status: parse_status(&status),
            title,
            artist,
            album,
            length_ms,
            position_ms: position.filter(|p| *p >= 0).map(|us| (us / 1000) as u64),
            can_next: p.get_property("CanGoNext").await.unwrap_or(false),
            can_prev: p.get_property("CanGoPrevious").await.unwrap_or(false),
            can_seek: p.get_property("CanSeek").await.unwrap_or(false),
        })
    }

    pub async fn state(&self) -> Result<MediaState> {
        let mut players = Vec::new();
        for name in self.player_names().await? {
            // A player that vanishes or hangs while we read it is skipped, not fatal.
            if let Ok(Ok(p)) = tokio::time::timeout(CALL_TIMEOUT, self.read_player(&name)).await {
                players.push(p);
            }
        }
        let players = dedupe(players);
        Ok(MediaState {
            current: pick_current(&players),
            players,
        })
    }

    pub async fn execute(&self, cmd: &MediaCommand) -> Result<()> {
        cmd.validate().map_err(Error::InvalidCommand)?;
        // Only a player that is running right now can be addressed.
        if !self.player_names().await?.iter().any(|n| n == cmd.player()) {
            return Err(Error::Rejected("that player is not running".into()));
        }
        let p = self.proxy(cmd.player(), PLAYER_IFACE).await?;
        let call = async {
            match cmd {
                MediaCommand::PlayPause { .. } => p.call_method("PlayPause", &()).await.map(drop),
                MediaCommand::Next { .. } => p.call_method("Next", &()).await.map(drop),
                MediaCommand::Previous { .. } => p.call_method("Previous", &()).await.map(drop),
                MediaCommand::SeekBy { offset_ms, .. } => p
                    .call_method("Seek", &(offset_ms.saturating_mul(1000),))
                    .await
                    .map(drop),
            }
        };
        tokio::time::timeout(CALL_TIMEOUT, call)
            .await
            .map_err(|_| Error::Timeout(CALL_TIMEOUT))?
            .map_err(err)
    }

    /// Emits `()` (debounced) when any player's state, or the set of players, may have changed.
    pub async fn watch(&self) -> Result<mpsc::Receiver<()>> {
        let rules = [
            MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .interface("org.freedesktop.DBus.Properties")
                .map_err(err)?
                .member("PropertiesChanged")
                .map_err(err)?
                .path(PLAYER_PATH)
                .map_err(err)?
                .build(),
            MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .interface(PLAYER_IFACE)
                .map_err(err)?
                .member("Seeked")
                .map_err(err)?
                .build(),
            MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender("org.freedesktop.DBus")
                .map_err(err)?
                .interface("org.freedesktop.DBus")
                .map_err(err)?
                .member("NameOwnerChanged")
                .map_err(err)?
                .build(),
        ];
        let mut streams = Vec::new();
        for r in rules {
            streams.push(
                MessageStream::for_match_rule(r, &self.conn, None)
                    .await
                    .map_err(err)?,
            );
        }
        let mut merged = futures_util::stream::select_all(streams);
        let (tx, rx) = mpsc::channel(4);
        tokio::spawn(async move {
            let mut pending = false;
            loop {
                let tick = tokio::time::sleep(Duration::from_millis(80));
                tokio::pin!(tick);
                tokio::select! {
                    m = merged.next() => match m {
                        Some(Ok(msg)) => {
                            // NameOwnerChanged fires for every bus name; only MPRIS ones matter.
                            let h = msg.header();
                            let is_name_change = h.member().is_some_and(|m| m.as_str() == "NameOwnerChanged");
                            if !is_name_change || msg.body().deserialize::<(String, String, String)>().map(|(n, _, _)| n.starts_with(MPRIS_PREFIX)).unwrap_or(false) {
                                pending = true;
                            }
                        }
                        Some(Err(_)) => {}
                        None => return,
                    },
                    _ = &mut tick, if pending => { pending = false; if tx.send(()).await.is_err() { return } }
                }
            }
        });
        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::process::{Child, Command, Stdio};
    use std::sync::{Arc, Mutex};
    use zbus::zvariant::Value as V;

    fn md(entries: Vec<(&str, V<'static>)>) -> HashMap<String, OwnedValue> {
        entries
            .into_iter()
            .map(|(k, v)| (k.to_string(), OwnedValue::try_from(v).unwrap()))
            .collect()
    }

    #[test]
    fn metadata_handles_the_type_variations_real_players_use() {
        // title string, artist as a list, length as i64 microseconds (Spotify-style)
        let m = md(vec![
            ("xesam:title", V::from("Night Drive")),
            ("xesam:artist", V::from(vec!["Moderator", "Guest"])),
            ("xesam:album", V::from("Bad Kingdom")),
            ("mpris:length", V::from(215_000_000i64)),
        ]);
        assert_eq!(
            parse_metadata(&m),
            (
                "Night Drive".into(),
                "Moderator, Guest".into(),
                "Bad Kingdom".into(),
                Some(215_000)
            )
        );
        // artist as a bare string, length as u64 (some browsers)
        let m = md(vec![
            ("xesam:artist", V::from("Solo")),
            ("mpris:length", V::from(60_000_000u64)),
        ]);
        assert_eq!(
            parse_metadata(&m),
            (String::new(), "Solo".into(), String::new(), Some(60_000))
        );
        // zero or negative length means "unknown", and control characters are stripped
        let m = md(vec![
            ("xesam:title", V::from("a\u{1b}[31mb")),
            ("mpris:length", V::from(0i64)),
        ]);
        assert_eq!(
            parse_metadata(&m),
            ("a[31mb".into(), String::new(), String::new(), None)
        );
        assert_eq!(
            parse_metadata(&HashMap::new()),
            (String::new(), String::new(), String::new(), None)
        );
    }

    fn player(
        id: &str,
        identity: &str,
        status: PlaybackStatus,
        title: &str,
        artist: &str,
        len: Option<u64>,
    ) -> MediaPlayer {
        MediaPlayer {
            id: id.into(),
            identity: identity.into(),
            status,
            title: title.into(),
            artist: artist.into(),
            album: String::new(),
            length_ms: len,
            position_ms: None,
            can_next: false,
            can_prev: false,
            can_seek: true,
        }
    }

    #[test]
    fn the_same_tab_reported_twice_by_a_browser_is_shown_once() {
        // what the reference machine really reports: Brave's own player + the Plasma integration, same video
        let a = player(
            "org.mpris.MediaPlayer2.brave.instance2038",
            "Brave",
            PlaybackStatus::Playing,
            "YouTube",
            "",
            Some(543_521),
        );
        let b = player(
            "org.mpris.MediaPlayer2.plasma-browser-integration",
            "Brave",
            PlaybackStatus::Playing,
            "Billie Eilish - Hotline Bling (slowed)",
            "BROTEX YT",
            Some(543_521),
        );
        let out = dedupe(vec![a.clone(), b.clone()]);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].id, b.id,
            "the one with the artist and the real title wins"
        );
        assert_eq!(
            dedupe(vec![b.clone(), a.clone()])[0].id,
            b.id,
            "order does not matter"
        );
    }

    #[test]
    fn genuinely_different_players_are_never_merged() {
        let spotify = player(
            "s",
            "Spotify",
            PlaybackStatus::Playing,
            "Song",
            "Artist",
            Some(200_000),
        );
        let brave = player(
            "b",
            "Brave",
            PlaybackStatus::Playing,
            "Video",
            "",
            Some(200_000),
        );
        let paused = player(
            "p",
            "Brave",
            PlaybackStatus::Paused,
            "Video",
            "",
            Some(200_000),
        );
        let unknown1 = player("u1", "Brave", PlaybackStatus::Playing, "Live A", "", None);
        let unknown2 = player("u2", "Brave", PlaybackStatus::Playing, "Live B", "", None);
        assert_eq!(
            dedupe(vec![spotify, brave, paused, unknown1, unknown2]).len(),
            5,
            "different app, different status, or unknown length: all kept"
        );
    }

    #[test]
    fn current_player_prefers_playing_then_paused() {
        let mk = |id: &str, status| MediaPlayer {
            id: id.into(),
            identity: id.into(),
            status,
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            length_ms: None,
            position_ms: None,
            can_next: false,
            can_prev: false,
            can_seek: false,
        };
        let ps = vec![
            mk("a", PlaybackStatus::Stopped),
            mk("b", PlaybackStatus::Paused),
            mk("c", PlaybackStatus::Playing),
        ];
        assert_eq!(pick_current(&ps).as_deref(), Some("c"));
        assert_eq!(pick_current(&ps[..2]).as_deref(), Some("b"));
        assert_eq!(pick_current(&ps[..1]).as_deref(), Some("a"));
        assert_eq!(pick_current(&[]), None);
    }

    // --- a fake MPRIS player on a PRIVATE session bus, so the user's real bus is never touched ---
    #[derive(Default)]
    struct Fake {
        status: String,
        position_us: i64,
        calls: Arc<Mutex<Vec<String>>>,
    }

    struct FakeRoot;

    #[zbus::interface(name = "org.mpris.MediaPlayer2")]
    impl FakeRoot {
        #[zbus(property)]
        fn identity(&self) -> String {
            "Fake Player".into()
        }
    }

    #[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
    impl Fake {
        fn play_pause(&mut self) {
            self.status = if self.status == "Playing" {
                "Paused".into()
            } else {
                "Playing".into()
            };
            self.calls.lock().unwrap().push("PlayPause".into());
        }
        fn next(&self) {
            self.calls.lock().unwrap().push("Next".into());
        }
        fn previous(&self) {
            self.calls.lock().unwrap().push("Previous".into());
        }
        fn seek(&mut self, offset: i64) {
            self.position_us += offset;
            self.calls.lock().unwrap().push(format!("Seek {offset}"));
        }
        #[zbus(property)]
        fn playback_status(&self) -> String {
            self.status.clone()
        }
        #[zbus(property)]
        fn position(&self) -> i64 {
            self.position_us
        }
        #[zbus(property)]
        fn metadata(&self) -> HashMap<String, OwnedValue> {
            md(vec![
                ("xesam:title", V::from("Test Track")),
                ("xesam:artist", V::from(vec!["Tester"])),
                ("mpris:length", V::from(200_000_000i64)),
            ])
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
    }

    struct Bus(Child);
    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn private_bus() -> (Bus, String) {
        use std::io::{BufRead, BufReader};
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("dbus-daemon is needed for this test");
        let mut addr = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut addr)
            .unwrap();
        (Bus(child), addr.trim().to_string())
    }

    #[tokio::test]
    async fn end_to_end_against_a_fake_player_on_a_private_bus() {
        let (_bus, addr) = private_bus();
        std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &addr);
        let calls = Arc::new(Mutex::new(vec![]));
        let _player = zbus::connection::Builder::session()
            .unwrap()
            .name("org.mpris.MediaPlayer2.zenvytest")
            .unwrap()
            .serve_at(PLAYER_PATH, FakeRoot)
            .unwrap()
            .serve_at(
                PLAYER_PATH,
                Fake {
                    status: "Paused".into(),
                    position_us: 5_000_000,
                    calls: calls.clone(),
                },
            )
            .unwrap()
            .build()
            .await
            .unwrap();
        // a decoy service that must never be reachable through the media API
        let _decoy = zbus::connection::Builder::session()
            .unwrap()
            .name("org.example.NotAPlayer")
            .unwrap()
            .build()
            .await
            .unwrap();

        let media = MediaAdapter::connect().await.unwrap();
        let mut events = media.watch().await.unwrap();

        let st = media.state().await.unwrap();
        assert_eq!(
            st.players.len(),
            1,
            "only the MPRIS player is listed, not the decoy or the bus itself"
        );
        let p = &st.players[0];
        assert_eq!(
            (
                p.id.as_str(),
                p.identity.as_str(),
                p.title.as_str(),
                p.artist.as_str()
            ),
            (
                "org.mpris.MediaPlayer2.zenvytest",
                "Fake Player",
                "Test Track",
                "Tester"
            )
        );
        assert_eq!(
            (p.status, p.length_ms, p.position_ms, p.can_next, p.can_seek),
            (
                PlaybackStatus::Paused,
                Some(200_000),
                Some(5_000),
                true,
                true
            )
        );
        assert_eq!(
            st.current.as_deref(),
            Some("org.mpris.MediaPlayer2.zenvytest")
        );

        let id = "org.mpris.MediaPlayer2.zenvytest".to_string();
        media
            .execute(&MediaCommand::PlayPause { player: id.clone() })
            .await
            .unwrap();
        media
            .execute(&MediaCommand::Next { player: id.clone() })
            .await
            .unwrap();
        media
            .execute(&MediaCommand::Previous { player: id.clone() })
            .await
            .unwrap();
        media
            .execute(&MediaCommand::SeekBy {
                player: id.clone(),
                offset_ms: -10_000,
            })
            .await
            .unwrap();
        assert_eq!(
            *calls.lock().unwrap(),
            vec!["PlayPause", "Next", "Previous", "Seek -10000000"],
            "fixed methods with microsecond seek"
        );
        assert_eq!(
            media.state().await.unwrap().players[0].status,
            PlaybackStatus::Playing
        );

        // commands aimed anywhere else are refused before reaching D-Bus
        for bad in [
            "org.example.NotAPlayer",
            "org.mpris.MediaPlayer2.notrunning",
            "org.freedesktop.DBus",
        ] {
            assert!(
                media
                    .execute(&MediaCommand::PlayPause { player: bad.into() })
                    .await
                    .is_err(),
                "{bad}"
            );
        }
        assert_eq!(calls.lock().unwrap().len(), 4, "nothing extra was called");

        // a player disappearing produces a (debounced) change notification
        while events.try_recv().is_ok() {}
        drop(_player);
        let got = tokio::time::timeout(Duration::from_secs(3), events.recv()).await;
        assert!(matches!(got, Ok(Some(()))), "player exit is announced");
        assert!(media.state().await.unwrap().players.is_empty());
    }
}
