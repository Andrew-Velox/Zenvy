//! `zenvy`: a small desktop window for the Zenvy agent. Pair a phone with a QR code, see who is connected, and switch each
//! phone's permissions on or off. It is only a front end to the agent's local admin socket (the same interface as the
//! `zenvyd` command line), so it has no power of its own and nothing here is reachable from the network.

mod admin;

use admin::{Device, PairEvent, Pairing, Status};
use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Stroke, Theme, Vec2};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

const LOGO: &[u8] = include_bytes!("../../../assets/logo/zenvy-icon-512.png");
const ACCENT: Color32 = Color32::from_rgb(0x00, 0x7A, 0xFF);

/// (permission, label, what it allows, asks for confirmation before it is switched on)
const SCOPES: &[(&str, &str, &str, bool)] = &[
    (
        "screen",
        "Screen",
        "See the PC screen and click through it",
        false,
    ),
    (
        "launch",
        "Apps",
        "Open installed apps, on a new workspace if you like",
        false,
    ),
    (
        "listen",
        "Speaker",
        "Hear the PC's sound on the phone",
        false,
    ),
    (
        "clipboard",
        "Clipboard",
        "Copy text between the phone and the PC",
        false,
    ),
    ("files", "Files", "Send files to and from the PC", false),
    (
        "power",
        "Power",
        "Lock, sleep, restart and shut down the PC",
        true,
    ),
    (
        "unlock",
        "Unlock",
        "Type your password on the lock screen",
        true,
    ),
    (
        "exec",
        "Actions",
        "Run the actions from your config file",
        true,
    ),
];

struct Palette {
    bg: Color32,
    card: Color32,
    text: Color32,
    muted: Color32,
    line: Color32,
    danger: Color32,
    ok: Color32,
}

fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            bg: Color32::from_rgb(0x1D, 0x1D, 0x1F),
            card: Color32::from_rgb(0x2B, 0x2B, 0x2D),
            text: Color32::from_rgb(0xF5, 0xF5, 0xF7),
            muted: Color32::from_rgb(0xAA, 0xAA, 0xAA),
            line: Color32::from_rgb(0x44, 0x44, 0x48),
            danger: Color32::from_rgb(0xFF, 0x6B, 0x63),
            ok: Color32::from_rgb(0x32, 0xD7, 0x4B),
        }
    } else {
        Palette {
            bg: Color32::from_rgb(0xF5, 0xF5, 0xF7),
            card: Color32::WHITE,
            text: Color32::from_rgb(0x1D, 0x1D, 0x1F),
            muted: Color32::from_rgb(0x63, 0x63, 0x68),
            line: Color32::from_rgb(0xD8, 0xD8, 0xDD),
            danger: Color32::from_rgb(0xD7, 0x00, 0x15),
            ok: Color32::from_rgb(0x1F, 0xA8, 0x3A),
        }
    }
}

fn visuals(dark: bool) -> egui::Visuals {
    let p = palette(dark);
    let mut v = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.panel_fill = p.bg;
    v.window_fill = p.card;
    v.extreme_bg_color = p.card;
    v.override_text_color = Some(p.text);
    v.selection.bg_fill = ACCENT;
    v.hyperlink_color = ACCENT;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.line);
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
    ] {
        w.corner_radius = CornerRadius::same(14);
    }
    v.widgets.inactive.bg_fill = p.card;
    v.widgets.inactive.weak_bg_fill = p.card;
    v
}

enum Msg {
    Poll(Result<(Status, Vec<Device>), String>),
    /// The result of a change the person asked for: `Err` is shown as a notice.
    Action(Result<String, String>),
}

struct PairState {
    pairing: Pairing,
    link: Option<(String, u64)>,
    qr: Option<(usize, Vec<bool>)>,
    approval: Option<(String, String)>,
    result: Option<String>,
    error: Option<String>,
    started: Instant,
}

enum View {
    Home,
    Pair(Box<PairState>),
}

