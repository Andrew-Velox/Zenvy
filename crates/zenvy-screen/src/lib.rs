//! Screen preview. `grim` takes one JPEG per frame, **only while somebody is viewing** (the first viewer starts the loop, the
//! last one to leave stops it: an idle agent captures nothing). A slow viewer skips frames; the newest frame always wins.
//!
//! Frame format: `[SCREEN_FRAME_TAG][u32 seq LE][u16 width LE][u16 height LE][JPEG bytes]`.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use zenvy_proto::{ScreenPreset, SCREEN_FRAME_TAG};

const MAX_FRAME_BYTES: u64 = 6 * 1024 * 1024;
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(4);

/// What to capture and how.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureOpts {
    /// Monitor connector (validated by the caller).
    pub output: String,
    /// The monitor's logical width, to size the image.
    pub logical_width: u32,
    pub preset: ScreenPreset,
}

/// Fixed argument vector for `grim`: JPEG to stdout of the chosen monitor, cursor included, scaled to the preset's width.
pub fn grim_args(o: &CaptureOpts) -> Vec<String> {
    let scale = (o.preset.max_width() as f64 / o.logical_width.max(1) as f64).min(1.0);
    ["-t", "jpeg", "-q"]
        .map(String::from)
        .into_iter()
        .chain([
            o.preset.quality().to_string(),
            "-s".into(),
            format!("{scale:.3}"),
            "-c".into(),
            "-o".into(),
            o.output.clone(),
            "-".into(),
        ])
        .collect()
}

/// Width and height from a JPEG's start-of-frame marker.
pub fn jpeg_size(b: &[u8]) -> Option<(u16, u16)> {
    if b.len() < 4 || b[0] != 0xff || b[1] != 0xd8 {
        return None;
    }
    let mut i = 2;
    while i + 9 < b.len() {
        if b[i] != 0xff {
            i += 1;
            continue;
        }
        let m = b[i + 1];
        if m == 0xff {
            i += 1;
            continue;
        }
        if (0xd0..=0xd9).contains(&m) || m == 0x01 {
            i += 2;
            continue;
        }
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        if matches!(m, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
            let h = u16::from_be_bytes([b[i + 5], b[i + 6]]);
            let w = u16::from_be_bytes([b[i + 7], b[i + 8]]);
            return (w > 0 && h > 0).then_some((w, h));
        }
        i += 2 + len.max(2);
    }
    None
}

pub fn frame(seq: u32, w: u16, h: u16, jpeg: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(9 + jpeg.len());
    v.push(SCREEN_FRAME_TAG);
    v.extend_from_slice(&seq.to_le_bytes());
    v.extend_from_slice(&w.to_le_bytes());
    v.extend_from_slice(&h.to_le_bytes());
    v.extend_from_slice(jpeg);
    v
}

pub fn parse_frame(d: &[u8]) -> Option<(u32, u16, u16, &[u8])> {
    (d.len() > 9 && d[0] == SCREEN_FRAME_TAG).then(|| {
        (
            u32::from_le_bytes([d[1], d[2], d[3], d[4]]),
            u16::from_le_bytes([d[5], d[6]]),
            u16::from_le_bytes([d[7], d[8]]),
            &d[9..],
        )
    })
}

/// A monitor's place in the layout, as Hyprland reports it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MonitorGeom {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    /// 0 normal, 1 = 90°, 2 = 180°, 3 = 270°, 4-7 = the same flipped.
    pub transform: i32,
}

/// The monitor's size in layout (logical) pixels: scaled down, and width and height swapped when it is rotated by 90° or 270°.
pub fn logical_size(m: &MonitorGeom) -> (f64, f64) {
    let (w, h) = if m.transform % 2 == 1 {
        (m.height, m.width)
    } else {
        (m.width, m.height)
    };
    let s = if m.scale > 0.0 { m.scale } else { 1.0 };
    (w as f64 / s, h as f64 / s)
}

