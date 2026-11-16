//! Listening bookkeeping: optionally silencing the PC speakers while a phone listens, and telling the person at
//! the PC that it is happening.
//!
//! Silencing is restored when the last listener leaves. A marker file records that *we* muted, so if the agent
//! dies while muted, the next start unmutes (and only then): a crash never leaves the PC silent for good.

use crate::app::App;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use zenvy_audio::AudioAdapter;
use zenvy_proto::AudioCommand;

pub struct ListenMute {
    audio: Option<Arc<AudioAdapter>>,
    flag: PathBuf,
    /// (phones that asked for muting, whether this agent is the one that muted)
    state: Mutex<(usize, bool)>,
}

impl ListenMute {
    pub fn new(audio: Option<Arc<AudioAdapter>>, flag: PathBuf) -> Self {
        Self {
            audio,
            flag,
            state: Mutex::new((0, false)),
        }
    }

    /// A phone asked to silence the PC while it listens.
    pub async fn acquire(&self) {
        let Some(audio) = &self.audio else { return };
        let need_mute = {
            let mut st = self.state.lock().unwrap();
            st.0 += 1;
            st.0 == 1
        };
        if !need_mute {
            return;
        }
        // Do not touch a PC the user already muted: we would unmute it later against their wishes.
        let already = audio
            .state()
            .await
            .ok()
            .and_then(|s| s.output)
            .is_some_and(|o| o.muted);
        if already {
            return;
        }
        if audio
            .execute(&AudioCommand::SetMute { muted: true })
            .await
            .is_ok()
        {
            if let Some(d) = self.flag.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(&self.flag, b"muted by zenvy while a phone listens");
            self.state.lock().unwrap().1 = true;
        }
    }

    /// A phone that asked for muting has left.
    pub async fn release(&self) {
        let Some(audio) = &self.audio else { return };
        let unmute = {
            let mut st = self.state.lock().unwrap();
            st.0 = st.0.saturating_sub(1);
            st.0 == 0 && std::mem::take(&mut st.1)
        };
        if unmute {
            let _ = audio.execute(&AudioCommand::SetMute { muted: false }).await;
            let _ = std::fs::remove_file(&self.flag);
        }
    }

    /// At startup: if a previous run muted the PC and died, undo it.
    pub async fn recover(&self) -> bool {
        if !self.flag.exists() {
            return false;
        }
        if let Some(audio) = &self.audio {
            let _ = audio.execute(&AudioCommand::SetMute { muted: false }).await;
        }
        let _ = std::fs::remove_file(&self.flag);
        true
    }
}

/// A device name that came from a phone, reduced to harmless characters for display.
pub fn clean_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-' || *c == '_')
        .take(40)
        .collect()
}

/// Best-effort desktop notification (fixed argv, no shell). `body` is built by us from fixed templates and
/// sanitised device names, never from phone text.
pub async fn notify_text(app: &App, body: &str) {
    let Some(bin) = &app.notify else { return };
    let _ = tokio::process::Command::new(bin)
        .args(["--app-name=Zenvy", "--icon=zenvy", "--", "Zenvy", body])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map(|mut c| {
            tokio::spawn(async move {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(3), c.wait()).await;
            })
        });
}

pub async fn notify(app: &App, device: &str, started: bool) {
    let name = clean_name(device);
    let body = if started {
        format!("{name} is listening to this PC's audio")
    } else {
        format!("{name} stopped listening")
    };
    notify_text(app, &body).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A fake pactl that keeps one mute flag in a file and logs the set-* calls.
    fn fake_pactl(dir: &std::path::Path, initially_muted: bool) -> Arc<AudioAdapter> {
        std::fs::write(
            dir.join("muted"),
            if initially_muted { "true" } else { "false" },
        )
        .unwrap();
        let p = dir.join("pactl");
        let sinks = |m: &str| {
            format!(
                r#"[{{"name":"s","description":"S","mute":{m},"volume":{{"mono":{{"value_percent":"50%"}}}}}}]"#
            )
        };
        std::fs::write(&p, format!(
            "#!/bin/sh\necho \"$@\" >> {d}/calls\nM=$(cat {d}/muted)\ncase \"$*\" in\n *info*) echo '{{\"default_sink_name\":\"s\",\"default_source_name\":\"\"}}';;\n *'list sinks'*) if [ \"$M\" = true ]; then echo '{t}'; else echo '{f}'; fi;;\n *'list sources'*) echo '[]';;\n *'set-sink-mute'*) case \"$*\" in *' 1') echo true > {d}/muted;; *) echo false > {d}/muted;; esac;;\nesac\n",
            d = dir.display(), t = sinks("true"), f = sinks("false")
        )).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        Arc::new(AudioAdapter::new(p))
    }

    fn muted(dir: &std::path::Path) -> bool {
        std::fs::read_to_string(dir.join("muted")).unwrap().trim() == "true"
    }

    #[tokio::test]
    async fn mutes_for_the_first_listener_and_restores_after_the_last() {
        let d = tempfile::tempdir().unwrap();
        let m = ListenMute::new(Some(fake_pactl(d.path(), false)), d.path().join("flag"));
        m.acquire().await;
        m.acquire().await;
        assert!(muted(d.path()) && d.path().join("flag").exists());
        m.release().await;
        assert!(muted(d.path()), "still muted for the remaining phone");
        m.release().await;
        assert!(
            !muted(d.path()) && !d.path().join("flag").exists(),
            "restored when the last phone leaves"
        );
    }

    #[tokio::test]
    async fn never_unmutes_a_pc_the_user_had_already_muted() {
        let d = tempfile::tempdir().unwrap();
        let m = ListenMute::new(Some(fake_pactl(d.path(), true)), d.path().join("flag"));
        m.acquire().await;
        m.release().await;
        assert!(muted(d.path()), "the user's own mute is left alone");
        assert!(!d.path().join("flag").exists());
    }

    #[tokio::test]
    async fn a_crash_while_muted_is_undone_on_the_next_start() {
        let d = tempfile::tempdir().unwrap();
        let first = ListenMute::new(Some(fake_pactl(d.path(), false)), d.path().join("flag"));
        first.acquire().await; // ... and the agent dies here, without releasing
        assert!(muted(d.path()));
        let second = ListenMute::new(
            Some(fake_pactl(d.path(), muted(d.path()))),
            d.path().join("flag"),
        );
        assert!(second.recover().await);
        assert!(!muted(d.path()), "the next run unmutes");
        assert!(!second.recover().await, "and only once");
    }

    #[tokio::test]
    async fn without_audio_control_it_does_nothing() {
        let d = tempfile::tempdir().unwrap();
        let m = ListenMute::new(None, d.path().join("flag"));
        m.acquire().await;
        m.release().await;
        assert!(!m.recover().await);
    }
}
