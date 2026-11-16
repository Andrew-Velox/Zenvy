use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use zenvy_proto::*;

/// Environment variables the profiler may look at. Everything else is ignored by construction.
pub const ENV_ALLOW: &[&str] = &["XDG_SESSION_TYPE", "XDG_CURRENT_DESKTOP", "WAYLAND_DISPLAY"];

#[derive(Debug, Clone, Default)]
pub struct Env {
    vars: BTreeMap<String, String>,
    /// HYPRLAND_INSTANCE_SIGNATURE is only recorded as present/absent (it embeds a commit hash).
    pub hyprland_signature_present: bool,
    pub path: Vec<PathBuf>,
}

impl Env {
    pub fn from_process() -> Self {
        let vars = ENV_ALLOW
            .iter()
            .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
            .collect();
        let path = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        Self {
            vars,
            hyprland_signature_present: std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some(),
            path,
        }
    }
    pub fn for_test(vars: &[(&str, &str)], sig: bool, path: Vec<PathBuf>) -> Self {
        Self {
            vars: vars
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            hyprland_signature_present: sig,
            path,
        }
    }
    pub fn get(&self, k: &str) -> Option<&str> {
        self.vars.get(k).map(String::as_str)
    }
}

fn read(root: &Path, rel: &str) -> Option<String> {
    fs::read_to_string(root.join(rel.trim_start_matches('/'))).ok()
}

/// Parses `/etc/os-release` into (ID, ID_LIKE, VERSION_ID).
pub fn parse_os_release(text: &str) -> (String, Vec<String>, Option<String>) {
    let mut id = String::from("unknown");
    let mut like = vec![];
    let mut ver = None;
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim().trim_matches('"').trim_matches('\'');
        match k {
            "ID" => id = v.to_ascii_lowercase(),
            "ID_LIKE" => {
                like = v
                    .split_whitespace()
                    .map(|s| s.to_ascii_lowercase())
                    .collect()
            }
            "VERSION_ID" => ver = Some(v.to_string()),
            _ => {}
        }
    }
    (id, like, ver)
}

fn package_manager(id: &str, like: &[String]) -> PackageManager {
    let all = std::iter::once(id).chain(like.iter().map(String::as_str));
    for d in all {
        match d {
            "arch" | "manjaro" | "endeavouros" | "cachyos" => return PackageManager::Pacman,
            "fedora" | "rhel" | "centos" | "nobara" => return PackageManager::Dnf,
            "debian" | "ubuntu" | "linuxmint" | "pop" => return PackageManager::Apt,
            "opensuse" | "opensuse-tumbleweed" | "opensuse-leap" | "suse" | "sles" => {
                return PackageManager::Zypper
            }
            _ => {}
        }
    }
    if id.starts_with("opensuse") {
        return PackageManager::Zypper;
    }
    PackageManager::Unknown
}

fn os(root: &Path) -> OsInfo {
    let (distro_id, distro_like, version_id) = read(root, "etc/os-release")
        .or_else(|| read(root, "usr/lib/os-release"))
        .map(|t| parse_os_release(&t))
        .unwrap_or_else(|| ("unknown".into(), vec![], None));
    OsInfo {
        package_manager: package_manager(&distro_id, &distro_like),
        distro_id,
        distro_like,
        version_id,
        kernel: read(root, "proc/sys/kernel/osrelease")
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
        arch: std::env::consts::ARCH.to_string(),
    }
}

fn session(env: &Env, globals: Option<&[String]>) -> SessionInfo {
    let desktop = env
        .get("XDG_CURRENT_DESKTOP")
        .unwrap_or("")
        .to_ascii_lowercase();
    let compositor = if env.hyprland_signature_present || desktop.contains("hyprland") {
        "hyprland"
    } else if desktop.contains("sway") {
        "sway"
    } else if desktop.contains("gnome") {
        "gnome"
    } else if desktop.contains("kde") {
        "kde"
    } else if desktop.is_empty() {
        "unknown"
    } else {
        "other"
    };
    SessionInfo {
        session_type: env.get("XDG_SESSION_TYPE").unwrap_or("unknown").to_string(),
        compositor: compositor.into(),
        wayland_display: env.get("WAYLAND_DISPLAY").is_some(),
        wayland_protocols: globals.map(|g| g.to_vec()).unwrap_or_default(),
        env: ENV_ALLOW
            .iter()
            .filter_map(|k| env.get(k).map(|v| (k.to_string(), v.to_string())))
            .collect(),
    }
}

fn gpus(root: &Path) -> Vec<Gpu> {
    let Ok(rd) = fs::read_dir(root.join("sys/class/drm")) else {
        return vec![];
    };
    let mut cards: Vec<_> = rd
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with("card") && n[4..].bytes().all(|b| b.is_ascii_digit())
        })
        .collect();
    cards.sort_by_key(|e| e.file_name());
    cards
        .into_iter()
        .filter_map(|e| {
            let dev = e.path().join("device");
            let vendor_id = fs::read_to_string(dev.join("vendor")).ok()?;
            let vendor = match vendor_id.trim() {
                "0x1002" => "AMD",
                "0x10de" => "NVIDIA",
                "0x8086" => "Intel",
                other => other,
            }
            .to_string();
            let driver = fs::read_link(dev.join("driver"))
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
            Some(Gpu { vendor, driver })
        })
        .collect()
}

