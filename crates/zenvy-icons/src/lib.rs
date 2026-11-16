//! Finds the icon of an application from its window class, the way a launcher does:
//! window class -> `.desktop` entry (by file name or `StartupWMClass`) -> `Icon=` -> an icon file in the icon theme.
//!
//! Only plain PNG and SVG files are ever returned (checked by content, not by name), capped in size, and a
//! window class can only select a *desktop entry*: it is never used as a path.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const MAX_ICON_BYTES: u64 = 128 * 1024;
const MAX_DESKTOP_BYTES: u64 = 32 * 1024;
const MAX_DESKTOP_FILES: usize = 6000;
const MAX_WALK_FILES: usize = 40_000;
/// The smallest pixel size we prefer for a PNG (the phone shows icons at ~28 dp, so 64 px is sharp enough).
const PREFERRED_PX: u32 = 64;

#[derive(Debug, PartialEq, Eq)]
pub struct Icon {
    pub mime: &'static str,
    pub bytes: Vec<u8>,
}

pub struct IconResolver {
    /// `.../applications` directories, most specific first.
    app_dirs: Vec<PathBuf>,
    /// Bases that contain icon themes (`<base>/<theme>/...`).
    icon_bases: Vec<PathBuf>,
    /// Plain-file icon directories (`/usr/share/pixmaps`).
    pixmap_dirs: Vec<PathBuf>,
    themes: Vec<String>,
    cache: Mutex<HashMap<String, Option<Arc<Icon>>>>,
}

/// A window class is only a lookup key: letters, digits and `._+-`, no leading dot.
pub fn valid_class(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && !s.starts_with('.')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c))
}

impl IconResolver {
    /// Uses the standard XDG locations and the user's GTK icon theme (falling back to Adwaita and hicolor).
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".local/share")));
        let mut data_dirs: Vec<PathBuf> = data_home.into_iter().collect();
        let sys = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
        let sys = if sys.is_empty() {
            "/usr/local/share:/usr/share".to_string()
        } else {
            sys
        };
        for d in sys.split(':').filter(|d| !d.is_empty()) {
            let d = PathBuf::from(d);
            if !data_dirs.contains(&d) {
                data_dirs.push(d);
            }
        }
        let mut themes = Vec::new();
        if let Some(h) = &home {
            for f in [
                ".config/gtk-3.0/settings.ini",
                ".config/gtk-4.0/settings.ini",
            ] {
                if let Some(t) = gtk_icon_theme(&h.join(f)) {
                    themes.push(t);
                }
            }
        }
        let mut r = Self::new(
            data_dirs,
            home.map(|h| vec![h.join(".icons")]).unwrap_or_default(),
            themes,
        );
        r.themes.dedup();
        r
    }

    /// `data_dirs` are XDG data directories; `extra_icon_bases` are additional theme bases such as `~/.icons`.
    pub fn new(
        data_dirs: Vec<PathBuf>,
        extra_icon_bases: Vec<PathBuf>,
        mut themes: Vec<String>,
    ) -> Self {
        for t in ["hicolor", "Adwaita"] {
            if !themes.iter().any(|x| x == t) {
                themes.push(t.to_string());
            }
        }
        Self {
            app_dirs: data_dirs.iter().map(|d| d.join("applications")).collect(),
            icon_bases: data_dirs
                .iter()
                .map(|d| d.join("icons"))
                .chain(extra_icon_bases)
                .collect(),
            pixmap_dirs: data_dirs.iter().map(|d| d.join("pixmaps")).collect(),
            themes,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Blocking (reads the file system). Results, including "not found", are cached.
    pub fn resolve(&self, class: &str) -> Option<Arc<Icon>> {
        if !valid_class(class) {
            return None;
        }
        let key = class.to_ascii_lowercase();
        if let Some(hit) = self.cache.lock().unwrap().get(&key) {
            return hit.clone();
        }
        let found = self.lookup(class).map(Arc::new);
        self.cache.lock().unwrap().insert(key, found.clone());
        found
    }

    fn lookup(&self, class: &str) -> Option<Icon> {
        let entry = self.find_desktop(class);
        // 1. the entry's own icon, 2. an icon named like the class (many apps ship one without a desktop entry)
        entry
            .and_then(|e| e.icon)
            .and_then(|name| self.icon_by_name(&name))
            .or_else(|| self.icon_by_name(class))
            .or_else(|| self.icon_by_name(&class.to_ascii_lowercase()))
    }

    fn find_desktop(&self, class: &str) -> Option<Desktop> {
        let want = class.to_ascii_lowercase();
        // exact and case-insensitive file name first (cheap), then StartupWMClass (scan)
        for dir in &self.app_dirs {
            let Ok(rd) = fs::read_dir(dir) else { continue };
            for e in rd.flatten().take(MAX_DESKTOP_FILES) {
                let n = e.file_name().to_string_lossy().to_ascii_lowercase();
                if n == format!("{want}.desktop") {
                    if let Some(d) = parse_desktop(&e.path()) {
                        return Some(d);
                    }
                }
            }
        }
        for dir in &self.app_dirs {
            let Ok(rd) = fs::read_dir(dir) else { continue };
            for e in rd.flatten().take(MAX_DESKTOP_FILES) {
                if e.file_name().to_string_lossy().ends_with(".desktop") {
                    if let Some(d) = parse_desktop(&e.path()) {
                        if d.wm_class
                            .as_deref()
                            .is_some_and(|c| c.eq_ignore_ascii_case(class))
                        {
                            return Some(d);
                        }
                    }
                }
            }
        }
        None
    }

    fn icon_by_name(&self, name: &str) -> Option<Icon> {
        if name.starts_with('/') {
            return load_icon(Path::new(name));
        }
        let name = name
            .strip_suffix(".png")
            .or_else(|| name.strip_suffix(".svg"))
            .or_else(|| name.strip_suffix(".xpm"))
            .unwrap_or(name);
        // a name is a bare token, never a path
        if name.is_empty()
            || name.len() > 120
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c))
            || name.starts_with('.')
        {
            return None;
        }
        for theme in &self.themes {
            let mut cands: Vec<(Rank, PathBuf)> = Vec::new();
            for base in &self.icon_bases {
                let root = base.join(theme);
                if root.is_dir() {
                    let mut budget = MAX_WALK_FILES;
                    walk(&root, 0, name, &mut cands, &mut budget);
                }
            }
            cands.sort_by_key(|c| c.0);
            for (_, p) in cands {
                if let Some(i) = load_icon(&p) {
                    return Some(i);
                }
            }
        }
        for dir in &self.pixmap_dirs {
            for ext in ["svg", "png"] {
                if let Some(i) = load_icon(&dir.join(format!("{name}.{ext}"))) {
                    return Some(i);
                }
            }
        }
        None
    }
}

