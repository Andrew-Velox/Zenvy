//! File transfer. Phone -> PC: the phone asks permission with a file name and size (WebSocket), gets a one-time ticket and
//! streams the body to `POST /upload/<ticket>`; the agent writes to a private part file and only a complete file of exactly
//! the promised size is moved into the inbox (never overwriting). PC -> phone: the person at the PC offers a file
//! (`zenvyd send`), the phone accepts, and downloads it once with a ticket.
//!
//! Memory is bounded by the chunk size, not the file size. Names from a phone are reduced to a plain file name; nothing from
//! a phone is ever used as a path.

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const MAX_ACTIVE_UPLOADS: usize = 2;
const OFFER_TTL: Duration = Duration::from_secs(60);
const OUTGOING_TTL: Duration = Duration::from_secs(600);
const MAX_PENDING_PER_DEVICE: usize = 4;
const PART_PREFIX: &str = ".zenvy-part-";

pub fn new_ticket() -> String {
    zenvy_auth::random_bytes::<24>()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A plain file name from whatever a phone sent: the last path component only, no control characters, no leading dots,
/// characters that trouble other systems replaced, at most 120 characters with the extension kept.
pub fn sanitize_filename(raw: &str) -> Option<String> {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| if "<>:\"|?*".contains(c) { '_' } else { c })
        .collect();
    let name = cleaned
        .trim()
        .trim_start_matches('.')
        .trim_end_matches(['.', ' '])
        .to_string();
    if name.is_empty() || name.chars().all(|c| c == '_') {
        return None;
    }
    if name.chars().count() <= 120 {
        return Some(name);
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if name.len() - i <= 16 && i > 0 => (&name[..i], &name[i..]),
        _ => (name.as_str(), ""),
    };
    let keep = 120usize.saturating_sub(ext.chars().count());
    Some(format!(
        "{}{}",
        stem.chars().take(keep).collect::<String>(),
        ext
    ))
}

/// Safe to show in a notification.
pub fn display_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric() || " ._-()".contains(*c))
        .take(60)
        .collect()
}

