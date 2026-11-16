//! Streams the PC's audio output to a listening phone: capture (`parec`), Opus encode, fan out.
//!
//! * The capture process runs **only while somebody is listening** (first listener starts it, the last one to
//!   leave kills it): an idle agent captures nothing.
//! * Real-time semantics: a slow listener *drops* frames (a broadcast ring of ~1.3 s); it never makes the
//!   agent buffer without bound and never delays other listeners.
//! * Frames are `[0x01][u32 LE sequence][Opus packet]`, 20 ms of 48 kHz stereo each.

use opus::{Application, Bitrate, Channels, Encoder};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use zenvy_proto::{AUDIO_CHANNELS, AUDIO_FRAME_MS, AUDIO_FRAME_TAG, AUDIO_RATE};

/// Samples per channel in one frame.
pub const FRAME_SAMPLES: usize = (AUDIO_RATE * AUDIO_FRAME_MS / 1000) as usize;
/// Bytes of 16-bit PCM in one frame (all channels).
pub const FRAME_BYTES: usize = FRAME_SAMPLES * AUDIO_CHANNELS as usize * 2;
const RING_FRAMES: usize = 64;

/// How to capture: a program and its fixed arguments. It must write raw s16le 48 kHz stereo to stdout.
#[derive(Clone, Debug)]
pub struct CaptureSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
}

impl CaptureSpec {
    /// Capture what the default output device is playing (its "monitor").
    pub fn parec(program: impl Into<PathBuf>) -> Self {
        let args = [
            "--device=@DEFAULT_MONITOR@",
            "--format=s16le",
            "--rate=48000",
            "--channels=2",
            "--latency-msec=20",
            "--raw",
        ]
        .map(String::from)
        .to_vec();
        Self {
            program: program.into(),
            args,
        }
    }
}

pub fn frame(seq: u32, opus_packet: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(5 + opus_packet.len());
    v.push(AUDIO_FRAME_TAG);
    v.extend_from_slice(&seq.to_le_bytes());
    v.extend_from_slice(opus_packet);
    v
}

pub fn parse_frame(data: &[u8]) -> Option<(u32, &[u8])> {
    (data.len() > 5 && data[0] == AUDIO_FRAME_TAG).then(|| {
        (
            u32::from_le_bytes([data[1], data[2], data[3], data[4]]),
            &data[5..],
        )
    })
}

pub fn new_encoder() -> Result<Encoder, opus::Error> {
    let mut e = Encoder::new(AUDIO_RATE, Channels::Stereo, Application::LowDelay)?;
    e.set_bitrate(Bitrate::Bits(96_000))?;
    Ok(e)
}

struct Inner {
    spec: CaptureSpec,
    tx: broadcast::Sender<Arc<Vec<u8>>>,
    seq: AtomicU32,
    /// (number of listeners, the capture task)
    state: Mutex<(usize, Option<JoinHandle<()>>)>,
}

#[derive(Clone)]
pub struct AudioStreamer {
    inner: Arc<Inner>,
}

/// One phone's subscription. Dropping it leaves the stream; when the last one leaves, capture stops.
pub struct Listener {
    rx: broadcast::Receiver<Arc<Vec<u8>>>,
    inner: Arc<Inner>,
}

impl AudioStreamer {
    pub fn new(spec: CaptureSpec) -> Self {
        Self {
            inner: Arc::new(Inner {
                spec,
                tx: broadcast::channel(RING_FRAMES).0,
                seq: AtomicU32::new(0),
                state: Mutex::new((0, None)),
            }),
        }
    }

    /// Joins the stream, starting capture if this is the first listener. Must be called inside a tokio runtime.
    pub fn listen(&self) -> Listener {
        let rx = self.inner.tx.subscribe();
        let mut st = self.inner.state.lock().unwrap();
        st.0 += 1;
        if st.1.is_none() {
            st.1 = Some(tokio::spawn(capture_loop(self.inner.clone())));
        }
        Listener {
            rx,
            inner: self.inner.clone(),
        }
    }

    pub fn listener_count(&self) -> usize {
        self.inner.state.lock().unwrap().0
    }
}

