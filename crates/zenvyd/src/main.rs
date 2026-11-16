use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use zenvy_core::{ui_manifest, Compositor, Registry};
use zenvy_hyprland::{probe_all, subscribe, HyprClient, HyprlandAdapter, StreamItem};
use zenvy_proto::{Address, DesktopCommand};

#[derive(Parser)]
#[command(
    name = "zenvyd",
    version,
    about = "Zenvy agent (Phase 1: Hyprland foundation)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Probe the system and print the capability table (add --json for the registry snapshot).
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Offer a file to the connected phones (they choose whether to download it).
    Send { path: std::path::PathBuf },
    /// Stop a suspend, restart or power-off that a phone has confirmed and that is still counting down.
    PowerCancel,
    /// Calibration helper: prints, once a second, every signal used to tell whether the PC is locked
    /// (lock the screen, unlock it, press Ctrl-C, then read the output). Changes nothing.
    LockProbe,
    /// Run the agent: HTTPS + WebSocket server for paired phones (foreground; use the systemd unit for background).
    Serve,
    /// Start pairing: shows a QR code and asks you to approve the phone that scans it.
    Pair,
    /// List paired devices.
    Devices,
    /// Give a paired device a permission (e.g. `zenvyd grant <id> listen` lets that phone hear the PC's audio).
    Grant { id: String, scope: String },
    /// Take a permission away from a paired device.
    Ungrant { id: String, scope: String },
    /// Revoke a paired device (id or unique id prefix), or --all.
    Revoke {
        id: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Collect the local device profile, save it (mode 0600) and print it as JSON.
    Profile,
    /// Validate the config file and print the effective configuration.
    Config,
    /// Print monitors, workspaces and windows once.
    State,
    /// Stream Hyprland events (coalesced) until Ctrl-C; reconnects automatically.
    Watch,
    /// Browser tab bridge (optional extension): print the token the extension needs.
    Browser {
        #[command(subcommand)]
        cmd: BrowserCmd,
    },
    /// Developer commands that execute typed desktop commands. Not part of the product surface.
    Dev {
        #[command(subcommand)]
        cmd: DevCmd,
    },
}

#[derive(Subcommand)]
enum BrowserCmd {
    /// Show the one-time token and the steps to install the extension.
    Setup,
}

#[derive(Subcommand)]
enum DevCmd {
    SwitchWorkspace { id: i64 },
    Focus { address: String },
    Close { address: String },
    Float { address: String },
    Move { address: String, workspace: i64 },
}

/// For one-shot, print-and-exit commands only: restore the default SIGPIPE so `zenvyd doctor | head`
/// ends quietly instead of panicking. NEVER call this for `serve`: Rust ignores SIGPIPE on purpose so a
/// client that disconnects mid-write yields an error instead of killing the whole agent.
fn quiet_when_pipe_closes() {
    // SAFETY: only resets a signal disposition, once, before any output is written.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    match Cli::parse().cmd {
        Cmd::Doctor { json } => {
            quiet_when_pipe_closes();
            doctor(json).await
        }
        Cmd::Serve => serve_cmd().await?,
        Cmd::PowerCancel => power_cancel_cmd().await?,
        Cmd::Send { path } => send_cmd(path).await?,
        Cmd::LockProbe => lock_probe_cmd().await?,
        Cmd::Pair => pair_cmd().await?,
        Cmd::Grant { id, scope } => scope_cmd("grant", id, scope).await?,
        Cmd::Ungrant { id, scope } => scope_cmd("ungrant", id, scope).await?,
        Cmd::Devices => {
            quiet_when_pipe_closes();
            devices_cmd().await?
        }
        Cmd::Revoke { id, all } => revoke_cmd(id, all).await?,
        Cmd::Browser {
            cmd: BrowserCmd::Setup,
        } => browser_setup()?,
        Cmd::Profile => {
            quiet_when_pipe_closes();
            profile_cmd().await?
        }
        Cmd::Config => config_cmd()?,
        Cmd::State => state().await?,
        Cmd::Watch => watch().await,
        Cmd::Dev { cmd } => dev(cmd).await?,
    }
    Ok(())
}

struct Scan {
    profile: zenvy_proto::DeviceProfile,
    registry: Registry,
    manifest: Vec<zenvy_core::Module>,
    warnings: Vec<String>,
}

fn load_config() -> Result<zenvy_config::Loaded, Box<dyn std::error::Error>> {
    Ok(match zenvy_config::config_path() {
        Some(p) => zenvy_config::load(&p)?,
        None => zenvy_config::parse("")?,
    })
}