struct Desktop {
    icon: Option<String>,
    wm_class: Option<String>,
}

fn parse_desktop(path: &Path) -> Option<Desktop> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_DESKTOP_BYTES {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
    let (mut in_entry, mut icon, mut wm_class) = (false, None, None);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
        } else if in_entry {
            if let Some(v) = line.strip_prefix("Icon=") {
                icon.get_or_insert_with(|| v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("StartupWMClass=") {
                wm_class.get_or_insert_with(|| v.trim().to_string());
            }
        }
    }
    Some(Desktop { icon, wm_class })
}

/// Lower is better: SVG first, then PNG sizes at or above the preferred size (smallest first), then smaller ones.
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug)]
struct Rank(u8, u32);

fn walk(dir: &Path, depth: usize, name: &str, out: &mut Vec<(Rank, PathBuf)>, budget: &mut usize) {
    if depth > 4 || *budget == 0 {
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let path = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            walk(&path, depth + 1, name, out, budget);
        } else if let Some(file) = path.file_name().and_then(|f| f.to_str()) {
            let rank = match file.strip_prefix(name) {
                Some(".svg") => Some(Rank(0, 0)),
                Some(".png") => {
                    let px = size_of_path(&path).unwrap_or(0);
                    Some(if px >= PREFERRED_PX {
                        Rank(1, px)
                    } else {
                        Rank(2, u32::MAX - px)
                    })
                }
                _ => None,
            };
            if let Some(r) = rank {
                out.push((r, path));
            }
        }
    }
}

/// `.../64x64/apps/x.png`, `.../64x64@2/apps/x.png` or `.../apps/64/x.png` -> 64
fn size_of_path(p: &Path) -> Option<u32> {
    p.components()
        .rev()
        .filter_map(|c| c.as_os_str().to_str())
        .find_map(|s| {
            let head = s.split('@').next()?;
            let num = head.split('x').next()?;
            (!num.is_empty()
                && num.chars().all(|c| c.is_ascii_digit())
                && (head == num || head.split('x').nth(1) == Some(num)))
            .then(|| num.parse().ok())
            .flatten()
        })
}

