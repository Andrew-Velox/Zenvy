// Drives the real phone: reads where things are from the running app (its WebView debug socket, debug builds only) and
// taps there with real touches through adb. Every tap first checks that the Zenvy app is the app in front, so nothing
// is ever tapped in another app, and the phone is never unlocked.
import { spawnSync } from 'node:child_process';
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const sleep = ms => new Promise(r => setTimeout(r, ms));
export const adb = (...a) => spawnSync('adb', a, { encoding: 'utf8' }).stdout || '';

export class Phone {
  async connect(port = 9333) {
    const pid = (adb('shell', 'cat /proc/net/unix').match(/webview_devtools_remote_(\d+)/) || [])[1];
    if (!pid) throw new Error('no debuggable WebView found (the debug build of the app must be in front)');
    adb('forward', `tcp:${port}`, `localabstract:webview_devtools_remote_${pid}`);
    const pages = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
    const page = pages.find(p => p.type === 'page' && /^https:/.test(p.url));
    if (!page) throw new Error('the app is not showing the PC page');
    this.ws = new WebSocket(page.webSocketDebuggerUrl);
    await new Promise((r, j) => { this.ws.addEventListener('open', r); this.ws.addEventListener('error', j); });
    this.n = 0; this.pending = new Map();
    this.ws.addEventListener('message', e => { const m = JSON.parse(e.data); if (m.id && this.pending.has(m.id)) { this.pending.get(m.id)(m); this.pending.delete(m.id); } });
    this.dpr = await this.ev('window.devicePixelRatio');
    await this.calibrate();
  }

  ev(expr) {
    return new Promise((res, rej) => {
      const id = ++this.n; this.pending.set(id, m => m.error || m.result.exceptionDetails ? rej(new Error(expr.slice(0, 80) + ' -> ' + JSON.stringify(m.error || m.result.exceptionDetails.text))) : res(m.result.result.value));
      this.ws.send(JSON.stringify({ id, method: 'Runtime.evaluate', params: { expression: expr, returnByValue: true, awaitPromise: true } }));
    });
  }

  focused() { return adb('shell', 'dumpsys window | grep mCurrentFocus').includes('app.zenvy.remote'); }
  must() { if (!this.focused()) throw new Error('the Zenvy app is not in front on the phone: stopping'); }
  tapXY(x, y) { this.must(); adb('shell', 'input', 'tap', String(Math.round(x)), String(Math.round(y))); }
  async swipe(x1, y1, x2, y2, ms = 500) { this.must(); await this.ripple(x1, y1); adb('shell', 'input', 'swipe', ...[x1, y1, x2, y2, ms].map(v => String(Math.round(v)))); }

  /** One tap on a spot in the empty part of the page tells where the page sits on the screen. */
  async calibrate() {
    await this.ev(`window.__zvtap = null; addEventListener('pointerdown', e => { window.__zvtap = [e.clientX, e.clientY]; }, true); 1`);
    this.tapXY(540, 2000); await sleep(400);
    const t = await this.ev('window.__zvtap');
    if (!t) throw new Error('calibration tap not seen');
    this.offX = 540 - t[0] * this.dpr; this.offY = 2000 - t[1] * this.dpr;
  }