/// Profiles the machine, probes every capability, resolves backends, attaches remediation.
async fn scan() -> Result<Scan, Box<dyn std::error::Error>> {
    let loaded = load_config()?;
    let root = Path::new("/");
    let globals = zenvy_profiler::wayland_globals();
    let displays = match adapter().await {
        Ok(a) => a.monitors().await.unwrap_or_default(),
        Err(_) => vec![],
    };
    let profile = zenvy_profiler::collect(
        root,
        &zenvy_profiler::Env::from_process(),
        globals.as_deref(),
        displays,
    );
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);

    let mut probes = probe_all().await;
    probes.extend(zenvy_profiler::probe_system(
        root,
        &profile,
        &loaded.config.backends,
        runtime.as_deref(),
        home.as_deref(),
    ));
    let registry = Registry::new();
    for (id, mut rec) in probes {
        zenvy_profiler::attach_remediation(
            root,
            &id,
            &mut rec,
            &profile.os,
            &profile.session.compositor,
        );
        registry.set(id, rec);
    }
    let manifest = ui_manifest(&registry.snapshot(), &loaded.config.features);
    Ok(Scan {
        profile,
        registry,
        manifest,
        warnings: loaded.warnings,
    })
}

async fn doctor(json: bool) {
    let sc = match scan().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    let snap = sc.registry.snapshot();
    if json {
        let v = serde_json::json!({ "profile": sc.profile, "capabilities": snap, "ui_manifest": sc.manifest });
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
        return;
    }
    let p = &sc.profile;
    println!(
        "System   : {} {} | kernel {} | {} | {:?}",
        p.os.distro_id,
        p.os.version_id.as_deref().unwrap_or(""),
        p.os.kernel,
        p.os.arch,
        p.os.package_manager
    );
    println!(
        "Session  : {} / {} (wayland globals: {})",
        p.session.session_type,
        p.session.compositor,
        p.session.wayland_protocols.len()
    );
    println!(
        "Hardware : {} ({} threads), {} MB RAM, {:?}, battery: {}",
        p.hardware.cpu_model,
        p.hardware.cpu_threads,
        p.hardware.ram_mb,
        p.hardware.form_factor,
        p.hardware.battery_present
    );
    for g in &p.hardware.gpus {
        println!(
            "GPU      : {} ({})",
            g.vendor,
            g.driver.as_deref().unwrap_or("?")
        );
    }
    for m in &p.displays {
        println!(
            "Display  : {} {}x{}@{:.0}Hz scale {} ({} {})",
            m.connector, m.width, m.height, m.refresh_hz, m.scale, m.make, m.model
        );
    }
    let pe = &p.peripherals;
    println!(
        "Input    : {} keyboards, {} pointers, {} touchpads, {} gamepads | audio out {} in {}",
        pe.keyboards, pe.pointers, pe.touchpads, pe.gamepads, pe.audio_outputs, pe.audio_inputs
    );
    println!();
    println!(
        "{:<22} {:<20} {:<24} reason",
        "capability", "state", "backend"
    );
    for (id, r) in &snap.capabilities {
        println!(
            "{:<22} {:<20} {:<24} {}",
            id,
            r.state.label(),
            r.backend.as_deref().unwrap_or("-"),
            r.reason.as_deref().unwrap_or("")
        );
    }
    let fixes: Vec<_> = snap
        .capabilities
        .iter()
        .filter(|(_, r)| r.remediation.is_some())
        .collect();
    if !fixes.is_empty() {
        println!("\nSuggested fixes (nothing is run automatically):");
        for (id, r) in fixes {
            let rem = r.remediation.as_ref().unwrap();
            println!("  [{id}] {}", rem.summary);
            for c in &rem.commands {
                println!(
                    "      {}: {}{}",
                    c.run_as,
                    c.argv.join(" "),
                    if c.needs_relogin {
                        "   (then log out and in)"
                    } else {
                        ""
                    }
                );
            }
        }
    }
    println!(
        "\nPhone UI modules: {}",
        sc.manifest
            .iter()
            .map(|m| if m.enabled {
                m.id.to_string()
            } else {
                format!("({} off)", m.id)
            })
            .collect::<Vec<_>>()
            .join(", ")
    );
    for w in &sc.warnings {
        println!("warning: {w}");
    }
}

fn data_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/share")))
        .map(|d| d.join("zenvy"))
}

async fn profile_cmd() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let sc = scan().await?;
    let json = serde_json::to_string_pretty(&sc.profile)?;
    if let Some(dir) = data_dir() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
        let path = dir.join("profile.json");
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?;
        f.write_all(json.as_bytes())?;
        eprintln!("saved {} (mode 0600, local only)", path.display());
    }
    println!("{json}");
    Ok(())
}

fn config_cmd() -> Result<(), Box<dyn std::error::Error>> {
    let loaded = load_config()?;
    match zenvy_config::config_path() {
        Some(p) => eprintln!(
            "config file: {} ({})",
            p.display(),
            if p.exists() {
                "found"
            } else {
                "not present, using defaults"
            }
        ),
        None => eprintln!("config file: <no HOME>"),
    }
    for w in &loaded.warnings {
        eprintln!("warning: {w}");
    }
    println!("{:#?}", loaded.config);
    Ok(())
}

