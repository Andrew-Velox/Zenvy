//! Desktop state snapshots and the event-driven publisher (no polling: it only wakes when
//! Hyprland sends an event).

use crate::app::App;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use zenvy_core::{ui_manifest, Compositor};
use zenvy_hyprland::{subscribe, StreamItem};

pub async fn audio_state(app: &App) -> Value {
    match &app.audio {
        Some(a) => a
            .state()
            .await
            .ok()
            .and_then(|s| serde_json::to_value(s).ok())
            .unwrap_or(Value::Null),
        None => Value::Null,
    }
}

pub async fn power_state(app: &App) -> Value {
    match &app.power {
        Some(p) => serde_json::to_value(p.state(app.input.is_some()).await).unwrap_or(Value::Null),
        None => Value::Null,
    }
}

pub fn brightness_state(app: &App) -> Value {
    match &app.brightness {
        Some(b) => serde_json::to_value(b.state()).unwrap_or(Value::Null),
        None => Value::Null,
    }
}

pub async fn media_state(app: &App) -> Value {
    match &app.media {
        Some(m) => m
            .state()
            .await
            .ok()
            .and_then(|s| serde_json::to_value(s).ok())
            .unwrap_or(Value::Null),
        None => Value::Null,
    }
}

/// What a phone sees of the desktop while the PC is locked: nothing.
pub fn redacted_desktop() -> Value {
    json!({ "monitors": [], "workspaces": [], "windows": [], "active_window": null })
}

/// The lock appeared or went away: tell phones, and swap the redacted desktop for the real one (or back).
pub async fn lock_changed(app: &App) {
    app.publish("power.state", power_state(app).await);
    app.publish("desktop.state", desktop_state(app).await);
    app.publish("browser.tabs", app.browser.view());
}

pub async fn desktop_state(app: &App) -> Value {
    if app.is_locked() {
        return redacted_desktop();
    }
    let monitors = app.adapter.monitors().await.unwrap_or_default();
    let workspaces = app.adapter.workspaces().await.unwrap_or_default();
    let windows = app.adapter.windows().await.unwrap_or_default();
    let active = app.adapter.active_window().await.ok().flatten();
    json!({ "monitors": monitors, "workspaces": workspaces, "windows": windows, "active_window": active })
}

/// Everything a freshly authenticated phone needs.
pub async fn snapshot(app: &App) -> Value {
    let caps = app.registry.snapshot();
    json!({
        "capabilities": caps,
        "ui_manifest": ui_manifest(&caps, &app.cfg.features),
        "desktop": desktop_state(app).await,
        "browser": if app.is_locked() { json!({ "browsers": [] }) } else { app.browser.view() },
        "audio": audio_state(app).await,
        "media": media_state(app).await,
        "brightness": brightness_state(app),
        "power": power_state(app).await,
        "actions": crate::actions::public_view(&app.cfg.actions),
        "files": match &app.cfg.files {
            Some(f) => json!({ "enabled": true, "max_bytes": f.max_bytes, "offers": f.outgoing_list().into_iter().map(|(id, name, size)| json!({ "id": id, "name": name, "size": size })).collect::<Vec<_>>() }),
            None => json!({ "enabled": false }),
        },
        "share": { "clipboard": app.cfg.clipboard.is_some(), "open_link": app.cfg.clipboard.as_ref().is_some_and(|c| c.open.is_some()) },
    })
}

/// Forwards Hyprland events (debounced 40 ms, only when the resulting state really changed) and
/// capability changes to all connected phones.
pub fn spawn_publisher(app: Arc<App>) {
    let a = app.clone();
    tokio::spawn(async move {
        let mut rx = a.registry.subscribe();
        while let Ok(change) = rx.recv().await {
            a.publish(
                "capability.changed",
                json!({ "id": change.id, "record": change.record }),
            );
        }
    });

    // Audio and media: pushed only when the system reports a change (pactl subscribe / D-Bus signals).
    if let Some(audio) = app.audio.clone() {
        let a = app.clone();
        let mut rx = audio.watch();
        tokio::spawn(async move {
            let mut last = String::new();
            while rx.recv().await.is_some() {
                let v = audio_state(&a).await;
                let text = v.to_string();
                if text != last {
                    last = text;
                    a.publish("audio.state", v);
                }
            }
        });
    }
    if let Some(brightness) = app.brightness.clone() {
        let a = app.clone();
        let mut rx = brightness.watch();
        tokio::spawn(async move {
            while rx.recv().await.is_some() {
                a.publish("brightness.state", brightness_state(&a));
            }
        });
    }
    // Lock state has no event source on Wayland, so it is sampled, but only while a phone is connected.
    if app.power.is_some() {
        let a = app.clone();
        tokio::spawn(async move {
            let mut last = String::new();
            loop {
                let watching = a.hub.receiver_count() > 0;
                let mut wait = Duration::from_secs(5);
                if watching {
                    let v = power_state(&a).await;
                    let locked = v["lock"] == "locked";
                    let text = v.to_string();
                    if a.set_locked(locked) {
                        last = text;
                        lock_changed(&a).await;
                    } else if text != last {
                        last = text;
                        a.publish("power.state", v);
                    }
                    wait = Duration::from_millis(1000);
                }
                tokio::time::sleep(wait).await;
            }
        });
    }
    if let Some(media) = app.media.clone() {
        let a = app.clone();
        tokio::spawn(async move {
            let Ok(mut rx) = media.watch().await else {
                return;
            };
            let mut last = String::new();
            while rx.recv().await.is_some() {
                let v = media_state(&a).await;
                let text = v.to_string();
                if text != last {
                    last = text;
                    a.publish("media.state", v);
                }
            }
        });
    }

    tokio::spawn(async move {
        let mut rx = subscribe();
        let mut last = String::new();
        while let Some(item) = rx.recv().await {
            match &item {
                StreamItem::Connected => {
                    app.publish("compositor.status", json!({ "connected": true }))
                }
                StreamItem::Disconnected => {
                    app.publish("compositor.status", json!({ "connected": false }))
                }
                StreamItem::Event(_) => {}
            }
            // Debounce: absorb the burst of events that usually accompanies one change.
            let quiet = tokio::time::sleep(Duration::from_millis(40));
            tokio::pin!(quiet);
            loop {
                tokio::select! {
                    more = rx.recv() => if more.is_none() { return },
                    _ = &mut quiet => break,
                }
            }
            let state = desktop_state(&app).await;
            let text = state.to_string();
            if text != last {
                last = text;
                app.publish("desktop.state", state);
            }
        }
    });
}