/// Reads an icon file, accepting only real PNG or SVG content within the size cap.
fn load_icon(path: &Path) -> Option<Icon> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_ICON_BYTES {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some(Icon {
            mime: "image/png",
            bytes,
        });
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]).to_string();
    let t = head.trim_start_matches('\u{feff}').trim_start();
    (t.starts_with('<') && head.contains("<svg")).then_some(Icon {
        mime: "image/svg+xml",
        bytes,
    })
}

fn gtk_icon_theme(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()?
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("gtk-icon-theme-name")
                .and_then(|r| r.trim_start().strip_prefix('='))
                .map(|v| v.trim().trim_matches('"').to_string())
        })
        .filter(|t| {
            !t.is_empty()
                && t.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_. ".contains(c))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3, 4];
    const SVG: &str =
        "<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 8 8\"/>";

    struct Sys(tempfile::TempDir);
    impl Sys {
        fn new() -> Self {
            Self(tempfile::tempdir().unwrap())
        }
        fn p(&self, rel: &str) -> PathBuf {
            let p = self.0.path().join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            p
        }
        fn write(&self, rel: &str, content: &[u8]) -> PathBuf {
            let p = self.p(rel);
            fs::write(&p, content).unwrap();
            p
        }
        fn desktop(&self, file: &str, body: &str) {
            self.write(
                &format!("share/applications/{file}"),
                format!("[Desktop Entry]\nName=X\n{body}\n").as_bytes(),
            );
        }
        fn resolver(&self) -> IconResolver {
            IconResolver::new(vec![self.0.path().join("share")], vec![], vec![])
        }
    }

    #[test]
    fn finds_the_icon_through_the_desktop_entry() {
        let s = Sys::new();
        s.desktop("kitty.desktop", "Icon=kitty");
        s.write("share/icons/hicolor/256x256/apps/kitty.png", PNG);
        let i = s.resolver().resolve("kitty").unwrap();
        assert_eq!((i.mime, i.bytes.as_slice()), ("image/png", PNG));
    }

    #[test]
    fn matches_case_insensitively_and_by_startup_wm_class() {
        let s = Sys::new();
        s.desktop("spotify.desktop", "Icon=spotify-client");
        s.write(
            "share/icons/hicolor/scalable/apps/spotify-client.svg",
            SVG.as_bytes(),
        );
        s.desktop(
            "brave-browser.desktop",
            "Icon=brave-desktop\nStartupWMClass=Brave-browser",
        );
        s.write("share/icons/hicolor/64x64/apps/brave-desktop.png", PNG);
        s.desktop(
            "org.example.Files.desktop",
            "Icon=folder\nStartupWMClass=files-window",
        );
        s.write("share/icons/hicolor/48x48/apps/folder.png", PNG);
        let r = s.resolver();
        assert_eq!(
            r.resolve("Spotify").unwrap().mime,
            "image/svg+xml",
            "window class capitalised differently from the file"
        );
        assert!(r.resolve("brave-browser").is_some());
        assert!(
            r.resolve("files-window").is_some(),
            "found through StartupWMClass"
        );
    }

    #[test]
    fn prefers_svg_then_a_png_at_least_64px_but_the_smallest_such() {
        let s = Sys::new();
        s.desktop("a.desktop", "Icon=a");
        for px in [16, 48, 128, 512] {
            s.write(&format!("share/icons/hicolor/{px}x{px}/apps/a.png"), PNG);
        }
        let r = s.resolver();
        let small = |p: &str| r.icon_by_name(p).map(|i| i.bytes.len());
        assert_eq!(small("a"), Some(PNG.len()));
        // which file was chosen: ranks
        let mut c = Vec::new();
        let mut b = 1000;
        walk(
            &s.0.path().join("share/icons/hicolor"),
            0,
            "a",
            &mut c,
            &mut b,
        );
        c.sort_by_key(|x| x.0);
        assert!(
            c[0].1.to_string_lossy().contains("128x128"),
            "128 beats 512 (smaller) and 48 (below 64): {:?}",
            c[0].1
        );
        s.write("share/icons/hicolor/scalable/apps/a.svg", SVG.as_bytes());
        assert_eq!(
            s.resolver().icon_by_name("a").unwrap().mime,
            "image/svg+xml",
            "scalable wins"
        );
    }

    #[test]
    fn user_theme_beats_hicolor_and_pixmaps_are_a_fallback() {
        let s = Sys::new();
        s.desktop("app.desktop", "Icon=app");
        s.write("share/icons/hicolor/64x64/apps/app.png", PNG);
        s.write("share/icons/Papirus/scalable/apps/app.svg", SVG.as_bytes());
        let r = IconResolver::new(
            vec![s.0.path().join("share")],
            vec![],
            vec!["Papirus".into()],
        );
        assert_eq!(r.resolve("app").unwrap().mime, "image/svg+xml");
        s.desktop("old.desktop", "Icon=old.png");
        s.write("share/pixmaps/old.png", PNG);
        assert!(
            r.resolve("old").is_some(),
            "pixmaps, and a stray extension in Icon= is tolerated"
        );
    }

    #[test]
    fn absolute_icon_paths_work_but_only_for_real_images() {
        let s = Sys::new();
        let good = s.write("opt/app/icon.png", PNG);
        let fake = s.write(
            "opt/app/secret.png",
            b"this is not an image, it is a secret",
        );
        let svg_ok = s.write("opt/app/i.svg", SVG.as_bytes());
        s.desktop("good.desktop", &format!("Icon={}", good.display()));
        s.desktop("fake.desktop", &format!("Icon={}", fake.display()));
        s.desktop("svg.desktop", &format!("Icon={}", svg_ok.display()));
        s.desktop("etc.desktop", "Icon=/etc/passwd");
        let r = s.resolver();
        assert!(r.resolve("good").is_some() && r.resolve("svg").is_some());
        assert!(
            r.resolve("fake").is_none(),
            "a .png that is not a PNG is refused"
        );
        assert!(
            r.resolve("etc").is_none(),
            "arbitrary files are refused by content"
        );
    }

    #[test]
    fn a_class_or_icon_name_can_never_be_a_path() {
        let s = Sys::new();
        s.write("secret.png", PNG);
        s.desktop("evil.desktop", "Icon=../../secret");
        s.desktop("evil2.desktop", "Icon=a/b");
        let r = s.resolver();
        for bad in [
            "../secret",
            "..",
            "a/b",
            "",
            "x y",
            "a;b",
            ".hidden",
            "a\0b",
            &"x".repeat(101),
        ] {
            assert!(!valid_class(bad), "{bad:?}");
            assert!(r.resolve(bad).is_none());
        }
        assert!(
            r.resolve("evil").is_none() && r.resolve("evil2").is_none(),
            "Icon= with slashes is not searched as a path"
        );
    }

    #[test]
    fn size_limits_unknown_apps_and_caching() {
        let s = Sys::new();
        s.desktop("big.desktop", "Icon=big");
        s.write(
            "share/icons/hicolor/64x64/apps/big.png",
            &[PNG, &vec![0u8; 200_000]].concat(),
        );
        let r = s.resolver();
        assert!(r.resolve("big").is_none(), "over 128 KB is refused");
        assert!(r.resolve("unknown-app").is_none());
        s.desktop("late.desktop", "Icon=late");
        s.write("share/icons/hicolor/64x64/apps/late.png", PNG);
        assert!(r.resolve("late").is_some());
        fs::remove_file(s.0.path().join("share/icons/hicolor/64x64/apps/late.png")).unwrap();
        assert!(
            r.resolve("late").is_some(),
            "cached: no file system access the second time"
        );
        assert!(r.resolve("LATE").is_some(), "cache key ignores case");
    }

    #[test]
    fn an_icon_named_like_the_class_works_without_a_desktop_entry() {
        let s = Sys::new();
        s.write("share/icons/hicolor/48x48/apps/standalone.png", PNG);
        assert!(s.resolver().resolve("standalone").is_some());
    }

    #[test]
    fn size_parsing() {
        assert_eq!(
            size_of_path(Path::new("/i/hicolor/64x64/apps/a.png")),
            Some(64)
        );
        assert_eq!(
            size_of_path(Path::new("/i/hicolor/128x128@2/apps/a.png")),
            Some(128)
        );
        assert_eq!(size_of_path(Path::new("/i/breeze/apps/48/a.png")), Some(48));
        assert_eq!(
            size_of_path(Path::new("/i/hicolor/scalable/apps/a.png")),
            None
        );
    }
}
