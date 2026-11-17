//! `zenvy`: a small desktop window for the Zenvy agent. Pair a phone with a QR code, see who is connected, and switch each
//! phone's permissions on or off. It is only a front end to the agent's local admin socket (the same interface as the
//! `zenvyd` command line), so it has no power of its own and nothing here is reachable from the network.
//! The look follows the Android app: the same colour tokens, tonal cards, pill buttons and line icons.

mod admin;
mod icons;
mod theme;

use admin::{Device, PairEvent, Pairing, Status, Twin};
use eframe::egui::{
    self, Align, Color32, CornerRadius, Layout, Margin, Rect, RichText, Sense, Theme, Vec2,
};
use icons::Icon;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};
use theme::{bold, palette, Palette, ON_PRIMARY, PRIMARY};

const LOGO: &[u8] = include_bytes!("../../../assets/logo/zenvy-icon-512.png");

/// (permission, label, icon, what it allows, asks for confirmation before it is switched on)
const SCOPES: &[(&str, &str, Icon, &str, bool)] = &[
    (
        "screen",
        "Screen",
        Icon::Screen,
        "See the PC screen and click through it",
        false,
    ),
    (
        "launch",
        "Apps",
        Icon::Apps,
        "Open installed apps, on a new workspace if you like",
        false,
    ),
    (
        "listen",
        "Speaker",
        Icon::Speaker,
        "Hear the PC's sound on the phone",
        false,
    ),
    (
        "clipboard",
        "Clipboard",
        Icon::Clipboard,
        "Copy text between the phone and the PC",
        false,
    ),
    (
        "files",
        "Files",
        Icon::File,
        "Send files to and from the PC",
        false,
    ),
    (
        "power",
        "Power",
        Icon::Power,
        "Lock, sleep, restart and shut down the PC",
        true,
    ),
    (
        "unlock",
        "Unlock",
        Icon::Unlock,
        "Type your password on the lock screen",
        true,
    ),
    (
        "exec",
        "Actions",
        Icon::Bolt,
        "Run the actions from your config file",
        true,
    ),
];

enum Msg {
    Poll(Result<(Status, Vec<Device>), String>),
    /// The result of a change the person asked for: `Err` is shown as a notice.
    Action(Result<String, String>),
}

struct PairState {
    pairing: Pairing,
    link: Option<(String, u64)>,
    qr: Option<(usize, Vec<bool>)>,
    approval: Option<(String, String, Vec<Twin>)>,
    /// Replace the earlier record of the same phone (only offered when exactly one exists).
    replace: bool,
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

// ---------------------------------------------------------------------------------------------------------- widgets

#[derive(Clone, Copy, PartialEq)]
enum Look {
    Primary,
    Tonal,
    Danger,
}

/// A fully rounded 46 px button, like the Android app's.
fn pill(ui: &mut egui::Ui, label: &str, look: Look, full: bool) -> egui::Response {
    let p = palette(ui.visuals().dark_mode);
    let (fill, fg) = match look {
        Look::Primary => (PRIMARY, ON_PRIMARY),
        Look::Tonal => (p.secondary, p.on_surface),
        Look::Danger => (
            p.error,
            if ui.visuals().dark_mode {
                Color32::BLACK
            } else {
                Color32::WHITE
            },
        ),
    };
    let b = egui::Button::new(RichText::new(label).family(bold()).size(15.0).color(fg))
        .fill(fill)
        .corner_radius(CornerRadius::same(23));
    if full {
        ui.add_sized([ui.available_width(), 46.0], b)
    } else {
        ui.add(b.min_size(Vec2::new(0.0, 46.0)))
    }
}

/// A tonal rounded block.
fn card(ui: &mut egui::Ui, fill: Color32, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(fill)
        .corner_radius(CornerRadius::same(22))
        .inner_margin(Margin::same(18))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        });
}

