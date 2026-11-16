// Makes the side-by-side README demo: the real phone UI next to a simulated Hyprland desktop (tools/demo/stage.html).
//
//   node tools/demo.mjs            # records docs/media/demo.webp
//   BROWSER=brave OUT=/tmp/zv node tools/demo.mjs inspect   # connects and prints some page facts (for development)
//
// It serves the page and the stage itself, starts its own headless browser with a temporary profile and stops only
// that process. Needs Brave or Chrome, Node 22 and ffmpeg (with libwebp).
import { spawn, execFileSync } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const BROWSER = process.env.BROWSER || 'brave';
const OUT = resolve(process.env.OUT || join(root, 'docs/media/raw-demo'));
const FINAL = resolve(process.env.FINAL || join(root, 'docs/media/demo.webp'));
const mode = process.argv[2] || 'record';
const PORT = 9334, WEB = 8766;
const sleep = ms => new Promise(r => setTimeout(r, ms));
mkdirSync(join(OUT, 'frames'), { recursive: true });

const files = { '/index.html': [join(root, 'crates/zenvy-net/src/page.html'), 'text/html'], '/stage.html': [join(root, 'tools/demo/stage.html'), 'text/html'], '/logo.svg': [join(root, 'assets/logo/zenvy-icon.svg'), 'image/svg+xml'] };
const web = createServer((req, res) => { const f = files[req.url.split('?')[0]]; if (!f) { res.writeHead(404).end(); return; } res.writeHead(200, { 'content-type': f[1] + '; charset=utf-8' }).end(readFileSync(f[0])); }).listen(WEB, '127.0.0.1');

const prof = mkdtempSync(join(tmpdir(), 'zenvy-demo-'));
const proc = spawn(BROWSER, ['--headless=new', `--user-data-dir=${prof}`, `--remote-debugging-port=${PORT}`, '--no-first-run', '--no-default-browser-check', '--disable-extensions', '--hide-scrollbars', '--force-color-profile=srgb', 'about:blank'], { stdio: 'ignore' });
let cleaned = false; const cleanup = () => { if (cleaned) return; cleaned = true; try { proc.kill('SIGTERM'); } catch {} try { web.close(); } catch {} setTimeout(() => { try { rmSync(prof, { recursive: true, force: true }); } catch {} }, 1500); };
process.on('exit', cleanup); process.on('SIGINT', () => { cleanup(); process.exit(1); });

