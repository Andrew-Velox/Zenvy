//! Power control (lock, suspend, reboot, power off) and lock-screen detection.
//!
//! * Every action is a fixed argument vector for `systemctl` or `loginctl`; nothing from a phone is ever
//!   put on a command line.
//! * "Is the PC locked?" has no single answer on Wayland. Hyprland itself knows (a session lock shows up as `LOCK` in a
//!   monitor's `solitaryBlockedBy`, verified on 0.56.2 with Quickshell's lock) and that is trusted first; otherwise
//!   logind's `LockedHint`, a running locker process or a user-supplied command are combined, and the result is
//!   `Unknown` when none can be read. A wrong *Locked* would be the dangerous mistake (a password typed
//!   into a visible window), so *Locked* needs a positive signal.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use zenvy_proto::{LockState, PowerAction};

const RUN_TIMEOUT: Duration = Duration::from_secs(15);

/// How to recognise a locked screen beyond logind's hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockConfig {
    /// Process names (`comm`) that only run while the screen is locked.
    pub processes: Vec<String>,
    /// Optional authoritative command: exit 0 = locked, exit 1 = unlocked, anything else = unknown.
    pub detect_cmd: Vec<String>,
    /// Ask Hyprland (`hyprctl -j monitors`) whether a session lock is active. Needs `hyprctl` to be given to the adapter.
    pub use_compositor: bool,
}

impl Default for LockConfig {
    fn default() -> Self {
        Self {
            processes: ["hyprlock", "swaylock", "gtklock", "waylock", "i3lock"]
                .map(String::from)
                .to_vec(),
            detect_cmd: Vec::new(),
            use_compositor: true,
        }
    }
}

/// Which actions the system will allow (from logind's `Can*` answers).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Can {
    pub lock: bool,
    pub suspend: bool,
    pub reboot: bool,
    pub poweroff: bool,
}

/// Raw lock signals, for the calibration tool.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LockSignals {
    /// Hyprland's own answer, when it can be read.
    pub compositor: Option<bool>,
    pub locked_hint: Option<bool>,
    pub processes: Vec<String>,
    pub detect_cmd: Option<i32>,
}

pub struct PowerAdapter {
    systemctl: PathBuf,
    loginctl: PathBuf,
    hyprctl: Option<PathBuf>,
    session: Option<String>,
    lock_cfg: LockConfig,
    can: Can,
}

struct Out {
    ok: bool,
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

async fn run(program: &Path, args: &[&str]) -> Option<Out> {
    let child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let o = tokio::time::timeout(RUN_TIMEOUT, child.wait_with_output())
        .await
        .ok()?
        .ok()?;
    Some(Out {
        ok: o.status.success(),
        code: o.status.code(),
        stdout: String::from_utf8_lossy(&o.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&o.stderr).trim().to_string(),
    })
}

fn my_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").ok().map(|m| m.uid())
}

/// A conservative session id: digits/letters only (it ends up as a `loginctl` argument).
fn clean_id(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty() && s.len() <= 32 && s.chars().all(|c| c.is_ascii_alphanumeric()))
        .then(|| s.to_string())
}

fn process_running(names: &[String]) -> Vec<String> {
    let mut found = Vec::new();
    if names.is_empty() {
        return found;
    }
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return found;
    };
    for e in rd.flatten() {
        let n = e.file_name();
        let n = n.to_string_lossy();
        if !n.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if let Ok(comm) = std::fs::read_to_string(e.path().join("comm")) {
            let comm = comm.trim();
            if names.iter().any(|x| x == comm) && !found.iter().any(|f| f == comm) {
                found.push(comm.to_string());
            }
        }
    }
    found
}