async fn adapter() -> Result<HyprlandAdapter, Box<dyn std::error::Error>> {
    Ok(HyprlandAdapter::connect(HyprClient::from_env()?).await?)
}

async fn state() -> Result<(), Box<dyn std::error::Error>> {
    let a = adapter().await?;
    println!("dispatch dialect: {:?}", a.mode());
    println!(
        "monitors:   {}",
        serde_json::to_string(&a.monitors().await?)?
    );
    println!(
        "workspaces: {}",
        serde_json::to_string(&a.workspaces().await?)?
    );
    for w in a.windows().await? {
        println!(
            "window {} ws={} float={} [{}] {}",
            w.address.as_str(),
            w.workspace,
            w.floating,
            w.class,
            w.title
        );
    }
    Ok(())
}

async fn watch() {
    let mut rx = subscribe();
    let mut ctrl_c = std::pin::pin!(tokio::signal::ctrl_c());
    loop {
        tokio::select! {
            _ = &mut ctrl_c => return,
            item = rx.recv() => match item {
                Some(StreamItem::Event(e)) => println!("{e:?}"),
                Some(StreamItem::Connected) => println!("-- connected"),
                Some(StreamItem::Disconnected) => println!("-- disconnected (retrying)"),
                None => return,
            }
        }
    }
}

async fn dev(cmd: DevCmd) -> Result<(), Box<dyn std::error::Error>> {
    let addr = |s: &str| Address::parse(s).map_err(|e| e.to_string());
    let c = match cmd {
        DevCmd::SwitchWorkspace { id } => DesktopCommand::SwitchWorkspace { id },
        DevCmd::Focus { address } => DesktopCommand::FocusWindow {
            address: addr(&address)?,
        },
        DevCmd::Close { address } => DesktopCommand::CloseWindow {
            address: addr(&address)?,
        },
        DevCmd::Float { address } => DesktopCommand::ToggleFloating {
            address: addr(&address)?,
        },
        DevCmd::Move { address, workspace } => DesktopCommand::MoveWindowToWorkspace {
            address: addr(&address)?,
            workspace,
        },
    };
    adapter().await?.execute(&c).await?;
    println!("ok");
    Ok(())
}

/// Audio control needs the `audio.volume` capability (a running server **and** `pactl`).
fn build_audio(sc: &Scan) -> Option<std::sync::Arc<zenvy_audio::AudioAdapter>> {
    if sc.registry.get("audio.volume")?.state != zenvy_proto::CapState::Available {
        eprintln!("audio control: not available (see `zenvyd doctor`)");
        return None;
    }
    let pactl = sc.profile.software_hints.get("pactl")?.clone()?;
    eprintln!("audio control: ready (pactl)");
    Some(std::sync::Arc::new(zenvy_audio::AudioAdapter::new(pactl)))
}

/// Brightness: detects the displays that really answer (slow, so only once at startup) and corrects the
/// capability if none do, e.g. a monitor with DDC/CI switched off in its own menu.
async fn build_brightness(
    sc: &Scan,
) -> Option<std::sync::Arc<zenvy_brightness::BrightnessAdapter>> {
    let rec = sc.registry.get("brightness")?;
    if rec.state != zenvy_proto::CapState::Available {
        eprintln!("brightness control: not available (see `zenvyd doctor`)");
        return None;
    }
    let hint = |n: &str| {
        sc.profile
            .software_hints
            .get(n)
            .cloned()
            .flatten()
            .map(std::path::PathBuf::from)
    };
    let ddc = rec
        .candidates
        .iter()
        .any(|c| c.backend == "ddc" && c.state == zenvy_proto::CapState::Available)
        .then(|| hint("ddcutil"))
        .flatten();
    let bl = rec
        .candidates
        .iter()
        .any(|c| c.backend == "sysfs-backlight" && c.state == zenvy_proto::CapState::Available)
        .then(|| hint("brightnessctl"))
        .flatten();
    let adapter = zenvy_brightness::BrightnessAdapter::detect(ddc, bl).await;
    if adapter.is_empty() {
        eprintln!(
            "brightness control: no display answers (for a monitor, enable DDC/CI in its menu)"
        );
        sc.registry.set(
            "brightness",
            zenvy_proto::CapabilityRecord::not(
                zenvy_proto::CapState::Unavailable,
                "ddc",
                "no display answers brightness requests; enable DDC/CI in the monitor's own menu",
            ),
        );
        return None;
    }
    eprintln!(
        "brightness control: ready ({} display(s))",
        adapter.state().displays.len()
    );
    Some(std::sync::Arc::new(adapter))
}