impl Listener {
    /// Next frame. Frames this listener was too slow for are skipped (real-time audio, not a recording).
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

impl Drop for Listener {
    fn drop(&mut self) {
        let mut st = self.inner.state.lock().unwrap();
        st.0 = st.0.saturating_sub(1);
        if st.0 == 0 {
            if let Some(h) = st.1.take() {
                h.abort(); // the child is `kill_on_drop`, so aborting the task kills the capture process
            }
        }
    }
}

async fn capture_loop(inner: Arc<Inner>) {
    let mut backoff = Duration::from_millis(300);
    loop {
        let mut enc = match new_encoder() {
            Ok(e) => e,
            Err(e) => {
                tracing::error!("opus encoder: {e}");
                return;
            }
        };
        let child = Command::new(&inner.spec.program)
            .args(&inner.spec.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn();
        if let Ok(mut child) = child {
            if let Some(mut out) = child.stdout.take() {
                let mut pcm = vec![0u8; FRAME_BYTES];
                let mut samples = vec![0i16; FRAME_SAMPLES * AUDIO_CHANNELS as usize];
                let mut packet = [0u8; 1500];
                while out.read_exact(&mut pcm).await.is_ok() {
                    backoff = Duration::from_millis(300);
                    for (s, b) in samples.iter_mut().zip(pcm.as_chunks::<2>().0) {
                        *s = i16::from_le_bytes(*b);
                    }
                    if let Ok(n) = enc.encode(&samples, &mut packet) {
                        let seq = inner.seq.fetch_add(1, Ordering::Relaxed);
                        let _ = inner.tx.send(Arc::new(frame(seq, &packet[..n])));
                        // no receivers is fine
                    }
                }
            }
        }
        // the capture ended (device change, server restart): try again while somebody is still listening
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn sine(frames: usize, amp: f32) -> Vec<i16> {
        (0..frames * FRAME_SAMPLES)
            .flat_map(|i| {
                let v =
                    (amp * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48_000.0).sin()) as i16;
                [v, v]
            })
            .collect()
    }

    #[test]
    fn frame_format_round_trips_and_rejects_garbage() {
        let f = frame(0x0102_0304, &[9, 8, 7]);
        assert_eq!(
            &f[..5],
            &[0x01, 4, 3, 2, 1],
            "tag then little-endian sequence"
        );
        assert_eq!(parse_frame(&f), Some((0x0102_0304, &[9u8, 8, 7][..])));
        for bad in [
            &[][..],
            &[1][..],
            &[1, 0, 0, 0, 0][..],
            &[2, 0, 0, 0, 0, 1][..],
        ] {
            assert_eq!(parse_frame(bad), None);
        }
    }

    #[test]
    fn a_tone_survives_encode_and_decode_at_a_sane_bitrate() {
        let pcm = sine(50, 12_000.0); // one second
        let mut enc = new_encoder().unwrap();
        let mut dec = opus::Decoder::new(AUDIO_RATE, Channels::Stereo).unwrap();
        let (mut total, mut decoded) = (0usize, Vec::new());
        for chunk in pcm.chunks(FRAME_SAMPLES * 2) {
            let mut out = [0u8; 1500];
            let n = enc.encode(chunk, &mut out).unwrap();
            total += n;
            let mut back = vec![0i16; FRAME_SAMPLES * 2];
            assert_eq!(
                dec.decode(&out[..n], &mut back, false).unwrap(),
                FRAME_SAMPLES
            );
            decoded.extend(back);
        }
        let rms = |v: &[i16]| {
            (v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / v.len() as f64).sqrt()
        };
        let (a, b) = (
            rms(&pcm[FRAME_SAMPLES * 4..]),
            rms(&decoded[FRAME_SAMPLES * 4..]),
        );
        assert!(
            (b / a - 1.0).abs() < 0.25,
            "decoded loudness within 25% of the original ({a:.0} vs {b:.0})"
        );
        assert!(
            total < 20_000,
            "about 96 kbit/s, not raw PCM: {total} bytes per second"
        );
        assert!(total > 2_000, "a tone is not silence: {total} bytes");
    }

    /// A fake capture: logs its PID on start and writes real-time silence in 20 ms frames.
    fn fake_capture(dir: &std::path::Path) -> CaptureSpec {
        let p = dir.join("fake-parec");
        std::fs::write(&p, format!("#!/bin/sh\necho $$ >> {log}\nwhile true; do head -c {FRAME_BYTES} /dev/zero; sleep 0.02; done\n", log = dir.join("cap.log").display())).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        CaptureSpec {
            program: p,
            args: vec![],
        }
    }

    /// PIDs of every capture process the fake has started so far.
    fn log(dir: &std::path::Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("cap.log"))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    fn alive(pid: &str) -> bool {
        // a killed child may linger as a zombie for a moment; /proc/<pid>/stat tells us
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .map(|s| !s.contains(") Z"))
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn capture_runs_only_while_somebody_listens() {
        let d = tempfile::tempdir().unwrap();
        let s = AudioStreamer::new(fake_capture(d.path()));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            log(d.path()).is_empty(),
            "nothing is captured while nobody listens"
        );

        let mut a = s.listen();
        let mut b = s.listen();
        assert_eq!(s.listener_count(), 2);
        let (f1, f2) = (a.recv().await.unwrap(), b.recv().await.unwrap());
        assert!(parse_frame(&f1).is_some() && parse_frame(&f2).is_some());
        assert_eq!(
            log(d.path()).len(),
            1,
            "two listeners share one capture process"
        );
        let pid = log(d.path())[0].clone();
        assert!(alive(&pid));

        // frames arrive in order, about one every 20 ms
        let t = std::time::Instant::now();
        let first = parse_frame(&a.recv().await.unwrap()).unwrap().0;
        let mut last = first;
        for _ in 0..24 {
            let n = parse_frame(&a.recv().await.unwrap()).unwrap().0;
            assert_eq!(n, last + 1, "no gaps for a prompt listener");
            last = n;
        }
        let ms = t.elapsed().as_millis();
        assert!(
            (350..900).contains(&ms),
            "25 frames took {ms} ms (expected ~500)"
        );

        drop(a);
        assert_eq!(s.listener_count(), 1);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(alive(&pid), "capture continues for the remaining listener");
        drop(b);
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            !alive(&pid),
            "the last listener leaving kills the capture process"
        );

        let seq_before = last;
        let mut c = s.listen();
        assert!(c.recv().await.is_some());
        assert_eq!(
            log(d.path()).len(),
            2,
            "a new listener starts a fresh capture"
        );
        let n = parse_frame(&c.recv().await.unwrap()).unwrap().0;
        assert!(
            n > seq_before,
            "sequence numbers keep increasing across restarts ({n} > {seq_before})"
        );
    }

    #[tokio::test]
    async fn a_slow_listener_skips_frames_and_does_not_hold_others_back() {
        let d = tempfile::tempdir().unwrap();
        let s = AudioStreamer::new(fake_capture(d.path()));
        let mut fast = s.listen();
        let mut slow = s.listen();
        let first_slow = parse_frame(&slow.recv().await.unwrap()).unwrap().0;
        let mut last_fast = 0;
        for _ in 0..120 {
            last_fast = parse_frame(&fast.recv().await.unwrap()).unwrap().0; // ~2.4 s, more than the 1.28 s ring
        }
        let resumed = parse_frame(&slow.recv().await.unwrap()).unwrap().0;
        assert!(resumed > first_slow + 5, "the slow listener jumped ahead instead of replaying old audio ({first_slow} -> {resumed})");
        assert!(last_fast >= resumed, "and the fast one was never delayed");
    }

    #[tokio::test]
    async fn a_broken_capture_command_does_not_crash_or_spin() {
        let s = AudioStreamer::new(CaptureSpec {
            program: "/nonexistent/parec".into(),
            args: vec![],
        });
        let mut l = s.listen();
        assert!(
            tokio::time::timeout(Duration::from_millis(600), l.recv())
                .await
                .is_err(),
            "no frames, no panic"
        );
        drop(l);
    }
}
