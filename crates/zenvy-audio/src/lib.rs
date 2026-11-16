//! Audio control through `pactl` (PulseAudio protocol, served natively by PipeWire).
//!
//! * Fixed argv, no shell, hard timeouts; the only caller-supplied value that reaches a command line is a
//!   device name, and that is checked against the live device list first.
//! * Event driven: `pactl subscribe` reports sink/source/server changes, which are debounced into one
//!   notification, so an idle system costs nothing.
//! * `pactl info` contains the host and user name; only the two default device names are read from it.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use zenvy_core::{Error, Result};
use zenvy_proto::{AudioCommand, AudioDevice, AudioState, OutputChoice};

const TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Deserialize)]
struct Info {
    #[serde(default)]
    default_sink_name: String,
    #[serde(default)]
    default_source_name: String,
}

#[derive(Deserialize)]
struct Chan {
    #[serde(default)]
    value_percent: String,
}

#[derive(Deserialize)]
struct Dev {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    mute: bool,
    #[serde(default)]
    volume: HashMap<String, Chan>,
}

impl Dev {
    /// Average of the channel percentages ("60%" -> 60). A device without volume reads as 0.
    fn percent(&self) -> u32 {
        let v: Vec<u32> = self
            .volume
            .values()
            .filter_map(|c| c.value_percent.trim_end_matches('%').trim().parse().ok())
            .collect();
        if v.is_empty() {
            0
        } else {
            (v.iter().sum::<u32>() as f64 / v.len() as f64).round() as u32
        }
    }
    fn into_device(self) -> AudioDevice {
        let volume = self.percent();
        AudioDevice {
            name: self.name,
            description: self.description,
            volume,
            muted: self.mute,
        }
    }
}

/// Builds the phone-facing state from `pactl --format=json` output. Pure, so it is tested with real fixtures.
pub fn parse_state(info: &str, sinks: &str, sources: &str) -> Result<AudioState> {
    let info: Info = serde_json::from_str(info)?;
    let sinks: Vec<Dev> = serde_json::from_str(sinks)?;
    let sources: Vec<Dev> = serde_json::from_str(sources)?;
    let outputs = sinks
        .iter()
        .map(|s| OutputChoice {
            name: s.name.clone(),
            description: s.description.clone(),
            default: s.name == info.default_sink_name,
        })
        .collect();
    let output = sinks
        .into_iter()
        .find(|s| s.name == info.default_sink_name)
        .map(Dev::into_device);
    // Monitors are "what the speakers play", not microphones.
    let mic = sources
        .into_iter()
        .find(|s| s.name == info.default_source_name && !s.name.ends_with(".monitor"))
        .map(Dev::into_device);
    Ok(AudioState {
        output,
        mic,
        outputs,
    })
}

/// Does a `pactl subscribe` line describe a change we care about? (`sink-input`, `client`, `card`… are noise.)
pub fn is_relevant_event(line: &str) -> bool {
    let Some(rest) = line.split(" on ").nth(1) else {
        return false;
    };
    matches!(
        rest.split(['#', ' ']).next().unwrap_or(""),
        "sink" | "source" | "server"
    )
}

#[derive(Clone)]
pub struct AudioAdapter {
    pactl: PathBuf,
}

impl AudioAdapter {
    pub fn new(pactl: impl Into<PathBuf>) -> Self {
        Self {
            pactl: pactl.into(),
        }
    }