impl PowerAdapter {
    /// Finds this user's graphical session and what the system allows. `busctl` (optional) answers the `Can*` questions.
    pub async fn discover(
        systemctl: PathBuf,
        loginctl: PathBuf,
        busctl: Option<PathBuf>,
        hyprctl: Option<PathBuf>,
        lock_cfg: LockConfig,
    ) -> Self {
        let mut session = None;
        if let Some(uid) = my_uid() {
            if let Some(o) = run(
                &loginctl,
                &["show-user", &uid.to_string(), "-p", "Display", "--value"],
            )
            .await
            {
                session = clean_id(&o.stdout);
            }
        }
        if session.is_none() {
            session = std::env::var("XDG_SESSION_ID")
                .ok()
                .and_then(|s| clean_id(&s));
        }
        let mut can = Can {
            lock: session.is_some(),
            suspend: true,
            reboot: true,
            poweroff: true,
        };
        if let Some(b) = busctl {
            for (method, slot) in [
                ("CanSuspend", &mut can.suspend),
                ("CanReboot", &mut can.reboot),
                ("CanPowerOff", &mut can.poweroff),
            ] {
                if let Some(o) = run(
                    &b,
                    &[
                        "--system",
                        "call",
                        "org.freedesktop.login1",
                        "/org/freedesktop/login1",
                        "org.freedesktop.login1.Manager",
                        method,
                    ],
                )
                .await
                {
                    // `s "yes"`, `s "challenge"` (needs authentication a phone cannot give: treated as no), `s "na"`, `s "no"`
                    *slot = o.ok && o.stdout.contains("\"yes\"");
                }
            }
        }
        Self {
            systemctl,
            loginctl,
            hyprctl,
            session,
            lock_cfg,
            can,
        }
    }

    pub fn can(&self) -> Can {
        self.can
    }

    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    /// Runs one fixed command. For suspend, reboot and power off the call returns as soon as the system has accepted the job.
    pub async fn execute(&self, action: PowerAction) -> Result<(), String> {
        let allowed = match action {
            PowerAction::Lock => self.can.lock,
            PowerAction::Suspend => self.can.suspend,
            PowerAction::Reboot => self.can.reboot,
            PowerAction::Poweroff => self.can.poweroff,
        };
        if !allowed {
            return Err(format!("this system does not allow {}", action.name()));
        }
        let out = match action {
            PowerAction::Lock => {
                let id = self
                    .session
                    .as_deref()
                    .ok_or("no graphical session found")?;
                run(&self.loginctl, &["lock-session", id]).await
            }
            PowerAction::Suspend => run(&self.systemctl, &["suspend"]).await,
            PowerAction::Reboot => run(&self.systemctl, &["reboot"]).await,
            PowerAction::Poweroff => run(&self.systemctl, &["poweroff"]).await,
        };
        match out {
            Some(o) if o.ok => Ok(()),
            Some(o) => Err(truncate(if o.stderr.is_empty() {
                format!("exited with {:?}", o.code)
            } else {
                o.stderr
            })),
            None => Err("the command could not be run".into()),
        }
    }

    /// Every signal, unprocessed (for `zenvyd lock-probe`).
    pub async fn lock_signals(&self) -> LockSignals {
        let compositor = match (&self.hyprctl, self.lock_cfg.use_compositor) {
            (Some(h), true) => run(h, &["-j", "monitors"])
                .await
                .filter(|o| o.ok)
                .and_then(|o| monitors_locked(&o.stdout)),
            _ => None,
        };
        let locked_hint = match &self.session {
            Some(id) => run(
                &self.loginctl,
                &["show-session", id, "-p", "LockedHint", "--value"],
            )
            .await
            .and_then(|o| match o.stdout.as_str() {
                "yes" => Some(true),
                "no" => Some(false),
                _ => None,
            }),
            None => None,
        };
        let names = self.lock_cfg.processes.clone();
        let processes = tokio::task::spawn_blocking(move || process_running(&names))
            .await
            .unwrap_or_default();
        let detect_cmd = match self.lock_cfg.detect_cmd.split_first() {
            Some((prog, rest)) => {
                let args: Vec<&str> = rest.iter().map(String::as_str).collect();
                run(Path::new(prog), &args).await.and_then(|o| o.code)
            }
            None => None,
        };
        LockSignals {
            compositor,
            locked_hint,
            processes,
            detect_cmd,
        }
    }

    pub async fn lock_state(&self) -> LockState {
        let s = self.lock_signals().await;
        classify(&self.lock_cfg, &s)
    }
}

