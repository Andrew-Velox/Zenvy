use crate::app::{App, Hub};
use crate::browser::BrowserCommand;
use crate::state::snapshot;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use zenvy_auth::{b64, new_nonce, unb64, verify_signature};
use zenvy_core::{Compositor, Input};
use zenvy_proto::{
    AudioCommand, BrightnessCommand, Button, ButtonState, DesktopCommand, InputCommand,
    ListenCommand, MediaCommand, PowerCommand, UnlockCommand,
};

const AUTH_TIMEOUT: Duration = Duration::from_secs(10);
const PING_EVERY: Duration = Duration::from_secs(10);
const DEAD_AFTER: Duration = Duration::from_secs(25);
const MAX_BAD_MESSAGES: u32 = 10;

#[derive(Deserialize)]
struct Msg {
    t: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    data: Value,
}

#[derive(Deserialize)]
struct AuthData {
    device_id: String,
    sig: String,
}

/// Browsers always send `Origin` on WebSocket upgrades; if present it must be our own origin
/// (blocks cross-site WebSocket hijacking from a page the phone happens to have open).
fn origin_ok(h: &HeaderMap) -> bool {
    match h.get(header::ORIGIN) {
        None => true, // non-browser client; a browser-borne attack always carries an Origin
        Some(o) => {
            let o = o.to_str().unwrap_or("");
            let host = h
                .get(header::HOST)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            o.strip_prefix("https://")
                .is_some_and(|a| a.eq_ignore_ascii_case(host))
        }
    }
}

pub async fn ws_handler(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !origin_ok(&headers) {
        app.audit
            .record("ws_bad_origin", None, &peer.ip().to_string());
        return (StatusCode::FORBIDDEN, "bad origin").into_response();
    }
    if app
        .limiter
        .lock()
        .unwrap()
        .check(&peer.ip(), Instant::now())
        .is_err()
    {
        return (StatusCode::TOO_MANY_REQUESTS, "slow down").into_response();
    }
    ws.max_message_size(16 * 1024)
        .max_frame_size(16 * 1024)
        .on_upgrade(move |s| session(app, peer.ip(), s))
}

async fn send(s: &mut WebSocket, v: Value) -> bool {
    s.send(Message::Text(v.to_string().into())).await.is_ok()
}

async fn close(s: &mut WebSocket, code: u16, why: &'static str) {
    let _ = s
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: why.into(),
        })))
        .await;
}

/// Challenge → signature. Returns the authenticated device id.
async fn authenticate(app: &App, ip: IpAddr, s: &mut WebSocket) -> Option<String> {
    let nonce = new_nonce();
    if !send(
        s,
        json!({ "v": 1, "t": "challenge", "data": { "nonce": b64(&nonce) } }),
    )
    .await
    {
        return None;
    }
    let fail = |why: &str, dev: Option<&str>| {
        app.limiter.lock().unwrap().fail(&ip, Instant::now());
        app.audit.record("auth_fail", dev, &format!("{ip}: {why}"));
    };
    let first = tokio::time::timeout(AUTH_TIMEOUT, s.recv()).await;
    let Ok(Some(Ok(Message::Text(text)))) = first else {
        fail("no auth message", None);
        return None;
    };
    let parsed = serde_json::from_str::<Msg>(&text)
        .ok()
        .filter(|m| m.t == "auth")
        .and_then(|m| serde_json::from_value::<AuthData>(m.data).ok());
    let Some(auth) = parsed else {
        fail("malformed auth", None);
        return None;
    };
    let (pubkey, scopes_ok) = {
        let st = app.store.lock().unwrap();
        match st.get(&auth.device_id) {
            Some(d) => (d.pubkey_bytes(), true),
            None => (None, false),
        }
    };
    // Same outcome and timing for "unknown device" and "bad signature".
    let sig = unb64(&auth.sig).unwrap_or_default();
    let ok =
        scopes_ok && pubkey.is_some_and(|pk| verify_signature(&pk, &auth.device_id, &nonce, &sig));
    if !ok {
        fail("bad signature or unknown device", Some(&auth.device_id));
        return None;
    }
    app.limiter.lock().unwrap().success(&ip);
    app.store.lock().unwrap().touch(&auth.device_id);
    app.audit
        .record("auth_ok", Some(&auth.device_id), &ip.to_string());
    Some(auth.device_id)
}

/// Token bucket: `rate` tokens/s, bursts up to `cap`.
struct Bucket {
    tokens: f64,
    rate: f64,
    cap: f64,
    last: Instant,
}