/// File transfer: the inbox folder (`~` expands to the home folder) and the size limit.
fn build_files(cfg: &zenvy_config::Config) -> Option<std::sync::Arc<zenvy_net::Transfers>> {
    let raw = cfg.files.inbox.trim();
    let inbox = match raw.strip_prefix("~/") {
        Some(rest) => std::path::PathBuf::from(std::env::var_os("HOME")?).join(rest),
        None => std::path::PathBuf::from(raw),
    };
    let t = zenvy_net::Transfers::new(inbox.clone(), cfg.files.max_size_mb * 1024 * 1024);
    t.clean_stale_parts();
    eprintln!(
        "file transfer: ready (inbox {}, up to {} MB); phones need the 'files' permission (zenvyd grant <id> files)",
        inbox.display(),
        cfg.files.max_size_mb
    );
    Some(std::sync::Arc::new(t))
}

/// Clipboard transfer and link opening need wl-clipboard (`wl-copy`, `wl-paste`); `xdg-open` adds link opening.
fn build_clipboard(sc: &Scan) -> Option<zenvy_net::ClipboardTools> {
    let tool = |n: &str| {
        sc.profile
            .software_hints
            .get(n)
            .cloned()
            .flatten()
            .map(std::path::PathBuf::from)
    };
    let (copy, paste) = (tool("wl-copy")?, tool("wl-paste")?);
    eprintln!("clipboard transfer: ready; phones need the 'clipboard' permission (zenvyd grant <id> clipboard)");
    Some(zenvy_net::ClipboardTools {
        copy,
        paste,
        open: tool("xdg-open"),
    })
}

/// Power and lock control needs `systemctl` and `loginctl` (the `power.control` capability).
async fn build_power(sc: &Scan, cfg: &zenvy_config::Config) -> Option<zenvy_net::PowerRuntime> {
    if sc.registry.get("power.control")?.state != zenvy_proto::CapState::Available {
        eprintln!("power control: not available (needs systemctl and loginctl)");
        return None;
    }
    let tool = |n: &str| {
        sc.profile
            .software_hints
            .get(n)
            .cloned()
            .flatten()
            .map(std::path::PathBuf::from)
    };
    let adapter = zenvy_power::PowerAdapter::discover(
        tool("systemctl")?,
        tool("loginctl")?,
        tool("busctl"),
        cfg.lock
            .use_compositor
            .then(|| std::path::PathBuf::from("hyprctl")),
        lock_config(cfg),
    )
    .await;
    let layout_us = keyboard_layout_is_us().await;
    let c = adapter.can();
    eprintln!(
        "power control: ready (lock {}, sleep {}, restart {}, power off {}); phones need the 'power' permission (zenvyd grant <id> power)",
        yn(c.lock), yn(c.suspend), yn(c.reboot), yn(c.poweroff)
    );
    let unlock = cfg.lock.unlock_from_phone
        && layout_us
        && sc
            .registry
            .get("input.keyboard")
            .is_some_and(|r| r.state == zenvy_proto::CapState::Available);
    eprintln!(
        "unlock from phone: {} (needs the 'unlock' permission: zenvyd grant <id> unlock)",
        if unlock {
            "ready"
        } else if !cfg.lock.unlock_from_phone {
            "off in config"
        } else if !layout_us {
            "off: the PC keyboard layout is not US"
        } else {
            "off: no keyboard injection"
        }
    );
    Some(zenvy_net::PowerRuntime::new(
        std::sync::Arc::new(adapter),
        cfg.lock.unlock_from_phone,
        layout_us,
        std::time::Duration::from_secs(cfg.lock.power_delay_seconds.clamp(1, 30)),
    ))
}

fn yn(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

fn lock_config(cfg: &zenvy_config::Config) -> zenvy_power::LockConfig {
    zenvy_power::LockConfig {
        processes: cfg.lock.processes.clone(),
        detect_cmd: cfg.lock.detect_cmd.clone(),
        use_compositor: cfg.lock.use_compositor,
    }
}

/// Keystrokes are sent as keycodes, which mean what we expect only on a US layout. Asks Hyprland for the layout.
async fn keyboard_layout_is_us() -> bool {
    let out = tokio::process::Command::new("hyprctl")
        .args(["getoption", "input:kb_layout", "-j"])
        .stdin(std::process::Stdio::null())
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => serde_json::from_slice::<serde_json::Value>(&o.stdout)
            .ok()
            .and_then(|v| {
                v["str"]
                    .as_str()
                    .map(|s| s.split(',').next().unwrap_or("").trim() == "us")
            })
            .unwrap_or(false),
        _ => false,
    }
}

/// The screen preview needs `grim` and a compositor that allows screen capture (the `screen.capture` capability).
fn build_screen(sc: &Scan) -> Option<zenvy_screen::ScreenStreamer> {
    let grim = sc.profile.software_hints.get("grim").cloned().flatten();
    let cap = sc
        .registry
        .get("screen.capture")
        .map(|c| c.state == zenvy_proto::CapState::Available)
        .unwrap_or(false);
    match (grim, cap) {
        (Some(g), true) => {
            eprintln!("screen preview: ready (grim); phones need the 'screen' permission (zenvyd grant <id> screen)");
            Some(zenvy_screen::ScreenStreamer::new(g))
        }
        _ => {
            eprintln!("screen preview: not available (needs grim and screen capture)");
            None
        }
    }
}

