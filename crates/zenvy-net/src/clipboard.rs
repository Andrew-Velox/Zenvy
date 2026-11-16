//! Explicit clipboard transfer and link opening, through `wl-copy`, `wl-paste` and `xdg-open` with fixed arguments.
//!
//! Nothing here runs on its own: a phone asks for the PC's clipboard, or sends text to it, and each request is audited
//! without its content. Content is plain text only, at most 64 KiB, and a link must be a plain http(s) address.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Largest clipboard text read from the PC.
pub const MAX_BYTES: usize = 64 * 1024;
/// Largest text a phone may send: the WebSocket caps a message at 16 KB, so bigger texts belong in a file transfer.
pub const MAX_SET_BYTES: usize = 12 * 1024;
const TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Clone, Debug)]
pub struct ClipboardTools {
    pub copy: PathBuf,
    pub paste: PathBuf,
    /// `xdg-open`, for opening a link on the PC.
    pub open: Option<PathBuf>,
}

async fn output(mut cmd: Command) -> Result<std::process::Output, String> {
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("could not start: {}", e.kind()))?;
    tokio::time::timeout(TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| "timed out".to_string())?
        .map_err(|e| format!("failed: {}", e.kind()))
}

impl ClipboardTools {
    /// The PC's clipboard text, `None` when it is empty or holds something that is not text (an image, files).
    pub async fn get(&self) -> Result<Option<String>, String> {
        let mut types = Command::new(&self.paste);
        types.arg("--list-types");
        let o = output(types).await?;
        let listed = String::from_utf8_lossy(&o.stdout);
        let has_text = o.status.success()
            && listed.lines().any(|l| {
                l.starts_with("text/plain") || l == "UTF8_STRING" || l == "STRING" || l == "TEXT"
            });
        if !has_text {
            return Ok(None);
        }
        let mut c = Command::new(&self.paste);
        c.args(["--no-newline", "--type", "text/plain"]);
        let o = output(c).await?;
        if !o.status.success() || o.stdout.is_empty() {
            return Ok(None);
        }
        if o.stdout.len() > MAX_BYTES {
            return Err("the clipboard is larger than 64 KB".into());
        }
        String::from_utf8(o.stdout)
            .map(Some)
            .map_err(|_| "the clipboard is not valid text".into())
    }

