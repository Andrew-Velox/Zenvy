//! Developer tool: capture the real default output through the streamer for 3 seconds and report only statistics:
//! frame count, packet size, decoded loudness and, importantly, how evenly frames are delivered (bursts here
//! become stutter on the phone). Nothing is saved. Usage: probe [extra parec args...]
use std::time::{Duration, Instant};
#[tokio::main]
async fn main() {
    let mut spec = zenvy_stream::CaptureSpec::parec("parec");
    let extra: Vec<String> = std::env::args().skip(1).collect();
    for e in &extra {
        spec.args
            .retain(|a| a.split('=').next() != e.split('=').next());
        spec.args.push(e.clone());
    }
    let s = zenvy_stream::AudioStreamer::new(spec.clone());
    let mut l = s.listen();
    let mut dec = opus::Decoder::new(48_000, opus::Channels::Stereo).unwrap();
    let (t0, mut last, mut gaps, mut peak) = (Instant::now(), None::<Instant>, Vec::new(), 0i32);
    let mut bytes = 0usize;
    while t0.elapsed() < Duration::from_secs(3) {
        let Ok(Some(f)) = tokio::time::timeout(Duration::from_millis(500), l.recv()).await else {
            continue;
        };
        let now = Instant::now();
        if let Some(p) = last {
            gaps.push(now.duration_since(p).as_secs_f64() * 1000.0);
        }
        last = Some(now);
        let (_, pkt) = zenvy_stream::parse_frame(&f).unwrap();
        bytes += pkt.len();
        let mut pcm = vec![0i16; zenvy_stream::FRAME_SAMPLES * 2];
        dec.decode(pkt, &mut pcm, false).unwrap();
        peak = peak.max(pcm.iter().map(|v| (*v as i32).abs()).max().unwrap_or(0));
    }
    gaps.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |p: f64| gaps[((gaps.len() as f64 - 1.0) * p) as usize];
    let bursts = gaps.iter().filter(|g| **g < 5.0).count();
    println!("args: {}", spec.args.join(" "));
    println!("frames {} | {} bytes avg | peak {} | frame spacing ms: min {:.1} p50 {:.1} p95 {:.1} p99 {:.1} max {:.1} | {} frames arrived <5 ms after the previous one (bursts)", gaps.len() + 1, bytes / (gaps.len() + 1), peak, gaps[0], pct(0.5), pct(0.95), pct(0.99), gaps[gaps.len() - 1], bursts);
}
