//! Screen brightness: external monitors over DDC/CI (`ddcutil`) and built-in backlights (`brightnessctl`).
//!
//! Design (DDC is slow, ~0.3 s a call, and monitors never announce changes):
//! * the state is **cached**; it is re-read from the hardware only on `refresh` (the phone asks when its
//!   screen opens), never on a timer;
//! * writes are **coalesced**: dragging a slider keeps only the latest value per display, applied by one
//!   worker with a short pause between writes, so the monitor is never flooded;
//! * nothing the phone sends reaches a command line except a number: the display is looked up by id in
//!   the list this crate detected itself, and the bus / device name comes from that list;
//! * monitor serial numbers appear in `ddcutil` output and are never read into any type.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::{mpsc, Notify};
use zenvy_core::{Error, Result};
use zenvy_proto::{BrightnessCommand, BrightnessDisplay, BrightnessKind, BrightnessState};

const READ_TIMEOUT: Duration = Duration::from_secs(8);
const DETECT_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_PAUSE: Duration = Duration::from_millis(120);
/// A backlight at 0 can switch a laptop panel off entirely; never let a phone do that.
const MIN_BACKLIGHT_PERCENT: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DdcDisplay {
    pub bus: u32,
    /// Hyprland-style connector, e.g. `HDMI-A-1` (the `cardN-` prefix removed).
    pub connector: String,
    pub model: String,
}

/// Parses `ddcutil detect --brief`. Only blocks starting with "Display N" (a monitor that answers DDC) count; the
/// serial number in the `Monitor:` line is dropped on purpose.
pub fn parse_detect(out: &str) -> Vec<DdcDisplay> {
    let mut v = Vec::new();
    let mut cur: Option<(Option<u32>, String, String)> = None;
    let flush = |cur: &mut Option<(Option<u32>, String, String)>, v: &mut Vec<DdcDisplay>| {
        if let Some((Some(bus), connector, model)) = cur.take() {
            v.push(DdcDisplay {
                bus,
                connector,
                model,
            });
        }
    };
    for line in out.lines() {
        let t = line.trim();
        if t.starts_with("Display ")
            && t[8..].trim().chars().all(|c| c.is_ascii_digit())
            && !t[8..].trim().is_empty()
        {
            flush(&mut cur, &mut v);
            cur = Some((None, String::new(), String::new()));
        } else if t.starts_with("Invalid display")
            || t.starts_with("Phantom display")
            || t.starts_with("Display not")
        {
            flush(&mut cur, &mut v);
            cur = None;
        } else if let Some((bus, conn, model)) = cur.as_mut() {
            if let Some(r) = t.strip_prefix("I2C bus:") {
                *bus = r
                    .trim()
                    .strip_prefix("/dev/i2c-")
                    .and_then(|n| n.parse().ok());
            } else if let Some(r) = t.strip_prefix("DRM connector:") {
                let c = r.trim();
                // "card1-HDMI-A-1" -> "HDMI-A-1"
                *conn = c
                    .split_once('-')
                    .filter(|(a, _)| a.starts_with("card"))
                    .map(|(_, b)| b)
                    .unwrap_or(c)
                    .to_string();
            } else if let Some(r) = t.strip_prefix("Monitor:") {
                // "MFG:MODEL:SERIAL": keep the model, never the serial
                *model = r
                    .trim()
                    .split(':')
                    .nth(1)
                    .unwrap_or("")
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(60)
                    .collect();
            }
        }
    }
    flush(&mut cur, &mut v);
    v
}

/// `VCP 10 C 100 100` -> (current, max). Anything else (unsupported feature, error) is `None`.
pub fn parse_vcp(out: &str) -> Option<(u32, u32)> {
    let p: Vec<&str> = out.split_whitespace().collect();
    if p.len() >= 5 && p[0] == "VCP" && p[2] == "C" {
        let (c, m) = (p[3].parse().ok()?, p[4].parse().ok()?);
        (m > 0).then_some((c, m))
    } else {
        None
    }
}