pub fn human_size(n: u64) -> String {
    const U: [&str; 4] = ["B", "KB", "MB", "GB"];
    let (mut v, mut i) = (n as f64, 0);
    while v >= 1024.0 && i < 3 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum UploadError {
    TooLarge,
    Short,
    Io(String),
    Aborted,
}

pub struct UploadOffer {
    pub device: String,
    pub name: String,
    pub size: u64,
    expires: Instant,
}

pub struct Outgoing {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    expires: Instant,
}

pub struct DownloadTicket {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub device: String,
    expires: Instant,
}

/// Removes the part file unless the upload completes, including when the connection simply vanishes mid-way.
struct PartGuard(Option<PathBuf>);
impl Drop for PartGuard {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

pub struct Transfers {
    pub inbox: PathBuf,
    pub max_bytes: u64,
    uploads: Mutex<HashMap<String, UploadOffer>>,
    outgoing: Mutex<HashMap<String, Outgoing>>,
    downloads: Mutex<HashMap<String, DownloadTicket>>,
    active: AtomicUsize,
}

/// Counts an upload as running while it is alive.
pub struct ActiveUpload<'a>(&'a Transfers);
impl Drop for ActiveUpload<'_> {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Transfers {
    pub fn new(inbox: PathBuf, max_bytes: u64) -> Self {
        Self {
            inbox,
            max_bytes,
            uploads: Default::default(),
            outgoing: Default::default(),
            downloads: Default::default(),
            active: AtomicUsize::new(0),
        }
    }

    /// Part files left behind by a crash.
    pub fn clean_stale_parts(&self) {
        if let Ok(rd) = std::fs::read_dir(&self.inbox) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with(PART_PREFIX) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }

    /// A phone wants to send a file: validates the name and size and hands out a one-time ticket.
    pub fn offer_upload(
        &self,
        device: &str,
        raw_name: &str,
        size: u64,
    ) -> Result<(String, String), &'static str> {
        let name = sanitize_filename(raw_name).ok_or("that file name cannot be used")?;
        if size == 0 || size > self.max_bytes {
            return Err("the file is empty or larger than this PC accepts");
        }
        let now = Instant::now();
        let mut up = self.uploads.lock().unwrap();
        up.retain(|_, o| o.expires > now);
        if up.values().filter(|o| o.device == device).count() >= MAX_PENDING_PER_DEVICE {
            return Err("too many files are waiting to start");
        }
        let ticket = new_ticket();
        up.insert(
            ticket.clone(),
            UploadOffer {
                device: device.to_string(),
                name: name.clone(),
                size,
                expires: now + OFFER_TTL,
            },
        );
        Ok((ticket, name))
    }

    /// The ticket is good once.
    pub fn take_upload(&self, ticket: &str) -> Option<UploadOffer> {
        let o = self.uploads.lock().unwrap().remove(ticket)?;
        (o.expires > Instant::now()).then_some(o)
    }

    pub fn begin_upload(&self) -> Option<ActiveUpload<'_>> {
        (self.active.fetch_add(1, Ordering::SeqCst) < MAX_ACTIVE_UPLOADS)
            .then_some(ActiveUpload(self))
            .or_else(|| {
                self.active.fetch_sub(1, Ordering::SeqCst);
                None
            })
    }

    /// Writes the body into the inbox. Success only for a complete file of exactly `offer.size` bytes.
    pub async fn receive<S, E>(
        &self,
        offer: &UploadOffer,
        mut body: S,
    ) -> Result<(String, u64), UploadError>
    where
        S: Stream<Item = Result<Bytes, E>> + Unpin,
    {
        std::fs::create_dir_all(&self.inbox).map_err(|e| UploadError::Io(e.kind().to_string()))?;
        let part = self.inbox.join(format!("{PART_PREFIX}{}", new_ticket()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&part)
            .map_err(|e| UploadError::Io(e.kind().to_string()))?;
        let mut guard = PartGuard(Some(part.clone()));
        let mut total: u64 = 0;
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|_| UploadError::Aborted)?;
            total += chunk.len() as u64;
            if total > offer.size {
                return Err(UploadError::TooLarge);
            }
            file.write_all(&chunk)
                .map_err(|e| UploadError::Io(e.kind().to_string()))?;
        }
        if total != offer.size {
            return Err(UploadError::Short);
        }
        file.sync_data()
            .map_err(|e| UploadError::Io(e.kind().to_string()))?;
        drop(file);
        let final_name =
            place(&part, &self.inbox, &offer.name).map_err(|e| UploadError::Io(e.to_string()))?;
        let _ = std::fs::remove_file(&part);
        guard.0 = None;
        Ok((final_name, total))
    }

    /// The person at the PC offers a file to the phones. Returns (offer id, shown name, size).
    pub fn add_outgoing(&self, path: &Path) -> Result<(String, String, u64), String> {
        let real = std::fs::canonicalize(path)
            .map_err(|e| format!("cannot read that path: {}", e.kind()))?;
        let meta =
            std::fs::metadata(&real).map_err(|e| format!("cannot read that file: {}", e.kind()))?;
        if !meta.is_file() {
            return Err("that is not a regular file".into());
        }
        if meta.len() == 0 || meta.len() > self.max_bytes {
            return Err("the file is empty or larger than the configured limit".into());
        }
        std::fs::File::open(&real).map_err(|e| format!("cannot open that file: {}", e.kind()))?;
        let name = real
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(sanitize_filename)
            .ok_or("that file name cannot be used")?;
        let id = new_ticket()[..24].to_string();
        let now = Instant::now();
        let mut o = self.outgoing.lock().unwrap();
        o.retain(|_, x| x.expires > now);
        o.insert(
            id.clone(),
            Outgoing {
                path: real,
                name: name.clone(),
                size: meta.len(),
                expires: now + OUTGOING_TTL,
            },
        );
        Ok((id, name, meta.len()))
    }

    pub fn outgoing_list(&self) -> Vec<(String, String, u64)> {
        let now = Instant::now();
        let mut o = self.outgoing.lock().unwrap();
        o.retain(|_, x| x.expires > now);
        o.iter()
            .map(|(i, x)| (i.clone(), x.name.clone(), x.size))
            .collect()
    }

    /// A phone accepts an offer: it becomes a one-time download ticket.
    pub fn accept_outgoing(&self, id: &str, device: &str) -> Option<(String, String, u64)> {
        let o = self.outgoing.lock().unwrap().remove(id)?;
        if o.expires <= Instant::now() {
            return None;
        }
        let ticket = new_ticket();
        let (name, size) = (o.name.clone(), o.size);
        self.downloads.lock().unwrap().insert(
            ticket.clone(),
            DownloadTicket {
                path: o.path,
                name: o.name,
                size: o.size,
                device: device.to_string(),
                expires: Instant::now() + OFFER_TTL,
            },
        );
        Some((ticket, name, size))
    }

    pub fn dismiss_outgoing(&self, id: &str) -> bool {
        self.outgoing.lock().unwrap().remove(id).is_some()
    }

    pub fn take_download(&self, ticket: &str) -> Option<DownloadTicket> {
        let d = self.downloads.lock().unwrap().remove(ticket)?;
        (d.expires > Instant::now()).then_some(d)
    }
}

