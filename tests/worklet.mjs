// Runs the phone's AudioWorklet processor in Node with simulated Wi-Fi jitter (no audio hardware needed).
import fs from 'node:fs';
import vm from 'node:vm';
let pass = 0, fail = 0;
const ok = (c, m) => { c ? pass++ : fail++; console.log(`${c ? 'PASS' : 'FAIL'}  ${m}`); };
const SR = 48000, N = 128;

function make(target) {
  let Cls; const sent = [];
  const ctx = { sampleRate: SR, AudioWorkletProcessor: class { constructor() { this.port = { postMessage: m => sent.push(m), onmessage: null }; } }, registerProcessor: (_n, c) => { Cls = c; }, console };
  vm.runInNewContext(fs.readFileSync(new URL('../crates/zenvy-net/src/audio-worklet.js', import.meta.url), 'utf8'), ctx);
  const p = new Cls({ processorOptions: { target } });
  let ctr = 0; // a ramp, so we can see ordering, duplication and dropped samples in the output
  const frame = (n = 960) => { const l = new Float32Array(n), r = new Float32Array(n); for (let i = 0; i < n; i++) { l[i] = ctr++; r[i] = l[i]; } return { l, r }; };
  const feed = () => { const f = frame(); p.port.onmessage({ data: f }); };
  const pull = () => { const L = new Float32Array(N), R = new Float32Array(N); p.process([], [[L, R]]); return L; };
  return { p, feed, pull, sent };
}

// Drives time in 128-sample ticks; `arrive(tick)` says how many 20 ms frames arrive at that tick.
function run(w, ticks, arrive) { const out = []; for (let t = 0; t < ticks; t++) { for (let k = arrive(t); k > 0; k--) w.feed(); out.push(...w.pull()); } return out; }
const perFrame = 960 / N; // ticks between frames (7.5)

// 1. steady delivery: no gaps, strictly in order, no repeats
{
  const w = make(0.06); let acc = 0;
  const out = run(w, 3000, () => { acc += 1 / perFrame; const k = Math.floor(acc); acc -= k; return k; });
  const played = out.filter(v => v !== 0);
  ok(w.p.under === 0, `steady delivery: no gaps (${w.p.under})`);
  ok(played.every((v, i) => i === 0 || v === played[i - 1] + 1), 'audio comes out complete and in order (no repeats, no lost samples)');
  ok(w.p.fill / SR < 0.1 && w.p.fill / SR > 0.02, `buffer sits near the target (${(w.p.fill / SR * 1000).toFixed(0)} ms)`);
}
// 2. a 150 ms Wi-Fi stall then a burst: one gap, buffer grows, delay does not stay high
{
  const w = make(0.05); let acc = 0;
  const arrive = t => { if (t > 1500 && t < 1500 + 150 * SR / 1000 / N) { acc += 1 / perFrame; return 0; } const k = Math.floor(acc + 1 / perFrame); const r = k; acc = acc + 1 / perFrame - k; return r; };
  run(w, 4000, arrive);
  ok(w.p.under >= 1, `the stall is a recorded gap (${w.p.under})`);
  ok(w.p.target > 0.05 * SR, `and the buffer target grows afterwards (${(w.p.target / SR * 1000).toFixed(0)} ms)`);
  ok(w.p.fill / SR < w.p.target / SR + 0.16, `latency is bounded after the burst (${(w.p.fill / SR * 1000).toFixed(0)} ms)`);
}
// 3. a long clean stretch shrinks the target back toward the minimum
{
  const w = make(0.2); let acc = 0;
  run(w, Math.floor(70 * SR / N), () => { acc += 1 / perFrame; const k = Math.floor(acc); acc -= k; return k; });
  ok(w.p.target / SR < 0.2 && w.p.target / SR >= 0.04 - 1e-9, `target shrinks while playback is clean (${(w.p.target / SR * 1000).toFixed(0)} ms, floor 40)`);
}
// 4. nothing arrives at all: silence, no crash, no NaN
{
  const w = make(0.06); const out = run(w, 500, () => 0);
  ok(out.every(v => v === 0), 'no data: pure silence');
}
// 5. a huge burst cannot overflow the ring or grow delay without bound
{
  const w = make(0.06); for (let i = 0; i < 200; i++) w.feed();
  w.pull();
  ok(w.p.fill <= w.p.cap && w.p.fill / SR < 0.3, `a 4 s burst is trimmed (${(w.p.fill / SR * 1000).toFixed(0)} ms left)`);
}
// 6. stats are reported about twice a second
{
  const w = make(0.06); run(w, Math.floor(3 * SR / N), () => 0);
  ok(w.sent.length >= 5 && w.sent.length <= 7 && 'fill' in w.sent[0] && 'under' in w.sent[0], `stats posted ~2/s (${w.sent.length} in 3 s)`);
}
console.log(`${pass} passed, ${fail} failed`); process.exit(fail ? 1 : 0);