/// The combination rule (pure, so it is unit-tested without a system).
pub fn classify(cfg: &LockConfig, s: &LockSignals) -> LockState {
    if !cfg.detect_cmd.is_empty() {
        return match s.detect_cmd {
            Some(0) => LockState::Locked,
            Some(1) => LockState::Unlocked,
            _ => LockState::Unknown,
        };
    }
    // Hyprland is the authority on its own session lock, whatever the locker program is
    if let Some(c) = s.compositor {
        return if c {
            LockState::Locked
        } else {
            LockState::Unlocked
        };
    }
    if s.locked_hint == Some(true) || !s.processes.is_empty() {
        return LockState::Locked;
    }
    if s.locked_hint == Some(false) {
        return LockState::Unlocked;
    }
    LockState::Unknown
}

/// `hyprctl -j monitors`: a session lock is reported as `LOCK` in a monitor's `solitaryBlockedBy`.
/// `None` when the field is missing everywhere (an older Hyprland), so the answer is never guessed.
pub fn monitors_locked(json: &str) -> Option<bool> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let mut seen = false;
    let mut locked = false;
    for m in v.as_array()? {
        if let Some(b) = m.get("solitaryBlockedBy").and_then(|x| x.as_array()) {
            seen = true;
            locked |= b.iter().any(|s| s == "LOCK");
        }
    }
    seen.then_some(locked)
}