fn battery_present(root: &Path) -> bool {
    let Ok(rd) = fs::read_dir(root.join("sys/class/power_supply")) else {
        return false;
    };
    rd.filter_map(|e| e.ok()).any(|e| {
        fs::read_to_string(e.path().join("type"))
            .map(|t| t.trim() == "Battery")
            .unwrap_or(false)
    })
}

/// Chassis *type* only (a small integer), never DMI serials/uuids.
fn form_factor(root: &Path, battery: bool) -> FormFactor {
    if battery {
        return FormFactor::Laptop;
    }
    match read(root, "sys/class/dmi/id/chassis_type").and_then(|s| s.trim().parse::<u32>().ok()) {
        Some(8 | 9 | 10 | 14 | 30 | 31 | 32) => FormFactor::Laptop,
        Some(3 | 4 | 5 | 6 | 7 | 13 | 15 | 16 | 17 | 23 | 24 | 35 | 36) => FormFactor::Desktop,
        _ => FormFactor::Unknown,
    }
}

fn peripherals(root: &Path) -> Peripherals {
    let mut p = Peripherals::default();
    if let Some(t) = read(root, "proc/bus/input/devices") {
        for block in t.split("\n\n") {
            let name = block
                .lines()
                .find_map(|l| l.strip_prefix("N: Name="))
                .unwrap_or("")
                .trim_matches('"')
                .to_ascii_lowercase();
            if name.is_empty() || name.starts_with("zenvy-") {
                continue; // our own virtual devices must not count as hardware
            }
            let handlers = block
                .lines()
                .find_map(|l| l.strip_prefix("H: Handlers="))
                .unwrap_or("");
            let has = |h: &str| {
                handlers
                    .split_whitespace()
                    .any(|x| x == h || x.starts_with(h))
            };
            if name.contains("touchpad") || name.contains("trackpad") {
                p.touchpads += 1;
            } else if has("mouse") && !name.contains("virtual") {
                p.pointers += 1;
            }
            if has("kbd") && has("leds") {
                p.keyboards += 1;
            }
            if has("js") {
                p.gamepads += 1;
            }
        }
    }
    if let Some(t) = read(root, "proc/asound/pcm") {
        for l in t.lines() {
            if l.contains("playback") {
                p.audio_outputs += 1;
            }
            if l.contains("capture") {
                p.audio_inputs += 1;
            }
        }
    }
    p
}

const HINT_TOOLS: &[&str] = &[
    "hyprctl",
    "brightnessctl",
    "wpctl",
    "pactl",
    "playerctl",
    "wl-copy",
    "wl-paste",
    "grim",
    "wf-recorder",
    "ydotool",
    "wtype",
    "notify-send",
    "parec",
    "systemctl",
    "loginctl",
    "busctl",
    "tailscale",
    "ddcutil",
    "gst-launch-1.0",
    "ffmpeg",
];

fn which(path: &[PathBuf], tool: &str) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    path.iter()
        .map(|d| d.join(tool))
        .find(|p| {
            p.metadata()
                .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        })
        .map(|p| p.display().to_string())
}