/// `brightnessctl -m` lines: `name,class,current,percent%,max`.
pub fn parse_backlights(out: &str) -> Vec<(String, u32, u32)> {
    out.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.trim().split(',').collect();
            if f.len() < 5 || f[1] != "backlight" {
                return None;
            }
            let name = f[0];
            let ok = !name.is_empty()
                && name.len() <= 64
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b));
            let (cur, max): (u32, u32) = (f[2].parse().ok()?, f[4].parse().ok()?);
            (ok && max > 0).then(|| (name.to_string(), cur, max))
        })
        .collect()
}

pub fn to_percent(cur: u32, max: u32) -> u8 {
    ((u64::from(cur) * 100 + u64::from(max) / 2) / u64::from(max.max(1))).min(100) as u8
}

pub fn from_percent(percent: u8, max: u32) -> u32 {
    ((u64::from(percent.min(100)) * u64::from(max) + 50) / 100) as u32
}

#[derive(Clone, Debug)]
enum Target {
    Ddc { bus: u32 },
    Backlight { device: String },
}

#[derive(Clone, Debug)]
struct Entry {
    id: String,
    name: String,
    model: String,
    target: Target,
    max: u32,
    percent: u8,
}

struct Inner {
    ddcutil: Option<PathBuf>,
    brightnessctl: Option<PathBuf>,
    entries: Mutex<Vec<Entry>>,
    pending: Mutex<HashMap<String, u8>>,
    wake: Notify,
    changed: Mutex<Option<mpsc::Sender<()>>>,
}

#[derive(Clone)]
pub struct BrightnessAdapter {
    inner: Arc<Inner>,
}

