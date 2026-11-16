// Zenvy phone speaker: plays decoded PC audio from a ring buffer on the audio thread, so UI work on the page
// can never make the sound late. The buffer size adapts: it grows after a gap (Wi-Fi hiccup) and slowly shrinks
// again while playback is clean, so delay stays as low as the connection allows.
class ZenvyPcm extends AudioWorkletProcessor {
  constructor(options) {
    super();
    const o = (options && options.processorOptions) || {};
    this.cap = Math.floor(sampleRate); // one second of ring
    this.l = new Float32Array(this.cap);
    this.r = new Float32Array(this.cap);
    this.rd = 0; this.wr = 0; this.fill = 0;
    this.min = 0.04 * sampleRate;
    this.max = 0.4 * sampleRate;
    this.target = Math.min(this.max, Math.max(this.min, (o.target || 0.08) * sampleRate));
    this.playing = false;
    this.under = 0; this.trims = 0;
    this.clean = 0;   // frames played since the last gap
    this.sincePost = 0;
    this.port.onmessage = (e) => { const d = e.data; if (d && d.l && d.r) this.push(d.l, d.r); };
  }

  push(l, r) {
    const n = Math.min(l.length, r.length, this.cap);
    const over = this.fill + n - this.cap;
    if (over > 0) { this.rd = (this.rd + over) % this.cap; this.fill -= over; this.trims++; }
    for (let i = 0; i < n; i++) { const p = (this.wr + i) % this.cap; this.l[p] = l[i]; this.r[p] = r[i]; }
    this.wr = (this.wr + n) % this.cap;
    this.fill += n;
    if (this.playing) this.trim();
  }

  // A stall followed by a burst leaves the buffer far too full: jump back to the target instead of playing
  // seconds late (one small glitch beats permanent delay).
  trim() {
    if (this.fill > this.target + 0.15 * sampleRate) {
      const drop = this.fill - this.target;
      this.rd = (this.rd + drop) % this.cap; this.fill -= drop; this.trims++;
    }
  }

  process(_inputs, outputs) {
    const out = outputs[0];
    const L = out[0], R = out[1] || out[0];
    const n = L.length;
    if (!this.playing) {
      if (this.fill >= this.target) { this.playing = true; this.trim(); }
      else { L.fill(0); if (R !== L) R.fill(0); this.report(n); return true; }
    }
    const have = Math.min(n, this.fill);
    for (let i = 0; i < have; i++) { const p = (this.rd + i) % this.cap; L[i] = this.l[p]; R[i] = this.r[p]; }
    for (let i = have; i < n; i++) { L[i] = 0; R[i] = 0; }
    this.rd = (this.rd + have) % this.cap; this.fill -= have;
    if (have < n) {
      // ran dry: pause until the target is buffered again, and be more careful next time
      this.playing = false; this.under++; this.clean = 0;
      this.target = Math.min(this.max, this.target + 0.02 * sampleRate);
    } else {
      this.clean += n;
      if (this.clean > 20 * sampleRate) { this.clean = 0; this.target = Math.max(this.min, this.target - 0.01 * sampleRate); }
    }
    this.report(n);
    return true;
  }

  report(n) {
    this.sincePost += n;
    if (this.sincePost >= sampleRate / 2) {
      this.sincePost = 0;
      this.port.postMessage({ fill: this.fill / sampleRate, target: this.target / sampleRate, under: this.under, trims: this.trims, playing: this.playing });
    }
  }
}
registerProcessor('zenvy-pcm', ZenvyPcm);