impl Bucket {
    fn new(rate: f64, cap: f64) -> Self {
        Self {
            tokens: cap,
            rate,
            cap,
            last: Instant::now(),
        }
    }
    fn take(&mut self) -> bool {
        let now = Instant::now();
        self.tokens =
            (self.tokens + now.duration_since(self.last).as_secs_f64() * self.rate).min(self.cap);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Per-connection state used while handling messages.
struct Ctx {
    device_id: String,
    scopes: HashSet<String>,
    /// Pointer buttons this phone is currently holding; released if it disconnects.
    held: HashSet<Button>,
    /// Present while this phone is listening to the PC's audio.
    listener: Option<zenvy_stream::Listener>,
    /// Whether this phone asked to silence the PC speakers while listening.
    muting: bool,
    /// A suspend/reboot/power-off this phone asked for and has not confirmed yet: (token, action, deadline).
    power_pending: Option<(String, zenvy_proto::PowerAction, std::time::Instant)>,
    /// Present while this phone is viewing the PC's screen.
    screen: Option<ScreenSession>,
    /// A confirm-first action this phone asked for: (token, action id, deadline).
    action_pending: Option<(String, String, std::time::Instant)>,
}

async fn session(app: Arc<App>, ip: IpAddr, mut s: WebSocket) {
    let Some(device_id) = authenticate(&app, ip, &mut s).await else {
        close(&mut s, 4401, "auth failed").await;
        return;
    };
    let scopes: HashSet<String> = app
        .store
        .lock()
        .unwrap()
        .get(&device_id)
        .map(|d| d.scopes.iter().cloned().collect())
        .unwrap_or_default();
    let mut ctx = Ctx {
        device_id: device_id.clone(),
        scopes,
        held: HashSet::new(),
        listener: None,
        screen: None,
        muting: false,
        power_pending: None,
        action_pending: None,
    };
    // Subscribe before taking the snapshot so no event can fall between the two.
    let mut hub = app.hub.subscribe();
    app.refresh_lock().await; // a new session never starts from a stale lock state
    let ready = json!({ "v": 1, "t": "ready", "data": { "device_id": device_id, "proto": 1, "session_ttl_s": app.cfg.session_ttl.as_secs() } });
    if !send(&mut s, ready).await
        || !send(
            &mut s,
            json!({ "v": 1, "t": "snapshot", "data": snapshot(&app).await }),
        )
        .await
    {
        return;
    }

    let deadline = tokio::time::Instant::now() + app.cfg.session_ttl;
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    let mut last_rx = Instant::now();
    // Discrete commands: 30/s sustained. Pointer/scroll stream (touchpad at up to 120 Hz): 240/s.
    let (mut slow, mut fast) = (Bucket::new(30.0, 60.0), Bucket::new(240.0, 480.0));
    let mut bad = 0u32;

    loop {
        tokio::select! {
            msg = s.recv() => {
                let Some(Ok(msg)) = msg else { break };
                last_rx = Instant::now();
                match msg {
                    Message::Text(t) => {
                        let Ok(m) = serde_json::from_str::<Msg>(&t) else {
                            bad += 1;
                            if bad >= MAX_BAD_MESSAGES { close(&mut s, 4400, "too many bad messages").await; break }
                            continue;
                        };
                        let streaming = m.t == "input" && matches!(m.data.get("cmd").and_then(Value::as_str), Some("pointer_move" | "scroll"));
                        if !(if streaming { fast.take() } else { slow.take() }) {
                            // Stream overload is dropped silently (replying would add to the flood).
                            if !streaming && !send(&mut s, json!({ "v": 1, "t": "err", "id": m.id, "data": { "code": "rate_limited" } })).await { break }
                            continue;
                        }
                        let (reply, was_bad) = handle(&app, &mut ctx, m).await;
                        if was_bad {
                            bad += 1;
                            if bad >= MAX_BAD_MESSAGES { close(&mut s, 4400, "too many bad messages").await; break }
                        }
                        if let Some(r) = reply { if !send(&mut s, r).await { break } }
                    }
                    Message::Close(_) => break,
                    Message::Binary(_) => { close(&mut s, 4400, "binary not supported").await; break }
                    Message::Ping(_) | Message::Pong(_) => {}
                }
            }
            // PC audio for a listening phone (only this phone's own listener; nothing else ever sends binary frames)
            frame = async { match ctx.listener.as_mut() { Some(l) => l.recv().await, None => std::future::pending().await } } => {
                let Some(f) = frame else { break };
                if s.send(Message::Binary(axum::body::Bytes::copy_from_slice(&f))).await.is_err() { break }
            }
            // Screen frames for a phone that is viewing (never while the PC is locked)
            frame = async { match ctx.screen.as_mut() { Some(v) => v.viewer.recv().await, None => std::future::pending().await } } => {
                let Some(f) = frame else { break };
                if app.is_locked() {
                    stop_screen(&app, &mut ctx).await;
                    if !send(&mut s, json!({ "v": 1, "t": "screen", "data": { "state": "stopped", "reason": "The PC is locked" } })).await { break }
                } else if s.send(Message::Binary(axum::body::Bytes::copy_from_slice(&f))).await.is_err() { break }
            }
            ev = hub.recv() => match ev {
                Ok(Hub::Scopes(id)) if id == device_id => {
                    ctx.scopes = app.store.lock().unwrap().get(&device_id).map(|d| d.scopes.iter().cloned().collect()).unwrap_or_default();
                    if !ctx.scopes.contains("screen") && ctx.screen.is_some() { stop_screen(&app, &mut ctx).await; let _ = send(&mut s, json!({ "v": 1, "t": "screen", "data": { "state": "stopped", "reason": "Permission removed" } })).await; }
                    if !ctx.scopes.contains("listen") && ctx.listener.is_some() { stop_listening(&app, &mut ctx).await; let _ = send(&mut s, json!({ "v": 1, "t": "listen", "data": { "state": "stopped", "reason": "permission removed" } })).await; }
                }
                Ok(Hub::Scopes(_)) => {}
                Ok(Hub::Evt(text)) => { if s.send(Message::Text(text.as_ref().into())).await.is_err() { break } }
                Ok(Hub::Revoked(id)) if id == device_id => { close(&mut s, 4403, "revoked").await; break }
                Ok(Hub::Revoked(_)) => {}
                // A slow phone missed events: resync it with a full snapshot instead of dropping it.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    app.refresh_lock().await;
                    if !send(&mut s, json!({ "v": 1, "t": "snapshot", "data": snapshot(&app).await })).await { break }
                }
                Err(_) => break,
            },
            _ = ping.tick() => {
                if last_rx.elapsed() > DEAD_AFTER { break }
                if s.send(Message::Ping(Vec::new().into())).await.is_err() { break }
            }
            _ = tokio::time::sleep_until(deadline) => { close(&mut s, 4408, "session expired").await; break }
        }
    }
    // A phone that disappears must stop capturing/muting for it too.
    if ctx.listener.is_some() {
        stop_listening(&app, &mut ctx).await;
    }
    if ctx.screen.is_some() {
        stop_screen(&app, &mut ctx).await;
    }
    // Never leave a pointer button stuck down because a phone vanished mid-drag.
    if let Some(input) = &app.input {
        input.release_buttons(ctx.held.drain());
    }
    app.audit
        .record("disconnect", Some(&device_id), &ip.to_string());
}

/// One phone's screen view: its place in the shared capture and which monitor it looks at.
struct ScreenSession {
    viewer: zenvy_screen::Viewer,
    output: String,
    preset: zenvy_proto::ScreenPreset,
}

fn device_name(app: &App, id: &str) -> String {
    crate::listen::clean_name(
        &app.store
            .lock()
            .unwrap()
            .get(id)
            .map(|d| d.name.clone())
            .unwrap_or_default(),
    )
}

/// Leaves the screen stream (the last viewer out stops capture) and tells the person at the PC.
async fn stop_screen(app: &App, ctx: &mut Ctx) {
    if ctx.screen.take().is_some() {
        app.audit.record("screen_stop", Some(&ctx.device_id), "");
        crate::listen::notify_text(
            app,
            &format!(
                "{} stopped viewing this PC's screen",
                device_name(app, &ctx.device_id)
            ),
        )
        .await;
    }
}

fn geom(m: &zenvy_proto::Monitor) -> zenvy_screen::MonitorGeom {
    zenvy_screen::MonitorGeom {
        x: m.x,
        y: m.y,
        width: m.width,
        height: m.height,
        scale: m.scale,
        transform: m.transform,
    }
}

/// Leaves the audio stream, undoes the PC mute this phone asked for, and tells the person at the PC.
async fn stop_listening(app: &App, ctx: &mut Ctx) {
    ctx.listener = None; // dropping it leaves the stream (the last one out stops capture)
    if std::mem::take(&mut ctx.muting) {
        app.listen.release().await;
    }
    let name = app
        .store
        .lock()
        .unwrap()
        .get(&ctx.device_id)
        .map(|d| d.name.clone())
        .unwrap_or_default();
    app.audit.record("listen_stop", Some(&ctx.device_id), "");
    crate::listen::notify(app, &name, false).await;
}

/// Handles one message. Returns the reply (if any) and whether the message counts as "bad".
async fn handle(app: &Arc<App>, ctx: &mut Ctx, m: Msg) -> (Option<Value>, bool) {
    let id = m.id.clone();
    let err = |code: &str, msg: &str, bad: bool| {
        (
            Some(json!({ "v": 1, "t": "err", "id": id, "data": { "code": code, "message": msg } })),
            bad,
        )
    };
    let ack = || json!({ "v": 1, "t": "ack", "id": id, "data": { "ok": true } });
    // A locked PC: no windows, no keyboard or mouse, no browser tabs. Media, volume, brightness, power and unlock still work.
    if app.is_locked()
        && matches!(
            m.t.as_str(),
            "cmd" | "input" | "browser" | "favicon" | "action" | "clipboard" | "file" | "screen" | "apps"
        )
    {
        return err("pc_locked", "The PC is locked. Unlock it first.", false);
    }
    match m.t.as_str() {
        "ping" => (Some(json!({ "v": 1, "t": "pong", "id": id })), false),
        "cmd" => {
            let cmd: DesktopCommand = match serde_json::from_value(m.data) {
                Ok(c) => c,
                Err(_) => return err("invalid_command", "unknown or malformed command", true),
            };
            if let Err(e) = cmd.validate() {
                return err("invalid_command", e, true);
            }
            if !ctx.scopes.contains("windows") {
                return err("forbidden", "this device lacks the 'windows' scope", false);
            }
            if matches!(cmd, DesktopCommand::CloseWindow { .. }) {
                app.audit
                    .record("cmd", Some(&ctx.device_id), "close_window");
            }
            match app.adapter.execute(&cmd).await {
                Ok(()) => (Some(ack()), false),
                Err(e) => err("failed", &e.to_string(), false),
            }
        }
        "input" => {
            let cmd: InputCommand = match serde_json::from_value(m.data) {
                Ok(c) => c,
                Err(_) => {
                    return err(
                        "invalid_command",
                        "unknown or malformed input command",
                        true,
                    )
                }
            };
            if let Err(e) = cmd.validate() {
                return err("invalid_command", e, true);
            }
            if !ctx.scopes.contains("input") {
                return err("forbidden", "this device lacks the 'input' scope", false);
            }
            let Some(input) = &app.input else {
                return err(
                    "capability_unavailable",
                    "input injection is not available on this PC",
                    false,
                );
            };
            // Track held buttons first so a failure mid-drag still gets released on disconnect.
            if let InputCommand::Button { button, state } = &cmd {
                match state {
                    ButtonState::Down => {
                        ctx.held.insert(*button);
                    }
                    ButtonState::Up => {
                        ctx.held.remove(button);
                    }
                    ButtonState::Click => {}
                }
            }
            if matches!(cmd, InputCommand::Text { .. }) {
                app.audit
                    .record("input_text", Some(&ctx.device_id), "text entry"); // length/contents deliberately not logged
            }
            match input.execute(&cmd).await {
                // High-rate commands are fire-and-forget unless the phone supplied an id.
                Ok(()) => (id.as_ref().map(|_| ack()), false),
                Err(e) => err("failed", &e.to_string(), false),
            }
        }
        "favicon" => {
            // A site's icon (a small image the browser extension took from the browser's own cache). Hosts only.
            let host = m
                .data
                .get("host")
                .and_then(|h| h.as_str())
                .unwrap_or("")
                .to_string();
            if !crate::browser::valid_host(&host) {
                return err("invalid_command", "bad host", true);
            }
            let url = app.browser.favicon(&host);
            (
                Some(
                    json!({ "v": 1, "t": "favicon", "id": id, "data": { "host": host, "url": url } }),
                ),
                false,
            )
        }
        "icon" => {
            // The phone asks for the icon of an application window by its class (once per class, then it caches).
            let class = m
                .data
                .get("class")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            if !zenvy_icons::valid_class(&class) {
                return err("invalid_command", "bad window class", true);
            }
            let icons = app.icons.clone();
            let c2 = class.clone();
            let found = tokio::task::spawn_blocking(move || icons.resolve(&c2))
                .await
                .ok()
                .flatten();
            use base64::Engine;
            let data = match found {
                Some(i) => {
                    json!({ "class": class, "mime": i.mime, "b64": base64::engine::general_purpose::STANDARD.encode(&i.bytes) })
                }
                None => json!({ "class": class, "none": true }),
            };
            (
                Some(json!({ "v": 1, "t": "icon", "id": id, "data": data })),
                false,
            )
        }
        "file" => {
            // Permission to move files: the phone gets a one-time ticket for the HTTPS transfer, the bytes do not use this socket.
            if !ctx.scopes.contains("files") {
                return err("forbidden", &format!("This phone may not transfer files yet. On the PC run: zenvyd grant {} files", ctx.device_id), false);
            }
            let Some(files) = &app.cfg.files else {
                return err(
                    "capability_unavailable",
                    "file transfer is turned off on this PC",
                    false,
                );
            };
            match m.data.get("cmd").and_then(Value::as_str).unwrap_or("") {
                "offer" => {
                    let (Some(name), Some(size)) = (
                        m.data.get("name").and_then(Value::as_str),
                        m.data.get("size").and_then(Value::as_u64),
                    ) else {
                        return err("invalid_command", "name and size are required", true);
                    };
                    match files.offer_upload(&ctx.device_id, name, size) {
                        Ok((ticket, clean)) => (
                            Some(
                                json!({ "v": 1, "t": "file", "id": id, "data": { "state": "ticket", "url": format!("/upload/{ticket}"), "name": clean } }),
                            ),
                            false,
                        ),
                        Err(e) => err("refused", e, false),
                    }
                }
                "accept" => {
                    let Some(offer) = m.data.get("id").and_then(Value::as_str) else {
                        return err("invalid_command", "id is required", true);
                    };
                    match files.accept_outgoing(offer, &ctx.device_id) {
                        Some((ticket, name, size)) => (
                            Some(
                                json!({ "v": 1, "t": "file", "id": id, "data": { "state": "ticket", "url": format!("/download/{ticket}"), "name": name, "size": size } }),
                            ),
                            false,
                        ),
                        None => err("not_found", "that offer is gone", false),
                    }
                }
                "dismiss" => {
                    let Some(offer) = m.data.get("id").and_then(Value::as_str) else {
                        return err("invalid_command", "id is required", true);
                    };
                    files.dismiss_outgoing(offer);
                    (Some(ack()), false)
                }
                _ => err("invalid_command", "unknown file command", true),
            }
        }
        "clipboard" => {
            // Explicit transfers only: nothing here runs unless a person tapped a button on the phone.
            if !ctx.scopes.contains("clipboard") {
                return err("forbidden", &format!("This phone may not use the clipboard yet. On the PC run: zenvyd grant {} clipboard", ctx.device_id), false);
            }
            let Some(tools) = &app.cfg.clipboard else {
                return err(
                    "capability_unavailable",
                    "clipboard transfer is not available on this PC (wl-clipboard is missing)",
                    false,
                );
            };
            let name = crate::listen::clean_name(
                &app.store
                    .lock()
                    .unwrap()
                    .get(&ctx.device_id)
                    .map(|d| d.name.clone())
                    .unwrap_or_default(),
            );
            match m.data.get("cmd").and_then(Value::as_str).unwrap_or("") {
                "get" => {
                    app.audit.record("clipboard_get", Some(&ctx.device_id), "");
                    crate::listen::notify_text(app, &format!("{name} read this PC's clipboard"))
                        .await;
                    match tools.get().await {
                        Ok(text) => (
                            Some(
                                json!({ "v": 1, "t": "clipboard", "id": id, "data": { "text": text } }),
                            ),
                            false,
                        ),
                        Err(e) => err("failed", &e, false),
                    }
                }
                "set" => {
                    let Some(text) = m.data.get("text").and_then(Value::as_str) else {
                        return err("invalid_command", "text is required", true);
                    };
                    app.audit.record("clipboard_set", Some(&ctx.device_id), "");
                    match tools.set(text).await {
                        Ok(()) => (Some(ack()), false),
                        Err(e) => err("failed", &e, false),
                    }
                }
                "open" => {
                    let Some(url) = m.data.get("url").and_then(Value::as_str) else {
                        return err("invalid_command", "url is required", true);
                    };
                    if let Err(e) = crate::clipboard::valid_url(url) {
                        return err("invalid_command", e, false);
                    }
                    app.audit.record("link_open", Some(&ctx.device_id), "");
                    crate::listen::notify_text(app, &format!("{name} opened a link on this PC"))
                        .await;
                    match tools.open(url).await {
                        Ok(()) => (Some(ack()), false),
                        Err(e) => err("failed", &e, false),
                    }
                }
                _ => err("invalid_command", "unknown clipboard command", true),
            }
        }
        "action" => {
            // Custom actions from the config. The phone sends an id; the command line never leaves the agent.
            if !ctx.scopes.contains("exec") {
                return err(
                    "forbidden",
                    &format!(
                        "This phone may not run actions yet. On the PC run: zenvyd grant {} exec",
                        ctx.device_id
                    ),
                    false,
                );
            }
            let cmd = m.data.get("cmd").and_then(Value::as_str).unwrap_or("");
            let find = |id: &str| app.cfg.actions.iter().find(|a| a.id == id).cloned();
            match cmd {
                "run" => {
                    let aid = m.data.get("id").and_then(Value::as_str).unwrap_or("");
                    let Some(a) = find(aid) else {
                        return err("unknown_action", "no such action", false);
                    };
                    if a.confirm {
                        let token = crate::power::hex_token();
                        ctx.action_pending = Some((
                            token.clone(),
                            a.id.clone(),
                            std::time::Instant::now() + crate::power::CONFIRM_WINDOW,
                        ));
                        return (
                            Some(
                                json!({ "v": 1, "t": "action", "id": id, "data": { "state": "pending", "token": token, "expires_ms": crate::power::CONFIRM_WINDOW.as_millis() as u64 } }),
                            ),
                            false,
                        );
                    }
                    app.audit.record("action_run", Some(&ctx.device_id), &a.id);
                    match crate::actions::run(&a).await {
                        Ok(()) => (Some(ack()), false),
                        Err(e) => err("failed", &e, false),
                    }
                }
                "confirm" => {
                    let token = m.data.get("token").and_then(Value::as_str).unwrap_or("");
                    let Some((want, aid, deadline)) = ctx.action_pending.take() else {
                        return err("not_pending", "nothing is waiting for confirmation", false);
                    };
                    if want != token || std::time::Instant::now() > deadline {
                        return err("not_pending", "that confirmation has expired", false);
                    }
                    let Some(a) = find(&aid) else {
                        return err("unknown_action", "no such action", false);
                    };
                    app.audit.record("action_run", Some(&ctx.device_id), &a.id);
                    match crate::actions::run(&a).await {
                        Ok(()) => (Some(ack()), false),
                        Err(e) => err("failed", &e, false),
                    }
                }
                _ => err("invalid_command", "unknown action command", true),
            }
        }
        "power" => {
            let cmd: PowerCommand = match serde_json::from_value(m.data) {
                Ok(c) => c,
                Err(_) => {
                    return err(
                        "invalid_command",
                        "unknown or malformed power command",
                        true,
                    )
                }
            };
            if let Err(e) = cmd.validate() {
                return err("invalid_command", e, true);
            }
            if !ctx.scopes.contains("power") {
                return err("forbidden", &format!("This phone may not control power yet. On the PC run: zenvyd grant {} power", ctx.device_id), false);
            }
            let Some(rt) = &app.power else {
                return err(
                    "capability_unavailable",
                    "power control is not available on this PC",
                    false,
                );
            };
            let name = crate::listen::clean_name(
                &app.store
                    .lock()
                    .unwrap()
                    .get(&ctx.device_id)
                    .map(|d| d.name.clone())
                    .unwrap_or_default(),
            );
            match cmd {
                PowerCommand::Request { action } => {
                    if !rt.can(action) {
                        return err(
                            "capability_unavailable",
                            &format!("this PC does not allow {} from here", action.name()),
                            false,
                        );
                    }
                    app.audit
                        .record("power_request", Some(&ctx.device_id), action.name());
                    if !action.needs_confirm() {
                        return match rt.adapter.execute(action).await {
                            Ok(()) => {
                                app.audit
                                    .record("power_exec", Some(&ctx.device_id), action.name());
                                (Some(ack()), false)
                            }
                            Err(e) => err("failed", &e, false),
                        };
                    }
                    let token = crate::power::hex_token();
                    ctx.power_pending = Some((
                        token.clone(),
                        action,
                        std::time::Instant::now() + crate::power::CONFIRM_WINDOW,
                    ));
                    (
                        Some(
                            json!({ "v": 1, "t": "power", "id": id, "data": { "state": "pending", "action": action.name(), "token": token, "expires_ms": crate::power::CONFIRM_WINDOW.as_millis() as u64, "delay_ms": rt.delay.as_millis() as u64 } }),
                        ),
                        false,
                    )
                }
                PowerCommand::Confirm { token } => {
                    let Some((want, action, deadline)) = ctx.power_pending.take() else {
                        return err("not_pending", "nothing is waiting for confirmation", false);
                    };
                    if want != token || std::time::Instant::now() > deadline {
                        return err("not_pending", "that confirmation has expired", false);
                    }
                    match rt.schedule(app.clone(), action) {
                        Ok(sid) => {
                            app.audit
                                .record("power_confirm", Some(&ctx.device_id), action.name());
                            let word = match action {
                                zenvy_proto::PowerAction::Suspend => "put to sleep",
                                zenvy_proto::PowerAction::Reboot => "restarted",
                                _ => "shut down",
                            };
                            crate::listen::notify_text(app, &format!("{name} will have this PC {word} in {} s. Run: zenvyd power-cancel", rt.delay.as_secs())).await;
                            let data = json!({ "id": sid, "action": action.name(), "delay_ms": rt.delay.as_millis() as u64, "by": name });
                            app.publish("power.scheduled", data.clone());
                            (
                                Some(
                                    json!({ "v": 1, "t": "power", "id": id, "data": { "state": "scheduled", "id": sid, "action": action.name(), "delay_ms": rt.delay.as_millis() as u64 } }),
                                ),
                                false,
                            )
                        }
                        Err(e) => err("busy", e, false),
                    }
                }
                PowerCommand::Cancel { token } => {
                    if rt.cancel(Some(&token)) {
                        app.audit.record("power_cancel", Some(&ctx.device_id), "");
                        app.publish("power.cancelled", json!({ "id": token }));
                        (Some(ack()), false)
                    } else {
                        err("not_pending", "nothing to cancel", false)
                    }
                }
            }
        }
        "unlock" => {
            // The password is never logged, audited or echoed: error texts below are fixed strings.
            let cmd: UnlockCommand = match serde_json::from_value(m.data) {
                Ok(c) => c,
                Err(_) => return err("invalid_command", "malformed unlock command", true),
            };
            if let Err(e) = cmd.validate() {
                return err("invalid_command", e, false);
            }
            let password = zeroize::Zeroizing::new(cmd.password);
            if !ctx.scopes.contains("unlock") {
                return err("forbidden", &format!("This phone may not unlock the PC yet. On the PC run: zenvyd grant {} unlock", ctx.device_id), false);
            }
            let (Some(rt), Some(input)) = (&app.power, &app.input) else {
                return err(
                    "capability_unavailable",
                    "unlocking from the phone is not available on this PC",
                    false,
                );
            };
            if !rt.unlock_enabled || !rt.layout_us {
                return err(
                    "capability_unavailable",
                    if rt.unlock_enabled {
                        "unlocking needs a US keyboard layout on the PC"
                    } else {
                        "unlocking from the phone is turned off in the config"
                    },
                    false,
                );
            }
            if let Err(secs) = rt.check_unlock(&ctx.device_id) {
                app.audit
                    .record("unlock_blocked", Some(&ctx.device_id), "too many attempts");
                return err(
                    "rate_limited",
                    &format!("Too many wrong passwords. Try again in {secs} s."),
                    false,
                );
            }
            if !rt.locked_steady().await {
                return err(
                    "not_locked",
                    "The PC is not locked, so nothing was typed.",
                    false,
                );
            }
            let name = crate::listen::clean_name(
                &app.store
                    .lock()
                    .unwrap()
                    .get(&ctx.device_id)
                    .map(|d| d.name.clone())
                    .unwrap_or_default(),
            );
            app.audit.record("unlock_attempt", Some(&ctx.device_id), "");
            crate::listen::notify_text(
                app,
                &format!("{name} is typing the password on the lock screen"),
            )
            .await;
            let typed = {
                let input = input.clone();
                let pw = password;
                tokio::task::spawn_blocking(move || {
                    input.type_secret(&pw, std::time::Duration::from_millis(15))
                })
                .await
            };
            if !matches!(typed, Ok(Ok(()))) {
                return err("failed", "The password could not be typed.", false);
            }
            // success = the lock goes away shortly after Enter
            let mut unlocked = false;
            for _ in 0..16 {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                if rt.adapter.lock_state().await == zenvy_proto::LockState::Unlocked {
                    unlocked = true;
                    break;
                }
            }
            if unlocked {
                rt.unlock_ok(&ctx.device_id);
                app.audit.record("unlock_ok", Some(&ctx.device_id), "");
                app.set_locked(false);
                crate::state::lock_changed(app).await;
                (Some(ack()), false)
            } else {
                rt.unlock_failed(&ctx.device_id);
                app.audit.record("unlock_failed", Some(&ctx.device_id), "");
                err(
                    "wrong_password",
                    "Still locked. Check the password and try again.",
                    false,
                )
            }
        }
        "apps" => {
            // The app launcher: the phone sees installed applications (names and ids) and asks to start one by id,
            // optionally on a new empty workspace. The command line comes from the PC's desktop entry, never the phone.
            if !ctx.scopes.contains("launch") {
                return err(
                    "forbidden",
                    &format!(
                        "This phone may not open apps yet. On the PC run: zenvyd grant {} launch",
                        ctx.device_id
                    ),
                    false,
                );
            }
            let Some(apps) = app.cfg.apps.clone() else {
                return err(
                    "capability_unavailable",
                    "app launching is not available on this PC",
                    false,
                );
            };
            match m.data.get("cmd").and_then(Value::as_str).unwrap_or("") {
                "list" => {
                    let a2 = apps.clone();
                    let list = tokio::task::spawn_blocking(move || a2.list())
                        .await
                        .unwrap_or_default();
                    let items: Vec<Value> = list
                        .iter()
                        .map(|a| json!({ "id": a.id, "name": a.name }))
                        .collect();
                    (
                        Some(json!({ "v": 1, "t": "apps", "id": id, "data": { "apps": items } })),
                        false,
                    )
                }
                "launch" => {
                    let aid = m.data.get("id").and_then(Value::as_str).unwrap_or("");
                    if !zenvy_apps::valid_id(aid) {
                        return err("invalid_command", "bad app id", true);
                    }
                    // where it opens: this workspace, the lowest empty one, or a numbered one
                    enum Target {
                        Here,
                        Empty,
                        Id(i64),
                    }
                    let target = match m.data.get("workspace") {
                        None => Target::Here,
                        Some(Value::String(w)) if w == "here" => Target::Here,
                        Some(Value::String(w)) if w == "empty" => Target::Empty,
                        Some(Value::Number(n)) => match n.as_i64() {
                            Some(i) if (1..=1000).contains(&i) => Target::Id(i),
                            _ => return err("invalid_command", "workspace out of range", true),
                        },
                        _ => return err("invalid_command", "bad workspace", true),
                    };
                    let a2 = apps.clone();
                    let aid2 = aid.to_string();
                    let found = tokio::task::spawn_blocking(move || a2.find(&aid2))
                        .await
                        .ok()
                        .flatten();
                    let Some(entry) = found else {
                        return err("unknown_app", "that app is not installed", false);
                    };
                    let ws_id = match target {
                        Target::Here => None,
                        Target::Id(i) => Some(i),
                        Target::Empty => match app.adapter.workspaces().await {
                            Ok(list) => Some(lowest_empty_workspace(&list)),
                            Err(e) => return err("failed", &e.to_string(), false),
                        },
                    };
                    if let Some(w) = ws_id {
                        if !ctx.scopes.contains("windows") {
                            return err(
                                "forbidden",
                                &format!("Switching workspaces needs the windows permission. On the PC run: zenvyd grant {} windows", ctx.device_id),
                                false,
                            );
                        }
                        if let Err(e) = app
                            .adapter
                            .execute(&DesktopCommand::SwitchWorkspace { id: w })
                            .await
                        {
                            return err("failed", &e.to_string(), false);
                        }
                    }
                    // the audit line names the program, never its arguments
                    app.audit.record(
                        "app_launch",
                        Some(&ctx.device_id),
                        &format!("{} ({})", entry.id, entry.program()),
                    );
                    match zenvy_apps::launch(&entry) {
                        Ok(()) => (
                            Some(json!({ "v": 1, "t": "apps", "id": id, "data": { "state": "launched", "name": entry.name, "workspace": ws_id } })),
                            false,
                        ),
                        Err(e) => err("failed", &e, false),
                    }
                }
                _ => err("invalid_command", "unknown apps command", true),
            }
        }
        "screen" => {
            use zenvy_proto::ScreenCommand;
            let cmd: ScreenCommand = match serde_json::from_value(m.data) {
                Ok(c) => c,
                Err(_) => {
                    return err(
                        "invalid_command",
                        "unknown or malformed screen command",
                        true,
                    )
                }
            };
            if let Err(e) = cmd.validate() {
                return err("invalid_command", e, true);
            }
            if !ctx.scopes.contains("screen") {
                return err("forbidden", &format!("This phone may not see the PC's screen yet. On the PC run: zenvyd grant {} screen", ctx.device_id), false);
            }
            let Some(streamer) = &app.cfg.screen else {
                return err(
                    "capability_unavailable",
                    "the screen preview is not available on this PC (grim is missing)",
                    false,
                );
            };
            let monitors = app.adapter.monitors().await.unwrap_or_default();
            let status = |ctx: &Ctx, state: &str| {
                let sess = ctx.screen.as_ref();
                let mon = sess.and_then(|s| monitors.iter().find(|m| m.connector == s.output));
                let (lw, lh) = mon
                    .map(|m| zenvy_screen::logical_size(&geom(m)))
                    .unwrap_or((0.0, 0.0));
                (
                    Some(
                        json!({ "v": 1, "t": "screen", "id": id, "data": { "state": state, "output": sess.map(|s| s.output.clone()), "preset": sess.map(|s| s.preset), "width": lw.round() as u32, "height": lh.round() as u32 } }),
                    ),
                    false,
                )
            };
            match cmd {
                ScreenCommand::Start { output, preset } => {
                    let chosen = match &output {
                        Some(o) => monitors.iter().find(|m| &m.connector == o),
                        None => monitors.iter().find(|m| m.focused).or(monitors.first()),
                    };
                    let Some(mon) = chosen else {
                        return err("not_found", "that monitor is not connected", false);
                    };
                    let preset = preset
                        .or(ctx.screen.as_ref().map(|s| s.preset))
                        .unwrap_or_default();
                    let opts = zenvy_screen::CaptureOpts {
                        output: mon.connector.clone(),
                        logical_width: zenvy_screen::logical_size(&geom(mon)).0.round() as u32,
                        preset,
                    };
                    if let Some(s) = ctx.screen.as_mut() {
                        streamer.set_opts(opts);
                        s.output = mon.connector.clone();
                        s.preset = preset;
                    } else {
                        ctx.screen = Some(ScreenSession {
                            viewer: streamer.view(opts),
                            output: mon.connector.clone(),
                            preset,
                        });
                        app.audit.record("screen_start", Some(&ctx.device_id), "");
                        crate::listen::notify_text(
                            app,
                            &format!(
                                "{} is viewing this PC's screen",
                                device_name(app, &ctx.device_id)
                            ),
                        )
                        .await;
                    }
                    status(ctx, "started")
                }
                ScreenCommand::Stop => {
                    stop_screen(app, ctx).await;
                    status(ctx, "stopped")
                }
                ScreenCommand::Preset { preset } => {
                    let Some(s) = ctx.screen.as_mut() else {
                        return err("not_found", "the screen is not being viewed", false);
                    };
                    let Some(mon) = monitors.iter().find(|m| m.connector == s.output) else {
                        return err("not_found", "that monitor is not connected", false);
                    };
                    s.preset = preset;
                    streamer.set_opts(zenvy_screen::CaptureOpts {
                        output: s.output.clone(),
                        logical_width: zenvy_screen::logical_size(&geom(mon)).0.round() as u32,
                        preset,
                    });
                    status(ctx, "started")
                }
                ScreenCommand::Click {
                    x,
                    y,
                    button,
                    count,
                } => {
                    if !ctx.scopes.contains("input") {
                        return err("forbidden", "this device lacks the 'input' scope", false);
                    }
                    let Some(input) = &app.input else {
                        return err(
                            "capability_unavailable",
                            "input injection is not available on this PC",
                            false,
                        );
                    };
                    let Some(s) = ctx.screen.as_ref() else {
                        return err("not_found", "the screen is not being viewed", false);
                    };
                    let Some(mon) = monitors.iter().find(|m| m.connector == s.output) else {
                        return err("not_found", "that monitor is not connected", false);
                    };
                    let Some((px, py)) = zenvy_screen::map_point(&geom(mon), x, y) else {
                        return err(
                            "invalid_command",
                            "the point must be inside the screen",
                            true,
                        );
                    };
                    if let Err(e) = app.adapter.cursor_move(px, py).await {
                        return err("failed", &e.to_string(), false);
                    }
                    let b = match button {
                        zenvy_proto::ClickKind::Left => Button::Left,
                        zenvy_proto::ClickKind::Right => Button::Right,
                    };
                    for i in 0..count.unwrap_or(1) {
                        if i > 0 {
                            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
                        }
                        if let Err(e) = input
                            .execute(&InputCommand::Button {
                                button: b,
                                state: ButtonState::Click,
                            })
                            .await
                        {
                            return err("failed", &e.to_string(), false);
                        }
                    }
                    (Some(ack()), false)
                }
            }
        }
        "listen" => {
            let cmd: ListenCommand = match serde_json::from_value(m.data) {
                Ok(c) => c,
                Err(_) => {
                    return err(
                        "invalid_command",
                        "unknown or malformed listen command",
                        true,
                    )
                }
            };
            if !ctx.scopes.contains("listen") {
                // The one place a phone is told what to do: the device id is its own, and granting needs the PC's owner.
                return err("forbidden", &format!("This phone may not listen to the PC yet. On the PC run: zenvyd grant {} listen", ctx.device_id), false);
            }
            let Some(stream) = &app.stream else {
                return err(
                    "capability_unavailable",
                    "listening is not available on this PC (parec is missing)",
                    false,
                );
            };
            let reply = |state: &str| {
                (
                    Some(
                        json!({ "v": 1, "t": "listen", "id": id, "data": { "state": state, "rate": zenvy_proto::AUDIO_RATE, "channels": zenvy_proto::AUDIO_CHANNELS, "frame_ms": zenvy_proto::AUDIO_FRAME_MS } }),
                    ),
                    false,
                )
            };
            match cmd {
                ListenCommand::Start { mute_pc } => {
                    if ctx.listener.is_some() {
                        return reply("started");
                    }
                    ctx.listener = Some(stream.listen());
                    if mute_pc {
                        app.listen.acquire().await;
                        ctx.muting = true;
                    }
                    let name = app
                        .store
                        .lock()
                        .unwrap()
                        .get(&ctx.device_id)
                        .map(|d| d.name.clone())
                        .unwrap_or_default();
                    app.audit.record(
                        "listen_start",
                        Some(&ctx.device_id),
                        if mute_pc { "pc muted" } else { "pc not muted" },
                    );
                    crate::listen::notify(app, &name, true).await;
                    reply("started")
                }
                ListenCommand::Stop => {
                    if ctx.listener.is_some() {
                        stop_listening(app, ctx).await;
                    }
                    reply("stopped")
                }
            }
        }
        "audio" => {
            let cmd: AudioCommand = match serde_json::from_value(m.data) {
                Ok(c) => c,
                Err(_) => {
                    return err(
                        "invalid_command",
                        "unknown or malformed audio command",
                        true,
                    )
                }
            };
            if let Err(e) = cmd.validate() {
                return err("invalid_command", e, true);
            }
            if !ctx.scopes.contains("system") {
                return err("forbidden", "this device lacks the 'system' scope", false);
            }
            let Some(audio) = &app.audio else {
                return err(
                    "capability_unavailable",
                    "audio control is not available on this PC",
                    false,
                );
            };
            match audio.execute(&cmd).await {
                Ok(()) => (id.as_ref().map(|_| ack()), false),
                Err(e) => err("failed", &e.to_string(), false),
            }
        }
        "brightness" => {
            let cmd: BrightnessCommand = match serde_json::from_value(m.data) {
                Ok(c) => c,
                Err(_) => {
                    return err(
                        "invalid_command",
                        "unknown or malformed brightness command",
                        true,
                    )
                }
            };
            if let Err(e) = cmd.validate() {
                return err("invalid_command", e, true);
            }
            if !ctx.scopes.contains("system") {
                return err("forbidden", "this device lacks the 'system' scope", false);
            }
            let Some(b) = &app.brightness else {
                return err(
                    "capability_unavailable",
                    "brightness control is not available on this PC",
                    false,
                );
            };
            match cmd {
                BrightnessCommand::Set {
                    id: display,
                    percent,
                } => match b.set(&display, percent) {
                    Ok(()) => (id.as_ref().map(|_| ack()), false),
                    Err(e) => err("failed", &e.to_string(), false),
                },
                BrightnessCommand::Refresh => {
                    b.refresh().await;
                    app.publish("brightness.state", crate::state::brightness_state(app));
                    (id.as_ref().map(|_| ack()), false)
                }
            }
        }
        "media" => {
            let cmd: MediaCommand = match serde_json::from_value(m.data) {
                Ok(c) => c,
                Err(_) => {
                    return err(
                        "invalid_command",
                        "unknown or malformed media command",
                        true,
                    )
                }
            };
            if let Err(e) = cmd.validate() {
                return err("invalid_command", e, true);
            }
            if !ctx.scopes.contains("system") {
                return err("forbidden", "this device lacks the 'system' scope", false);
            }
            let Some(media) = &app.media else {
                return err(
                    "capability_unavailable",
                    "media control is not available on this PC",
                    false,
                );
            };
            match media.execute(&cmd).await {
                Ok(()) => (id.as_ref().map(|_| ack()), false),
                Err(e) => err("failed", &e.to_string(), false),
            }
        }
        "browser" => {
            let cmd: BrowserCommand = match serde_json::from_value(m.data) {
                Ok(c) => c,
                Err(_) => {
                    return err(
                        "invalid_command",
                        "unknown or malformed browser command",
                        true,
                    )
                }
            };
            if let Err(e) = cmd.validate() {
                return err("invalid_command", e, true);
            }
            if !ctx.scopes.contains("windows") {
                return err("forbidden", "this device lacks the 'windows' scope", false);
            }
            if matches!(cmd, BrowserCommand::CloseTab { .. }) {
                app.audit.record("cmd", Some(&ctx.device_id), "close_tab");
            }
            match app.browser.command(cmd.browser(), &cmd) {
                Ok(()) => (Some(ack()), false),
                Err(e) => err("browser_unavailable", e, false),
            }
        }
        _ => err("unknown_type", "unsupported message type", true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn h(origin: Option<&str>, host: &str) -> HeaderMap {
        let mut m = HeaderMap::new();
        m.insert(header::HOST, HeaderValue::from_str(host).unwrap());
        if let Some(o) = origin {
            m.insert(header::ORIGIN, HeaderValue::from_str(o).unwrap());
        }
        m
    }

    #[test]
    fn origin_must_match_host() {
        assert!(origin_ok(&h(None, "192.168.0.2:7878")));
        assert!(origin_ok(&h(
            Some("https://192.168.0.2:7878"),
            "192.168.0.2:7878"
        )));
        assert!(!origin_ok(&h(
            Some("https://evil.example"),
            "192.168.0.2:7878"
        )));
        assert!(
            !origin_ok(&h(Some("http://192.168.0.2:7878"), "192.168.0.2:7878")),
            "plain-http origin is rejected"
        );
        assert!(!origin_ok(&h(Some("null"), "192.168.0.2:7878")));
    }
}

/// The lowest numbered workspace (1 and up) that has no windows: where "open on a new workspace" goes.
fn lowest_empty_workspace(list: &[zenvy_proto::Workspace]) -> i64 {
    let busy: std::collections::HashSet<i64> =
        list.iter().filter(|w| w.windows > 0).map(|w| w.id).collect();
    (1..=1000).find(|i| !busy.contains(i)).unwrap_or(1000)
}

#[cfg(test)]
mod apps_tests {
    use super::*;

    fn w(id: i64, windows: u32) -> zenvy_proto::Workspace {
        zenvy_proto::Workspace {
            id,
            name: id.to_string(),
            monitor: "M".into(),
            windows,
        }
    }

    #[test]
    fn the_new_workspace_is_the_lowest_one_without_windows() {
        assert_eq!(lowest_empty_workspace(&[]), 1);
        assert_eq!(lowest_empty_workspace(&[w(1, 2), w(2, 1), w(3, 4)]), 4);
        assert_eq!(
            lowest_empty_workspace(&[w(1, 2), w(2, 0), w(3, 4)]),
            2,
            "an existing empty workspace is reused"
        );
        assert_eq!(lowest_empty_workspace(&[w(1, 1), w(3, 1)]), 2, "gaps are filled");
        assert_eq!(
            lowest_empty_workspace(&[w(-98, 3), w(1, 0)]),
            1,
            "scratchpads never count"
        );
    }
}
