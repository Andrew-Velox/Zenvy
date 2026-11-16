//! Fix-it guidance for capabilities that are not available (PLAN.md §7.4/§11).
//! Zenvy only *describes* these steps; running anything privileged needs explicit user approval,
//! and Zenvy never edits Hyprland config or udev rules by itself.

use std::os::unix::fs::MetadataExt;
use std::path::Path;
use zenvy_proto::{
    CapState, CapabilityRecord, OsInfo, PackageManager, Remediation, RemediationCommand,
};

/// Name of the group that owns a device node, via `/etc/group` (None if unknown or root-owned).
fn device_group(root: &Path, dev_rel: &str) -> Option<String> {
    let gid = std::fs::metadata(root.join(dev_rel)).ok()?.gid();
    if gid == 0 {
        return None;
    }
    std::fs::read_to_string(root.join("etc/group"))
        .ok()?
        .lines()
        .find_map(|l| {
            let mut p = l.split(':');
            let name = p.next()?;
            let _ = p.next();
            (p.next()?.parse::<u32>().ok()? == gid).then(|| name.to_string())
        })
}

fn cmd(run_as: &str, argv: &[&str], relogin: bool) -> RemediationCommand {
    RemediationCommand {
        run_as: run_as.into(),
        argv: argv.iter().map(|s| s.to_string()).collect(),
        needs_relogin: relogin,
    }
}

fn install(pm: PackageManager, pkg: &str) -> Option<RemediationCommand> {
    Some(match pm {
        PackageManager::Pacman => cmd("root", &["pacman", "-S", "--needed", pkg], false),
        PackageManager::Dnf => cmd("root", &["dnf", "install", pkg], false),
        PackageManager::Apt => cmd("root", &["apt", "install", pkg], false),
        PackageManager::Zypper => cmd("root", &["zypper", "install", pkg], false),
        PackageManager::Unknown => return None,
    })
}

fn has_state(rec: &CapabilityRecord, backend: &str, state: CapState) -> bool {
    // "uinput-ascii" is the same device node as "uinput", so the same fix applies.
    rec.candidates.iter().any(|c| {
        (c.backend == backend || c.backend.strip_suffix("-ascii") == Some(backend))
            && c.state == state
    })
}

fn add_user_to_group(summary_dev: &str, group: Option<String>) -> Remediation {
    match group {
        Some(g) => Remediation {
            summary: format!("{summary_dev} is not accessible to your user. Add your user to the '{g}' group, then log out and back in. This lets every program running as you use the device, so review it first."),
            commands: vec![cmd("root", &["usermod", "-aG", &g, "$USER"], true)],
            requires_user_approval: true,
        },
        None => Remediation {
            summary: format!("{summary_dev} is not accessible to your user and is owned by root. Add a udev rule granting a group access (see docs); Zenvy will not create it for you."),
            commands: vec![],
            requires_user_approval: true,
        },
    }
}

