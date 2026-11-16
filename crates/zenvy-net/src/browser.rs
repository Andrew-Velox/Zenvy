//! Browser bridge: an optional browser extension reports the user's tabs to the agent so the phone
//! can list, switch and close them (Hyprland only sees a browser as one window).
//!
//! Privacy and safety design:
//! * Loopback only (`127.0.0.1`), plain WebSocket, **token required** (256-bit, file mode 0600).
//! * The extension sends **titles and host names only**: never full URLs, and incognito windows are
//!   not visible to it unless the user explicitly allows that. The agent re-sanitises everything.
//! * A web page cannot talk to this port: any `Origin` that is not a browser-extension origin is refused.
//! * The agent only *relays* three typed commands (activate, close, new tab); the extension is never
//!   told to run anything else.

use crate::app::App;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;
use tokio::sync::mpsc;
use zenvy_auth::{b64, random_bytes};
use zenvy_proto::{CapState, CapabilityRecord};

const MAX_TABS: usize = 400;
const MAX_TITLE_CHARS: usize = 160;
const MAX_HOST_CHARS: usize = 80;
/// A site icon arrives as a small `data:` image; anything bigger is refused (a 32 px icon is 1-4 KB).
const MAX_FAVICON_CHARS: usize = 20_000;
const MAX_FAVICONS_PER_CONN: usize = 300;
const MAX_FAVICONS_PER_MESSAGE: usize = 24;
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const DEAD_AFTER: Duration = Duration::from_secs(70);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Tab {
    pub id: i64,
    pub window_id: i64,
    pub title: String,
    pub host: String,
    pub active: bool,
    pub pinned: bool,
    pub audible: bool,
}

#[derive(Deserialize)]
struct RawTab {
    id: i64,
    #[serde(rename = "windowId")]
    window_id: i64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    host: String,
    #[serde(default)]
    active: bool,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    audible: bool,
}

fn clean_title(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(MAX_TITLE_CHARS)
        .collect()
}

/// Host names only: letters, digits and `.-:[]`. Anything else (a path, a query, credentials) is dropped.
fn clean_host(s: &str) -> String {
    let h: String = s.chars().take(MAX_HOST_CHARS).collect();
    if h.chars()
        .all(|c| c.is_ascii_alphanumeric() || ".-:[]_ ".contains(c))
    {
        h
    } else {
        String::new()
    }
}

/// A host the phone may ask an icon for: a real host name, nothing else.
pub fn valid_host(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_HOST_CHARS
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-:[]_".contains(c))
}

/// Raster images only (never SVG): `data:image/png;base64,...` within the size cap and with strict base64 characters.
pub fn valid_data_url(s: &str) -> bool {
    const MIMES: [&str; 6] = ["png", "jpeg", "webp", "gif", "x-icon", "vnd.microsoft.icon"];
    let Some(rest) = s.strip_prefix("data:image/") else {
        return false;
    };
    let Some((mime, b64)) = rest.split_once(";base64,") else {
        return false;
    };
    s.len() <= MAX_FAVICON_CHARS
        && MIMES.contains(&mime)
        && !b64.is_empty()
        && b64
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
}