struct ZenvyApp {
    status: Option<Status>,
    devices: Vec<Device>,
    error: Option<String>,
    loaded: bool,
    rx: Receiver<Msg>,
    tx: Sender<Msg>,
    poke: Sender<()>,
    view: View,
    logo: egui::TextureHandle,
    confirm_remove: Option<String>,
    confirm_grant: Option<(String, String)>,
    notice: Option<(String, Instant)>,
    start_note: Option<String>,
}

fn load_png(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = dec.read_info().ok()?;
    let mut buf = vec![0; r.output_buffer_size()?];
    let info = r.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect(),
        _ => return None,
    };
    Some((rgba, info.width, info.height))
}

/// "just now", "5 min ago", "3 h ago", "2 d ago"
fn ago(secs: u64) -> String {
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86_400),
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The address without the scheme, for display.
fn host_only(address: &str) -> &str {
    address
        .strip_prefix("https://")
        .unwrap_or(address)
        .trim_end_matches('/')
}

impl ZenvyApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals_of(Theme::Light, visuals(false));
        cc.egui_ctx.set_visuals_of(Theme::Dark, visuals(true));
        cc.egui_ctx.all_styles_mut(|s| {
            s.spacing.item_spacing = Vec2::new(10.0, 10.0);
            s.spacing.button_padding = Vec2::new(14.0, 8.0);
            s.text_styles
                .insert(egui::TextStyle::Heading, egui::FontId::proportional(22.0));
            s.text_styles
                .insert(egui::TextStyle::Body, egui::FontId::proportional(15.0));
            s.text_styles
                .insert(egui::TextStyle::Button, egui::FontId::proportional(15.0));
            s.text_styles
                .insert(egui::TextStyle::Small, egui::FontId::proportional(12.5));
        });
        let (rgba, w, h) = load_png(LOGO).unwrap_or((vec![0, 0, 0, 0], 1, 1));
        let logo = cc.egui_ctx.load_texture(
            "logo",
            egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba),
            egui::TextureOptions::LINEAR,
        );
        let (tx, rx) = channel();
        let (poke, poked) = channel::<()>();
        let (t2, ctx) = (tx.clone(), cc.egui_ctx.clone());
        std::thread::spawn(move || loop {
            if t2.send(Msg::Poll(admin::poll())).is_err() {
                break;
            }
            ctx.request_repaint();
            let _ = poked.recv_timeout(Duration::from_millis(1500));
        });
        let mut app = Self {
            status: None,
            devices: vec![],
            error: None,
            loaded: false,
            rx,
            tx,
            poke,
            view: View::Home,
            logo,
            confirm_remove: None,
            confirm_grant: None,
            notice: None,
            start_note: None,
        };
        for a in std::env::args() {
            match a.as_str() {
                "--light" => cc.egui_ctx.set_theme(egui::ThemePreference::Light),
                "--dark" => cc.egui_ctx.set_theme(egui::ThemePreference::Dark),
                _ => {}
            }
        }
        if std::env::args().any(|a| a == "--pair") {
            app.start_pairing(&cc.egui_ctx); // `zenvy --pair`: straight to the QR code (a launcher or status-bar click)
        }
        app
    }

    fn say(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), Instant::now()));
    }

    /// Runs one change on a worker thread, then refreshes the list at once.
    fn act(
        &self,
        ctx: &egui::Context,
        work: impl FnOnce() -> Result<String, String> + Send + 'static,
    ) {
        let (tx, poke, ctx) = (self.tx.clone(), self.poke.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Action(work()));
            let _ = poke.send(());
            ctx.request_repaint();
        });
    }

    fn drain(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Poll(Ok((s, d))) => {
                    self.status = Some(s);
                    self.devices = d;
                    self.error = None;
                    self.loaded = true;
                }
                Msg::Poll(Err(e)) => {
                    self.status = None;
                    self.error = Some(e);
                    self.loaded = true;
                }
                Msg::Action(Ok(t)) => {
                    if !t.is_empty() {
                        self.say(t)
                    }
                }
                Msg::Action(Err(e)) => self.say(e),
            }
        }
        if let View::Pair(p) = &mut self.view {
            while let Ok(ev) = p.pairing.events.try_recv() {
                match ev {
                    PairEvent::Link { url, ttl } => {
                        p.qr = qr_modules(&url);
                        p.link = Some((url, ttl));
                    }
                    PairEvent::Approval { name, code } => p.approval = Some((name, code)),
                    PairEvent::Done(r) => p.result = Some(r),
                    PairEvent::Error(e) => p.error = Some(e),
                }
            }
        }
    }

    fn start_pairing(&mut self, ctx: &egui::Context) {
        let c = ctx.clone();
        match Pairing::start(move || c.request_repaint()) {
            Ok(pairing) => {
                self.view = View::Pair(Box::new(PairState {
                    pairing,
                    link: None,
                    qr: None,
                    approval: None,
                    result: None,
                    error: None,
                    started: Instant::now(),
                }))
            }
            Err(e) => self.say(e),
        }
    }
}