let target; for (let i = 0; i < 60 && !target; i++) { try { target = (await (await fetch(`http://127.0.0.1:${PORT}/json/list`)).json()).find(t => t.type === 'page'); } catch {} if (!target) await sleep(250); }
if (!target) { cleanup(); throw new Error('browser did not start'); }
const ws = new WebSocket(target.webSocketDebuggerUrl); await new Promise(r => ws.addEventListener('open', r));
let n = 0; const pending = new Map();
ws.addEventListener('message', e => { const m = JSON.parse(e.data); if (m.id && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id); } });
const cdp = (method, params = {}) => new Promise((res, rej) => { const id = ++n; pending.set(id, m => m.error ? rej(new Error(method + ': ' + m.error.message)) : res(m.result)); ws.send(JSON.stringify({ id, method, params })); });
const run = async expr => { const r = await cdp('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true }); if (r.exceptionDetails) throw new Error(expr.slice(0, 100) + ' -> ' + (r.exceptionDetails.exception?.description || r.exceptionDetails.text)); return r.result.value; };
// run code inside the phone's frame
const P = code => run(`document.getElementById('ph').contentWindow.eval(${JSON.stringify(code)})`);

await cdp('Page.enable');
await cdp('Emulation.setDeviceMetricsOverride', { width: 1280, height: 720, deviceScaleFactor: 1, mobile: false });
await cdp('Page.navigate', { url: `http://localhost:${WEB}/stage.html` }); await sleep(800);
await run('stage.start()'); await sleep(1800);

if (mode === 'inspect') {
  console.log(await P(`JSON.stringify({view, tab, hasWs: !!ws, readyState: ws && ws.readyState, windows: state && state.windows.length, nav: [...document.querySelectorAll('#nav button')].map(b => b.dataset.tab)})`));
  await P(`setTab('media')`); await sleep(400);
  console.log('media', await P(`JSON.stringify({ranges: [...document.querySelectorAll('#v-media input[type=range]')].map(r => r.id || r.dataset.vol || r.className || r.getAttribute('aria-label')), buttons: [...document.querySelectorAll('#v-media button')].map(b => (b.id || b.className) + ':' + (b.getAttribute('aria-label') || b.textContent.trim()).slice(0, 18)).slice(0, 14)})`));
  await P(`setTab('screen')`); await sleep(400);
  console.log('screen', await P(`JSON.stringify({ranges: [...document.querySelectorAll('#sc-bright input')].map(r => r.dataset.bd), go: !!document.getElementById('sc-go')})`));
  console.log('lock dock', await P(`JSON.stringify([...document.querySelectorAll('#lk-dock button, #lk-dock input')].map(b => b.id + ':' + (b.getAttribute('aria-label') || '')))`));
  console.log('pad', await P(`JSON.stringify(!!document.getElementById('padwrap'))`));
  await cdp('Page.captureScreenshot', { format: 'png' }).then(r => writeFileSync(join(OUT, 'inspect.png'), Buffer.from(r.data, 'base64')));
  cleanup(); await sleep(300); process.exit(0);
}

if (mode === 'debug-media') {
  await P(`setTab('media')`); await sleep(500);
  console.log('before', await P(`!!document.getElementById('mplay')`));
  await P(`(() => { const q = document.querySelector('#vol'); q.value = 80; q.dispatchEvent(new Event('input', { bubbles: true })); q.dispatchEvent(new Event('change', { bubbles: true })); })()`); await sleep(600);
  console.log('after vol', await P(`!!document.getElementById('mplay')`), await P(`document.getElementById('m-player').innerHTML.length`));
  await sleep(2000);
  console.log('later', await P(`!!document.getElementById('mplay')`), await P(`JSON.stringify({view, media: media && media.players.length, cur: mediaSel})`));
  cleanup(); await sleep(300); process.exit(0);
}
// ------------------------------------------------------------------ the tour
await P(`(() => { const s = document.createElement('style'); s.textContent = '.zv-tap{position:fixed;width:52px;height:52px;margin:-26px 0 0 -26px;border-radius:50%;border:2.5px solid rgba(0,122,255,.95);background:rgba(0,122,255,.22);pointer-events:none;z-index:99999;animation:zvtap .55s ease-out forwards}@keyframes zvtap{from{transform:scale(.4);opacity:1}to{transform:scale(1.35);opacity:0}}'; document.head.appendChild(s); })()`);
const dir = join(OUT, 'frames'); const durs = []; let fi = 0, last = Date.now();
const frame = async () => { const r = await cdp('Page.captureScreenshot', { format: 'jpeg', quality: 86 }); const t = Date.now(); const name = `f${String(fi++).padStart(5, '0')}.jpg`; writeFileSync(join(dir, name), Buffer.from(r.data, 'base64')); durs.push([name, Math.max(0.04, (t - last) / 1000)]); last = t; };
const hold = async ms => { const end = Date.now() + ms; while (Date.now() < end) await frame(); };
const cap = async (a, b) => { await run(`stage.caption(${JSON.stringify(a)}, ${JSON.stringify(b)})`); };
const tapEl = async sel => { await P(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); if (!e) throw new Error('no ${sel}'); const r = e.getBoundingClientRect(); const d = document.createElement('i'); d.className = 'zv-tap'; d.style.left = (r.x + r.width / 2) + 'px'; d.style.top = (r.y + r.height / 2) + 'px'; document.body.appendChild(d); setTimeout(() => d.remove(), 600); })()`); await hold(240); await P(`document.querySelector(${JSON.stringify(sel)}).click()`); };
const ripple = async (x, y) => { await P(`(() => { const d = document.createElement('i'); d.className = 'zv-tap'; d.style.left = '${x}px'; d.style.top = '${y}px'; document.body.appendChild(d); setTimeout(() => d.remove(), 600); })()`); await hold(240); };
const pe = (sel, type, x, y) => P(`document.querySelector(${JSON.stringify(sel)}).dispatchEvent(new PointerEvent('${type}',{bubbles:true,cancelable:true,pointerId:7,clientX:${x},clientY:${y},isPrimary:true,button:0,pointerType:'touch'}))`);
const rect = sel => P(`(() => { const r = document.querySelector(${JSON.stringify(sel)}).getBoundingClientRect(); return [r.x, r.y, r.width, r.height]; })()`);
const typeInto = async (sel, text, step = 130, enter = false) => { for (let i = 1; i <= text.length; i++) { await P(`(() => { const q = document.querySelector(${JSON.stringify(sel)}); q.value = ${JSON.stringify(text)}.slice(0, ${i}); q.dispatchEvent(new Event('input', { bubbles: true })); })()`); await hold(step); } if (enter) await P(`document.querySelector(${JSON.stringify(sel)}).dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }))`); };
const slide = async (sel, from, to, steps = 14) => { for (let i = 0; i <= steps; i++) { const v = Math.round(from + (to - from) * i / steps); await P(`(() => { const q = document.querySelector(${JSON.stringify(sel)}); q.value = ${v}; q.dispatchEvent(new Event('input', { bubbles: true })); })()`); await hold(70); } await P(`document.querySelector(${JSON.stringify(sel)}).dispatchEvent(new Event('change', { bubbles: true }))`); };

await cap('Control your Hyprland PC from your phone', 'The phone shows the real app. The desktop is a simulation.'); await hold(2200);

await cap('Switch workspaces', 'Tap a workspace on the phone and the PC follows.');
await tapEl('[data-ws="2"]'); await hold(1500); await tapEl('[data-ws="1"]'); await hold(1200);

await cap('Open an app on a new workspace', 'Tap +, search your installed apps, tap one. It opens on the first empty workspace.');
await tapEl('#ws-add'); await hold(900); await typeInto('#ap-q', 'spo', 150); await hold(500); await tapEl('[data-app=spotify]'); await hold(2300);

await cap('Move a window to another workspace', 'Choose where it goes, and whether to follow it there.');
await tapEl('[data-move]'); await hold(900); await tapEl('#sheet .set-row'); await hold(500); await tapEl('[data-mvto="1"]'); await hold(2300);

await cap('Touchpad', 'Move, click and scroll. Focus follows the pointer, just like at the PC.');
await tapEl('#nav [data-tab=pad]'); await hold(700);
{ const [px, py, pw, ph] = await rect('#padwrap'); for (let t = 0; t <= 24; t++) { const k = t / 24; await pe('#padwrap', t === 0 ? 'pointerdown' : 'pointermove', px + pw * (0.78 - 0.55 * k), py + ph * (0.4 + 0.2 * k)); await hold(55); }
  await pe('#padwrap', 'pointerup', px + pw * 0.23, py + ph * 0.6); await hold(500);
  await pe('#padwrap', 'pointerdown', px + pw * 0.5, py + ph * 0.5); await hold(60); await pe('#padwrap', 'pointerup', px + pw * 0.5, py + ph * 0.5); await hold(900); }

await cap('Keyboard', 'Type on the phone and it types on the PC, any layout.');
await tapEl('#nav [data-tab=keys]'); await hold(700); await typeInto('#kb', 'cargo test', 140, true); await hold(2600);

await cap('Media and volume', 'The PC\'s volume and player, from your pocket.');
await tapEl('#nav [data-tab=media]'); await hold(900); await slide('#vol', 60, 92); await hold(300); await slide('#vol', 92, 38); await hold(500); await tapEl('.mplay'); await hold(1100); await tapEl('.mplay'); await hold(700);

await cap('See the PC screen, and click it', 'A live picture of the monitor. Tap it to click. Brightness is right above.');
await tapEl('#nav [data-tab=screen]'); await hold(700); await tapEl('#sc-go'); await hold(1600);
await slide('[data-bd]', 70, 25); await hold(500); await slide('[data-bd]', 25, 70); await hold(400);
{ const [cx, cy, cw, ch] = await rect('#sc-cv'); const x = cx + cw * 0.72, y = cy + ch * 0.3; await ripple(x, y); await pe('#sc-cv', 'pointerdown', x, y); await hold(60); await pe('#sc-cv', 'pointerup', x, y); await hold(900); }

await cap('A floating screen window', 'It stays on top of every tab. Drag it, resize it, keep controlling.');
await tapEl('#nav [data-tab=pad]'); await hold(1300);
{ const g = await P(`(() => { const r = document.getElementById('sc-grip').getBoundingClientRect(); return [r.x + 10, r.y + 14]; })()`); for (let t = 1; t <= 10; t++) { await pe('#sc-grip', t === 1 ? 'pointerdown' : 'pointermove', g[0] - t * 5, g[1] - t * 34); await hold(60); } await pe('#sc-mh', 'pointerup', g[0] - 50, g[1] - 340); await hold(1100); }
await tapEl('#sc-mx'); await hold(500);

await cap('Lock and unlock', 'Lock the PC from the phone. Type the password on the phone to unlock it.');
await tapEl('#pwr'); await hold(900); await tapEl('[data-sa=lock]'); await hold(2200);
await typeInto('#lk-pw', 'demo-pass', 120); await hold(400); await tapEl('#lk-go'); await hold(2600);

await cap('Zenvy', 'Open source. No cloud, no account, no telemetry.'); await hold(1800);

writeFileSync(join(OUT, 'frames.txt'), durs.map(([f, d]) => `file 'frames/${f}'\nduration ${d.toFixed(3)}`).join('\n') + `\nfile 'frames/${durs.at(-1)[0]}'\n`);
console.log('frames', durs.length, 'seconds', durs.reduce((a, b) => a + b[1], 0).toFixed(1));
cleanup(); await sleep(300);
execFileSync('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-y', '-f', 'concat', '-safe', '0', '-i', join(OUT, 'frames.txt'), '-vf', `fps=${process.env.FPS || 12},scale=${process.env.WIDTH || 960}:-1:flags=lanczos`, '-c:v', 'libwebp_anim', '-quality', process.env.QUALITY || '68', '-compression_level', '6', '-loop', '0', '-an', FINAL]);
console.log('wrote', FINAL);
process.exit(0);