    async fn run(&self, args: &[&str]) -> Result<String> {
        let child = Command::new(&self.pactl)
            .args(args)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(Error::Io)?;
        let out = tokio::time::timeout(TIMEOUT, child.wait_with_output())
            .await
            .map_err(|_| Error::Timeout(TIMEOUT))?
            .map_err(Error::Io)?;
        if !out.status.success() {
            return Err(Error::Rejected(format!(
                "pactl {} failed ({})",
                args.first().unwrap_or(&""),
                out.status
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub async fn state(&self) -> Result<AudioState> {
        let (info, sinks, sources) = tokio::try_join!(
            self.run(&["--format=json", "info"]),
            self.run(&["--format=json", "list", "sinks"]),
            self.run(&["--format=json", "list", "sources"])
        )?;
        parse_state(&info, &sinks, &sources)
    }

    pub async fn execute(&self, cmd: &AudioCommand) -> Result<()> {
        cmd.validate().map_err(Error::InvalidCommand)?;
        match cmd {
            AudioCommand::SetVolume { percent } => self
                .run(&["set-sink-volume", "@DEFAULT_SINK@", &format!("{percent}%")])
                .await
                .map(drop),
            AudioCommand::SetMute { muted } => self
                .run(&[
                    "set-sink-mute",
                    "@DEFAULT_SINK@",
                    if *muted { "1" } else { "0" },
                ])
                .await
                .map(drop),
            AudioCommand::SetMicMute { muted } => self
                .run(&[
                    "set-source-mute",
                    "@DEFAULT_SOURCE@",
                    if *muted { "1" } else { "0" },
                ])
                .await
                .map(drop),
            AudioCommand::SetDefaultOutput { name } => {
                // Only names that exist right now: a made-up name cannot reach the command line.
                let known = self.state().await?.outputs.iter().any(|o| &o.name == name);
                if !known {
                    return Err(Error::Rejected("no such output device".into()));
                }
                self.run(&["set-default-sink", name]).await.map(drop)
            }
        }
    }

    /// Emits `()` (debounced) whenever the audio state may have changed. Restarts `pactl subscribe` if it exits.
    pub fn watch(&self) -> mpsc::Receiver<()> {
        let (tx, rx) = mpsc::channel(4);
        let pactl = self.pactl.clone();
        tokio::spawn(async move {
            let mut backoff = Duration::from_millis(500);
            loop {
                if tx.is_closed() {
                    return;
                }
                let child = Command::new(&pactl)
                    .arg("subscribe")
                    .env("LC_ALL", "C")
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .kill_on_drop(true)
                    .spawn();
                if let Ok(mut child) = child {
                    if let Some(out) = child.stdout.take() {
                        let mut lines = BufReader::new(out).lines();
                        let mut pending = false;
                        loop {
                            let tick = tokio::time::sleep(Duration::from_millis(80));
                            tokio::pin!(tick);
                            tokio::select! {
                                l = lines.next_line() => match l {
                                    Ok(Some(l)) => { if is_relevant_event(&l) { pending = true; } }
                                    _ => break,
                                },
                                _ = &mut tick, if pending => {
                                    pending = false;
                                    if tx.send(()).await.is_err() { return }
                                }
                            }
                        }
                        backoff = Duration::from_millis(500);
                    }
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(10));
            }
        });
        rx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    // Structure taken from real `pactl --format=json` output on the reference machine.
    const INFO: &str = r#"{"server_string":"/run/user/1000/pulse/native","user_name":"someone","host_name":"somehost","server_name":"PulseAudio (on PipeWire 1.6.8)",
        "default_sink_name":"alsa_output.usb-Headset.analog-stereo","default_source_name":"alsa_input.usb-Headset.mono-fallback"}"#;
    const SINKS: &str = r#"[
      {"index":1,"name":"virtual_sink","description":"Virtual","mute":false,"volume":{"front-left":{"value":1,"value_percent":"51%","db":"x"}},"state":"SUSPENDED"},
      {"index":63,"name":"alsa_output.usb-Headset.analog-stereo","description":"Headset Analog Stereo","mute":true,
       "volume":{"front-left":{"value":39322,"value_percent":"60%","db":"-13 dB"},"front-right":{"value":39322,"value_percent":"61%","db":"-13 dB"}},"state":"RUNNING"}]"#;
    const SOURCES: &str = r#"[
      {"name":"alsa_output.usb-Headset.analog-stereo.monitor","description":"Monitor","mute":false,"volume":{"mono":{"value_percent":"100%"}}},
      {"name":"alsa_input.usb-Headset.mono-fallback","description":"Headset Mono","mute":false,"volume":{"mono":{"value_percent":"67%"}}}]"#;

    #[test]
    fn parses_real_shaped_output() {
        let s = parse_state(INFO, SINKS, SOURCES).unwrap();
        let o = s.output.unwrap();
        assert_eq!(
            (o.name.as_str(), o.volume, o.muted),
            ("alsa_output.usb-Headset.analog-stereo", 61, true),
            "channels are averaged (60 and 61 -> 61 after rounding)"
        );
        assert_eq!(s.outputs.len(), 2);
        assert!(s
            .outputs
            .iter()
            .find(|x| x.default)
            .unwrap()
            .name
            .ends_with("analog-stereo"));
        let m = s.mic.unwrap();
        assert_eq!((m.volume, m.muted), (67, false));
    }

    #[test]
    fn host_and_user_names_are_never_carried() {
        let s = parse_state(INFO, SINKS, SOURCES).unwrap();
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("somehost") && !json.contains("someone"));
    }

    #[test]
    fn a_monitor_is_never_reported_as_the_microphone() {
        let info = r#"{"default_sink_name":"a","default_source_name":"x.monitor"}"#;
        let s = parse_state(
            info,
            "[]",
            r#"[{"name":"x.monitor","description":"M","mute":false,"volume":{}}]"#,
        )
        .unwrap();
        assert!(s.mic.is_none() && s.output.is_none());
    }

    #[test]
    fn missing_fields_do_not_panic() {
        let s = parse_state("{}", r#"[{"name":"s"}]"#, "[]").unwrap();
        assert!(s.output.is_none() && s.outputs.len() == 1 && !s.outputs[0].default);
        assert!(parse_state("not json", "[]", "[]").is_err());
    }

    #[test]
    fn only_device_events_count() {
        for l in [
            "Event 'change' on sink #63",
            "Event 'change' on source #5",
            "Event 'change' on server",
            "Event 'new' on sink #70",
            "Event 'remove' on sink #70",
        ] {
            assert!(is_relevant_event(l), "{l}");
        }
        for l in [
            "Event 'change' on client #425",
            "Event 'change' on sink-input #9",
            "Event 'change' on card #53",
            "Event 'new' on client #1",
            "Event 'change' on source-output #3",
            "garbage",
            "",
        ] {
            assert!(!is_relevant_event(l), "{l}");
        }
    }

    /// A fake `pactl` that records its arguments and serves canned JSON, so commands are verified without touching real audio.
    fn fake(dir: &std::path::Path) -> PathBuf {
        let p = dir.join("pactl");
        let script = format!(
            "#!/bin/sh\necho \"$@\" >> {log}\ncase \"$*\" in\n *info*) cat <<'J'\n{INFO}\nJ\n;;\n *'list sinks'*) cat <<'J'\n{SINKS}\nJ\n;;\n *'list sources'*) cat <<'J'\n{SOURCES}\nJ\n;;\n esac\n",
            log = dir.join("calls.log").display()
        );
        std::fs::write(&p, script).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn calls(dir: &std::path::Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    #[tokio::test]
    async fn commands_become_fixed_argv() {
        let d = tempfile::tempdir().unwrap();
        let a = AudioAdapter::new(fake(d.path()));
        a.execute(&AudioCommand::SetVolume { percent: 42 })
            .await
            .unwrap();
        a.execute(&AudioCommand::SetMute { muted: true })
            .await
            .unwrap();
        a.execute(&AudioCommand::SetMicMute { muted: false })
            .await
            .unwrap();
        a.execute(&AudioCommand::SetDefaultOutput {
            name: "virtual_sink".into(),
        })
        .await
        .unwrap();
        let c = calls(d.path());
        assert!(
            c.contains(&"set-sink-volume @DEFAULT_SINK@ 42%".to_string()),
            "{c:?}"
        );
        assert!(c.contains(&"set-sink-mute @DEFAULT_SINK@ 1".to_string()));
        assert!(c.contains(&"set-source-mute @DEFAULT_SOURCE@ 0".to_string()));
        assert!(c.contains(&"set-default-sink virtual_sink".to_string()));
    }

    #[tokio::test]
    async fn invalid_or_unknown_devices_never_reach_pactl() {
        let d = tempfile::tempdir().unwrap();
        let a = AudioAdapter::new(fake(d.path()));
        assert!(a
            .execute(&AudioCommand::SetVolume { percent: 101 })
            .await
            .is_err());
        assert!(a
            .execute(&AudioCommand::SetDefaultOutput {
                name: "x; rm -rf /".into()
            })
            .await
            .is_err());
        assert!(
            a.execute(&AudioCommand::SetDefaultOutput {
                name: "not_a_real_sink".into()
            })
            .await
            .is_err(),
            "well-formed but unknown names are refused too"
        );
        assert!(
            !calls(d.path()).iter().any(|l| l.starts_with("set-")),
            "no set-* command was run: {:?}",
            calls(d.path())
        );
    }

    #[tokio::test]
    async fn state_through_the_fake_pactl() {
        let d = tempfile::tempdir().unwrap();
        let s = AudioAdapter::new(fake(d.path())).state().await.unwrap();
        assert_eq!(s.output.unwrap().volume, 61);
    }

    #[tokio::test]
    async fn a_missing_binary_is_an_error_not_a_panic() {
        assert!(AudioAdapter::new("/nonexistent/pactl")
            .state()
            .await
            .is_err());
    }
}