fn qr_modules(url: &str) -> Option<(usize, Vec<bool>)> {
    let code = qrcode::QrCode::new(url.as_bytes()).ok()?;
    let n = code.width();
    Some((
        n,
        code.to_colors()
            .into_iter()
            .map(|c| c == qrcode::Color::Dark)
            .collect(),
    ))
}

fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let p = palette(ui.visuals().dark_mode);
    egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.line))
        .corner_radius(CornerRadius::same(18))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        });
}

fn dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.5, color);
}

fn primary(text: &str) -> egui::Button<'static> {
    egui::Button::new(
        RichText::new(text.to_string())
            .color(Color32::WHITE)
            .strong(),
    )
    .fill(ACCENT)
    .corner_radius(CornerRadius::same(22))
    .min_size(Vec2::new(0.0, 40.0))
}

fn quiet(ui: &egui::Ui, text: &str) -> egui::Button<'static> {
    let p = palette(ui.visuals().dark_mode);
    egui::Button::new(RichText::new(text.to_string()).color(p.text))
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.line))
        .corner_radius(CornerRadius::same(22))
        .min_size(Vec2::new(0.0, 40.0))
}

impl eframe::App for ZenvyApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain();
        let ctx = ui.ctx().clone();
        if matches!(self.view, View::Pair(_)) {
            ctx.request_repaint_after(Duration::from_millis(500)); // the countdown
        }
        if self
            .notice
            .as_ref()
            .is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(4))
        {
            self.notice = None;
        }
        let p = palette(ui.visuals().dark_mode);
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(p.bg)
                    .inner_margin(Margin::symmetric(20, 16)),
            )
            .show(ui, |ui| {
                self.header(ui);
                ui.add_space(6.0);
                match &self.view {
                    View::Home => self.home(ui, &ctx),
                    View::Pair(_) => self.pairing(ui, &ctx),
                }
                if let Some((text, _)) = &self.notice {
                    ui.add_space(8.0);
                    card(ui, |ui| {
                        ui.label(RichText::new(text.clone()));
                    });
                }
            });
    }
}

