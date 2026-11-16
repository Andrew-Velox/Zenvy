use crate::browser::BrowserHub;
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, oneshot};
use zenvy_audio::AudioAdapter;
use zenvy_auth::{AuditLog, DeviceStore, Limiter, PairingManager};
use zenvy_brightness::BrightnessAdapter;
use zenvy_core::Registry;
use zenvy_hyprland::HyprlandAdapter;
use zenvy_input::{InputAdapter, UinputSink};
use zenvy_media::MediaAdapter;
use zenvy_stream::AudioStreamer;

#[derive(Clone)]
pub enum Hub {
    /// A ready-to-send `evt` envelope.
    Evt(Arc<str>),
    /// A device was revoked: its live sockets must close now.
    Revoked(String),
    /// A device's scopes changed (granted/removed on the PC): its live session reloads them.
    Scopes(String),
}

pub struct ApprovalRequest {
    pub name: String,
    pub code: String,
    pub reply: oneshot::Sender<bool>,
}

#[derive(Clone)]
pub struct NetConfig {
    pub session_ttl: Duration,
    pub pairing_ttl: Duration,
    pub max_devices: usize,
    /// `host:port` values the Host header may carry (DNS-rebinding defence).
    pub allowed_hosts: Vec<String>,
    pub features: BTreeMap<String, bool>,
    /// Custom actions from the config (the argument vectors stay here).
    pub actions: Vec<crate::actions::ActionDef>,
    /// `None` when `wl-copy`/`wl-paste` are missing: no clipboard transfer.
    pub clipboard: Option<crate::clipboard::ClipboardTools>,
    /// `None` when file transfer is off.
    pub files: Option<Arc<crate::files::Transfers>>,
}

pub struct App {
    pub adapter: HyprlandAdapter,
    /// `None` when no input backend is available; phones then get `capability_unavailable`.
    pub input: Option<Arc<InputAdapter<UinputSink>>>,
    /// `None` when `pactl` or the audio server is missing.
    pub audio: Option<Arc<AudioAdapter>>,
    /// `None` when no display can be controlled (no DDC/CI answer and no backlight).
    pub brightness: Option<Arc<BrightnessAdapter>>,
    /// `None` when `parec` is missing: the phone cannot be a speaker.
    pub stream: Option<Arc<AudioStreamer>>,
    pub listen: crate::listen::ListenMute,
    /// `notify-send`, if installed: tells the person at the PC when a phone starts or stops listening.
    pub notify: Option<std::path::PathBuf>,
    /// `None` when `systemctl`/`loginctl` are missing: no power sheet, no lock banner.
    pub power: Option<crate::power::PowerRuntime>,
    /// Application icons for the Desk screen.
    pub icons: Arc<zenvy_icons::IconResolver>,
    /// `None` when there is no D-Bus session bus.
    pub media: Option<Arc<MediaAdapter>>,
    pub registry: Registry,
    pub browser: BrowserHub,
    pub cfg: NetConfig,
    pub store: Mutex<DeviceStore>,
    pub pairing: Mutex<PairingManager>,
    pub limiter: Mutex<Limiter<IpAddr>>,
    pub audit: AuditLog,
    pub hub: broadcast::Sender<Hub>,
    pub approvals: Mutex<Option<tokio::sync::mpsc::Sender<ApprovalRequest>>>,
    seq: AtomicU64,
    /// The PC's screen is locked (cached; refreshed while phones are connected). While true, phones can only use
    /// media, volume, brightness, power and unlock: no windows, input or browser tabs, and no window titles are sent.
    locked: AtomicBool,
    pub tls_fingerprint: String,
}

impl App {
    // One constructor wiring every backend; a builder would add ceremony without adding safety.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        adapter: HyprlandAdapter,
        input: Option<Arc<InputAdapter<UinputSink>>>,
        audio: Option<Arc<AudioAdapter>>,
        brightness: Option<Arc<BrightnessAdapter>>,
        stream: Option<Arc<AudioStreamer>>,
        notify: Option<std::path::PathBuf>,
        power: Option<crate::power::PowerRuntime>,
        state_dir: std::path::PathBuf,
        media: Option<Arc<MediaAdapter>>,
        registry: Registry,
        cfg: NetConfig,
        store: DeviceStore,
        audit: AuditLog,
        tls_fingerprint: String,
    ) -> Arc<Self> {
        let audio_for_mute = audio.clone();
        Arc::new(Self {
            adapter,
            input,
            audio,
            brightness,
            stream,
            notify,
            power,
            icons: Arc::new(zenvy_icons::IconResolver::from_env()),
            listen: crate::listen::ListenMute::new(audio_for_mute, state_dir.join("listen-muted")),
            media,
            registry,
            browser: BrowserHub::default(),
            cfg,
            store: Mutex::new(store),
            pairing: Mutex::new(PairingManager::new()),
            limiter: Mutex::new(Limiter::new()),
            audit,
            hub: broadcast::channel(256).0,
            approvals: Mutex::new(None),
            seq: AtomicU64::new(1),
            locked: AtomicBool::new(false),
            tls_fingerprint,
        })
    }

    /// Builds an `evt` envelope with the next sequence number.
    pub fn evt(&self, topic: &str, value: serde_json::Value) -> Arc<str> {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        serde_json::json!({ "v": 1, "t": "evt", "seq": seq, "data": { "topic": topic, "value": value } }).to_string().into()
    }

    pub fn is_locked(&self) -> bool {
        self.locked.load(Ordering::Relaxed)
    }

    /// Returns whether the value changed.
    pub fn set_locked(&self, v: bool) -> bool {
        self.locked.swap(v, Ordering::Relaxed) != v
    }

    /// Reads the lock state now (called when a phone connects, so the cache is never stale for a new session).
    pub async fn refresh_lock(&self) {
        if let Some(p) = &self.power {
            let locked = p.adapter.lock_state().await == zenvy_proto::LockState::Locked;
            self.set_locked(locked);
        }
    }

    /// Everything goes through here, so a locked PC never leaks window titles or tabs to a phone.
    pub fn publish(&self, topic: &str, value: serde_json::Value) {
        let value = if self.is_locked() {
            match topic {
                "desktop.state" => crate::state::redacted_desktop(),
                "browser.tabs" => serde_json::json!({ "browsers": [] }),
                "browser.favicons" => serde_json::json!({ "hosts": [] }),
                "file.offer" => serde_json::Value::Null,
                _ => value,
            }
        } else {
            value
        };
        let _ = self.hub.send(Hub::Evt(self.evt(topic, value)));
    }
}
