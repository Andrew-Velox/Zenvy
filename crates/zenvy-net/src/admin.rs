//! Local admin socket (`$XDG_RUNTIME_DIR/zenvy/admin.sock`, dir 0700, socket 0600): the only way to
//! start pairing, approve a phone, list or revoke devices. Never reachable from the network.
//! Protocol: one JSON object per line in each direction.

use crate::app::{App, ApprovalRequest, Hub};
use serde_json::{json, Value};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

pub async fn serve_admin(app: Arc<App>, sock: &Path, public_base: String) -> std::io::Result<()> {
    if let Some(dir) = sock.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    let _ = std::fs::remove_file(sock);
    let listener = UnixListener::bind(sock)?;
    std::fs::set_permissions(sock, std::fs::Permissions::from_mode(0o600))?;
    loop {
        let (stream, _) = listener.accept().await?;
        let (app, base) = (app.clone(), public_base.clone());
        tokio::spawn(async move {
            if let Err(e) = handle(app, stream, base).await {
                tracing::debug!("admin connection: {e}");
            }
        });
    }
}

async fn put(w: &mut tokio::net::unix::OwnedWriteHalf, v: Value) -> std::io::Result<()> {
    w.write_all(format!("{v}\n").as_bytes()).await
}

async fn handle(app: Arc<App>, stream: UnixStream, base: String) -> std::io::Result<()> {
    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();
    let Some(line) = lines.next_line().await? else {
        return Ok(());
    };
    let req: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
    match req["op"].as_str() {
        Some("devices") => {
            let list: Vec<Value> = app.store.lock().unwrap().list().iter().map(|d| json!({ "id": d.id, "name": d.name, "scopes": d.scopes, "created_at": d.created_at, "last_seen": d.last_seen, "online": app.online.is_online(&d.id) })).collect();
            put(&mut w, json!({ "type": "devices", "devices": list })).await
        }
        Some("status") => {
            let c = &app.cfg;
            put(&mut w, json!({
                "type": "status",
                "version": env!("CARGO_PKG_VERSION"),
                "address": base,
                "fingerprint": app.tls_fingerprint,
                "locked": app.is_locked(),
                "phones_online": app.online.count(),
                "features": {
                    "input": app.input.is_some(), "audio": app.audio.is_some(), "media": app.media.is_some(),
                    "brightness": app.brightness.is_some(), "listen": app.stream.is_some(), "power": app.power.is_some(),
                    "screen": c.screen.is_some(), "apps": c.apps.is_some(), "files": c.files.is_some(), "clipboard": c.clipboard.is_some(),
                },
            })).await
        }
        Some("revoke") => {
            let id = req["id"].as_str().unwrap_or("");
            let removed = app.store.lock().unwrap().revoke(id).ok().flatten();
            if let Some(d) = &removed {
                app.audit
                    .record("revoke", Some(&d.id), &format!("name={}", d.name));
                let _ = app.hub.send(Hub::Revoked(d.id.clone()));
            }
            put(&mut w, json!({ "type": "revoked", "ok": removed.is_some(), "name": removed.map(|d| d.name) })).await
        }
        Some(op @ ("grant" | "ungrant")) => {
            let (id, scope) = (
                req["id"].as_str().unwrap_or(""),
                req["scope"].as_str().unwrap_or(""),
            );
            if !zenvy_proto::valid_scope(scope) {
                return put(&mut w, json!({ "type": "scoped", "ok": false, "message": format!("unknown scope '{scope}' (known: {})", zenvy_proto::KNOWN_SCOPES.join(", ")) })).await;
            }
            let updated = app
                .store
                .lock()
                .unwrap()
                .set_scope(id, scope, op == "grant")
                .ok()
                .flatten();
            if let Some(d) = &updated {
                app.audit.record(op, Some(&d.id), scope);
                let _ = app.hub.send(Hub::Scopes(d.id.clone()));
            }
            put(&mut w, json!({ "type": "scoped", "ok": updated.is_some(), "name": updated.as_ref().map(|d| d.name.clone()), "scopes": updated.map(|d| d.scopes) })).await
        }
        Some("send") => {
            let path = req["path"].as_str().unwrap_or("");
            let Some(files) = &app.cfg.files else {
                return put(&mut w, json!({ "type": "scoped", "ok": false, "message": "file transfer is turned off" })).await;
            };
            match files.add_outgoing(std::path::Path::new(path)) {
                Ok((id, name, size)) => {
                    app.audit
                        .record("file_offered", None, &crate::files::human_size(size));
                    app.publish(
                        "file.offer",
                        json!({ "id": id, "name": name, "size": size }),
                    );
                    put(&mut w, json!({ "type": "scoped", "ok": true, "message": format!("offered {} ({}) to connected phones for 10 minutes", name, crate::files::human_size(size)) })).await
                }
                Err(e) => {
                    put(
                        &mut w,
                        json!({ "type": "scoped", "ok": false, "message": e }),
                    )
                    .await
                }
            }
        }
        Some("power_cancel") => {
            let done = app.power.as_ref().is_some_and(|p| p.cancel(None));
            if done {
                app.audit.record("power_cancel", None, "from the PC");
                app.publish("power.cancelled", serde_json::json!({ "id": null }));
            }
            put(&mut w, json!({ "type": "scoped", "ok": done, "message": if done { "cancelled" } else { "nothing was counting down" } })).await
        }
        Some("revoke_all") => {
            let all = app.store.lock().unwrap().revoke_all().unwrap_or_default();
            for d in &all {
                app.audit.record("revoke", Some(&d.id), "revoke_all");
                let _ = app.hub.send(Hub::Revoked(d.id.clone()));
            }
            put(
                &mut w,
                json!({ "type": "revoked", "ok": true, "count": all.len() }),
            )
            .await
        }
        Some("pair") => pair(app, &mut lines, &mut w, base).await,
        _ => put(&mut w, json!({ "type": "error", "message": "unknown op" })).await,
    }
}