impl ZenvyApp {
    fn header(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.image((self.logo.id(), Vec2::splat(30.0)));
            ui.label(RichText::new("Zenvy").heading().strong());
        });
    }

    fn status_card(&mut self, ui: &mut egui::Ui) {
        let p = palette(ui.visuals().dark_mode);
        card(ui, |ui| match (&self.status, &self.error) {
            (Some(s), _) => {
                ui.horizontal(|ui| {
                    dot(ui, p.ok);
                    ui.label(RichText::new("Agent running").strong());
                    if s.locked {
                        ui.label(RichText::new("PC locked").small().color(p.muted));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("v{}", s.version))
                                .small()
                                .color(p.muted),
                        );
                    });
                });
                ui.label(
                    RichText::new(host_only(&s.address))
                        .monospace()
                        .color(p.muted),
                );
            }
            (None, err) => {
                ui.horizontal(|ui| {
                    dot(ui, p.danger);
                    ui.label(RichText::new("Agent not running").strong());
                });
                if self.loaded {
                    ui.label(RichText::new(err.clone().unwrap_or_default()).color(p.muted));
                }
                ui.horizontal(|ui| {
                    if ui.add(primary("Start the agent")).clicked() {
                        self.start_note = Some(match std::process::Command::new("systemctl").args(["--user", "start", "zenvyd"]).status() {
                            Ok(s) if s.success() => "Starting…".into(),
                            _ => "That did not work. Run zenvyd serve in a terminal, or install the service with packaging/install.sh.".into(),
                        });
                        let _ = self.poke.send(());
                    }
                });
                if let Some(n) = &self.start_note {
                    ui.label(RichText::new(n.clone()).small().color(p.muted));
                }
            }
        });
    }

    fn home(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.status_card(ui);
        let p = palette(ui.visuals().dark_mode);
        let running = self.status.is_some();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Phones").strong().size(17.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(running, primary("Pair a phone")).clicked() {
                    self.start_pairing(ctx);
                }
            });
        });
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if running && self.devices.is_empty() {
                    card(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            ui.add_space(10.0);
                            ui.label(RichText::new("No phones yet").size(18.0).strong());
                            ui.label(RichText::new("Pair one to control this PC.").color(p.muted));
                            ui.add_space(10.0);
                        });
                    });
                }
                for d in self.devices.clone() {
                    self.phone_card(ui, ctx, &d);
                }
                ui.add_space(6.0);
            });
    }

    fn phone_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, d: &Device) {
        let p = palette(ui.visuals().dark_mode);
        card(ui, |ui| {
            ui.horizontal(|ui| {
                dot(ui, if d.online { p.ok } else { p.line });
                ui.label(RichText::new(&d.name).strong().size(16.5));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(RichText::new("Remove").color(p.danger)).frame(false),
                        )
                        .clicked()
                    {
                        self.confirm_remove = Some(d.id.clone());
                    }
                });
            });
            let seen = if d.online {
                "Connected now".to_string()
            } else {
                d.last_seen
                    .map(|t| format!("Last seen {}", ago(now_secs().saturating_sub(t))))
                    .unwrap_or_else(|| "Never connected".into())
            };
            ui.label(RichText::new(seen).small().color(p.muted));

            if self.confirm_remove.as_deref() == Some(d.id.as_str()) {
                ui.separator();
                ui.label(format!("Remove {}? It will have to pair again.", d.name));
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Button::new(RichText::new("Remove").color(Color32::WHITE))
                                .fill(p.danger)
                                .corner_radius(CornerRadius::same(20)),
                        )
                        .clicked()
                    {
                        let id = d.id.clone();
                        self.confirm_remove = None;
                        self.act(ctx, move || {
                            admin::revoke(&id).map(|_| "Removed".to_string())
                        });
                    }
                    if ui.add(quiet(ui, "Cancel")).clicked() {
                        self.confirm_remove = None;
                    }
                });
            }

            ui.add_space(4.0);
            ui.label(RichText::new("Allowed").small().color(p.muted));
            ui.horizontal_wrapped(|ui| {
                for (scope, label, help, sensitive) in SCOPES {
                    let on = d.scopes.iter().any(|s| s == scope);
                    let (fill, text, stroke) = if on {
                        (ACCENT, Color32::WHITE, Stroke::NONE)
                    } else {
                        (Color32::TRANSPARENT, p.text, Stroke::new(1.0, p.line))
                    };
                    let b = egui::Button::new(RichText::new(*label).color(text))
                        .fill(fill)
                        .stroke(stroke)
                        .corner_radius(CornerRadius::same(16))
                        .min_size(Vec2::new(0.0, 32.0));
                    if ui.add(b).on_hover_text(*help).clicked() {
                        if on {
                            let (id, sc) = (d.id.clone(), scope.to_string());
                            self.act(ctx, move || {
                                admin::set_scope(&id, &sc, false).map(|_| String::new())
                            });
                        } else if *sensitive {
                            self.confirm_grant = Some((d.id.clone(), scope.to_string()));
                        } else {
                            let (id, sc) = (d.id.clone(), scope.to_string());
                            self.act(ctx, move || {
                                admin::set_scope(&id, &sc, true).map(|_| String::new())
                            });
                        }
                    }
                }
            });
            if let Some((id, scope)) = self.confirm_grant.clone() {
                if id == d.id {
                    let (_, label, help, _) = SCOPES
                        .iter()
                        .find(|s| s.0 == scope)
                        .copied()
                        .unwrap_or(("", "", "", false));
                    ui.separator();
                    ui.label(format!("Let {} use {}? {}.", d.name, label, help));
                    ui.horizontal(|ui| {
                        if ui.add(primary("Allow")).clicked() {
                            self.confirm_grant = None;
                            self.act(ctx, move || {
                                admin::set_scope(&id, &scope, true).map(|_| String::new())
                            });
                        }
                        if ui.add(quiet(ui, "Cancel")).clicked() {
                            self.confirm_grant = None;
                        }
                    });
                }
            }
            ui.label(
                RichText::new("Always on: touchpad, keyboard, windows, volume and brightness.")
                    .small()
                    .color(p.muted),
            );
        });
    }

    fn pairing(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = palette(ui.visuals().dark_mode);
        let mut back = false;
        let mut approve: Option<bool> = None;
        let mut copy: Option<String> = None;
        if let View::Pair(st) = &self.view {
            ui.label(RichText::new("Pair a phone").size(18.0).strong());
            card(ui, |ui| {
                if let Some(e) = &st.error {
                    ui.label(RichText::new(e.clone()).color(p.danger));
                } else if let Some(r) = &st.result {
                    let (title, sub) = match r.as_str() {
                        "approved" => ("Paired", "The phone can now control this PC."),
                        "denied" => ("Not paired", "You did not approve the phone."),
                        "expired" => ("The code expired", "Start again to get a new one."),
                        _ => ("Cancelled", ""),
                    };
                    ui.vertical_centered(|ui| {
                        ui.add_space(8.0);
                        ui.label(RichText::new(title).size(20.0).strong());
                        if !sub.is_empty() {
                            ui.label(RichText::new(sub).color(p.muted));
                        }
                        ui.add_space(8.0);
                    });
                } else if let Some((name, code)) = &st.approval {
                    ui.vertical_centered(|ui| {
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(format!("{name} wants to pair"))
                                .size(18.0)
                                .strong(),
                        );
                        ui.label(
                            RichText::new("Approve only if the phone shows this code")
                                .color(p.muted),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(
                                code.chars().map(String::from).collect::<Vec<_>>().join(" "),
                            )
                            .size(44.0)
                            .strong()
                            .monospace(),
                        );
                        ui.add_space(6.0);
                    });
                    ui.horizontal(|ui| {
                        if ui.add(primary("Approve")).clicked() {
                            approve = Some(true);
                        }
                        if ui.add(quiet(ui, "Deny")).clicked() {
                            approve = Some(false);
                        }
                    });
                } else if let Some((n, modules)) = &st.qr {
                    let px = (300.0 / (*n as f32 + 8.0)).floor().max(3.0);
                    let side = px * (*n as f32 + 8.0);
                    ui.vertical_centered(|ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(Vec2::splat(side), egui::Sense::hover());
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(14), Color32::WHITE);
                        for y in 0..*n {
                            for x in 0..*n {
                                if modules[y * n + x] {
                                    let o = rect.min
                                        + Vec2::new((x as f32 + 4.0) * px, (y as f32 + 4.0) * px);
                                    ui.painter().rect_filled(
                                        egui::Rect::from_min_size(o, Vec2::splat(px)),
                                        0.0,
                                        Color32::BLACK,
                                    );
                                }
                            }
                        }
                    });
                    ui.add_space(4.0);
                    ui.label(RichText::new("1  Open the Zenvy app on your phone").color(p.text));
                    ui.label(RichText::new("2  Tap Scan QR code and point it here").color(p.text));
                    if let Some((url, ttl)) = &st.link {
                        let left = ttl.saturating_sub(st.started.elapsed().as_secs());
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("Valid for {}:{:02}", left / 60, left % 60))
                                    .small()
                                    .color(p.muted),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                RichText::new("Copy link").small().color(ACCENT),
                                            )
                                            .frame(false),
                                        )
                                        .on_hover_text(
                                            "For a phone that cannot scan; the link works once",
                                        )
                                        .clicked()
                                    {
                                        copy = Some(url.clone());
                                    }
                                },
                            );
                        });
                    }
                } else {
                    ui.vertical_centered(|ui| {
                        ui.add_space(24.0);
                        ui.label(RichText::new("Starting…").color(p.muted));
                        ui.add_space(24.0);
                    });
                }
            });
            let finished = st.result.is_some() || st.error.is_some();
            if ui
                .add(quiet(ui, if finished { "Done" } else { "Cancel" }))
                .clicked()
            {
                back = true;
            }
        }
        if let Some(a) = approve {
            if let View::Pair(st) = &mut self.view {
                st.pairing.answer(a);
                st.approval = None;
            }
        }
        if let Some(url) = copy {
            ctx.copy_text(url);
            self.say("Link copied");
        }
        if back {
            self.view = View::Home; // dropping the session closes the socket, which cancels pairing on the agent
            let _ = self.poke.send(());
        }
    }
}