/// Validates the `icons` object of a `favicons` message: `{ host: data-url }`.
pub fn sanitize_favicons(raw: &Value) -> HashMap<String, String> {
    raw.get("icons")
        .and_then(Value::as_object)
        .map(|o| {
            o.iter()
                .take(MAX_FAVICONS_PER_MESSAGE)
                .filter(|(h, v)| valid_host(h) && v.as_str().is_some_and(valid_data_url))
                .map(|(h, v)| (h.clone(), v.as_str().unwrap_or_default().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Validates and bounds a tab list from the extension.
pub fn sanitize_tabs(raw: &Value) -> (Option<i64>, Vec<Tab>) {
    let focused = raw.get("focused_window").and_then(Value::as_i64);
    let tabs = raw
        .get("tabs")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .take(MAX_TABS)
                .filter_map(|t| serde_json::from_value::<RawTab>(t.clone()).ok())
                .filter(|t| t.id >= 0 && t.window_id >= 0)
                .map(|t| Tab {
                    id: t.id,
                    window_id: t.window_id,
                    title: clean_title(&t.title),
                    host: clean_host(&t.host),
                    active: t.active,
                    pinned: t.pinned,
                    audible: t.audible,
                })
                .collect()
        })
        .unwrap_or_default();
    (focused, tabs)
}

struct Conn {
    name: String,
    tx: mpsc::Sender<String>,
    focused: Option<i64>,
    tabs: Vec<Tab>,
    /// host -> data URL, sent by the extension from the browser's own icon cache; asked for lazily by phones.
    favicons: HashMap<String, String>,
}

/// All connected browser extensions and their tabs.
#[derive(Default)]
pub struct BrowserHub {
    conns: Mutex<BTreeMap<u64, Conn>>,
    next: AtomicU64,
    /// When a wrong token was last written to the audit log (a stale extension retries forever; log it once a minute).
    last_fail_logged: Mutex<Option<Instant>>,
}

impl BrowserHub {
    /// JSON view sent to phones: `{ browsers: [{ id, name, focused_window, windows: [{ id, tabs }] }] }`.
    pub fn view(&self) -> Value {
        let conns = self.conns.lock().unwrap();
        let browsers: Vec<Value> = conns
            .iter()
            .map(|(id, c)| {
                let mut wins: BTreeMap<i64, Vec<&Tab>> = BTreeMap::new();
                for t in &c.tabs {
                    wins.entry(t.window_id).or_default().push(t);
                }
                let windows: Vec<Value> = wins
                    .into_iter()
                    .map(|(wid, tabs)| json!({ "id": wid, "tabs": tabs }))
                    .collect();
                json!({ "id": id, "name": c.name, "focused_window": c.focused, "windows": windows })
            })
            .collect();
        json!({ "browsers": browsers })
    }

    fn register(&self, name: &str, tx: mpsc::Sender<String>) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let name: String = name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .take(24)
            .collect();
        self.conns.lock().unwrap().insert(
            id,
            Conn {
                name: if name.is_empty() {
                    "browser".into()
                } else {
                    name
                },
                tx,
                focused: None,
                tabs: vec![],
                favicons: HashMap::new(),
            },
        );
        id
    }

    /// Returns true if the stored tabs actually changed.
    fn update(&self, id: u64, focused: Option<i64>, tabs: Vec<Tab>) -> bool {
        let mut conns = self.conns.lock().unwrap();
        match conns.get_mut(&id) {
            Some(c) if c.focused != focused || c.tabs != tabs => {
                c.focused = focused;
                c.tabs = tabs;
                true
            }
            _ => false,
        }
    }

    /// Stores icons from one extension; returns the hosts that are new or changed.
    fn add_favicons(&self, id: u64, icons: HashMap<String, String>) -> Vec<String> {
        let mut conns = self.conns.lock().unwrap();
        let Some(c) = conns.get_mut(&id) else {
            return vec![];
        };
        let mut added = Vec::new();
        for (host, url) in icons {
            if c.favicons.get(&host) == Some(&url) {
                continue;
            }
            if !c.favicons.contains_key(&host) && c.favicons.len() >= MAX_FAVICONS_PER_CONN {
                continue;
            }
            c.favicons.insert(host.clone(), url);
            added.push(host);
        }
        added
    }

    /// The icon of a host from any connected browser.
    pub fn favicon(&self, host: &str) -> Option<String> {
        self.conns
            .lock()
            .unwrap()
            .values()
            .find_map(|c| c.favicons.get(host).cloned())
    }

    fn remove(&self, id: u64) {
        self.conns.lock().unwrap().remove(&id);
    }

    pub fn connected(&self) -> Vec<String> {
        self.conns
            .lock()
            .unwrap()
            .values()
            .map(|c| c.name.clone())
            .collect()
    }

    /// Relays one typed command to a connected browser. `Err` says why it could not be delivered.
    pub fn command(&self, browser: u64, cmd: &BrowserCommand) -> Result<(), &'static str> {
        let conns = self.conns.lock().unwrap();
        let c = conns.get(&browser).ok_or("that browser is not connected")?;
        c.tx.try_send(cmd.to_wire().to_string())
            .map_err(|_| "the browser did not accept the command")
    }
}

/// What a phone may ask a browser to do. Ids are plain non-negative integers.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum BrowserCommand {
    ActivateTab {
        browser: u64,
        tab_id: i64,
        window_id: i64,
    },
    CloseTab {
        browser: u64,
        tab_id: i64,
    },
    NewTab {
        browser: u64,
        window_id: i64,
    },
}