/// Starts a pairing session and relays the phone's approval request to the operator's terminal.
async fn pair(
    app: Arc<App>,
    lines: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    w: &mut tokio::net::unix::OwnedWriteHalf,
    base: String,
) -> std::io::Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ApprovalRequest>(1);
    let busy = {
        let mut slot = app.approvals.lock().unwrap();
        if slot.as_ref().is_some_and(|t| !t.is_closed()) {
            true
        } else {
            *slot = Some(tx);
            false
        }
    };
    if busy {
        return put(
            w,
            json!({ "type": "error", "message": "another pairing is already in progress" }),
        )
        .await;
    }
    let ttl = app.cfg.pairing_ttl;
    let secret = app.pairing.lock().unwrap().start(ttl, Instant::now());
    app.audit.record("pair_start", None, "");
    let url = format!(
        "{base}/#s={secret}&fp={}",
        app.tls_fingerprint.replace(':', "")
    );
    put(w, json!({ "type": "pairing", "url": url, "ttl": ttl.as_secs(), "fingerprint": app.tls_fingerprint })).await?;

    let outcome = tokio::select! {
        req = rx.recv() => match req {
            Some(req) => {
                put(w, json!({ "type": "approval", "name": req.name, "code": req.code })).await?;
                let answer = lines.next_line().await?.and_then(|l| serde_json::from_str::<Value>(&l).ok()).and_then(|v| v["approve"].as_bool()).unwrap_or(false);
                let _ = req.reply.send(answer);
                if answer { "approved" } else { "denied" }
            }
            None => "cancelled",
        },
        _ = tokio::time::sleep(ttl) => "expired",
        eof = lines.next_line() => { let _ = eof; "cancelled" }
    };
    app.pairing.lock().unwrap().cancel();
    *app.approvals.lock().unwrap() = None;
    put(w, json!({ "type": "done", "result": outcome })).await
}