/// A round tonal background with an icon in it.
fn disc(ui: &mut egui::Ui, size: f32, fill: Color32, icon: Icon, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    ui.painter().circle_filled(rect.center(), size / 2.0, fill);
    icons::paint(
        ui.painter(),
        Rect::from_center_size(rect.center(), Vec2::splat(size * 0.5)),
        icon,
        color,
    );
}

/// A small monospace pill for an address.
fn address_chip(ui: &mut egui::Ui, text: &str, p: &Palette) {
    egui::Frame::new()
        .fill(p.container_high)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.label(
                RichText::new(text)
                    .monospace()
                    .size(13.5)
                    .color(p.on_surface),
            );
        });
}

/// A checkbox row in the same style as the chips. Returns true when it was clicked.
fn toggle_row(ui: &mut egui::Ui, on: bool, label: &str, p: &Palette) -> bool {
    let galley = ui.painter().layout(
        label.to_string(),
        egui::FontId::proportional(14.5),
        p.on_surface,
        ui.available_width() - 44.0,
    );
    let size = Vec2::new(ui.available_width(), galley.size().y.max(24.0) + 16.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let fill = if resp.hovered() {
        p.container_high
    } else {
        p.container
    };
    ui.painter().rect_filled(rect, CornerRadius::same(16), fill);
    let boxr = Rect::from_min_size(
        egui::pos2(rect.min.x + 12.0, rect.center().y - 11.0),
        Vec2::splat(22.0),
    );
    ui.painter().rect_filled(
        boxr,
        CornerRadius::same(7),
        if on { PRIMARY } else { p.secondary },
    );
    if on {
        icons::paint(ui.painter(), boxr.shrink(3.0), Icon::Check, ON_PRIMARY);
    }
    ui.painter().galley(
        egui::pos2(rect.min.x + 44.0, rect.center().y - galley.size().y / 2.0),
        galley,
        p.on_surface,
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// A toggle that reads like the Android app's tonal chips: filled with the accent and a check when it is on.
fn chip(ui: &mut egui::Ui, label: &str, icon: Icon, on: bool, p: &Palette) -> egui::Response {
    let font = egui::FontId::new(14.5, bold());
    let color = if on { ON_PRIMARY } else { p.on_surface };
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, color);
    let size = Vec2::new(14.0 + 18.0 + 7.0 + galley.size().x + 14.0, 36.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let fill = if on {
        PRIMARY
    } else if resp.hovered() {
        p.container_high
    } else {
        p.secondary
    };
    ui.painter().rect_filled(rect, CornerRadius::same(18), fill);
    let icon_rect = Rect::from_min_size(
        egui::pos2(rect.min.x + 14.0, rect.center().y - 9.0),
        Vec2::splat(18.0),
    );
    icons::paint(
        ui.painter(),
        icon_rect,
        if on { Icon::Check } else { icon },
        if on { ON_PRIMARY } else { p.variant },
    );
    ui.painter().galley(
        egui::pos2(
            rect.min.x + 14.0 + 18.0 + 7.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        color,
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

// ---------------------------------------------------------------------------------------------------------- app

impl ZenvyApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx
            .set_visuals_of(Theme::Light, theme::visuals(false));
        cc.egui_ctx
            .set_visuals_of(Theme::Dark, theme::visuals(true));
        theme::install_fonts(&cc.egui_ctx);
        cc.egui_ctx.all_styles_mut(|s| {
            s.spacing.item_spacing = Vec2::new(10.0, 10.0);
            s.spacing.button_padding = Vec2::new(18.0, 10.0);
            s.spacing.scroll.bar_width = 6.0;
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
                    PairEvent::Approval {
                        name,
                        code,
                        same_name,
                    } => {
                        // the usual cause of a twin is the same phone paired again: replace it unless it is connected right now
                        p.replace = matches!(same_name.as_slice(), [t] if !t.online);
                        p.approval = Some((name, code, same_name));
                    }
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
                    replace: false,
                    result: None,
                    error: None,
                    started: Instant::now(),
                }))
            }
            Err(e) => self.say(e),
        }
    }
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
            .frame(egui::Frame::new().fill(p.surface).inner_margin(Margin {
                left: 24,
                right: 24,
                top: 20,
                bottom: 20,
            }))
            .show(ui, |ui| {
                self.header(ui, &p);
                ui.add_space(8.0);
                match &self.view {
                    View::Home => self.home(ui, &ctx, &p),
                    View::Pair(_) => self.pairing(ui, &ctx, &p),
                }
                if let Some((text, _)) = &self.notice {
                    ui.add_space(6.0);
                    card(ui, p.container_high, |ui| {
                        ui.label(RichText::new(text.clone()).size(14.5));
                    });
                }
            });
    }
}