fn truncate(mut s: String) -> String {
    if s.len() > 200 {
        s.truncate(200);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    /// No Hyprland, no logind hints or lockers: the plain case.
    fn plain() -> LockConfig {
        LockConfig {
            processes: vec![],
            detect_cmd: vec![],
            use_compositor: false,
        }
    }

    /// Fakes that log their arguments, and answer `show-user`/`show-session` from files.
    fn fakes(d: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let log = d.join("log");
        let ctl = script(d, "loginctl", &format!("echo \"loginctl $*\" >> {l}\ncase \"$1\" in show-user) echo 7;; show-session) cat {d}/hint 2>/dev/null || echo no;; lock-session) [ -f {d}/fail ] && {{ echo 'Access denied' >&2; exit 1; }}; exit 0;; esac", l = log.display(), d = d.display()));
        let sys = script(d, "systemctl", &format!("echo \"systemctl $*\" >> {l}\n[ -f {d}/fail ] && {{ echo 'Operation inhibited' >&2; exit 1; }}\nexit 0", l = log.display(), d = d.display()));
        let bus = script(d, "busctl", "case \"$*\" in *CanSuspend*) echo 's \"yes\"';; *CanReboot*) echo 's \"challenge\"';; *CanPowerOff*) echo 's \"yes\"';; esac");
        (sys, ctl, bus)
    }

    fn calls(d: &Path) -> Vec<String> {
        std::fs::read_to_string(d.join("log"))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    async fn adapter(d: &Path, cfg: LockConfig) -> PowerAdapter {
        let (sys, ctl, bus) = fakes(d);
        PowerAdapter::discover(sys, ctl, Some(bus), None, cfg).await
    }

    #[tokio::test]
    async fn discovers_the_session_and_what_the_system_allows() {
        let d = tempfile::tempdir().unwrap();
        let p = adapter(d.path(), LockConfig::default()).await;
        assert_eq!(p.session(), Some("7"));
        let c = p.can();
        assert!(c.lock && c.suspend && c.poweroff, "{c:?}");
        assert!(
            !c.reboot,
            "'challenge' means a password prompt a phone cannot answer: not offered"
        );
    }

    #[tokio::test]
    async fn actions_run_fixed_commands_and_report_failures() {
        let d = tempfile::tempdir().unwrap();
        let p = adapter(d.path(), LockConfig::default()).await;
        p.execute(PowerAction::Lock).await.unwrap();
        p.execute(PowerAction::Suspend).await.unwrap();
        p.execute(PowerAction::Poweroff).await.unwrap();
        assert!(
            p.execute(PowerAction::Reboot).await.is_err(),
            "not allowed by the system"
        );
        let c = calls(d.path());
        assert!(
            c.contains(&"loginctl lock-session 7".to_string())
                && c.contains(&"systemctl suspend".to_string())
                && c.contains(&"systemctl poweroff".to_string()),
            "{c:?}"
        );
        assert!(
            !c.iter().any(|l| l.contains("reboot")),
            "a refused action never reaches the system: {c:?}"
        );
        std::fs::write(d.path().join("fail"), "").unwrap();
        let e = p.execute(PowerAction::Suspend).await.unwrap_err();
        assert!(
            e.contains("inhibited"),
            "the system's reason is passed on: {e}"
        );
    }

    #[test]
    fn the_combination_rule_requires_a_positive_signal_for_locked() {
        let cfg = LockConfig::default();
        let s = |h, p: &[&str], c| LockSignals {
            compositor: None,
            locked_hint: h,
            processes: p.iter().map(|x| x.to_string()).collect(),
            detect_cmd: c,
        };
        assert_eq!(classify(&cfg, &s(Some(true), &[], None)), LockState::Locked);
        assert_eq!(
            classify(&cfg, &s(Some(false), &["hyprlock"], None)),
            LockState::Locked,
            "a locker process wins over a stale hint"
        );
        assert_eq!(
            classify(&cfg, &s(Some(false), &[], None)),
            LockState::Unlocked
        );
        assert_eq!(
            classify(&cfg, &s(None, &[], None)),
            LockState::Unknown,
            "no signal at all is unknown, never locked"
        );
        let custom = LockConfig {
            processes: vec![],
            detect_cmd: vec!["x".into()],
            use_compositor: false,
        };
        assert_eq!(
            classify(&custom, &s(Some(true), &["hyprlock"], Some(1))),
            LockState::Unlocked,
            "a configured command is authoritative"
        );
        assert_eq!(
            classify(&custom, &s(Some(false), &[], Some(0))),
            LockState::Locked
        );
        assert_eq!(
            classify(&custom, &s(Some(true), &[], Some(7))),
            LockState::Unknown
        );
        assert_eq!(
            classify(&custom, &s(Some(true), &[], None)),
            LockState::Unknown
        );
    }

    #[test]
    fn hyprland_is_the_authority_when_it_answers() {
        let cfg = LockConfig::default();
        let sig = |c, h, p: &[&str]| LockSignals {
            compositor: c,
            locked_hint: h,
            processes: p.iter().map(|x| x.to_string()).collect(),
            detect_cmd: None,
        };
        assert_eq!(
            classify(&cfg, &sig(Some(true), Some(false), &[])),
            LockState::Locked,
            "the case on this PC: Quickshell's lock leaves logind's hint false"
        );
        assert_eq!(
            classify(&cfg, &sig(Some(false), Some(true), &["hyprlock"])),
            LockState::Unlocked,
            "a stale hint or a leftover process cannot keep the phone in locked mode"
        );
        assert_eq!(
            classify(&cfg, &sig(None, Some(true), &[])),
            LockState::Locked,
            "without Hyprland's answer the other signals decide"
        );
    }

    #[test]
    fn the_monitor_json_is_read_conservatively() {
        // the shapes recorded on a real Hyprland 0.56.2 while unlocked and while locked
        let unlocked = r#"[{"id":0,"solitaryBlockedBy":["WINDOWED","CANDIDATE"]}]"#;
        let locked = r#"[{"id":0,"solitaryBlockedBy":["LOCK","WINDOWED","OFFSET","CANDIDATE"]}]"#;
        let two =
            r#"[{"id":0,"solitaryBlockedBy":["WINDOWED"]},{"id":1,"solitaryBlockedBy":["LOCK"]}]"#;
        assert_eq!(monitors_locked(unlocked), Some(false));
        assert_eq!(monitors_locked(locked), Some(true));
        assert_eq!(
            monitors_locked(two),
            Some(true),
            "any monitor under the lock means locked"
        );
        assert_eq!(
            monitors_locked(r#"[{"id":0}]"#),
            None,
            "an older Hyprland without the field: no guess"
        );
        assert_eq!(monitors_locked("[]"), None);
        assert_eq!(monitors_locked("not json"), None);
        assert_eq!(monitors_locked(r#"{"error":"x"}"#), None);
    }

    #[tokio::test]
    async fn a_fake_hyprctl_drives_the_state() {
        let d = tempfile::tempdir().unwrap();
        let (sys, ctl, bus) = fakes(d.path());
        let hypr = script(d.path(), "hyprctl", &format!("if [ -f {d}/lockflag ]; then echo '[{{\"solitaryBlockedBy\":[\"LOCK\",\"CANDIDATE\"]}}]'; else echo '[{{\"solitaryBlockedBy\":[\"CANDIDATE\"]}}]'; fi", d = d.path().display()));
        let on = LockConfig {
            processes: vec![],
            detect_cmd: vec![],
            use_compositor: true,
        };
        let p = PowerAdapter::discover(
            sys.clone(),
            ctl.clone(),
            Some(bus.clone()),
            Some(hypr.clone()),
            on,
        )
        .await;
        assert_eq!(p.lock_state().await, LockState::Unlocked);
        std::fs::write(d.path().join("lockflag"), "").unwrap();
        assert_eq!(p.lock_state().await, LockState::Locked);
        let off = PowerAdapter::discover(sys, ctl, Some(bus), Some(hypr), plain()).await;
        assert_eq!(
            off.lock_state().await,
            LockState::Unlocked,
            "switched off in the config: Hyprland is not asked"
        );
    }

    #[tokio::test]
    async fn locked_hint_process_and_command_signals_are_read() {
        let d = tempfile::tempdir().unwrap();
        let p = adapter(d.path(), plain()).await;
        assert_eq!(p.lock_state().await, LockState::Unlocked);
        std::fs::write(d.path().join("hint"), "yes\n").unwrap();
        assert_eq!(p.lock_state().await, LockState::Locked);
        std::fs::write(d.path().join("hint"), "no\n").unwrap();

        // a locker process (a symlink named like the locker, so the kernel reports that name)
        let name = "zvlockertest";
        std::os::unix::fs::symlink("/bin/sleep", d.path().join(name)).unwrap();
        let mut child = std::process::Command::new(d.path().join(name))
            .arg("5")
            .spawn()
            .unwrap();
        let q = adapter(
            d.path(),
            LockConfig {
                processes: vec![name.into()],
                detect_cmd: vec![],
                use_compositor: false,
            },
        )
        .await;
        assert_eq!(q.lock_state().await, LockState::Locked);
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(q.lock_state().await, LockState::Unlocked);

        // an authoritative command
        let yes = script(d.path(), "is-locked", "exit 0");
        let no = script(d.path(), "is-unlocked", "exit 1");
        let r = adapter(
            d.path(),
            LockConfig {
                processes: vec![],
                detect_cmd: vec![yes.display().to_string()],
                use_compositor: false,
            },
        )
        .await;
        assert_eq!(r.lock_state().await, LockState::Locked);
        let r = adapter(
            d.path(),
            LockConfig {
                processes: vec![],
                detect_cmd: vec![no.display().to_string()],
                use_compositor: false,
            },
        )
        .await;
        assert_eq!(r.lock_state().await, LockState::Unlocked);
    }

    #[tokio::test]
    async fn a_missing_tool_is_unknown_not_locked() {
        let p = PowerAdapter::discover(
            "/nonexistent/systemctl".into(),
            "/nonexistent/loginctl".into(),
            None,
            None,
            plain(),
        )
        .await;
        assert_eq!(p.lock_state().await, LockState::Unknown);
        assert!(p.execute(PowerAction::Suspend).await.is_err());
    }

    #[test]
    fn session_ids_are_sanitised() {
        assert_eq!(clean_id("2\n"), Some("2".into()));
        assert_eq!(clean_id("c1"), Some("c1".into()));
        for bad in ["", "a b", "x;y", "../1", &"1".repeat(40)] {
            assert_eq!(clean_id(bad), None, "{bad:?}");
        }
    }
}