/// Listening needs `parec` and a running audio server (the `audio.listen` capability).
fn build_stream(sc: &Scan) -> Option<std::sync::Arc<zenvy_stream::AudioStreamer>> {
    if sc.registry.get("audio.listen")?.state != zenvy_proto::CapState::Available {
        eprintln!("listening (PC audio to a phone): not available (needs parec)");
        return None;
    }
    let parec = sc.profile.software_hints.get("parec")?.clone()?;
    eprintln!("listening (PC audio to a phone): ready; phones need the 'listen' permission (zenvyd grant <id> listen)");
    Some(std::sync::Arc::new(zenvy_stream::AudioStreamer::new(
        zenvy_stream::CaptureSpec::parec(parec),
    )))
}

/// Media control needs the user's D-Bus session bus (MPRIS).
async fn build_media(reg: &Registry) -> Option<std::sync::Arc<zenvy_media::MediaAdapter>> {
    if reg.get("media.control")?.state != zenvy_proto::CapState::Available {
        eprintln!("media control: not available (no D-Bus session bus)");
        return None;
    }
    match zenvy_media::MediaAdapter::connect().await {
        Ok(m) => {
            eprintln!("media control: ready (MPRIS)");
            Some(std::sync::Arc::new(m))
        }
        Err(e) => {
            eprintln!("media control: cannot connect to D-Bus: {e}");
            None
        }
    }
}

/// Creates the uinput devices if the capability registry says a uinput backend is usable.
async fn build_input(
    reg: &Registry,
) -> Option<std::sync::Arc<zenvy_input::InputAdapter<zenvy_input::UinputSink>>> {
    let pointer_ok = reg
        .get("input.pointer")
        .is_some_and(|r| r.backend.as_deref() == Some("uinput"));
    let keyboard_ok = reg
        .get("input.keyboard")
        .is_some_and(|r| r.backend.as_deref() == Some("uinput"));
    if !(pointer_ok && keyboard_ok) {
        eprintln!("input injection: not available (see `zenvyd doctor`)");
        return None;
    }
    let text = match reg.get("input.text").and_then(|r| r.backend).as_deref() {
        Some("wtype") => zenvy_input::TextBackend::Wtype,
        Some("uinput-ascii") => zenvy_input::TextBackend::UinputAscii,
        _ => zenvy_input::TextBackend::None,
    };
    match tokio::task::spawn_blocking(move || zenvy_input::InputAdapter::create(text)).await {
        Ok(Ok(a)) => {
            eprintln!("input injection: ready (uinput devices created; text via {text:?})");
            Some(std::sync::Arc::new(a))
        }
        Ok(Err(e)) => {
            eprintln!("input injection: cannot create uinput devices: {e}");
            None
        }
        Err(e) => {
            eprintln!("input injection: {e}");
            None
        }
    }
}

// ---------------------------------------------------------------- phase 3: server + admin client

fn state_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/state")))
        .map(|d| d.join("zenvy"))
}

fn admin_sock() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(
        PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR not set")?)
            .join("zenvy/admin.sock"),
    )
}

/// IPv4 address of the interface that owns the default route (read from /proc/net/route).
/// This machine has two LAN-ish interfaces, so "the" address must be derived, not guessed.
fn default_route_ip() -> Option<std::net::IpAddr> {
    let table = std::fs::read_to_string("/proc/net/route").ok()?;
    let iface = table.lines().skip(1).find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        (f.get(1) == Some(&"00000000") && f.get(7) == Some(&"00000000")).then(|| f[0].to_string())
    })?;
    if_addrs::get_if_addrs()
        .ok()?
        .into_iter()
        .find(|i| i.name == iface && i.ip().is_ipv4())
        .map(|i| i.ip())
}

fn resolve_bind(
    cfg: &zenvy_config::Config,
) -> Result<std::net::SocketAddr, Box<dyn std::error::Error>> {
    let first = cfg
        .server
        .bind
        .first()
        .map(String::as_str)
        .unwrap_or("auto");
    let ip: std::net::IpAddr = if first == "auto" {
        default_route_ip().ok_or("cannot determine the LAN address automatically; set [server] bind = [\"<ip>\"] in the config")?
    } else {
        first
            .parse()
            .map_err(|_| format!("invalid bind address '{first}'"))?
    };
    if cfg.server.bind.len() > 1 {
        eprintln!("warning: only the first bind address is used for now");
    }
    Ok(std::net::SocketAddr::new(ip, cfg.server.port))
}