impl BrowserCommand {
    pub fn browser(&self) -> u64 {
        match self {
            Self::ActivateTab { browser, .. }
            | Self::CloseTab { browser, .. }
            | Self::NewTab { browser, .. } => *browser,
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        // Browsers hand out ids up to JavaScript's safe-integer range; anything beyond is not a real id.
        let ok = |v: &i64| (0..=9_007_199_254_740_991_i64).contains(v);
        match self {
            Self::ActivateTab {
                tab_id, window_id, ..
            } if !(ok(tab_id) && ok(window_id)) => Err("id out of range"),
            Self::CloseTab { tab_id, .. } if !ok(tab_id) => Err("id out of range"),
            Self::NewTab { window_id, .. } if !ok(window_id) => Err("id out of range"),
            _ => Ok(()),
        }
    }

    /// The exact message the extension receives (fixed shape, integers only).
    pub fn to_wire(&self) -> Value {
        match self {
            Self::ActivateTab {
                tab_id, window_id, ..
            } => json!({ "t": "activate", "tab_id": tab_id, "window_id": window_id }),
            Self::CloseTab { tab_id, .. } => json!({ "t": "close", "tab_id": tab_id }),
            Self::NewTab { window_id, .. } => json!({ "t": "new", "window_id": window_id }),
        }
    }
}

/// Loads the bridge token, creating a fresh 256-bit one (file mode 0600) on first use.
pub fn load_or_create_token(path: &Path) -> std::io::Result<String> {
    if let Ok(t) = std::fs::read_to_string(path) {
        let t = t.trim().to_string();
        if t.len() >= 32 {
            return Ok(t);
        }
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let t = b64(&random_bytes::<32>());
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(t.as_bytes())?;
    Ok(t)
}

/// Only browser-extension origins (or none, for non-browser tools) may connect. A web page never can.
pub fn origin_allowed(h: &HeaderMap) -> bool {
    match h.get(header::ORIGIN).and_then(|o| o.to_str().ok()) {
        None => !h.contains_key(header::ORIGIN),
        Some(o) => o.starts_with("chrome-extension://") || o.starts_with("moz-extension://"),
    }
}

pub fn host_allowed(h: &HeaderMap, port: u16) -> bool {
    let host = h
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    host.eq_ignore_ascii_case(&format!("127.0.0.1:{port}"))
        || host.eq_ignore_ascii_case(&format!("localhost:{port}"))
}

#[derive(Clone)]
struct BridgeState {
    app: Arc<App>,
    token: Arc<String>,
    port: u16,
}

fn set_capability(app: &App) {
    let names = app.browser.connected();
    let rec = if names.is_empty() {
        CapabilityRecord::not(
            CapState::Unavailable,
            "browser-extension",
            "no browser extension connected",
        )
    } else {
        CapabilityRecord::available(format!("extension: {}", names.join(", ")))
    };
    app.registry.set("browser.tabs", rec);
}

pub fn publish_view(app: &App) {
    app.publish("browser.tabs", app.browser.view());
}

/// Serves the loopback bridge until the process exits.
pub async fn serve_browser(app: Arc<App>, addr: SocketAddr, token: String) -> std::io::Result<()> {
    debug_assert!(
        addr.ip().is_loopback(),
        "the browser bridge must never listen beyond loopback"
    );
    if !addr.ip().is_loopback() {
        return Err(std::io::Error::other(
            "browser bridge refuses to listen on a non-loopback address",
        ));
    }
    set_capability(&app);
    let state = BridgeState {
        app,
        token: Arc::new(token),
        port: addr.port(),
    };
    let router = Router::new()
        .route("/browser", get(upgrade))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
}

async fn upgrade(
    State(st): State<BridgeState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !host_allowed(&headers, st.port) {
        return (StatusCode::MISDIRECTED_REQUEST, "unknown host").into_response();
    }
    if !origin_allowed(&headers) {
        st.app.audit.record(
            "browser_bad_origin",
            None,
            "web page tried the browser bridge",
        );
        return (StatusCode::FORBIDDEN, "bad origin").into_response();
    }
    ws.max_message_size(256 * 1024)
        .max_frame_size(256 * 1024)
        .on_upgrade(move |s| session(st, peer.ip(), s))
}

async fn session(st: BridgeState, ip: IpAddr, mut s: WebSocket) {
    let app = st.app.clone();
    // 1. hello with the token, within a few seconds.
    let hello = tokio::time::timeout(HELLO_TIMEOUT, s.recv()).await;
    let parsed = match hello {
        Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<Value>(&t).ok(),
        _ => None,
    };
    let ok = parsed.as_ref().is_some_and(|v| {
        v["t"] == "hello"
            && v["token"]
                .as_str()
                .is_some_and(|t| bool::from(t.as_bytes().ct_eq(st.token.as_bytes())))
    });
    if !ok {
        // No lockout: the token has 256 bits (it cannot be guessed), and everything on loopback shares one IP,
        // so a lockout would let one stale extension block the correct one. A short delay throttles retries.
        let log_now = {
            let mut last = app.browser.last_fail_logged.lock().unwrap();
            let due = last.is_none_or(|t| t.elapsed() > Duration::from_secs(60));
            if due {
                *last = Some(Instant::now());
            }
            due
        };
        if log_now {
            app.audit.record(
                "browser_auth_fail",
                None,
                "wrong or missing token (repeats are not logged for a minute)",
            );
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        let _ = s
            .send(Message::Text(
                json!({ "t": "error", "message": "bad token" })
                    .to_string()
                    .into(),
            ))
            .await;
        return;
    }
    let _ = ip;
    let name = parsed
        .as_ref()
        .and_then(|v| v["browser"].as_str())
        .unwrap_or("browser")
        .to_string();
    let (tx, mut rx) = mpsc::channel::<String>(32);
    let id = app.browser.register(&name, tx);
    app.audit.record("browser_connected", None, &name);
    set_capability(&app);
    publish_view(&app);
    let _ = s
        .send(Message::Text(json!({ "t": "ready" }).to_string().into()))
        .await;

    let mut ping = tokio::time::interval(Duration::from_secs(20));
    ping.tick().await;
    let mut last_rx = Instant::now();
    loop {
        tokio::select! {
            msg = s.recv() => {
                let Some(Ok(msg)) = msg else { break };
                last_rx = Instant::now();
                if let Message::Text(t) = msg {
                    let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                    if v["t"] == "tabs" {
                        let (focused, tabs) = sanitize_tabs(&v["data"]);
                        if app.browser.update(id, focused, tabs) { publish_view(&app); }
                    } else if v["t"] == "favicons" {
                        let added = app.browser.add_favicons(id, sanitize_favicons(&v["data"]));
                        // hosts only: the images are fetched on demand, so a phone that asked too early can ask again
                        if !added.is_empty() { app.publish("browser.favicons", json!({ "hosts": added })); }
                    }
                } else if matches!(msg, Message::Close(_)) { break }
            }
            cmd = rx.recv() => {
                let Some(c) = cmd else { break };
                if s.send(Message::Text(c.into())).await.is_err() { break }
            }
            _ = ping.tick() => {
                if last_rx.elapsed() > DEAD_AFTER { break }
                if s.send(Message::Ping(Vec::new().into())).await.is_err() { break }
            }
        }
    }
    app.browser.remove(id);
    app.audit.record("browser_disconnected", None, &name);
    set_capability(&app);
    publish_view(&app);
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    const PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAA";

    #[test]
    fn only_small_raster_images_for_real_hosts_are_accepted() {
        assert!(valid_data_url(PNG));
        assert!(valid_data_url("data:image/x-icon;base64,AAABAAEA"));
        for bad in [
            "data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=",
            "data:text/html;base64,PHNjcmlwdD4=",
            "data:image/png;base64,",
            "data:image/png,rawbytes",
            "https://example.com/favicon.ico",
            "data:image/png;base64,AA AA",
            "data:image/png;base64,AA\"onerror=x",
            "javascript:alert(1)",
            "",
        ] {
            assert!(!valid_data_url(bad), "{bad:?}");
        }
        let huge = format!("data:image/png;base64,{}", "A".repeat(MAX_FAVICON_CHARS));
        assert!(!valid_data_url(&huge));
        for h in ["docs.rs", "localhost:8080", "[::1]", "a-b.c_d.example"] {
            assert!(valid_host(h), "{h}");
        }
        for h in [
            "",
            "a/b",
            "a b",
            "bank.example/account?token=abc",
            "x\u{e9}",
            &"a".repeat(81),
        ] {
            assert!(!valid_host(h), "{h:?}");
        }
    }

    #[test]
    fn favicon_messages_are_filtered_entry_by_entry() {
        let raw = json!({ "icons": {
            "docs.rs": PNG,
            "evil.example": "data:image/svg+xml;base64,PHN2Zz4=",
            "a/b": PNG,
            "": PNG,
            "numbers.example": 7,
        } });
        let m = sanitize_favicons(&raw);
        assert_eq!(m.len(), 1);
        assert_eq!(m.get("docs.rs").map(String::as_str), Some(PNG));
        assert!(
            sanitize_favicons(&json!({ "icons": "no" })).is_empty()
                && sanitize_favicons(&json!({})).is_empty()
        );
        let many: serde_json::Map<String, Value> = (0..100)
            .map(|i| (format!("h{i}.example"), json!(PNG)))
            .collect();
        assert_eq!(
            sanitize_favicons(&json!({ "icons": many })).len(),
            MAX_FAVICONS_PER_MESSAGE,
            "one message cannot flood the store"
        );
    }

    #[test]
    fn the_hub_stores_serves_and_forgets_favicons() {
        let hub = BrowserHub::default();
        let (tx, _rx) = mpsc::channel(4);
        let id = hub.register("brave", tx);
        let one: HashMap<String, String> = [("docs.rs".to_string(), PNG.to_string())].into();
        assert_eq!(
            hub.add_favicons(id, one.clone()),
            vec!["docs.rs".to_string()]
        );
        assert!(
            hub.add_favicons(id, one).is_empty(),
            "the same icon again is not news"
        );
        assert_eq!(hub.favicon("docs.rs").as_deref(), Some(PNG));
        assert_eq!(hub.favicon("unknown.example"), None);
        let flood: HashMap<String, String> = (0..MAX_FAVICONS_PER_CONN + 50)
            .map(|i| (format!("h{i}.example"), PNG.to_string()))
            .collect();
        hub.add_favicons(id, flood);
        assert!(
            hub.conns.lock().unwrap()[&id].favicons.len() <= MAX_FAVICONS_PER_CONN,
            "bounded per browser"
        );
        hub.remove(id);
        assert_eq!(
            hub.favicon("docs.rs"),
            None,
            "an icon list never outlives its browser connection"
        );
    }

    fn hm(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        m
    }

    #[test]
    fn sanitize_bounds_and_cleans() {
        let long = "x".repeat(500);
        let mut tabs = vec![];
        for i in 0..600 {
            tabs.push(json!({ "id": i, "windowId": 1, "title": format!("t{i}\u{1b}[31m{long}"), "host": "example.com/path?q=secret", "active": i == 0 }));
        }
        tabs.push(json!({ "id": -1, "windowId": 1 }));
        tabs.push(json!({ "nonsense": true }));
        let (f, out) = sanitize_tabs(&json!({ "focused_window": 7, "tabs": tabs }));
        assert_eq!(f, Some(7));
        assert_eq!(out.len(), MAX_TABS, "tab count is capped");
        assert!(
            out[0].title.chars().count() <= MAX_TITLE_CHARS && !out[0].title.contains('\u{1b}'),
            "titles are bounded and control characters stripped"
        );
        assert_eq!(
            out[0].host, "",
            "a host containing a path or query is dropped entirely, never forwarded"
        );
    }

    #[test]
    fn plain_hosts_survive() {
        let (_, out) = sanitize_tabs(
            &json!({ "tabs": [{ "id": 1, "windowId": 2, "title": "A", "host": "docs.rs" }, { "id": 2, "windowId": 2, "host": "localhost:8080" }, { "id": 3, "windowId": 2, "host": "[::1]" }] }),
        );
        assert_eq!(
            out.iter().map(|t| t.host.as_str()).collect::<Vec<_>>(),
            ["docs.rs", "localhost:8080", "[::1]"]
        );
    }

    #[test]
    fn garbage_is_harmless() {
        for v in [
            json!(null),
            json!("x"),
            json!({ "tabs": "no" }),
            json!({ "tabs": [1, 2, 3] }),
        ] {
            let (_, t) = sanitize_tabs(&v);
            assert!(t.is_empty());
        }
    }

    #[test]
    fn hub_groups_by_window_and_reports_changes_only_once() {
        let hub = BrowserHub::default();
        let (tx, _rx) = mpsc::channel(4);
        let id = hub.register("brave", tx);
        let tab = |i, w, a| Tab {
            id: i,
            window_id: w,
            title: format!("T{i}"),
            host: "h".into(),
            active: a,
            pinned: false,
            audible: false,
        };
        assert!(hub.update(
            id,
            Some(1),
            vec![tab(1, 1, true), tab(2, 1, false), tab(3, 9, true)]
        ));
        assert!(
            !hub.update(
                id,
                Some(1),
                vec![tab(1, 1, true), tab(2, 1, false), tab(3, 9, true)]
            ),
            "identical snapshot is not a change"
        );
        let v = hub.view();
        let b = &v["browsers"][0];
        assert_eq!(b["name"], "brave");
        assert_eq!(b["windows"].as_array().unwrap().len(), 2);
        assert_eq!(b["windows"][0]["tabs"].as_array().unwrap().len(), 2);
        hub.remove(id);
        assert!(hub.view()["browsers"].as_array().unwrap().is_empty());
    }

    #[test]
    fn commands_are_typed_validated_and_fixed_shape() {
        let c: BrowserCommand =
            serde_json::from_str(r#"{"cmd":"activate_tab","browser":1,"tab_id":42,"window_id":7}"#)
                .unwrap();
        assert_eq!(
            c.to_wire(),
            json!({ "t": "activate", "tab_id": 42, "window_id": 7 })
        );
        assert!(
            serde_json::from_str::<BrowserCommand>(r#"{"cmd":"eval","code":"alert(1)"}"#).is_err()
        );
        assert!(serde_json::from_str::<BrowserCommand>(
            r#"{"cmd":"close_tab","browser":1,"tab_id":"1; drop"}"#
        )
        .is_err());
        assert!(BrowserCommand::CloseTab {
            browser: 1,
            tab_id: -5
        }
        .validate()
        .is_err());
        assert!(BrowserCommand::NewTab {
            browser: 1,
            window_id: i64::MAX
        }
        .validate()
        .is_err());
        assert!(
            BrowserCommand::CloseTab {
                browser: 1,
                tab_id: 2_049_152_584
            }
            .validate()
            .is_ok(),
            "real Chromium tab ids can exceed 2^31/2"
        );
    }

    #[test]
    fn command_to_missing_browser_fails_cleanly() {
        let hub = BrowserHub::default();
        assert!(hub
            .command(
                99,
                &BrowserCommand::CloseTab {
                    browser: 99,
                    tab_id: 1
                }
            )
            .is_err());
        let (tx, mut rx) = mpsc::channel(4);
        let id = hub.register("brave", tx);
        hub.command(
            id,
            &BrowserCommand::NewTab {
                browser: id,
                window_id: 3,
            },
        )
        .unwrap();
        assert_eq!(
            rx.try_recv().unwrap(),
            json!({ "t": "new", "window_id": 3 }).to_string()
        );
    }

    #[test]
    fn web_pages_cannot_use_the_bridge() {
        assert!(origin_allowed(&hm(&[])));
        assert!(origin_allowed(&hm(&[(
            "origin",
            "chrome-extension://abcdefghijklmnop"
        )])));
        assert!(origin_allowed(&hm(&[("origin", "moz-extension://1234")])));
        for bad in [
            "https://evil.example",
            "http://127.0.0.1:7879",
            "null",
            "file://",
            "chrome://settings",
        ] {
            assert!(!origin_allowed(&hm(&[("origin", bad)])), "{bad}");
        }
    }

    #[test]
    fn host_header_must_be_loopback() {
        assert!(host_allowed(&hm(&[("host", "127.0.0.1:7879")]), 7879));
        assert!(host_allowed(&hm(&[("host", "localhost:7879")]), 7879));
        assert!(!host_allowed(&hm(&[("host", "evil.example:7879")]), 7879));
        assert!(!host_allowed(&hm(&[("host", "127.0.0.1:1")]), 7879));
        assert!(!host_allowed(&hm(&[]), 7879));
    }

    #[test]
    fn token_is_created_once_with_private_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("browser.token");
        let a = load_or_create_token(&p).unwrap();
        assert!(a.len() >= 40);
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            a,
            load_or_create_token(&p).unwrap(),
            "stable across restarts"
        );
    }
}