/// A tap at fraction (`nx`, `ny`) of the monitor as shown, as a point in the layout. `None` for anything outside the screen.
pub fn map_point(m: &MonitorGeom, nx: f64, ny: f64) -> Option<(i32, i32)> {
    if !(nx.is_finite() && ny.is_finite() && (0.0..=1.0).contains(&nx) && (0.0..=1.0).contains(&ny))
    {
        return None;
    }
    let (lw, lh) = logical_size(m);
    let x = m.x as f64 + (nx * (lw - 1.0)).round();
    let y = m.y as f64 + (ny * (lh - 1.0)).round();
    Some((x as i32, y as i32))
}

struct State {
    viewers: usize,
    opts: Option<CaptureOpts>,
    task: Option<JoinHandle<()>>,
}

struct Inner {
    grim: PathBuf,
    tx: broadcast::Sender<Arc<Vec<u8>>>,
    seq: AtomicU32,
    state: Mutex<State>,
}

#[derive(Clone)]
pub struct ScreenStreamer {
    inner: Arc<Inner>,
}

/// One phone's view. Dropping it leaves; when the last one leaves, capturing stops.
pub struct Viewer {
    rx: broadcast::Receiver<Arc<Vec<u8>>>,
    inner: Arc<Inner>,
}

impl ScreenStreamer {
    pub fn new(grim: impl Into<PathBuf>) -> Self {
        Self {
            inner: Arc::new(Inner {
                grim: grim.into(),
                tx: broadcast::channel(3).0,
                seq: AtomicU32::new(0),
                state: Mutex::new(State {
                    viewers: 0,
                    opts: None,
                    task: None,
                }),
            }),
        }
    }

    /// Joins the stream, starting capture if this is the first viewer. Must be called inside a tokio runtime.
    pub fn view(&self, opts: CaptureOpts) -> Viewer {
        let rx = self.inner.tx.subscribe();
        let mut st = self.inner.state.lock().unwrap();
        st.viewers += 1;
        st.opts = Some(opts);
        if st.task.is_none() {
            st.task = Some(tokio::spawn(capture_loop(self.inner.clone())));
        }
        Viewer {
            rx,
            inner: self.inner.clone(),
        }
    }

    /// Changes what is captured (monitor, quality) for everybody who is viewing.
    pub fn set_opts(&self, opts: CaptureOpts) {
        self.inner.state.lock().unwrap().opts = Some(opts);
    }

    pub fn viewers(&self) -> usize {
        self.inner.state.lock().unwrap().viewers
    }
}