async fn serve_cmd() -> Result<(), Box<dyn std::error::Error>> {
    let loaded = load_config()?;
    for w in &loaded.warnings {
        eprintln!("warning: {w}");
    }
    let cfg = loaded.config;
    let addr = resolve_bind(&cfg)?;
    let data = data_dir().ok_or("HOME not set")?;
    let state = state_dir().ok_or("HOME not set")?;

    let mut sans = vec!["localhost".to_string(), addr.ip().to_string()];
    sans.dedup();
    let tls = zenvy_net::tls::load_or_create(&data.join("tls"), &sans)?;

    let sc = scan().await?;
    let mut hosts = vec![addr.to_string()];
    if addr.ip().is_loopback() {
        hosts.extend([
            format!("localhost:{}", addr.port()),
            format!("127.0.0.1:{}", addr.port()),
        ]);
    }
    let net_cfg = zenvy_net::NetConfig {
        session_ttl: std::time::Duration::from_secs(cfg.security.session_ttl_hours * 3600),
        pairing_ttl: std::time::Duration::from_secs(cfg.security.pairing_ttl_seconds),
        max_devices: cfg.security.max_devices,
        allowed_hosts: hosts,
        features: cfg.features.clone(),
        clipboard: build_clipboard(&sc),
        files: build_files(&cfg),
        screen: build_screen(&sc),
        actions: cfg
            .actions
            .iter()
            .map(|a| zenvy_net::ActionDef {
                id: a.id.clone(),
                label: a.label.clone(),
                icon: a.icon.clone(),
                confirm: a.confirm,
                argv: a.argv.clone(),
            })
            .collect(),
    };
    if !cfg.actions.is_empty() {
        eprintln!(
            "custom actions: {} configured; phones need the 'exec' permission (zenvyd grant <id> exec)",
            cfg.actions.len()
        );
    }
    let input = build_input(&sc.registry).await;
    let audio = build_audio(&sc);
    let media = build_media(&sc.registry).await;
    let brightness = build_brightness(&sc).await;
    let stream = build_stream(&sc);
    let power = build_power(&sc, &cfg).await;
    let notify = sc
        .profile
        .software_hints
        .get("notify-send")
        .cloned()
        .flatten()
        .map(std::path::PathBuf::from);
    let store = zenvy_auth::DeviceStore::open(data.join("devices.json"), cfg.security.max_devices)?;
    let audit = zenvy_auth::AuditLog::new(state.join("audit.log"));
    let app = zenvy_net::App::new(
        adapter().await?,
        input,
        audio,
        brightness,
        stream,
        notify,
        power,
        state.clone(),
        media,
        sc.registry,
        net_cfg,
        store,
        audit,
        tls.fingerprint.clone(),
    );

    if app.listen.recover().await {
        eprintln!("listening: unmuted the PC speakers, which a previous run had muted while a phone listened");
    }
    zenvy_net::spawn_publisher(app.clone());
    let sock = admin_sock()?;
    let base = format!("https://{addr}");
    let admin_app = app.clone();
    tokio::spawn(async move {
        if let Err(e) = zenvy_net::serve_admin(admin_app, &sock, base).await {
            eprintln!("admin socket error: {e}");
        }
    });

    if cfg.browser.enabled {
        let token = zenvy_net::load_or_create_token(&data.join("browser.token"))?;
        let bridge = std::net::SocketAddr::from(([127, 0, 0, 1], cfg.browser.port));
        let bapp = app.clone();
        tokio::spawn(async move {
            if let Err(e) = zenvy_net::serve_browser(bapp, bridge, token).await {
                eprintln!("browser bridge error: {e}");
            }
        });
        eprintln!(
            "browser bridge: 127.0.0.1:{} (loopback only, token required; see `zenvyd browser setup`)",
            cfg.browser.port
        );
    }
    eprintln!(
        "zenvy listening on https://{addr}  (cert SHA-256 {})",
        tls.fingerprint
    );
    eprintln!("run `zenvyd pair` in another terminal to pair a phone");
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let shutdown = async move {
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
    };
    zenvy_net::serve(app, addr, &tls, shutdown).await?;
    eprintln!("bye");
    Ok(())
}

async fn admin_connect() -> Result<
    (
        tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
        tokio::net::unix::OwnedWriteHalf,
    ),
    Box<dyn std::error::Error>,
> {
    let s = tokio::net::UnixStream::connect(admin_sock()?)
        .await
        .map_err(|e| format!("cannot reach the agent ({e}); is `zenvyd serve` running?"))?;
    let (r, w) = s.into_split();
    Ok((tokio::io::BufReader::new(r), w))
}

async fn admin_line(
    r: &mut tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    use tokio::io::AsyncBufReadExt;
    let mut l = String::new();
    if r.read_line(&mut l).await? == 0 {
        return Err("agent closed the connection".into());
    }
    Ok(serde_json::from_str(&l)?)
}