/// Collects the whole profile. `displays` come from the compositor adapter (already serial-free).
pub fn collect(
    root: &Path,
    env: &Env,
    globals: Option<&[String]>,
    displays: Vec<Monitor>,
) -> DeviceProfile {
    let battery = battery_present(root);
    let cpuinfo = read(root, "proc/cpuinfo").unwrap_or_default();
    let meminfo = read(root, "proc/meminfo").unwrap_or_default();
    DeviceProfile {
        schema_version: 1,
        os: os(root),
        session: session(env, globals),
        hardware: HardwareInfo {
            cpu_model: cpuinfo
                .lines()
                .find_map(|l| l.strip_prefix("model name"))
                .and_then(|l| l.split_once(':'))
                .map(|x| x.1.trim().to_string())
                .unwrap_or_default(),
            cpu_threads: cpuinfo
                .lines()
                .filter(|l| l.starts_with("processor"))
                .count() as u32,
            ram_mb: meminfo
                .lines()
                .find_map(|l| l.strip_prefix("MemTotal:"))
                .and_then(|l| l.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
                .map(|kb| kb / 1024)
                .unwrap_or(0),
            gpus: gpus(root),
            form_factor: form_factor(root, battery),
            battery_present: battery,
        },
        displays,
        peripherals: peripherals(root),
        software_hints: HINT_TOOLS
            .iter()
            .map(|t| (t.to_string(), which(&env.path, t)))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn os_release_for_the_five_target_distros() {
        let cases = [
            ("ID=arch\nNAME=\"Arch Linux\"\n", PackageManager::Pacman),
            ("ID=fedora\nVERSION_ID=41\n", PackageManager::Dnf),
            (
                "ID=ubuntu\nID_LIKE=debian\nVERSION_ID=\"24.04\"\n",
                PackageManager::Apt,
            ),
            ("ID=debian\nVERSION_ID=\"13\"\n", PackageManager::Apt),
            (
                "ID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\"\n",
                PackageManager::Zypper,
            ),
            ("ID=cachyos\nID_LIKE=arch\n", PackageManager::Pacman),
            ("ID=weirdos\n", PackageManager::Unknown),
        ];
        for (text, want) in cases {
            let (id, like, _) = parse_os_release(text);
            assert_eq!(package_manager(&id, &like), want, "{text}");
        }
        assert_eq!(
            parse_os_release("ID=ubuntu\nVERSION_ID=\"24.04\"\n")
                .2
                .as_deref(),
            Some("24.04")
        );
    }

    fn tree() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let w = |p: &str, c: &str| {
            let f = d.path().join(p);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, c).unwrap();
        };
        w("etc/os-release", "ID=fedora\nVERSION_ID=41\n");
        w("proc/sys/kernel/osrelease", "6.11.0\n");
        w("proc/cpuinfo", "processor\t: 0\nmodel name\t: Test CPU 9000\n\nprocessor\t: 1\nmodel name\t: Test CPU 9000\n");
        w("proc/meminfo", "MemTotal:       16384000 kB\n");
        w("sys/class/power_supply/BAT0/type", "Battery\n");
        w("sys/class/dmi/id/chassis_type", "10\n");
        // These must never be read or surfaced:
        w("sys/class/dmi/id/product_serial", "SERIAL-SECRET-123\n");
        w("sys/class/dmi/id/product_uuid", "UUID-SECRET-456\n");
        w("etc/machine-id", "MACHINEID-SECRET-789\n");
        w("proc/bus/input/devices", "I: Bus=0011\nN: Name=\"AT Translated Set 2 keyboard\"\nH: Handlers=sysrq kbd leds event3\n\nI: Bus=0018\nN: Name=\"SYNA Touchpad\"\nH: Handlers=mouse0 event5\n\nI: Bus=0003\nN: Name=\"Logitech Mouse\"\nH: Handlers=mouse1 event6\n\nI: Bus=0006\nN: Name=\"zenvy-spike-kbd\"\nH: Handlers=sysrq kbd leds event9\n");
        w(
            "proc/asound/pcm",
            "00-00: ALC: playback 1 : capture 1\n00-03: HDMI : playback 1\n",
        );
        d
    }

    #[test]
    fn collects_laptop_fixture_without_identifiers() {
        let d = tree();
        let env = Env::for_test(
            &[
                ("XDG_SESSION_TYPE", "wayland"),
                ("XDG_CURRENT_DESKTOP", "Hyprland"),
            ],
            true,
            vec![],
        );
        let p = collect(d.path(), &env, Some(&["wl_seat".to_string()]), vec![]);
        assert_eq!(p.os.distro_id, "fedora");
        assert_eq!(p.os.package_manager, PackageManager::Dnf);
        assert_eq!(p.os.kernel, "6.11.0");
        assert_eq!(p.hardware.cpu_threads, 2);
        assert_eq!(p.hardware.ram_mb, 16000);
        assert_eq!(p.hardware.form_factor, FormFactor::Laptop);
        assert!(p.hardware.battery_present);
        assert_eq!(p.session.compositor, "hyprland");
        assert_eq!(
            (
                p.peripherals.keyboards,
                p.peripherals.touchpads,
                p.peripherals.pointers
            ),
            (1, 1, 1)
        );
        assert_eq!(
            (p.peripherals.audio_outputs, p.peripherals.audio_inputs),
            (2, 1)
        );
        let json = serde_json::to_string(&p).unwrap();
        for secret in ["SERIAL-SECRET", "UUID-SECRET", "MACHINEID-SECRET"] {
            assert!(!json.contains(secret), "{secret} leaked into profile");
        }
    }

    #[test]
    fn desktop_without_battery() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("sys/class/dmi/id")).unwrap();
        fs::write(d.path().join("sys/class/dmi/id/chassis_type"), "3\n").unwrap();
        assert_eq!(
            form_factor(d.path(), battery_present(d.path())),
            FormFactor::Desktop
        );
    }

    #[test]
    fn missing_everything_does_not_panic() {
        let d = tempfile::tempdir().unwrap();
        let p = collect(d.path(), &Env::default(), None, vec![]);
        assert_eq!(p.os.distro_id, "unknown");
        assert_eq!(p.hardware.cpu_threads, 0);
        assert_eq!(p.hardware.form_factor, FormFactor::Unknown);
    }

    #[test]
    fn only_allowlisted_env_is_recorded() {
        let env = Env::for_test(&[("XDG_SESSION_TYPE", "wayland")], false, vec![]);
        let p = collect(tempfile::tempdir().unwrap().path(), &env, None, vec![]);
        assert!(p
            .session
            .env
            .keys()
            .all(|k| ENV_ALLOW.contains(&k.as_str())));
    }
}
