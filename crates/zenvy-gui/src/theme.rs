//! The same colour tokens as the Android app (res/values and values-night) and the phone UI: a graphite and white ramp, one
//! accent (#007AFF), tonal fills instead of outlines.

use eframe::egui::{self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, Visuals};

pub const PRIMARY: Color32 = Color32::from_rgb(0x00, 0x7A, 0xFF);
pub const ON_PRIMARY: Color32 = Color32::WHITE;

pub struct Palette {
    pub surface: Color32,
    pub container: Color32,
    pub container_high: Color32,
    pub secondary: Color32,
    pub on_surface: Color32,
    pub variant: Color32,
    pub outline: Color32,
    pub error: Color32,
    pub ok: Color32,
}

const fn rgb(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

pub fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            surface: rgb(0x1D1D1F),
            container: rgb(0x2B2B2D),
            container_high: rgb(0x343437),
            secondary: rgb(0x38383B),
            on_surface: rgb(0xF5F5F7),
            variant: rgb(0xAAAAAA),
            outline: rgb(0x7C7C80),
            error: rgb(0xFF6B63),
            ok: rgb(0x30D158),
        }
    } else {
        Palette {
            surface: rgb(0xF5F5F7),
            container: rgb(0xEEEEF1),
            container_high: rgb(0xE7E7EB),
            secondary: rgb(0xE6E6EA),
            on_surface: rgb(0x1D1D1F),
            variant: rgb(0x636368),
            outline: rgb(0x8E8E93),
            error: rgb(0xD70015),
            ok: rgb(0x1FA83A),
        }
    }
}

pub fn visuals(dark: bool) -> Visuals {
    let p = palette(dark);
    let mut v = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    v.panel_fill = p.surface;
    v.window_fill = p.container;
    v.extreme_bg_color = p.container;
    v.override_text_color = Some(p.on_surface);
    v.selection.bg_fill = PRIMARY;
    v.hyperlink_color = PRIMARY;
    v.widgets.noninteractive.bg_stroke = egui::Stroke::NONE;
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
    ] {
        w.corner_radius = CornerRadius::same(24);
        w.bg_stroke = egui::Stroke::NONE;
    }
    v.widgets.inactive.bg_fill = p.secondary;
    v.widgets.inactive.weak_bg_fill = p.secondary;
    v
}

pub fn bold() -> FontFamily {
    FontFamily::Name("bold".into())
}

fn system_font(pattern: &str) -> Option<Vec<u8>> {
    let out = std::process::Command::new("fc-match")
        .args(["-f", "%{file}", pattern])
        .output()
        .ok()?;
    let path = String::from_utf8(out.stdout).ok()?;
    let path = path.trim();
    // egui reads single-face TrueType and OpenType files, not collections
    if !out.status.success() || !(path.ends_with(".ttf") || path.ends_with(".otf")) {
        return None;
    }
    std::fs::read(path).ok()
}

/// Uses the system's sans-serif (regular and bold) so the text looks like the rest of the desktop and like the phone
/// app; if it cannot be found egui's built-in fonts stay.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let regular = system_font("sans-serif");
    let heavy = system_font("sans-serif:bold");
    if let Some(r) = &regular {
        fonts.font_data.insert(
            "system".into(),
            std::sync::Arc::new(FontData::from_owned(r.clone())),
        );
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "system".into());
    }
    let mut bold_chain: Vec<String> = Vec::new();
    if let Some(b) = heavy.or(regular) {
        fonts.font_data.insert(
            "system-bold".into(),
            std::sync::Arc::new(FontData::from_owned(b)),
        );
        bold_chain.push("system-bold".into());
    }
    bold_chain.extend(
        fonts
            .families
            .get(&FontFamily::Proportional)
            .cloned()
            .unwrap_or_default(),
    );
    fonts.families.insert(bold(), bold_chain);
    ctx.set_fonts(fonts);
}