async fn send_cmd(path: std::path::PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    use tokio::io::AsyncWriteExt;
    let abs =
        std::fs::canonicalize(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let (mut r, mut w) = admin_connect().await?;
    w.write_all(format!("{}\n", serde_json::json!({ "op": "send", "path": abs })).as_bytes())
        .await?;
    let m = admin_line(&mut r).await?;
    if m["ok"] == true {
        println!("{}", m["message"].as_str().unwrap_or("offered"));
        Ok(())
    } else {
        Err(m["message"]
            .as_str()
            .unwrap_or("the agent refused")
            .to_string()
            .into())
    }
}

async fn power_cancel_cmd() -> Result<(), Box<dyn std::error::Error>> {
    use tokio::io::AsyncWriteExt;
    let (mut r, mut w) = admin_connect().await?;
    w.write_all(b"{\"op\":\"power_cancel\"}\n").await?;
    let m = admin_line(&mut r).await?;
    println!("{}", m["message"].as_str().unwrap_or("done"));
    Ok(())
}

/// Prints the lock signals once a second until interrupted. Read-only.
async fn lock_probe_cmd() -> Result<(), Box<dyn std::error::Error>> {
    let loaded = load_config()?;
    let tool = |n: &str| -> std::path::PathBuf { n.into() };
    let p = zenvy_power::PowerAdapter::discover(
        tool("systemctl"),
        tool("loginctl"),
        Some(tool("busctl")),
        loaded.config.lock.use_compositor.then(|| tool("hyprctl")),
        lock_config(&loaded.config),
    )
    .await;
    println!("session: {:?}", p.session());
    println!("Lock the screen, unlock it again, then press Ctrl-C. Each line shows what changed.");
    let mut last = String::new();
    loop {
        let s = p.lock_signals().await;
        let layers = layer_namespaces().await;
        let line = format!(
            "LockedHint={:?} lockers={:?} detect_cmd={:?} state={:?} {} layers={}",
            s.locked_hint,
            s.processes,
            s.detect_cmd,
            p.lock_state().await,
            probe_extras(p.session()).await,
            layers
        );
        if line != last {
            println!("{} {line}", chrono_like_now());
            last = line;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

fn chrono_like_now() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!(
        "[{:02}:{:02}:{:02}]",
        (t / 3600) % 24,
        (t / 60) % 60,
        t % 60
    )
}

/// More candidate signals, for calibration only: focus, monitor scan-out blockers and logind's view of the session.
async fn probe_extras(session: Option<&str>) -> String {
    async fn json(args: &[&str]) -> Option<serde_json::Value> {
        let o = tokio::process::Command::new("hyprctl")
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .ok()?;
        serde_json::from_slice(&o.stdout).ok()
    }
    let active = json(&["-j", "activewindow"])
        .await
        .map(|v| {
            if v["address"].as_str().is_some_and(|a| !a.is_empty()) {
                "yes"
            } else {
                "none"
            }
        })
        .unwrap_or("?");
    let mon = json(&["-j", "monitors"])
        .await
        .and_then(|v| v.as_array().and_then(|a| a.first().cloned()));
    let mon = match mon {
        Some(m) => format!(
            "dpms={} solitary={} scanout={} tearing={}",
            m["dpmsStatus"],
            m["solitaryBlockedBy"],
            m["directScanoutBlockedBy"],
            m["tearingBlockedBy"]
        ),
        None => "?".into(),
    };
    let sess = match session {
        Some(id) => {
            let o = tokio::process::Command::new("loginctl")
                .args([
                    "show-session",
                    id,
                    "-p",
                    "IdleHint",
                    "-p",
                    "Active",
                    "-p",
                    "State",
                    "--value",
                ])
                .stdin(std::process::Stdio::null())
                .output()
                .await
                .ok();
            o.map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default()
        }
        None => String::new(),
    };
    format!("activewindow={active} mon[{mon}] session[{sess}]")
}

/// The distinct layer-shell namespaces Hyprland reports (names of shell components, no window content).
async fn layer_namespaces() -> String {
    let out = tokio::process::Command::new("hyprctl")
        .args(["-j", "layers"])
        .stdin(std::process::Stdio::null())
        .output()
        .await;
    let Ok(o) = out else { return "?".into() };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&o.stdout) else {
        return "?".into();
    };
    let mut names: Vec<String> = Vec::new();
    if let Some(mons) = v.as_object() {
        for m in mons.values() {
            if let Some(levels) = m["levels"].as_object() {
                for l in levels.values().filter_map(|l| l.as_array()) {
                    for e in l {
                        if let Some(n) = e["namespace"].as_str() {
                            if !names.iter().any(|x| x == n) {
                                names.push(n.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    names.sort();
    names.join(",")
}

async fn pair_cmd() -> Result<(), Box<dyn std::error::Error>> {
    use tokio::io::AsyncWriteExt;
    let (mut r, mut w) = admin_connect().await?;
    w.write_all(b"{\"op\":\"pair\"}\n").await?;
    let first = admin_line(&mut r).await?;
    if first["type"] == "error" {
        return Err(first["message"].as_str().unwrap_or("error").into());
    }
    let url = first["url"].as_str().ok_or("bad reply")?;
    let qr = qrcode::QrCode::new(url.as_bytes())?;
    println!(
        "{}",
        qr.render::<qrcode::render::unicode::Dense1x2>()
            .quiet_zone(true)
            .build()
    );
    println!(
        "Scan with your phone camera (valid {} s, single use).",
        first["ttl"]
    );
    println!("Link: {url}");
    println!(
        "The phone will show a self-signed certificate warning. Its SHA-256 should be:\n  {}",
        first["fingerprint"].as_str().unwrap_or("?")
    );
    loop {
        let m = admin_line(&mut r).await?;
        match m["type"].as_str() {
            Some("approval") => {
                println!(
                    "\nA phone asks to pair: '{}'. Confirmation code on its screen must be {}.",
                    m["name"].as_str().unwrap_or("?"),
                    m["code"].as_str().unwrap_or("?")
                );
                print!("Approve? [y/N] ");
                use std::io::Write;
                std::io::stdout().flush()?;
                let ans = tokio::task::spawn_blocking(|| {
                    let mut a = String::new();
                    let _ = std::io::stdin().read_line(&mut a);
                    a
                })
                .await?;
                let yes = matches!(ans.trim().to_ascii_lowercase().as_str(), "y" | "yes");
                w.write_all(format!("{{\"approve\":{yes}}}\n").as_bytes())
                    .await?;
            }
            Some("done") => {
                println!("pairing {}", m["result"].as_str().unwrap_or("?"));
                return Ok(());
            }
            _ => {}
        }
    }
}

async fn devices_cmd() -> Result<(), Box<dyn std::error::Error>> {
    use tokio::io::AsyncWriteExt;
    let (mut r, mut w) = admin_connect().await?;
    w.write_all(b"{\"op\":\"devices\"}\n").await?;
    let m = admin_line(&mut r).await?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let list = m["devices"].as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        println!("no paired devices");
    }
    for d in list {
        let seen = d["last_seen"]
            .as_u64()
            .map(|t| format!("{} s ago", now.saturating_sub(t)))
            .unwrap_or_else(|| "never".into());
        println!(
            "{:<14} {:<24} last seen {:<12} scopes: {}",
            d["id"].as_str().unwrap_or("?"),
            d["name"].as_str().unwrap_or("?"),
            seen,
            d["scopes"]
        );
    }
    Ok(())
}

async fn revoke_cmd(id: Option<String>, all: bool) -> Result<(), Box<dyn std::error::Error>> {
    use tokio::io::AsyncWriteExt;
    let (mut r, mut w) = admin_connect().await?;
    let req = if all {
        serde_json::json!({ "op": "revoke_all" })
    } else {
        serde_json::json!({ "op": "revoke", "id": id.ok_or("give a device id or --all")? })
    };
    w.write_all(format!("{req}\n").as_bytes()).await?;
    let m = admin_line(&mut r).await?;
    if m["ok"] == true {
        println!(
            "revoked {}",
            if all {
                format!("{} device(s)", m["count"])
            } else {
                m["name"].as_str().unwrap_or("device").to_string()
            }
        );
        Ok(())
    } else {
        Err("no unique device matches that id".into())
    }
}

fn browser_setup() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = load_config()?.config;
    let data = data_dir().ok_or("HOME not set")?;
    let token = zenvy_net::load_or_create_token(&data.join("browser.token"))?;
    let ext = std::env::current_exe()
        .ok()
        .and_then(|_| std::env::var_os("ZENVY_EXTENSION_DIR"))
        .map(PathBuf::from);
    println!("Zenvy Browser Bridge lets the phone list, switch and close your browser tabs.");
    println!("It sends only tab titles and site names (never full addresses) to this PC, over loopback.\n");
    println!("1. Open your browser's extensions page (brave://extensions or chrome://extensions)");
    println!("2. Turn on Developer mode, choose Load unpacked, and pick the `browser-extension` folder{}", ext.map(|p| format!(" ({})", p.display())).unwrap_or_default());
    println!(
        "3. Open the extension's options and paste this token (port {}):\n",
        cfg.browser.port
    );
    println!("   {token}\n");
    println!("Keep `zenvyd serve` running. Tabs appear on the phone's Desk screen under the browser window.");
    Ok(())
}

async fn scope_cmd(op: &str, id: String, scope: String) -> Result<(), Box<dyn std::error::Error>> {
    use tokio::io::AsyncWriteExt;
    let (mut r, mut w) = admin_connect().await?;
    w.write_all(
        format!(
            "{}\n",
            serde_json::json!({ "op": op, "id": id, "scope": scope })
        )
        .as_bytes(),
    )
    .await?;
    let m = admin_line(&mut r).await?;
    if m["ok"] == true {
        println!(
            "{} now has: {}",
            m["name"].as_str().unwrap_or("device"),
            m["scopes"]
        );
        Ok(())
    } else {
        Err(m["message"]
            .as_str()
            .unwrap_or("no unique device matches that id")
            .to_string()
            .into())
    }
}