impl Viewer {
    /// The newest frame; frames this viewer was too slow for are skipped.
    pub async fn recv(&mut self) -> Option<Arc<Vec<u8>>> {
        loop {
            match self.rx.recv().await {
                Ok(f) => return Some(f),
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        let mut st = self.inner.state.lock().unwrap();
        st.viewers = st.viewers.saturating_sub(1);
        if st.viewers == 0 {
            st.opts = None;
            if let Some(h) = st.task.take() {
                h.abort(); // grim is `kill_on_drop`
            }
        }
    }
}

async fn grab(grim: &PathBuf, opts: &CaptureOpts) -> Option<Vec<u8>> {
    let mut child = Command::new(grim)
        .args(grim_args(opts))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let mut buf = Vec::with_capacity(256 * 1024);
    let read = tokio::time::timeout(
        CAPTURE_TIMEOUT,
        (&mut out).take(MAX_FRAME_BYTES + 1).read_to_end(&mut buf),
    )
    .await;
    let status = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
    (read.is_ok()
        && matches!(status, Ok(Ok(s)) if s.success())
        && buf.len() as u64 <= MAX_FRAME_BYTES)
        .then_some(buf)
}

async fn capture_loop(inner: Arc<Inner>) {
    let mut backoff = Duration::from_millis(500);
    loop {
        let t0 = Instant::now();
        let Some(opts) = inner.state.lock().unwrap().opts.clone() else {
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        match grab(&inner.grim, &opts).await {
            Some(jpeg) => match jpeg_size(&jpeg) {
                Some((w, h)) => {
                    backoff = Duration::from_millis(500);
                    let seq = inner.seq.fetch_add(1, Ordering::Relaxed);
                    let _ = inner.tx.send(Arc::new(frame(seq, w, h, &jpeg)));
                }
                None => tokio::time::sleep(backoff).await,
            },
            None => {
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(3));
            }
        }
        let every = Duration::from_millis(1000 / opts.preset.fps().max(1) as u64);
        tokio::time::sleep(
            every
                .saturating_sub(t0.elapsed())
                .max(Duration::from_millis(15)),
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A minimal JPEG header with the given size (SOI, APP0, SOF0): enough for `jpeg_size`.
    fn tiny_jpeg(w: u16, h: u16) -> Vec<u8> {
        let mut v = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10];
        v.extend_from_slice(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
        v.extend_from_slice(&[0xff, 0xc0, 0x00, 0x11, 0x08]);
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&[0x03, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1, 0xff, 0xd9]);
        v
    }

    #[test]
    fn the_jpeg_size_is_read_from_the_header_and_garbage_is_refused() {
        assert_eq!(jpeg_size(&tiny_jpeg(640, 360)), Some((640, 360)));
        assert_eq!(jpeg_size(&tiny_jpeg(1280, 720)), Some((1280, 720)));
        for bad in [
            &b""[..],
            b"\xff\xd8",
            b"not a jpeg at all",
            b"\x89PNG\r\n\x1a\n....",
            &tiny_jpeg(0, 10)[..],
        ] {
            assert_eq!(jpeg_size(bad), None);
        }
        let f = frame(7, 640, 360, &tiny_jpeg(640, 360));
        let (seq, w, h, j) = parse_frame(&f).unwrap();
        assert_eq!(
            (seq, w, h, j.len()),
            (7, 640, 360, tiny_jpeg(640, 360).len())
        );
        assert_eq!(f[0], SCREEN_FRAME_TAG);
        assert!(
            parse_frame(&[SCREEN_FRAME_TAG, 1, 2]).is_none() && parse_frame(&[9; 20]).is_none()
        );
    }

    #[test]
    fn grim_gets_a_fixed_argument_vector() {
        let o = CaptureOpts {
            output: "HDMI-A-1".into(),
            logical_width: 1920,
            preset: ScreenPreset::Medium,
        };
        assert_eq!(
            grim_args(&o),
            ["-t", "jpeg", "-q", "55", "-s", "0.500", "-c", "-o", "HDMI-A-1", "-"]
        );
        let small = CaptureOpts {
            logical_width: 800,
            preset: ScreenPreset::High,
            ..o.clone()
        };
        assert_eq!(
            grim_args(&small)[5],
            "1.000",
            "a small screen is never enlarged"
        );
        let high = CaptureOpts {
            preset: ScreenPreset::High,
            ..o
        };
        assert!(grim_args(&high)[3] == "65" && grim_args(&high)[5] == "0.667");
    }

    #[test]
    fn a_tap_maps_onto_the_right_point_of_the_layout() {
        let m = |x, y, w, h, scale, transform| MonitorGeom {
            x,
            y,
            width: w,
            height: h,
            scale,
            transform,
        };
        // plain 1920x1080 at the origin
        assert_eq!(
            map_point(&m(0, 0, 1920, 1080, 1.0, 0), 0.0, 0.0),
            Some((0, 0))
        );
        assert_eq!(
            map_point(&m(0, 0, 1920, 1080, 1.0, 0), 1.0, 1.0),
            Some((1919, 1079))
        );
        assert_eq!(
            map_point(&m(0, 0, 1920, 1080, 1.0, 0), 0.5, 0.5),
            Some((960, 540))
        );
        // a second monitor to the right at x = 1920
        assert_eq!(
            map_point(&m(1920, 0, 1920, 1080, 1.0, 0), 0.0, 0.0),
            Some((1920, 0))
        );
        // a 4K screen scaled 2x is 1920x1080 logical pixels
        assert_eq!(logical_size(&m(0, 0, 3840, 2160, 2.0, 0)), (1920.0, 1080.0));
        assert_eq!(
            map_point(&m(0, 0, 3840, 2160, 2.0, 0), 1.0, 1.0),
            Some((1919, 1079))
        );
        // fractional scale 1.5
        assert_eq!(logical_size(&m(0, 0, 2880, 1620, 1.5, 0)), (1920.0, 1080.0));
        // rotated 90 or 270 degrees: width and height swap (a 1920x1080 panel standing up is 1080 wide)
        assert_eq!(logical_size(&m(0, 0, 1920, 1080, 1.0, 1)), (1080.0, 1920.0));
        assert_eq!(logical_size(&m(0, 0, 1920, 1080, 1.0, 3)), (1080.0, 1920.0));
        assert_eq!(
            logical_size(&m(0, 0, 1920, 1080, 1.0, 2)),
            (1920.0, 1080.0),
            "180 degrees keeps the shape"
        );
        assert_eq!(
            logical_size(&m(0, 0, 1920, 1080, 1.0, 5)),
            (1080.0, 1920.0),
            "flipped and rotated"
        );
        assert_eq!(
            map_point(&m(-1080, 0, 1920, 1080, 1.0, 1), 0.5, 1.0),
            Some((-1080 + 540, 1919))
        );
        for (x, y) in [
            (-0.01, 0.5),
            (1.01, 0.5),
            (0.5, f64::NAN),
            (f64::INFINITY, 0.1),
            (0.5, -1.0),
        ] {
            assert_eq!(
                map_point(&m(0, 0, 1920, 1080, 1.0, 0), x, y),
                None,
                "{x} {y}"
            );
        }
    }

    fn fake_grim(dir: &std::path::Path) -> PathBuf {
        let p = dir.join("grim");
        let jpg = dir.join("f.jpg");
        std::fs::write(&jpg, tiny_jpeg(64, 48)).unwrap();
        std::fs::write(
            &p,
            format!(
                "#!/bin/sh\necho \"$@\" >> {d}/calls\ncat {j}\n",
                d = dir.display(),
                j = jpg.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn opts() -> CaptureOpts {
        CaptureOpts {
            output: "HDMI-A-1".into(),
            logical_width: 1920,
            preset: ScreenPreset::High,
        }
    }

    fn calls(d: &std::path::Path) -> usize {
        std::fs::read_to_string(d.join("calls"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn nothing_is_captured_until_somebody_views_and_it_stops_when_they_leave() {
        let d = tempfile::tempdir().unwrap();
        let s = ScreenStreamer::new(fake_grim(d.path()));
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(calls(d.path()), 0, "an idle streamer never runs grim");
        let mut v = s.view(opts());
        let f = tokio::time::timeout(Duration::from_secs(3), v.recv())
            .await
            .unwrap()
            .unwrap();
        let (_, w, h, _) = parse_frame(&f).unwrap();
        assert_eq!((w, h), (64, 48));
        let mut last = 0;
        for _ in 0..6 {
            let n = parse_frame(&v.recv().await.unwrap()).unwrap().0;
            assert!(n > last || last == 0, "sequence numbers rise");
            last = n;
        }
        assert!(
            std::fs::read_to_string(d.path().join("calls"))
                .unwrap()
                .lines()
                .all(|l| l == "-t jpeg -q 65 -s 0.667 -c -o HDMI-A-1 -"),
            "grim is always called with the fixed arguments"
        );
        drop(v);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let n = calls(d.path());
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(calls(d.path()), n, "no viewer, no capture");
        assert_eq!(s.viewers(), 0);
    }

    #[tokio::test]
    async fn changing_the_settings_applies_to_the_next_frame_and_a_broken_grim_does_not_crash() {
        let d = tempfile::tempdir().unwrap();
        let s = ScreenStreamer::new(fake_grim(d.path()));
        let mut v = s.view(CaptureOpts {
            preset: ScreenPreset::Low,
            ..opts()
        });
        v.recv().await.unwrap();
        s.set_opts(CaptureOpts {
            output: "DP-2".into(),
            ..opts()
        });
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            std::fs::read_to_string(d.path().join("calls"))
                .unwrap()
                .lines()
                .last()
                .unwrap()
                .contains("-o DP-2"),
            "the new monitor is used"
        );
        drop(v);
        let bad = ScreenStreamer::new("/nonexistent/grim");
        let mut v = bad.view(opts());
        assert!(
            tokio::time::timeout(Duration::from_millis(700), v.recv())
                .await
                .is_err(),
            "no frames, no panic"
        );
    }
}