/// Fills `rec.remediation` for non-available capabilities. `root` is `/` in production.
pub fn attach_remediation(
    root: &Path,
    id: &str,
    rec: &mut CapabilityRecord,
    os: &OsInfo,
    compositor: &str,
) {
    if rec.state == CapState::Available {
        return;
    }
    let rem = match id {
        "input.pointer" | "input.keyboard" | "input.text" if has_state(rec, "uinput", CapState::PermissionRequired) => {
            Some(add_user_to_group("/dev/uinput", device_group(root, "dev/uinput")))
        }
        "input.pointer" | "input.keyboard" | "input.text" if has_state(rec, "uinput", CapState::Unavailable) => Some(Remediation {
            summary: "Load the 'uinput' kernel module (modprobe uinput); add it to /etc/modules-load.d/ to keep it across reboots.".into(),
            commands: vec![cmd("root", &["modprobe", "uinput"], false)],
            requires_user_approval: true,
        }),
        "brightness" if has_state(rec, "ddc", CapState::PermissionRequired) => {
            let dev = std::fs::read_dir(root.join("dev")).ok().and_then(|rd| {
                rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("i2c-")).min()
            });
            Some(add_user_to_group("/dev/i2c-*", dev.and_then(|d| device_group(root, &format!("dev/{d}")))))
        }
        "screen.stream.portal" if has_state(rec, "portal-screencast", CapState::Unavailable) => {
            let pkg = match compositor {
                "hyprland" => "xdg-desktop-portal-hyprland",
                "sway" => "xdg-desktop-portal-wlr",
                _ => "xdg-desktop-portal",
            };
            let verified = os.package_manager == PackageManager::Pacman;
            Some(Remediation {
                summary: format!(
                    "Install a screen-sharing portal backend ({pkg}) and make sure PipeWire is running.{}",
                    if verified { "" } else { " Package name not yet verified on this distribution." }
                ),
                commands: install(os.package_manager, pkg).into_iter().collect(),
                requires_user_approval: true,
            })
        }
        "brightness" if rec.candidates.iter().any(|c| c.backend == "ddc" && c.reason.as_deref().is_some_and(|r| r.contains("ddcutil is not installed"))) => Some(Remediation {
            summary: "Install ddcutil to control external monitor brightness. The monitor must also have DDC/CI enabled in its own menu.".into(),
            commands: install(os.package_manager, "ddcutil").into_iter().collect(),
            requires_user_approval: true,
        }),
        "audio.volume" => Some(Remediation {
            summary: "Start the audio server for your session.".into(),
            commands: vec![cmd("user", &["systemctl", "--user", "enable", "--now", "pipewire", "wireplumber"], false)],
            requires_user_approval: true,
        }),
        _ => None,
    };
    rec.remediation = rem;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::MetadataExt;

    fn os(pm: PackageManager) -> OsInfo {
        OsInfo {
            distro_id: "x".into(),
            distro_like: vec![],
            version_id: None,
            kernel: "".into(),
            arch: "x86_64".into(),
            package_manager: pm,
        }
    }

    fn denied_uinput() -> CapabilityRecord {
        let mut r = CapabilityRecord::not(
            CapState::PermissionRequired,
            "uinput",
            "no write access to /dev/uinput",
        );
        r.state = CapState::PermissionRequired;
        r
    }

    #[test]
    fn names_the_group_that_owns_the_device() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("dev")).unwrap();
        fs::create_dir_all(d.path().join("etc")).unwrap();
        fs::write(d.path().join("dev/uinput"), "").unwrap();
        let gid = fs::metadata(d.path().join("dev/uinput")).unwrap().gid();
        fs::write(
            d.path().join("etc/group"),
            format!("root:x:0:\ninputgrp:x:{gid}:bob\n"),
        )
        .unwrap();
        let mut r = denied_uinput();
        attach_remediation(
            d.path(),
            "input.pointer",
            &mut r,
            &os(PackageManager::Pacman),
            "hyprland",
        );
        let rem = r.remediation.unwrap();
        assert_eq!(
            rem.commands[0].argv,
            ["usermod", "-aG", "inputgrp", "$USER"]
        );
        assert!(rem.commands[0].needs_relogin && rem.requires_user_approval);
    }

    #[test]
    fn root_owned_device_gets_no_command() {
        let d = tempfile::tempdir().unwrap(); // device missing => gid unknown => udev-rule advice, no command
        let mut r = denied_uinput();
        attach_remediation(
            d.path(),
            "input.pointer",
            &mut r,
            &os(PackageManager::Pacman),
            "hyprland",
        );
        assert!(r.remediation.unwrap().commands.is_empty());
    }

    #[test]
    fn portal_install_command_per_package_manager() {
        for (pm, first) in [
            (PackageManager::Pacman, "pacman"),
            (PackageManager::Dnf, "dnf"),
            (PackageManager::Apt, "apt"),
            (PackageManager::Zypper, "zypper"),
        ] {
            let mut r = CapabilityRecord::not(CapState::Unavailable, "portal-screencast", "none");
            attach_remediation(
                Path::new("/nonexistent"),
                "screen.stream.portal",
                &mut r,
                &os(pm),
                "hyprland",
            );
            let rem = r.remediation.unwrap();
            assert_eq!(rem.commands[0].argv[0], first);
            assert!(rem.commands[0]
                .argv
                .contains(&"xdg-desktop-portal-hyprland".to_string()));
            assert_eq!(
                rem.summary.contains("not yet verified"),
                pm != PackageManager::Pacman
            );
        }
        let mut r = CapabilityRecord::not(CapState::Unavailable, "portal-screencast", "none");
        attach_remediation(
            Path::new("/nonexistent"),
            "screen.stream.portal",
            &mut r,
            &os(PackageManager::Unknown),
            "hyprland",
        );
        assert!(r.remediation.unwrap().commands.is_empty());
    }

    #[test]
    fn available_capabilities_get_nothing() {
        let mut r = CapabilityRecord::available("uinput");
        attach_remediation(
            Path::new("/"),
            "input.pointer",
            &mut r,
            &os(PackageManager::Pacman),
            "hyprland",
        );
        assert!(r.remediation.is_none());
    }
}