async fn run(bin: &PathBuf, args: &[&str], limit: Duration) -> Result<String> {
    let child = Command::new(bin)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(Error::Io)?;
    let out = tokio::time::timeout(limit, child.wait_with_output())
        .await
        .map_err(|_| Error::Timeout(limit))?
        .map_err(Error::Io)?;
    if !out.status.success() {
        return Err(Error::Rejected(format!(
            "{} failed ({})",
            bin.display(),
            out.status
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

impl BrightnessAdapter {
    /// Finds every controllable display (slow: DDC probing). Displays that do not answer a brightness read are left out.
    pub async fn detect(ddcutil: Option<PathBuf>, brightnessctl: Option<PathBuf>) -> Self {
        let mut entries = Vec::new();
        if let Some(d) = &ddcutil {
            if let Ok(out) = run(d, &["detect", "--brief"], DETECT_TIMEOUT).await {
                for disp in parse_detect(&out) {
                    let bus = disp.bus.to_string();
                    if let Ok(v) =
                        run(d, &["getvcp", "10", "--bus", &bus, "--brief"], READ_TIMEOUT).await
                    {
                        if let Some((cur, max)) = parse_vcp(&v) {
                            entries.push(Entry {
                                id: format!("ddc:{}", disp.bus),
                                name: disp.connector,
                                model: disp.model,
                                target: Target::Ddc { bus: disp.bus },
                                max,
                                percent: to_percent(cur, max),
                            });
                        }
                    }
                }
            }
        }
        if let Some(b) = &brightnessctl {
            if let Ok(out) = run(b, &["-m", "-c", "backlight", "-l"], READ_TIMEOUT).await {
                for (name, cur, max) in parse_backlights(&out) {
                    entries.push(Entry {
                        id: format!("bl:{name}"),
                        name: name.clone(),
                        model: String::new(),
                        target: Target::Backlight { device: name },
                        max,
                        percent: to_percent(cur, max),
                    });
                }
            }
        }
        let inner = Arc::new(Inner {
            ddcutil,
            brightnessctl,
            entries: Mutex::new(entries),
            pending: Mutex::new(HashMap::new()),
            wake: Notify::new(),
            changed: Mutex::new(None),
        });
        tokio::spawn(worker(inner.clone()));
        Self { inner }
    }

    pub fn is_empty(&self) -> bool {
        self.inner.entries.lock().unwrap().is_empty()
    }

    /// The cached state (no hardware access).
    pub fn state(&self) -> BrightnessState {
        BrightnessState {
            displays: self
                .inner
                .entries
                .lock()
                .unwrap()
                .iter()
                .map(|e| BrightnessDisplay {
                    id: e.id.clone(),
                    name: e.name.clone(),
                    model: e.model.clone(),
                    kind: if matches!(e.target, Target::Ddc { .. }) {
                        BrightnessKind::Ddc
                    } else {
                        BrightnessKind::Backlight
                    },
                    percent: e.percent,
                })
                .collect(),
        }
    }

    /// Re-reads the real values from the hardware (a monitor's own buttons change brightness silently).
    pub async fn refresh(&self) {
        let entries = self.inner.entries.lock().unwrap().clone();
        let mut fresh = Vec::new();
        for mut e in entries {
            let v = match &e.target {
                Target::Ddc { bus } => match &self.inner.ddcutil {
                    Some(d) => run(
                        d,
                        &["getvcp", "10", "--bus", &bus.to_string(), "--brief"],
                        READ_TIMEOUT,
                    )
                    .await
                    .ok()
                    .and_then(|o| parse_vcp(&o)),
                    None => None,
                },
                Target::Backlight { device } => match &self.inner.brightnessctl {
                    Some(b) => run(b, &["-d", device, "-m", "info"], READ_TIMEOUT)
                        .await
                        .ok()
                        .and_then(|o| parse_backlights(&o).into_iter().next())
                        .map(|(_, c, m)| (c, m)),
                    None => None,
                },
            };
            if let Some((cur, max)) = v {
                e.max = max;
                e.percent = to_percent(cur, max);
            }
            fresh.push(e);
        }
        // keep any display that was added or removed while we were reading untouched: only update known ids
        let mut live = self.inner.entries.lock().unwrap();
        for f in fresh {
            if let Some(slot) = live.iter_mut().find(|x| x.id == f.id) {
                *slot = f;
            }
        }
    }

    /// Queues a brightness change and returns at once. Later values for the same display replace earlier unapplied ones.
    pub fn set(&self, id: &str, percent: u8) -> Result<()> {
        BrightnessCommand::Set {
            id: id.to_string(),
            percent,
        }
        .validate()
        .map_err(Error::InvalidCommand)?;
        let mut entries = self.inner.entries.lock().unwrap();
        let e = entries
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or_else(|| Error::Rejected("no such display".into()))?;
        let p = if matches!(e.target, Target::Backlight { .. }) {
            percent.max(MIN_BACKLIGHT_PERCENT)
        } else {
            percent
        };
        e.percent = p; // optimistic, so the phone's state matches immediately
        drop(entries);
        self.inner.pending.lock().unwrap().insert(id.to_string(), p);
        self.inner.wake.notify_one();
        Ok(())
    }

    /// Emits `()` after the worker applied changes (so the agent can push the new state to phones).
    pub fn watch(&self) -> mpsc::Receiver<()> {
        let (tx, rx) = mpsc::channel(4);
        *self.inner.changed.lock().unwrap() = Some(tx);
        rx
    }
}

async fn worker(inner: Arc<Inner>) {
    loop {
        inner.wake.notified().await;
        loop {
            let batch: Vec<(String, u8)> = inner.pending.lock().unwrap().drain().collect();
            if batch.is_empty() {
                break;
            }
            for (id, percent) in batch {
                let entry = inner
                    .entries
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|e| e.id == id)
                    .cloned();
                let Some(e) = entry else { continue };
                let raw = from_percent(percent, e.max);
                let res = match &e.target {
                    Target::Ddc { bus } => match &inner.ddcutil {
                        Some(d) => run(
                            d,
                            &[
                                "setvcp",
                                "10",
                                &raw.to_string(),
                                "--bus",
                                &bus.to_string(),
                                "--noverify",
                            ],
                            READ_TIMEOUT,
                        )
                        .await
                        .map(drop),
                        None => Err(Error::Rejected("ddcutil missing".into())),
                    },
                    Target::Backlight { device } => match &inner.brightnessctl {
                        Some(b) => run(
                            b,
                            &["-d", device, "set", &raw.max(1).to_string()],
                            READ_TIMEOUT,
                        )
                        .await
                        .map(drop),
                        None => Err(Error::Rejected("brightnessctl missing".into())),
                    },
                };
                if let Err(err) = res {
                    tracing::warn!("brightness write failed for {id}: {err}");
                }
                tokio::time::sleep(WRITE_PAUSE).await;
            }
            let tx = inner.changed.lock().unwrap().clone();
            if let Some(tx) = tx {
                let _ = tx.try_send(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    // Real `ddcutil detect --brief` output from the reference machine, with the serial replaced.
    const DETECT: &str = "Display 1\n   I2C bus:          /dev/i2c-2\n   DRM connector:    card1-HDMI-A-1\n   drm_connector_id: 393\n   Monitor:          MSI:MSI MP225:SERIAL-SECRET-123\n\nInvalid display\n   I2C bus:          /dev/i2c-5\n   DRM connector:    card1-DP-1\n   Monitor:          :Unknown:\n\nDisplay 2\n   I2C bus:          /dev/i2c-7\n   DRM connector:    card0-eDP-1\n   Monitor:          BOE:NE156QHM:ABC\n";

    #[test]
    fn detect_keeps_working_displays_and_drops_serials() {
        let d = parse_detect(DETECT);
        assert_eq!(
            d,
            vec![
                DdcDisplay {
                    bus: 2,
                    connector: "HDMI-A-1".into(),
                    model: "MSI MP225".into()
                },
                DdcDisplay {
                    bus: 7,
                    connector: "eDP-1".into(),
                    model: "NE156QHM".into()
                }
            ]
        );
        assert!(!format!("{d:?}").contains("SECRET"));
        assert!(parse_detect("").is_empty() && parse_detect("garbage\nDisplay x\n").is_empty());
    }

    #[test]
    fn vcp_and_backlight_parsing() {
        assert_eq!(parse_vcp("VCP 10 C 100 100\n"), Some((100, 100)));
        assert_eq!(parse_vcp("VCP 10 C 37 80"), Some((37, 80)));
        for bad in [
            "VCP 10 ERR",
            "VCP 10 SNC x",
            "",
            "VCP 10 C 5 0",
            "VCP 10 C a b",
        ] {
            assert_eq!(parse_vcp(bad), None, "{bad:?}");
        }
        // backlight class only: the NIC LED that `brightnessctl -l` also lists must be ignored
        let out = "intel_backlight,backlight,400,40%,1000\nenp5s0-3::lan,leds,0,0%,255\nbad name,backlight,1,1%,2\n";
        assert_eq!(
            parse_backlights(out),
            vec![("intel_backlight".to_string(), 400, 1000)]
        );
    }

    #[test]
    fn percent_conversion_round_trips() {
        assert_eq!(
            (
                to_percent(100, 100),
                to_percent(37, 80),
                to_percent(0, 255),
                to_percent(255, 255)
            ),
            (100, 46, 0, 100)
        );
        // fine scales (>= 100 steps) round-trip exactly; a coarse one (80 steps) cannot represent every percent, but stays within 1
        for max in [100u32, 255, 7500, 96000] {
            for p in 0..=100u8 {
                assert_eq!(
                    to_percent(from_percent(p, max), max),
                    p,
                    "max {max} percent {p}"
                );
            }
        }
        for p in 0..=100u8 {
            assert!(
                to_percent(from_percent(p, 80), 80).abs_diff(p) <= 1,
                "coarse scale drifted at {p}"
            );
        }
    }

    /// Fake tools with fixed answers that log their argv.
    fn fakes(dir: &std::path::Path) -> (PathBuf, PathBuf) {
        let log = dir.join("calls.log");
        let mk = |name: &str, body: String| {
            let p = dir.join(name);
            std::fs::write(
                &p,
                format!("#!/bin/sh\necho \"$@\" >> {}\n{body}", log.display()),
            )
            .unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        let ddc = mk("ddcutil", format!("case \"$1\" in\n detect) cat <<'J'\n{DETECT}\nJ\n;;\n getvcp) echo 'VCP 10 C 50 100';;\n esac\n"));
        let bl = mk("brightnessctl", "case \"$*\" in\n *-l*) echo 'intel_backlight,backlight,400,40%,1000';;\n *info*) echo 'intel_backlight,backlight,400,40%,1000';;\n esac\n".into());
        (ddc, bl)
    }

    fn calls(dir: &std::path::Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    #[tokio::test]
    async fn detects_and_sets_with_fixed_argv_and_coalesces() {
        let d = tempfile::tempdir().unwrap();
        let (ddc, bl) = fakes(d.path());
        let a = BrightnessAdapter::detect(Some(ddc), Some(bl)).await;
        let st = a.state();
        assert_eq!(
            st.displays
                .iter()
                .map(|x| (x.id.as_str(), x.name.as_str(), x.percent))
                .collect::<Vec<_>>(),
            vec![
                ("ddc:2", "HDMI-A-1", 50),
                ("ddc:7", "eDP-1", 50),
                ("bl:intel_backlight", "intel_backlight", 40)
            ]
        );
        assert!(
            !serde_json::to_string(&st).unwrap().contains("SECRET"),
            "no serial number reaches the state"
        );
        let mut rx = a.watch();
        for p in [10, 20, 30, 80] {
            a.set("ddc:2", p).unwrap(); // a slider drag
        }
        tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let sets: Vec<String> = calls(d.path())
            .into_iter()
            .filter(|l| l.starts_with("setvcp"))
            .collect();
        assert_eq!(sets.last().unwrap(), "setvcp 10 80 --bus 2 --noverify");
        assert!(
            sets.len() <= 2,
            "a burst of four values is coalesced into at most two writes, got {sets:?}"
        );
        assert_eq!(a.state().displays[0].percent, 80);
        a.set("bl:intel_backlight", 0).unwrap();
        tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            calls(d.path()).contains(&"-d intel_backlight set 10".to_string()),
            "backlight never goes below 1%: {:?}",
            calls(d.path())
        );
    }

    #[tokio::test]
    async fn unknown_or_malformed_displays_never_reach_the_tools() {
        let d = tempfile::tempdir().unwrap();
        let (ddc, bl) = fakes(d.path());
        let a = BrightnessAdapter::detect(Some(ddc), Some(bl)).await;
        for bad in ["ddc:9", "bl:other", "ddc:2; rm -rf ~", "../../x", ""] {
            assert!(a.set(bad, 50).is_err(), "{bad:?}");
        }
        assert!(a.set("ddc:2", 101).is_err());
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            !calls(d.path())
                .iter()
                .any(|l| l.starts_with("setvcp") || l.contains(" set ")),
            "no write happened: {:?}",
            calls(d.path())
        );
    }

    #[tokio::test]
    async fn missing_tools_mean_no_displays_not_a_crash() {
        let a = BrightnessAdapter::detect(Some("/nonexistent/ddcutil".into()), None).await;
        assert!(a.is_empty() && a.state().displays.is_empty());
        assert!(a.set("ddc:2", 50).is_err());
        a.refresh().await;
    }
}
