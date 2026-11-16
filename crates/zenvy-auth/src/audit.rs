use serde_json::json;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Mutex;

const MAX_BYTES: u64 = 5 * 1024 * 1024;

/// Append-only JSONL audit log (mode 0600), rotated once at 5 MB. Local only.
/// Records *what happened*, never secrets, tokens, file contents or typed text.
pub struct AuditLog {
    path: PathBuf,
    lock: Mutex<()>,
}

impl AuditLog {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    pub fn record(&self, kind: &str, device_id: Option<&str>, detail: &str) {
        let _g = self.lock.lock().unwrap();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // Strip control characters so a hostile device name cannot forge extra log lines.
        let clean: String = detail
            .chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect();
        let line =
            json!({ "ts": ts, "kind": kind, "device": device_id, "detail": clean }).to_string();
        if let Some(d) = self.path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if std::fs::metadata(&self.path)
            .map(|m| m.len() > MAX_BYTES)
            .unwrap_or(false)
        {
            let _ = std::fs::rename(&self.path, self.path.with_extension("log.1"));
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)
        {
            let _ = writeln!(f, "{line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_one_json_line_per_event_and_cannot_be_forged() {
        let d = tempfile::tempdir().unwrap();
        let log = AuditLog::new(d.path().join("audit.log"));
        log.record(
            "pair_approved",
            Some("abc"),
            "name=evil\n{\"kind\":\"forged\"}",
        );
        log.record("auth_fail", None, "bad sig");
        let text = std::fs::read_to_string(d.path().join("audit.log")).unwrap();
        assert_eq!(text.lines().count(), 2);
        for l in text.lines() {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            assert_ne!(v["kind"], "forged");
        }
    }
}