/// Moves a finished part file to `dir/name`, adding " (1)", " (2)" before the extension if the name is taken. `hard_link`
/// fails when the target exists, so a file is never overwritten, even in a race.
pub fn place(part: &Path, dir: &Path, name: &str) -> std::io::Result<String> {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for n in 0..1000 {
        let candidate = if n == 0 {
            name.to_string()
        } else {
            format!("{stem} ({n}){ext}")
        };
        match std::fs::hard_link(part, dir.join(&candidate)) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("too many files with that name"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;

    fn chunks(parts: &[&[u8]]) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Unpin {
        stream::iter(
            parts
                .iter()
                .map(|p| Ok(Bytes::copy_from_slice(p)))
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn file_names_are_reduced_to_a_plain_safe_name() {
        for (raw, want) in [
            ("photo.jpg", Some("photo.jpg")),
            ("../../etc/passwd", Some("passwd")),
            ("/etc/shadow", Some("shadow")),
            ("C:\\Windows\\system32\\evil.dll", Some("evil.dll")),
            ("a/b/c.txt", Some("c.txt")),
            (".bashrc", Some("bashrc")),
            ("...hidden", Some("hidden")),
            ("name with spaces .txt", Some("name with spaces .txt")),
            ("tab\there\nnewline\u{7}.txt", Some("tabherenewline.txt")),
            (
                "report: Q3 <final>|v2?.pdf",
                Some("report_ Q3 _final__v2_.pdf"),
            ),
            ("trailing dots...", Some("trailing dots")),
            (
                "h\u{e9}llo \u{65e5}\u{672c}.txt",
                Some("h\u{e9}llo \u{65e5}\u{672c}.txt"),
            ),
        ] {
            assert_eq!(sanitize_filename(raw).as_deref(), want, "{raw:?}");
        }
        for bad in [
            "", ".", "..", "...", "/", "a/", "///", "  ", "\u{0}", "???", ".. .",
        ] {
            assert_eq!(sanitize_filename(bad), None, "{bad:?}");
        }
        let long = format!("{}.tar.gz", "x".repeat(300));
        let s = sanitize_filename(&long).unwrap();
        assert!(
            s.chars().count() <= 120 && s.ends_with(".gz"),
            "long names keep their extension: {s}"
        );
        assert!(!s.contains('/') && !s.contains('\\') && !s.starts_with('.'));
    }

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn names_never_overwrite() {
        let d = tempfile::tempdir().unwrap();
        let mk = |c: &str| {
            let p = d.path().join(new_ticket());
            std::fs::write(&p, c).unwrap();
            p
        };
        assert_eq!(place(&mk("1"), d.path(), "a.txt").unwrap(), "a.txt");
        assert_eq!(place(&mk("2"), d.path(), "a.txt").unwrap(), "a (1).txt");
        assert_eq!(place(&mk("3"), d.path(), "a.txt").unwrap(), "a (2).txt");
        assert_eq!(place(&mk("4"), d.path(), "noext").unwrap(), "noext");
        assert_eq!(place(&mk("5"), d.path(), "noext").unwrap(), "noext (1)");
        assert_eq!(
            std::fs::read_to_string(d.path().join("a.txt")).unwrap(),
            "1",
            "the first file is untouched"
        );
    }

    #[tokio::test]
    async fn a_complete_upload_lands_in_the_inbox_and_nothing_else_is_left() {
        let d = tempfile::tempdir().unwrap();
        let t = Transfers::new(d.path().join("inbox"), 1000);
        let (ticket, name) = t.offer_upload("dev", "../notes.txt", 11).unwrap();
        assert_eq!(name, "notes.txt");
        let offer = t.take_upload(&ticket).unwrap();
        assert!(t.take_upload(&ticket).is_none(), "a ticket works once");
        let (final_name, n) = t
            .receive(&offer, chunks(&[b"hello ", b"world"]))
            .await
            .unwrap();
        assert_eq!((final_name.as_str(), n), ("notes.txt", 11));
        assert_eq!(
            std::fs::read(d.path().join("inbox/notes.txt")).unwrap(),
            b"hello world"
        );
        let left: Vec<_> = std::fs::read_dir(d.path().join("inbox"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(left, vec!["notes.txt"], "no part file remains: {left:?}");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(d.path().join("inbox/notes.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "private to the owner, never executable"
        );
    }

    #[tokio::test]
    async fn short_long_and_broken_uploads_leave_nothing() {
        let d = tempfile::tempdir().unwrap();
        let t = Transfers::new(d.path().join("inbox"), 1000);
        let offer = |size| {
            let (tk, _) = t.offer_upload("dev", "f.bin", size).unwrap();
            t.take_upload(&tk).unwrap()
        };
        assert_eq!(
            t.receive(&offer(10), chunks(&[b"12345"]))
                .await
                .unwrap_err(),
            UploadError::Short
        );
        assert_eq!(
            t.receive(&offer(4), chunks(&[b"12345"])).await.unwrap_err(),
            UploadError::TooLarge
        );
        let broken = stream::iter(vec![
            Ok(Bytes::from_static(b"abc")),
            Err(std::io::Error::other("connection reset")),
        ]);
        assert_eq!(
            t.receive(&offer(10), broken).await.unwrap_err(),
            UploadError::Aborted
        );
        assert_eq!(
            std::fs::read_dir(d.path().join("inbox")).unwrap().count(),
            0,
            "failed uploads leave no file and no part file"
        );
        // a future that is dropped half way (the client vanished) is cleaned up too
        let slow = stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from_static(b"abc"))])
            .chain(stream::pending());
        let o = offer(10);
        let fut = t.receive(&o, Box::pin(slow));
        let _ = tokio::time::timeout(Duration::from_millis(100), fut).await;
        assert_eq!(
            std::fs::read_dir(d.path().join("inbox")).unwrap().count(),
            0,
            "a dropped upload removes its part file"
        );
    }

    #[test]
    fn offers_are_validated_and_limited() {
        let d = tempfile::tempdir().unwrap();
        let t = Transfers::new(d.path().to_path_buf(), 1000);
        assert!(t.offer_upload("d", "ok.txt", 1000).is_ok());
        assert!(
            t.offer_upload("d", "ok.txt", 1001).is_err()
                && t.offer_upload("d", "ok.txt", 0).is_err()
                && t.offer_upload("d", "..", 5).is_err()
        );
        assert!(
            t.offer_upload("d", "b.txt", 5).is_ok()
                && t.offer_upload("d", "c.txt", 5).is_ok()
                && t.offer_upload("d", "d.txt", 5).is_ok()
        );
        assert!(
            t.offer_upload("d", "e.txt", 5).is_err(),
            "at most four waiting offers per phone"
        );
        assert!(
            t.offer_upload("other", "e.txt", 5).is_ok(),
            "another phone is not affected"
        );
        let a = t.begin_upload().unwrap();
        let b = t.begin_upload().unwrap();
        assert!(
            t.begin_upload().is_none(),
            "at most two uploads run at once"
        );
        drop(a);
        assert!(t.begin_upload().is_some());
        drop(b);
    }

    #[test]
    fn offering_a_file_from_the_pc_checks_it_and_downloads_once() {
        let d = tempfile::tempdir().unwrap();
        let t = Transfers::new(d.path().join("inbox"), 100);
        let f = d.path().join("report.pdf");
        std::fs::write(&f, b"0123456789").unwrap();
        let (id, name, size) = t.add_outgoing(&f).unwrap();
        assert_eq!((name.as_str(), size), ("report.pdf", 10));
        assert!(
            t.add_outgoing(d.path()).is_err() && t.add_outgoing(&d.path().join("missing")).is_err()
        );
        std::fs::write(d.path().join("big"), vec![0u8; 200]).unwrap();
        std::fs::write(d.path().join("empty"), b"").unwrap();
        assert!(
            t.add_outgoing(&d.path().join("big")).is_err()
                && t.add_outgoing(&d.path().join("empty")).is_err()
        );
        assert_eq!(t.outgoing_list().len(), 1);
        let (ticket, _, _) = t.accept_outgoing(&id, "dev").unwrap();
        assert!(
            t.accept_outgoing(&id, "dev").is_none(),
            "an offer can be accepted once"
        );
        assert!(
            t.take_download(&ticket).is_some() && t.take_download(&ticket).is_none(),
            "a download ticket works once"
        );
        let (id2, _, _) = t.add_outgoing(&f).unwrap();
        assert!(t.dismiss_outgoing(&id2) && !t.dismiss_outgoing(&id2));
    }
}