fn main() -> eframe::Result {
    let icon = load_png(LOGO).map(|(rgba, width, height)| egui::IconData {
        rgba,
        width,
        height,
    });
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Zenvy")
        .with_app_id("zenvy")
        .with_inner_size([460.0, 680.0])
        .with_min_inner_size([380.0, 480.0]);
    if let Some(i) = icon {
        viewport = viewport.with_icon(i);
    }
    eframe::run_native(
        "Zenvy",
        eframe::NativeOptions {
            viewport,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(ZenvyApp::new(cc)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_and_addresses_read_naturally() {
        assert_eq!(ago(5), "just now");
        assert_eq!(ago(300), "5 min ago");
        assert_eq!(ago(7300), "2 h ago");
        assert_eq!(ago(200_000), "2 d ago");
        assert_eq!(host_only("https://192.168.1.20:7878"), "192.168.1.20:7878");
        assert_eq!(host_only("192.168.1.20:7878/"), "192.168.1.20:7878");
    }

    #[test]
    fn a_pairing_link_becomes_a_square_qr_code() {
        let (n, m) = qr_modules("https://192.168.1.20:7878/#s=abcdefghijklmnop&fp=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef").unwrap();
        assert!(n >= 21 && m.len() == n * n && m.iter().any(|&b| b) && m.iter().any(|&b| !b));
    }

    #[test]
    fn the_logo_decodes() {
        let (rgba, w, h) = load_png(LOGO).expect("logo");
        assert!(w > 0 && h > 0 && rgba.len() == (w * h * 4) as usize);
    }

    #[test]
    fn every_permission_the_window_offers_is_one_the_agent_knows() {
        for (scope, ..) in SCOPES {
            assert!(zenvy_proto::valid_scope(scope), "{scope}");
        }
        // the basics are not switchable here
        for basic in ["view", "input", "windows", "system"] {
            assert!(!SCOPES.iter().any(|s| s.0 == basic));
        }
    }
}
