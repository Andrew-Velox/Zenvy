//! Active capability probes (PLAN.md §7.2). A probe tests real access/availability; tool presence
//! is never accepted as proof. Probes never write, create devices, or change state.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use zenvy_core::resolve;
use zenvy_proto::{Candidate, CapState, CapabilityRecord, DeviceProfile, FormFactor};

#[derive(Debug, PartialEq, Eq)]
pub enum Access {
    Ok,
    Denied,
    Missing,
    Error(String),
}

/// Opens a device node for writing (no ioctl, so `/dev/uinput` is NOT turned into an input device).
pub fn check_write(path: &Path) -> Access {
    match OpenOptions::new().write(true).open(path) {
        Ok(_) => Access::Ok,
        Err(e) => match e.kind() {
            ErrorKind::PermissionDenied => Access::Denied,
            ErrorKind::NotFound => Access::Missing,
            _ => Access::Error(e.to_string()),
        },
    }
}

fn cand(backend: &str, state: CapState, reason: impl Into<Option<String>>) -> Candidate {
    Candidate {
        backend: backend.into(),
        state,
        reason: reason.into(),
    }
}

fn from_access(backend: &str, a: Access, path: &str, missing_hint: &str) -> Candidate {
    match a {
        Access::Ok => cand(backend, CapState::Available, None),
        Access::Denied => cand(
            backend,
            CapState::PermissionRequired,
            format!("no write access to {path}"),
        ),
        Access::Missing => cand(
            backend,
            CapState::Unavailable,
            format!("{path} missing ({missing_hint})"),
        ),
        Access::Error(e) => cand(backend, CapState::Error, format!("{path}: {e}")),
    }
}

struct Globals<'a> {
    list: &'a [String],
    wayland: bool,
}

impl Globals<'_> {
    /// Candidate that needs every listed Wayland global.
    fn need(&self, backend: &str, names: &[&str]) -> Candidate {
        if !self.wayland {
            return cand(
                backend,
                CapState::Unsupported,
                "no Wayland session".to_string(),
            );
        }
        let missing: Vec<&str> = names
            .iter()
            .copied()
            .filter(|n| !self.list.iter().any(|g| g == n))
            .collect();
        if missing.is_empty() {
            cand(backend, CapState::Available, None)
        } else {
            cand(
                backend,
                CapState::Unavailable,
                format!("compositor does not advertise {}", missing.join(", ")),
            )
        }
    }
}

fn socket_candidate(backend: &str, path: &Path) -> Candidate {
    match UnixStream::connect(path) {
        Ok(_) => cand(backend, CapState::Available, None),
        Err(e) => match e.kind() {
            ErrorKind::PermissionDenied => cand(
                backend,
                CapState::PermissionRequired,
                format!("no access to {}", path.display()),
            ),
            ErrorKind::NotFound | ErrorKind::ConnectionRefused => cand(
                backend,
                CapState::Unavailable,
                format!("{} not running", path.display()),
            ),
            _ => cand(backend, CapState::Error, format!("{}: {e}", path.display())),
        },
    }
}

fn i2c_devices(root: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(root.join("dev")) else {
        return vec![];
    };
    let mut v: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("i2c-"))
        .map(|e| e.path())
        .collect();
    v.sort();
    v
}

fn writable_dir(p: &Path) -> bool {
    p.is_dir() && rustix::fs::access(p, rustix::fs::Access::WRITE_OK).is_ok()
}