  async center(sel) {
    const r = await this.ev(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); if (!e) return null; const b = e.getBoundingClientRect(); return [b.x, b.y, b.width, b.height]; })()`);
    if (!r) throw new Error('not on the page: ' + sel);
    return { x: (r[0] + r[2] / 2) * this.dpr + this.offX, y: (r[1] + r[3] / 2) * this.dpr + this.offY, w: r[2] * this.dpr, h: r[3] * this.dpr, left: r[0] * this.dpr + this.offX, top: r[1] * this.dpr + this.offY };
  }

  async waitFor(sel, ms = 6000) {
    const end = Date.now() + ms;
    while (Date.now() < end) { if (await this.ev(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); return !!e && e.getClientRects().length > 0 && getComputedStyle(e).visibility !== 'hidden'; })()`)) return; await sleep(150); }
    throw new Error('did not appear: ' + sel);
  }

  /** Scrolls the target into view first (the workspace chips scroll sideways), then taps its centre. */
  async tap(sel) {
    await this.waitFor(sel);
    const bar = await this.ev(`(() => { const b = document.getElementById('wsbar'); return b ? b.scrollLeft : 0; })()`);
    await this.ev(`document.querySelector(${JSON.stringify(sel)}).scrollIntoView({ block: 'nearest', inline: 'center' })`);
    await sleep(350);
    const c = await this.center(sel); await this.ripple(c.x, c.y); this.tapXY(c.x, c.y);
    // the workspace chips scrolled to bring the target into reach: put them back, so the row reads from "1" again
    await sleep(500); await this.ev(`(() => { const b = document.getElementById('wsbar'); if (b) b.scrollLeft = ${bar}; return 1; })()`);
  }

  /** Focuses a text field without raising the on-screen keyboard (it resizes the page and blanks the recording). */
  async focusQuietly(sel) {
    await this.ev(`(() => { const q = document.querySelector(${JSON.stringify(sel)}); q.setAttribute('inputmode', 'none'); q.focus(); return 1; })()`);
    await sleep(300);
  }

  /** Taps the same spot twice from one adb command, about a second apart (for "tap to arm, tap again to confirm"). */
  async tapTwice(sel) {
    await this.waitFor(sel); const c = await this.center(sel);
    this.must(); await this.ripple(c.x, c.y);
    const [x, y] = [Math.round(c.x), Math.round(c.y)];
    adb('shell', `input tap ${x} ${y}; sleep 0.3; input tap ${x} ${y}`);
  }

  /** Sends any DevTools command and waits for its answer. */
  cdp(method, params = {}) {
    return new Promise((res, rej) => {
      const id = ++this.n; this.pending.set(id, m => m.error ? rej(new Error(method + ': ' + m.error.message)) : res(m.result));
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }

  /**
   * A double tap. `adb shell input tap` takes most of a second per tap on some phones and the shell may not write to the
   * touchscreen, so the two quick touches go through the app's debug connection (the same touch events a finger makes, with
   * exact timing): two touches within Dolphin's double-click time.
   */
  async doubleTap(x, y, gapMs = 90) {
    this.must(); await this.ripple(x, y);
    const pt = [{ x: (x - this.offX) / this.dpr, y: (y - this.offY) / this.dpr, id: 1 }];
    for (let i = 0; i < 2; i++) {
      await this.cdp('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: pt }); await sleep(45);
      await this.cdp('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
      if (i === 0) await sleep(gapMs);
    }
  }

  /** Draws a ring where a real touch lands (the recording has no touch indicator of its own). Takes screen pixels. */
  async ripple(x, y) {
    const cx = (x - this.offX) / this.dpr, cy = (y - this.offY) / this.dpr;
    await this.ev(`(() => { if (!document.getElementById('zv-tap-css')) { const s = document.createElement('style'); s.id = 'zv-tap-css'; s.textContent = '.zv-tap{position:fixed;width:52px;height:52px;margin:-26px 0 0 -26px;border-radius:50%;border:2.5px solid rgba(0,122,255,.95);background:rgba(0,122,255,.22);pointer-events:none;z-index:99999;animation:zvtap .6s ease-out forwards}@keyframes zvtap{from{transform:scale(.4);opacity:1}to{transform:scale(1.35);opacity:0}}'; document.head.appendChild(s); } const d = document.createElement('i'); d.className = 'zv-tap'; d.style.left = '${cx}px'; d.style.top = '${cy}px'; document.body.appendChild(d); setTimeout(() => d.remove(), 700); return 1; })()`);
    await sleep(120);
  }

  /** Streams the app's own rendering (no status bar, no notifications) as JPEG frames with the time each arrived. */
  startScreencast(dir) {
    mkdirSync(dir, { recursive: true }); this.shots = 0; this.frames = [];
    this.ws.addEventListener('message', e => {
      const m = JSON.parse(e.data); if (m.method !== 'Page.screencastFrame') return;
      const t = Date.now(), name = `p${String(this.shots++).padStart(5, '0')}.jpg`;
      writeFileSync(join(dir, name), Buffer.from(m.params.data, 'base64')); this.frames.push({ t, name });
      this.ws.send(JSON.stringify({ id: 100000 + this.shots, method: 'Page.screencastFrameAck', params: { sessionId: m.params.sessionId } }));
    });
    this.ws.send(JSON.stringify({ id: 99999, method: 'Page.startScreencast', params: { format: 'jpeg', quality: 88, maxWidth: 540, maxHeight: 1170, everyNthFrame: 1 } }));
  }
  stopScreencast() { try { this.ws.send(JSON.stringify({ id: 99998, method: 'Page.stopScreencast' })); } catch {} }

  /** Types through the phone's own keyboard, one character at a time like a person. */
  async type(text, pace = 120) {
    const words = text.split(/(?<= )/);   // a word and the space after it per touch of adb, which is slow per call
    for (const w of words) { this.must(); adb('shell', 'input', 'text', w.replace(/ /g, '%s')); await sleep(pace); }
  }
  enter() { this.must(); adb('shell', 'input', 'keyevent', '66'); }
}