impl ZenvyApp {
    fn header(&self, ui: &mut egui::Ui, p: &Palette) {
        ui.horizontal(|ui| {
            ui.add(
                egui::Image::new((self.logo.id(), Vec2::splat(38.0)))
                    .corner_radius(CornerRadius::same(10)),
            );
            ui.add_space(4.0);
            ui.label(RichText::new("Zenvy").family(bold()).size(27.0));
            if let Some(s) = &self.status {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("v{}", s.version))
                            .size(12.5)
                            .color(p.variant),
                    );
                });
            }
        });
    }

    fn status_card(&mut self, ui: &mut egui::Ui, p: &Palette) {
        card(ui, p.container, |ui| match (&self.status, &self.error) {
            (Some(s), _) => {
                ui.horizontal(|ui| {
                    disc(ui, 46.0, p.secondary, Icon::Check, PRIMARY);
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 5.0;
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Agent running").family(bold()).size(16.5));
                            if s.locked {
                                ui.label(RichText::new("PC locked").size(12.5).color(p.variant));
                            }
                        });
                        address_chip(ui, host_only(&s.address), p);
                    });
                });
            }
            (None, err) => {
                ui.horizontal(|ui| {
                    disc(ui, 46.0, p.secondary, Icon::WifiOff, p.error);
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new("Agent not running").family(bold()).size(16.5));
                        if self.loaded {
                            ui.label(
                                RichText::new(err.clone().unwrap_or_default())
                                    .size(13.5)
                                    .color(p.variant),
                            );
                        }
                    });
                });
                ui.add_space(2.0);
                if pill(ui, "Start the agent", Look::Primary, true).clicked() {
                    self.start_note = Some(match std::process::Command::new("systemctl").args(["--user", "start", "zenvyd"]).status() {
                        Ok(s) if s.success() => "Starting…".into(),
                        _ => "That did not work. Run zenvyd serve in a terminal, or install the service with packaging/install.sh.".into(),
                    });
                    let _ = self.poke.send(());
                }
                if let Some(n) = &self.start_note {
                    ui.label(RichText::new(n.clone()).size(13.0).color(p.variant));
                }
            }
        });
    }

    fn home(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, p: &Palette) {
        self.status_card(ui, p);
        ui.add_space(6.0);
        let running = self.status.is_some();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Phones").family(bold()).size(18.0));
            if !self.devices.is_empty() {
                ui.label(
                    RichText::new(self.devices.len().to_string())
                        .size(14.0)
                        .color(p.variant),
                );
            }
        });
        let room = (ui.available_height() - 62.0).max(80.0);
        egui::ScrollArea::vertical()
            .max_height(room)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if running && self.devices.is_empty() {
                    card(ui, p.container, |ui| {
                        ui.vertical_centered(|ui| {
                            ui.add_space(14.0);
                            disc(ui, 64.0, p.secondary, Icon::Phone, PRIMARY);
                            ui.add_space(6.0);
                            ui.label(RichText::new("No phones yet").family(bold()).size(18.0));
                            ui.label(
                                RichText::new("Pair one to control this PC.").color(p.variant),
                            );
                            ui.add_space(14.0);
                        });
                    });
                }
                for d in self.devices.clone() {
                    self.phone_card(ui, ctx, &d, p);
                }
            });
        ui.add_space(4.0);
        if ui
            .add_enabled_ui(running, |ui| pill(ui, "Pair a phone", Look::Primary, true))
            .inner
            .clicked()
        {
            self.start_pairing(ctx);
        }
    }

    fn phone_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, d: &Device, p: &Palette) {
        // two phones with the same model name (an old record and a new pairing) are told apart by when they paired
        let twin = self.devices.iter().filter(|x| x.name == d.name).count() > 1;
        card(ui, p.container, |ui| {
            ui.horizontal(|ui| {
                disc(ui, 46.0, p.secondary, Icon::Phone, PRIMARY);
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    ui.label(RichText::new(&d.name).family(bold()).size(17.0));
                    ui.horizontal(|ui| {
                        status_dot(ui, if d.online { p.ok } else { p.outline });
                        let seen = if d.online {
                            "Connected now".to_string()
                        } else {
                            d.last_seen
                                .map(|t| format!("Last seen {}", ago(now_secs().saturating_sub(t))))
                                .unwrap_or_else(|| "Never connected".into())
                        };
                        let seen = match (twin, d.created_at) {
                            (true, Some(c)) => {
                                format!("{seen}  ·  paired {}", ago(now_secs().saturating_sub(c)))
                            }
                            _ => seen,
                        };
                        ui.label(RichText::new(seen).size(13.0).color(p.variant));
                    });
                });
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    if ui
                        .add(
                            egui::Button::new(RichText::new("Remove").size(14.0).color(p.error))
                                .frame(false),
                        )
                        .clicked()
                    {
                        self.confirm_remove = Some(d.id.clone());
                    }
                });
            });

            if self.confirm_remove.as_deref() == Some(d.id.as_str()) {
                card(ui, p.container_high, |ui| {
                    ui.label(format!("Remove {}? It will have to pair again.", d.name));
                    ui.horizontal(|ui| {
                        if pill(ui, "Remove", Look::Danger, false).clicked() {
                            let id = d.id.clone();
                            self.confirm_remove = None;
                            self.act(ctx, move || {
                                admin::revoke(&id).map(|_| "Removed".to_string())
                            });
                        }
                        if pill(ui, "Cancel", Look::Tonal, false).clicked() {
                            self.confirm_remove = None;
                        }
                    });
                });
            }

            ui.add_space(4.0);
            ui.label(RichText::new("Allowed").size(13.0).color(p.variant));
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                for (scope, label, icon, help, sensitive) in SCOPES {
                    let on = d.scopes.iter().any(|s| s == scope);
                    if chip(ui, label, *icon, on, p).on_hover_text(*help).clicked() {
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
                    let (_, label, _, help, _) = SCOPES
                        .iter()
                        .find(|s| s.0 == scope)
                        .copied()
                        .unwrap_or(("", "", Icon::Check, "", false));
                    card(ui, p.container_high, |ui| {
                        ui.label(format!("Let {} use {}? {}.", d.name, label, help));
                        ui.horizontal(|ui| {
                            if pill(ui, "Allow", Look::Primary, false).clicked() {
                                self.confirm_grant = None;
                                self.act(ctx, move || {
                                    admin::set_scope(&id, &scope, true).map(|_| String::new())
                                });
                            }
                            if pill(ui, "Cancel", Look::Tonal, false).clicked() {
                                self.confirm_grant = None;
                            }
                        });
                    });
                }
            }
            ui.label(
                RichText::new("Always on: touchpad, keyboard, windows, volume and brightness.")
                    .size(12.5)
                    .color(p.variant),
            );
        });
    }

    fn pairing(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, p: &Palette) {
        let mut back = false;
        let mut approve: Option<bool> = None;
        let mut copy: Option<String> = None;
        let mut flip = false;
        if let View::Pair(st) = &self.view {
            ui.label(RichText::new("Pair a phone").family(bold()).size(22.0));
            let finished = st.result.is_some() || st.error.is_some();
            egui::ScrollArea::vertical()
                .auto_shrink([false, true])
                .max_height((ui.available_height() - 62.0).max(120.0))
                .show(ui, |ui| {
                    if let Some(e) = &st.error {
                        card(ui, p.container, |ui| {
                            ui.vertical_centered(|ui| {
                                ui.add_space(10.0);
                                disc(ui, 64.0, p.secondary, Icon::WifiOff, p.error);
                                ui.add_space(6.0);
                                ui.label(RichText::new(e.clone()).color(p.variant));
                                ui.add_space(10.0);
                            });
                        });
                    } else if let Some(r) = &st.result {
                        let (title, sub, icon, tint) = match r.as_str() {
                            "approved" => (
                                "Paired",
                                "The phone can now control this PC.",
                                Icon::Check,
                                PRIMARY,
                            ),
                            "denied" => (
                                "Not paired",
                                "You did not approve the phone.",
                                Icon::Shield,
                                p.error,
                            ),
                            "expired" => (
                                "The code expired",
                                "Start again to get a new one.",
                                Icon::WifiOff,
                                p.variant,
                            ),
                            _ => ("Cancelled", "", Icon::Shield, p.variant),
                        };
                        card(ui, p.container, |ui| {
                            ui.vertical_centered(|ui| {
                                ui.add_space(14.0);
                                disc(ui, 72.0, p.secondary, icon, tint);
                                ui.add_space(8.0);
                                ui.label(RichText::new(title).family(bold()).size(21.0));
                                if !sub.is_empty() {
                                    ui.label(RichText::new(sub).color(p.variant));
                                }
                                ui.add_space(14.0);
                            });
                        });
                    } else if let Some((name, code, twins)) = &st.approval {
                        card(ui, p.container, |ui| {
                            ui.vertical_centered(|ui| {
                                ui.add_space(8.0);
                                disc(ui, 56.0, p.secondary, Icon::Phone, PRIMARY);
                                ui.add_space(4.0);
                                ui.label(
                                    RichText::new(format!("{name} wants to pair"))
                                        .family(bold())
                                        .size(19.0),
                                );
                                ui.label(
                                    RichText::new("Approve only if the phone shows this code")
                                        .color(p.variant),
                                );
                                ui.add_space(8.0);
                                ui.horizontal(|ui| {
                                    let n = code.chars().count() as f32;
                                    ui.add_space(
                                        ((ui.available_width() - n * 60.0 - (n - 1.0) * 8.0) / 2.0)
                                            .max(0.0),
                                    );
                                    ui.spacing_mut().item_spacing.x = 8.0;
                                    for ch in code.chars() {
                                        let (rect, _) = ui.allocate_exact_size(
                                            Vec2::new(60.0, 74.0),
                                            Sense::hover(),
                                        );
                                        ui.painter().rect_filled(
                                            rect,
                                            CornerRadius::same(16),
                                            p.container_high,
                                        );
                                        ui.painter().text(
                                            rect.center(),
                                            egui::Align2::CENTER_CENTER,
                                            ch,
                                            egui::FontId::new(38.0, bold()),
                                            p.on_surface,
                                        );
                                    }
                                });
                                ui.add_space(8.0);
                            });
                        });
                        ui.add_space(4.0);
                        if let [twin] = twins.as_slice() {
                            let seen = if twin.online {
                                "connected now".to_string()
                            } else {
                                twin.last_seen
                                    .map(|t| {
                                        format!("last seen {}", ago(now_secs().saturating_sub(t)))
                                    })
                                    .unwrap_or_else(|| "never seen".into())
                            };
                            let label = format!(
                                "Replace the earlier {name} ({seen}) and keep its permissions"
                            );
                            if toggle_row(ui, st.replace, &label, p) {
                                flip = true;
                            }
                            ui.add_space(2.0);
                        }
                        if pill(ui, "Approve", Look::Primary, true).clicked() {
                            approve = Some(true);
                        }
                        if pill(ui, "Deny", Look::Tonal, true).clicked() {
                            approve = Some(false);
                        }
                    } else if let Some((n, modules)) = &st.qr {
                        let px = (252.0 / (*n as f32 + 6.0)).floor().max(3.0);
                        let side = px * (*n as f32 + 6.0);
                        ui.vertical_centered(|ui| {
                            let (rect, _) =
                                ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
                            ui.painter()
                                .rect_filled(rect, CornerRadius::same(22), Color32::WHITE);
                            for y in 0..*n {
                                for x in 0..*n {
                                    if modules[y * n + x] {
                                        let o = rect.min
                                            + Vec2::new(
                                                (x as f32 + 3.0) * px,
                                                (y as f32 + 3.0) * px,
                                            );
                                        ui.painter().rect_filled(
                                            Rect::from_min_size(o, Vec2::splat(px)),
                                            0.0,
                                            Color32::BLACK,
                                        );
                                    }
                                }
                            }
                        });
                        ui.add_space(4.0);
                        for (i, text) in [
                            "Open the Zenvy app on your phone",
                            "Tap Scan QR code and point it here",
                        ]
                        .iter()
                        .enumerate()
                        {
                            ui.horizontal(|ui| {
                                let (rect, _) =
                                    ui.allocate_exact_size(Vec2::splat(30.0), Sense::hover());
                                ui.painter().circle_filled(rect.center(), 15.0, p.secondary);
                                ui.painter().text(
                                    rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    i + 1,
                                    egui::FontId::new(14.5, bold()),
                                    PRIMARY,
                                );
                                ui.add_space(4.0);
                                ui.label(RichText::new(*text).size(15.5));
                            });
                        }
                        if let Some((url, ttl)) = &st.link {
                            let left = ttl.saturating_sub(st.started.elapsed().as_secs());
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!(
                                        "Valid for {}:{:02}",
                                        left / 60,
                                        left % 60
                                    ))
                                    .size(13.0)
                                    .color(p.variant),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                RichText::new("Copy link")
                                                    .size(13.5)
                                                    .color(PRIMARY),
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
                                });
                            });
                        }
                    } else {
                        card(ui, p.container, |ui| {
                            ui.vertical_centered(|ui| {
                                ui.add_space(28.0);
                                ui.label(RichText::new("Starting…").color(p.variant));
                                ui.add_space(28.0);
                            });
                        });
                    }
                });
            if st.approval.is_none() || finished {
                ui.add_space(4.0);
                if pill(
                    ui,
                    if finished { "Done" } else { "Cancel" },
                    Look::Tonal,
                    true,
                )
                .clicked()
                {
                    back = true;
                }
            }
        }
        if flip {
            if let View::Pair(st) = &mut self.view {
                st.replace = !st.replace;
            }
        }
        if let Some(a) = approve {
            if let View::Pair(st) = &mut self.view {
                let twin = match st.approval.as_ref().map(|(_, _, t)| t.as_slice()) {
                    Some([t]) if st.replace && a => Some(t.id.clone()),
                    _ => None,
                };
                st.pairing.answer(a, twin);
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
        .with_inner_size([460.0, 700.0])
        .with_min_inner_size([400.0, 520.0]);
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

    #[test]
    fn the_colours_match_the_android_app() {
        // res/values/colors.xml and values-night/colors.xml
        let l = palette(false);
        assert_eq!(
            (l.surface, l.on_surface),
            (
                Color32::from_rgb(0xF5, 0xF5, 0xF7),
                Color32::from_rgb(0x1D, 0x1D, 0x1F)
            )
        );
        let d = palette(true);
        assert_eq!(
            (d.surface, d.container, d.variant),
            (
                Color32::from_rgb(0x1D, 0x1D, 0x1F),
                Color32::from_rgb(0x2B, 0x2B, 0x2D),
                Color32::from_rgb(0xAA, 0xAA, 0xAA)
            )
        );
        assert_eq!(PRIMARY, Color32::from_rgb(0x00, 0x7A, 0xFF));
    }
}