    /// Puts text on the PC's clipboard (it goes through stdin, never the command line).
    pub async fn set(&self, text: &str) -> Result<(), String> {
        if text.is_empty() || text.len() > MAX_SET_BYTES || text.contains('\0') {
            return Err("the text is empty, contains a NUL or is larger than 12 KB".into());
        }
        let mut child = Command::new(&self.copy)
            .args(["--type", "text/plain"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("could not start: {}", e.kind()))?;
        let mut stdin = child.stdin.take().ok_or("no stdin")?;
        stdin
            .write_all(text.as_bytes())
            .await
            .map_err(|e| format!("failed: {}", e.kind()))?;
        drop(stdin);
        match tokio::time::timeout(TIMEOUT, child.wait()).await {
            Ok(Ok(s)) if s.success() => Ok(()),
            Ok(Ok(_)) => Err("wl-copy refused the text".into()),
            Ok(Err(e)) => Err(format!("failed: {}", e.kind())),
            Err(_) => Err("timed out".into()),
        }
    }

    /// Opens a link in the PC's default browser.
    pub async fn open(&self, url: &str) -> Result<(), String> {
        valid_url(url).map_err(String::from)?;
        let bin = self.open.as_ref().ok_or("xdg-open is not installed")?;
        let mut c = Command::new(bin);
        c.arg(url);
        let o = output(c).await?;
        if o.status.success() {
            Ok(())
        } else {
            Err("the PC could not open the link".into())
        }
    }
}

/// Only plain http(s) addresses: ASCII, no spaces, no credentials, a real host.
pub fn valid_url(url: &str) -> Result<(), &'static str> {
    if url.len() > 2048 {
        return Err("the link is too long");
    }
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or("only http and https links can be opened")?;
    if !url.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err("the link has spaces or characters that must be percent-encoded");
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return Err("the link has no host, or has credentials in it");
    }
    let host = authority
        .rsplit_once(':')
        .filter(|(_, p)| p.bytes().all(|b| b.is_ascii_digit()))
        .map_or(authority, |(h, _)| h);
    if host.is_empty()
        || !host.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || b == b'.'
                || b == b'-'
                || b == b'['
                || b == b']'
                || b == b':'
        })
    {
        return Err("the link's host name is not valid");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    /// wl-paste: types from `types`, content from `clip`; wl-copy: stdin to `clip`; xdg-open: argument to `opened`.
    fn tools(d: &Path) -> ClipboardTools {
        let (t, c, o) = (d.join("types"), d.join("clip"), d.join("opened"));
        std::fs::write(&t, "text/plain;charset=utf-8\nUTF8_STRING\n").unwrap();
        ClipboardTools {
            paste: script(
                d,
                "wl-paste",
                &format!(
                    "case \"$1\" in --list-types) cat {t};; *) cat {c};; esac",
                    t = t.display(),
                    c = c.display()
                ),
            ),
            copy: script(d, "wl-copy", &format!("cat > {c}", c = c.display())),
            open: Some(script(
                d,
                "xdg-open",
                &format!("echo \"$@\" >> {o}", o = o.display()),
            )),
        }
    }

    #[tokio::test]
    async fn text_round_trips_and_non_text_is_none() {
        let d = tempfile::tempdir().unwrap();
        let t = tools(d.path());
        t.set("h\u{e9}llo w\u{f6}rld \u{65e5}\u{672c} \"quotes\" $(not run) `no`")
            .await
            .unwrap();
        assert_eq!(
            t.get().await.unwrap().as_deref(),
            Some("h\u{e9}llo w\u{f6}rld \u{65e5}\u{672c} \"quotes\" $(not run) `no`")
        );
        std::fs::write(d.path().join("types"), "image/png\n").unwrap();
        assert_eq!(
            t.get().await.unwrap(),
            None,
            "an image on the clipboard is not text"
        );
        std::fs::write(d.path().join("types"), "text/plain\n").unwrap();
        std::fs::write(d.path().join("clip"), "").unwrap();
        assert_eq!(t.get().await.unwrap(), None, "an empty clipboard");
    }

    #[tokio::test]
    async fn size_and_encoding_limits() {
        let d = tempfile::tempdir().unwrap();
        let t = tools(d.path());
        assert!(
            t.set("").await.is_err()
                && t.set("a\0b").await.is_err()
                && t.set(&"x".repeat(MAX_SET_BYTES + 1)).await.is_err()
        );
        assert!(t.set(&"x".repeat(MAX_SET_BYTES)).await.is_ok());
        std::fs::write(d.path().join("clip"), vec![b'a'; MAX_BYTES + 1]).unwrap();
        assert!(
            t.get().await.unwrap_err().contains("larger"),
            "an oversize clipboard is refused, not truncated"
        );
        std::fs::write(d.path().join("clip"), [0xffu8, 0xfe, 0x41]).unwrap();
        assert!(t.get().await.unwrap_err().contains("not valid text"));
    }

    #[test]
    fn only_plain_web_links_pass() {
        for ok in [
            "https://example.com",
            "http://localhost:8080/a?b=c#d",
            "https://docs.rs/zenvy-net/latest/",
            "http://[::1]:3000/",
            "https://a-b.example.org/path%20with%20encoding",
        ] {
            assert!(valid_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "ftp://example.com",
            "mailto:a@b.c",
            "//example.com",
            "https://",
            "https:///nohost",
            "https://user:pw@example.com/",
            "https://evil.example@good.example/",
            "https://exa mple.com",
            "https://example.com/a b",
            "https://example.com/\u{e9}",
            "https://ex\u{e4}mple.com",
            "https://example.com/\n",
            "HTTPS://example.com",
            &format!("https://example.com/{}", "a".repeat(2100)),
            "x",
        ] {
            assert!(valid_url(bad).is_err(), "{bad:?}");
        }
    }

    #[tokio::test]
    async fn open_passes_exactly_the_validated_link() {
        let d = tempfile::tempdir().unwrap();
        let t = tools(d.path());
        t.open("https://example.com/a?b=1").await.unwrap();
        assert_eq!(
            std::fs::read_to_string(d.path().join("opened")).unwrap(),
            "https://example.com/a?b=1\n"
        );
        assert!(t.open("javascript:alert(1)").await.is_err());
        assert!(
            std::fs::read_to_string(d.path().join("opened"))
                .unwrap()
                .lines()
                .count()
                == 1,
            "a refused link never reaches xdg-open"
        );
        let none = ClipboardTools {
            open: None,
            ..tools(d.path())
        };
        assert!(none.open("https://example.com").await.is_err());
    }
}