/// Runs all system-level probes (Hyprland-specific ones live in `zenvy-hyprland`).
/// `runtime_dir` is `$XDG_RUNTIME_DIR`, `home` is `$HOME`; both are parameters so tests can fake them.
pub fn probe_system(
    root: &Path,
    profile: &DeviceProfile,
    overrides: &BTreeMap<String, String>,
    runtime_dir: Option<&Path>,
    home: Option<&Path>,
) -> Vec<(String, CapabilityRecord)> {
    let g = Globals {
        list: &profile.session.wayland_protocols,
        wayland: profile.session.session_type == "wayland" || profile.session.wayland_display,
    };
    let ov = |cap: &str| zenvy_core::override_for(overrides, cap);
    let uinput = || {
        from_access(
            "uinput",
            check_write(&root.join("dev/uinput")),
            "/dev/uinput",
            "kernel module 'uinput' not loaded",
        )
    };
    let mut out: Vec<(String, CapabilityRecord)> = vec![];
    let mut add = |id: &str, c: Vec<Candidate>| out.push((id.to_string(), resolve(c, ov(id))));

    // Wayland-protocol backends exist in the compositor but Zenvy has not implemented them yet:
    // report that honestly instead of promising a fallback that would fail at runtime.
    let unimplemented = |c: Candidate| {
        if c.state == CapState::Available {
            cand(
                &c.backend,
                CapState::Unavailable,
                "advertised by the compositor, but Zenvy's backend for it is not implemented yet"
                    .to_string(),
            )
        } else {
            c
        }
    };
    add(
        "input.pointer",
        vec![
            uinput(),
            unimplemented(g.need("wlr-virtual-pointer", &["zwlr_virtual_pointer_manager_v1"])),
        ],
    );
    add(
        "input.keyboard",
        vec![
            uinput(),
            unimplemented(g.need("virtual-keyboard", &["zwp_virtual_keyboard_manager_v1"])),
        ],
    );

    // Text: `wtype` drives the virtual-keyboard protocol and can type any Unicode (verified in Phase 0).
    // Raw uinput keycodes are ASCII-only and keyboard-layout dependent, so they are an explicitly partial fallback.
    let has_wtype = profile
        .software_hints
        .get("wtype")
        .is_some_and(|p| p.is_some());
    let mut wtype = g.need("wtype", &["zwp_virtual_keyboard_manager_v1"]);
    if wtype.state == CapState::Available && !has_wtype {
        wtype = cand(
            "wtype",
            CapState::Unavailable,
            "wtype is not installed".to_string(),
        );
    }
    let mut uin = uinput();
    uin.backend = "uinput-ascii".into();
    if uin.state == CapState::Available {
        uin.reason = Some("partial: ASCII only, depends on the PC's keyboard layout".into());
    }
    add("input.text", vec![wtype, uin]);

    add(
        "clipboard.sync",
        vec![
            g.need("ext-data-control", &["ext_data_control_manager_v1"]),
            g.need("wlr-data-control", &["zwlr_data_control_manager_v1"]),
        ],
    );
    add(
        "screen.capture",
        vec![
            g.need(
                "ext-image-copy-capture",
                &[
                    "ext_image_copy_capture_manager_v1",
                    "ext_output_image_capture_source_manager_v1",
                ],
            ),
            g.need("wlr-screencopy", &["zwlr_screencopy_manager_v1"]),
        ],
    );

    // Screen streaming via the desktop portal needs a portal backend + PipeWire, and a consent
    // dialog the first time: report that honestly instead of "available".
    let portal_dir = root.join("usr/share/xdg-desktop-portal/portals");
    let portal_has_screencast = std::fs::read_dir(&portal_dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok()).any(|e| {
                std::fs::read_to_string(e.path())
                    .map(|t| t.contains("ScreenCast"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    let pw = runtime_dir.map(|r| socket_candidate("pipewire", &r.join("pipewire-0")));
    let portal = if !portal_has_screencast {
        cand(
            "portal-screencast",
            CapState::Unavailable,
            "no xdg-desktop-portal backend with ScreenCast installed".to_string(),
        )
    } else if pw.as_ref().is_some_and(|c| c.state != CapState::Available) {
        cand(
            "portal-screencast",
            CapState::Unavailable,
            "PipeWire is not running".to_string(),
        )
    } else {
        cand(
            "portal-screencast",
            CapState::PermissionRequired,
            "backend present; the user must approve screen sharing on first use".to_string(),
        )
    };
    add("screen.stream.portal", vec![portal]);

    // Brightness: laptop backlight (via logind, no root) or external-monitor DDC/CI.
    let backlight = std::fs::read_dir(root.join("sys/class/backlight"))
        .map(|rd| rd.filter_map(|e| e.ok()).count())
        .unwrap_or(0);
    let bl = if backlight > 0 {
        cand(
            "sysfs-backlight",
            CapState::Available,
            "write path via logind is not yet verified".to_string(),
        )
    } else {
        cand(
            "sysfs-backlight",
            CapState::Unavailable,
            "no /sys/class/backlight device".to_string(),
        )
    };
    let i2c = i2c_devices(root);
    let ddc = if i2c.is_empty() {
        cand(
            "ddc",
            CapState::Unavailable,
            "no /dev/i2c-* (i2c-dev module not loaded)".to_string(),
        )
    } else if i2c.iter().any(|d| matches!(check_write(d), Access::Ok)) {
        cand(
            "ddc",
            CapState::Available,
            "i2c access OK; whether the monitor has DDC/CI enabled is not verified".to_string(),
        )
    } else {
        cand(
            "ddc",
            CapState::PermissionRequired,
            "no access to /dev/i2c-*".to_string(),
        )
    };
    // The backends drive `brightnessctl` / `ddcutil`; the device being reachable is not enough without the tool.
    let tool = |name: &str| {
        profile
            .software_hints
            .get(name)
            .is_some_and(|p| p.is_some())
    };
    let needs = |c: Candidate, tool_name: &str| {
        if c.state == CapState::Available && !tool(tool_name) {
            cand(
                &c.backend,
                CapState::Unavailable,
                format!("{tool_name} is not installed"),
            )
        } else {
            c
        }
    };
    add(
        "brightness",
        vec![needs(bl, "brightnessctl"), needs(ddc, "ddcutil")],
    );

    let battery = if profile.hardware.battery_present {
        cand("sysfs-power-supply", CapState::Available, None)
    } else if profile.hardware.form_factor == FormFactor::Desktop {
        cand(
            "sysfs-power-supply",
            CapState::Unsupported,
            "no battery present (desktop)".to_string(),
        )
    } else {
        cand(
            "sysfs-power-supply",
            CapState::Unavailable,
            "no battery found".to_string(),
        )
    };
    add("battery.monitor", vec![battery]);

    // Audio is driven through `pactl` (the PulseAudio protocol, served natively by PipeWire): the server
    // socket must answer AND the tool must exist. A socket without the tool would fail at the first command.
    let has_pactl = profile
        .software_hints
        .get("pactl")
        .is_some_and(|p| p.is_some());
    let needs_pactl = |c: Candidate| {
        if c.state == CapState::Available && !has_pactl {
            cand(
                &c.backend,
                CapState::Unavailable,
                "the audio server is running but `pactl` is not installed".to_string(),
            )
        } else {
            c
        }
    };
    let has_parec = profile
        .software_hints
        .get("parec")
        .is_some_and(|p| p.is_some());
    let audio_up = runtime_dir.is_some_and(|r| {
        socket_candidate("pipewire", &r.join("pipewire-0")).state == CapState::Available
            || socket_candidate("pulse", &r.join("pulse/native")).state == CapState::Available
    });
    // Power (lock, suspend, reboot, power off) goes through logind with `systemctl` and `loginctl`.
    {
        let has = |t: &str| profile.software_hints.get(t).is_some_and(|p| p.is_some());
        add(
            "power.control",
            vec![if has("systemctl") && has("loginctl") {
                cand("logind", CapState::Available, None)
            } else {
                cand(
                    "logind",
                    CapState::Unavailable,
                    "systemctl and loginctl are needed".to_string(),
                )
            }],
        );
    }
    // Listening (the PC's audio on a phone) captures the default output's monitor with `parec`.
    add(
        "audio.listen",
        vec![if !has_parec {
            cand(
                "parec",
                CapState::Unavailable,
                "parec is not installed".to_string(),
            )
        } else if !audio_up {
            cand(
                "parec",
                CapState::Unavailable,
                "no running audio server".to_string(),
            )
        } else {
            cand("parec", CapState::Available, None)
        }],
    );
    match runtime_dir {
        Some(r) => add(
            "audio.volume",
            vec![
                needs_pactl(socket_candidate("pipewire", &r.join("pipewire-0"))),
                needs_pactl(socket_candidate("pulse", &r.join("pulse/native"))),
            ],
        ),
        None => add(
            "audio.volume",
            vec![cand(
                "pipewire",
                CapState::Unavailable,
                "XDG_RUNTIME_DIR not set".to_string(),
            )],
        ),
    }

    // Media control talks MPRIS over the user's D-Bus session bus. "Available" means the bus answers; whether
    // a player is running right now is runtime state, not a capability.
    match runtime_dir {
        Some(r) => add(
            "media.control",
            vec![socket_candidate("mpris-dbus", &r.join("bus"))],
        ),
        None => add(
            "media.control",
            vec![cand(
                "mpris-dbus",
                CapState::Unavailable,
                "XDG_RUNTIME_DIR not set".to_string(),
            )],
        ),
    }

    let dl = home.map(|h| h.join("Downloads"));
    let ft = match (home, dl) {
        (Some(_), Some(d)) if writable_dir(&d) => cand("inbox", CapState::Available, None),
        (Some(h), _) if writable_dir(h) => cand(
            "inbox",
            CapState::Available,
            "~/Downloads missing; will create it on first transfer".to_string(),
        ),
        (Some(_), _) => cand(
            "inbox",
            CapState::PermissionRequired,
            "home directory is not writable".to_string(),
        ),
        (None, _) => cand("inbox", CapState::Unavailable, "HOME not set".to_string()),
    };
    add("file.transfer", vec![ft]);

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{collect, Env};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn profile(root: &Path, globals: &[&str], wayland: bool) -> DeviceProfile {
        let env = if wayland {
            Env::for_test(
                &[
                    ("XDG_SESSION_TYPE", "wayland"),
                    ("WAYLAND_DISPLAY", "wayland-1"),
                ],
                true,
                vec![],
            )
        } else {
            Env::default()
        };
        let g: Vec<String> = globals.iter().map(|s| s.to_string()).collect();
        collect(root, &env, Some(&g), vec![])
    }

    fn put(root: &Path, rel: &str, mode: u32) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn rec<'a>(v: &'a [(String, CapabilityRecord)], id: &str) -> &'a CapabilityRecord {
        &v.iter()
            .find(|(k, _)| k == id)
            .unwrap_or_else(|| panic!("no {id}"))
            .1
    }

    #[test]
    fn uinput_states_are_distinguished() {
        // present + writable
        let d = tempfile::tempdir().unwrap();
        put(d.path(), "dev/uinput", 0o600);
        let p = profile(d.path(), &[], true);
        let r = probe_system(d.path(), &p, &BTreeMap::new(), None, None);
        assert_eq!(rec(&r, "input.pointer").state, CapState::Available);
        assert_eq!(rec(&r, "input.pointer").backend.as_deref(), Some("uinput"));

        // present but no permission => permission_required (not "unavailable")
        let d = tempfile::tempdir().unwrap();
        put(d.path(), "dev/uinput", 0o000);
        let r = probe_system(
            d.path(),
            &profile(d.path(), &[], true),
            &BTreeMap::new(),
            None,
            None,
        );
        assert_eq!(rec(&r, "input.pointer").state, CapState::PermissionRequired);

        // absent => unavailable
        let d = tempfile::tempdir().unwrap();
        let r = probe_system(
            d.path(),
            &profile(d.path(), &[], true),
            &BTreeMap::new(),
            None,
            None,
        );
        assert_eq!(rec(&r, "input.pointer").state, CapState::Unavailable);
    }

    #[test]
    fn protocol_alone_is_not_promised_until_the_backend_exists() {
        let d = tempfile::tempdir().unwrap();
        let p = profile(
            d.path(),
            &[
                "zwlr_virtual_pointer_manager_v1",
                "zwp_virtual_keyboard_manager_v1",
            ],
            true,
        );
        let r = probe_system(d.path(), &p, &BTreeMap::new(), None, None);
        let ptr = rec(&r, "input.pointer");
        assert_eq!(ptr.state, CapState::Unavailable);
        assert!(ptr.reason.as_ref().unwrap().contains("not implemented"));
        assert_eq!(
            rec(&r, "input.text").state,
            CapState::Unavailable,
            "no wtype installed => no text backend"
        );
    }

    #[test]
    fn text_uses_wtype_only_when_protocol_and_tool_are_both_present() {
        let d = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let tool = bin.path().join("wtype");
        fs::write(&tool, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
        let env = Env::for_test(
            &[("XDG_SESSION_TYPE", "wayland"), ("WAYLAND_DISPLAY", "w")],
            true,
            vec![bin.path().to_path_buf()],
        );
        let g = vec!["zwp_virtual_keyboard_manager_v1".to_string()];
        let p = collect(d.path(), &env, Some(&g), vec![]);
        assert_eq!(
            rec(
                &probe_system(d.path(), &p, &BTreeMap::new(), None, None),
                "input.text"
            )
            .backend
            .as_deref(),
            Some("wtype")
        );
        // protocol without the tool, and tool without the protocol: neither is enough
        let p = collect(
            d.path(),
            &Env::for_test(&[("XDG_SESSION_TYPE", "wayland")], true, vec![]),
            Some(&g),
            vec![],
        );
        assert_eq!(
            rec(
                &probe_system(d.path(), &p, &BTreeMap::new(), None, None),
                "input.text"
            )
            .state,
            CapState::Unavailable
        );
        let p = collect(d.path(), &env, Some(&[]), vec![]);
        assert_eq!(
            rec(
                &probe_system(d.path(), &p, &BTreeMap::new(), None, None),
                "input.text"
            )
            .state,
            CapState::Unavailable
        );
    }

    #[test]
    fn text_via_uinput_is_flagged_partial() {
        let d = tempfile::tempdir().unwrap();
        put(d.path(), "dev/uinput", 0o600);
        let r = probe_system(
            d.path(),
            &profile(d.path(), &[], true),
            &BTreeMap::new(),
            None,
            None,
        );
        let t = rec(&r, "input.text");
        assert_eq!(t.backend.as_deref(), Some("uinput-ascii"));
        assert!(t.reason.as_ref().unwrap().contains("partial"));
    }

    #[test]
    fn user_override_respected_only_when_usable() {
        let d = tempfile::tempdir().unwrap();
        put(d.path(), "dev/uinput", 0o600);
        let p = profile(d.path(), &["zwlr_virtual_pointer_manager_v1"], true);
        let ov = BTreeMap::from([("input.pointer".to_string(), "uinput".to_string())]);
        assert_eq!(
            rec(
                &probe_system(d.path(), &p, &ov, None, None),
                "input.pointer"
            )
            .backend
            .as_deref(),
            Some("uinput")
        );
        let bad = BTreeMap::from([(
            "input.pointer".to_string(),
            "wlr-virtual-pointer".to_string(),
        )]);
        let r = probe_system(d.path(), &p, &bad, None, None);
        assert_eq!(
            rec(&r, "input.pointer").backend.as_deref(),
            Some("uinput"),
            "unusable override must not win"
        );
        assert!(rec(&r, "input.pointer")
            .reason
            .as_ref()
            .unwrap()
            .contains("override 'wlr-virtual-pointer' ignored"));
    }

    #[test]
    fn no_wayland_means_unsupported_protocol_features() {
        let d = tempfile::tempdir().unwrap();
        let r = probe_system(
            d.path(),
            &profile(d.path(), &[], false),
            &BTreeMap::new(),
            None,
            None,
        );
        assert_eq!(
            rec(&r, "clipboard.sync").candidates[0].state,
            CapState::Unsupported
        );
    }

    #[test]
    fn desktop_has_unsupported_battery() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("sys/class/dmi/id")).unwrap();
        fs::write(d.path().join("sys/class/dmi/id/chassis_type"), "3").unwrap();
        let r = probe_system(
            d.path(),
            &profile(d.path(), &[], true),
            &BTreeMap::new(),
            None,
            None,
        );
        assert_eq!(rec(&r, "battery.monitor").state, CapState::Unsupported);
    }

    #[test]
    fn portal_needs_backend_and_reports_consent() {
        let d = tempfile::tempdir().unwrap();
        let r = probe_system(
            d.path(),
            &profile(d.path(), &[], true),
            &BTreeMap::new(),
            None,
            None,
        );
        assert_eq!(rec(&r, "screen.stream.portal").state, CapState::Unavailable);

        fs::create_dir_all(d.path().join("usr/share/xdg-desktop-portal/portals")).unwrap();
        fs::write(
            d.path()
                .join("usr/share/xdg-desktop-portal/portals/hyprland.portal"),
            "[portal]\nInterfaces=org.freedesktop.impl.portal.ScreenCast;\n",
        )
        .unwrap();
        let rt = tempfile::tempdir().unwrap();
        let _l = std::os::unix::net::UnixListener::bind(rt.path().join("pipewire-0")).unwrap();
        let r = probe_system(
            d.path(),
            &profile(d.path(), &[], true),
            &BTreeMap::new(),
            Some(rt.path()),
            None,
        );
        assert_eq!(
            rec(&r, "screen.stream.portal").state,
            CapState::PermissionRequired
        );
    }

    #[test]
    fn ddc_permission_vs_missing() {
        let d = tempfile::tempdir().unwrap();
        put(d.path(), "dev/i2c-4", 0o000);
        let r = probe_system(
            d.path(),
            &profile(d.path(), &[], true),
            &BTreeMap::new(),
            None,
            None,
        );
        assert_eq!(rec(&r, "brightness").state, CapState::PermissionRequired);
    }

    #[test]
    fn audio_needs_both_the_server_socket_and_pactl() {
        let d = tempfile::tempdir().unwrap();
        let rt = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let tool = bin.path().join("pactl");
        fs::write(&tool, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
        let with_tool = |root: &Path| {
            collect(
                root,
                &Env::for_test(
                    &[("XDG_SESSION_TYPE", "wayland")],
                    true,
                    vec![bin.path().to_path_buf()],
                ),
                Some(&[]),
                vec![],
            )
        };
        let without_tool = |root: &Path| {
            collect(
                root,
                &Env::for_test(&[("XDG_SESSION_TYPE", "wayland")], true, vec![]),
                Some(&[]),
                vec![],
            )
        };

        // nothing running
        let r = probe_system(
            d.path(),
            &with_tool(d.path()),
            &BTreeMap::new(),
            Some(rt.path()),
            None,
        );
        assert_eq!(rec(&r, "audio.volume").state, CapState::Unavailable);
        // server running, tool present => available via pipewire
        let _l = std::os::unix::net::UnixListener::bind(rt.path().join("pipewire-0")).unwrap();
        let r = probe_system(
            d.path(),
            &with_tool(d.path()),
            &BTreeMap::new(),
            Some(rt.path()),
            None,
        );
        assert_eq!(rec(&r, "audio.volume").backend.as_deref(), Some("pipewire"));
        // server running but no pactl => not available, and the reason says why
        let r = probe_system(
            d.path(),
            &without_tool(d.path()),
            &BTreeMap::new(),
            Some(rt.path()),
            None,
        );
        assert_eq!(rec(&r, "audio.volume").state, CapState::Unavailable);
        assert!(rec(&r, "audio.volume")
            .reason
            .as_ref()
            .unwrap()
            .contains("pactl"));
    }

    #[test]
    fn media_control_follows_the_session_bus() {
        let d = tempfile::tempdir().unwrap();
        let rt = tempfile::tempdir().unwrap();
        let p = profile(d.path(), &[], true);
        assert_eq!(
            rec(
                &probe_system(d.path(), &p, &BTreeMap::new(), Some(rt.path()), None),
                "media.control"
            )
            .state,
            CapState::Unavailable
        );
        let _l = std::os::unix::net::UnixListener::bind(rt.path().join("bus")).unwrap();
        let r = probe_system(d.path(), &p, &BTreeMap::new(), Some(rt.path()), None);
        assert_eq!(
            rec(&r, "media.control").backend.as_deref(),
            Some("mpris-dbus")
        );
    }
}
