// Captures the README screenshots and the demo animation frames from the phone UI's built-in demo mode
// (sample data, no connection to any PC). Needs Brave or Chrome and Node 22.
//
//   python3 -m http.server 8765 --directory <dir containing index.html = crates/zenvy-net/src/page.html>
//   BROWSER=brave OUT=docs/media/raw node tools/screenshots.mjs
//
// It starts its own headless browser with a temporary profile and stops only that process.
import { spawn } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const BROWSER = process.env.BROWSER || 'brave';
const BASE = process.env.BASE || 'http://localhost:8765/index.html';
const OUT = process.env.OUT || 'docs/media/raw';
const PORT = 9333;
const W = 412, H = 820;
mkdirSync(OUT, { recursive: true });
mkdirSync(join(OUT, 'frames'), { recursive: true });
const sleep = ms => new Promise(r => setTimeout(r, ms));

const prof = mkdtempSync(join(tmpdir(), 'zenvy-shots-'));
const proc = spawn(BROWSER, ['--headless=new', `--user-data-dir=${prof}`, `--remote-debugging-port=${PORT}`, '--no-first-run',
  '--no-default-browser-check', '--disable-extensions', '--hide-scrollbars', '--force-color-profile=srgb', 'about:blank'], { stdio: 'ignore' });
let cleaned = false;
const cleanup = () => { if (cleaned) return; cleaned = true; try { proc.kill('SIGTERM'); } catch {} setTimeout(() => { try { rmSync(prof, { recursive: true, force: true }); } catch {} }, 1500); };
process.on('exit', cleanup); process.on('SIGINT', () => { cleanup(); process.exit(1); });

let target;
for (let i = 0; i < 60 && !target; i++) {
  try { target = (await (await fetch(`http://127.0.0.1:${PORT}/json/list`)).json()).find(t => t.type === 'page'); } catch {}
  if (!target) await sleep(250);
}
if (!target) { cleanup(); throw new Error('browser did not start'); }

const ws = new WebSocket(target.webSocketDebuggerUrl);
await new Promise(r => ws.addEventListener('open', r));
let n = 0; const pending = new Map();
ws.addEventListener('message', e => { const m = JSON.parse(e.data); if (m.id && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id); } });
const cdp = (method, params = {}) => new Promise((res, rej) => { const id = ++n; pending.set(id, m => m.error ? rej(new Error(method + ': ' + m.error.message)) : res(m.result)); ws.send(JSON.stringify({ id, method, params })); });
const run = async expr => { const r = await cdp('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true }); if (r.exceptionDetails) throw new Error(expr.slice(0, 80) + ' -> ' + (r.exceptionDetails.exception?.description || r.exceptionDetails.text)); return r.result.value; };

let scale = 2;
const metrics = async () => cdp('Emulation.setDeviceMetricsOverride', { width: W, height: H, deviceScaleFactor: scale, mobile: true });
await cdp('Page.enable'); await metrics();

const MOCK = `(() => { // a believable but neutral desktop for the screen preview
  const c = document.createElement('canvas'); c.width = 960; c.height = 540; const g = c.getContext('2d');
  g.fillStyle = '#e9e9ec'; g.fillRect(0, 0, 960, 540);
  const win = (x, y, w, h, t, dark, lines) => { g.fillStyle = dark ? '#1d1d1f' : '#fbfbfc'; g.fillRect(x, y, w, h); g.fillStyle = dark ? '#2c2c2e' : '#e4e4e8'; g.fillRect(x, y, w, 26);
    g.fillStyle = dark ? '#aaa' : '#555'; g.font = '13px sans-serif'; g.fillText(t, x + 12, y + 18);
    lines.forEach((l, i) => { g.fillStyle = l[1] || (dark ? '#d8d8dc' : '#444'); g.font = '12px monospace'; g.fillText(l[0], x + 14, y + 50 + i * 20); }); };
  win(40, 50, 420, 300, 'zenvy plan', true, [['$ cargo test --workspace', '#9ad'], ['   Compiling zenvy-net v0.1.0'], ['   Compiling zenvy-apps v0.1.0'], ['test result: ok. 39 passed', '#7c7'], ['test result: ok. 16 passed', '#7c7'], ['$ zenvyd pair'], ['pairing code  4 8 1 5']]);
  win(300, 150, 600, 340, 'Hyprland wiki - Brave', false, [['Dispatchers', '#222'], ['workspace, movetoworkspace, exec', '#666'], ['movetoworkspacesilent, togglefloating', '#666']]);
  g.fillStyle = '#d4d4d8'; g.fillRect(0, 510, 960, 30); g.fillStyle = '#007aff'; for (let i = 0; i < 4; i++) g.fillRect(380 + i * 50, 518, 36, 14);
  return c; })()`;

async function goto(hash, after = '') {
  await cdp('Page.navigate', { url: `${BASE}?v=${Date.now()}#${hash}` });
  await sleep(900); await metrics();
  await run(`(() => { const s = document.createElement('style'); s.textContent = '.zv-tap{position:fixed;width:48px;height:48px;margin:-24px 0 0 -24px;border-radius:50%;border:2px solid rgba(0,122,255,.9);background:rgba(0,122,255,.2);pointer-events:none;z-index:99999;animation:zvtap .55s ease-out forwards}@keyframes zvtap{from{transform:scale(.4);opacity:1}to{transform:scale(1.35);opacity:0}}'; document.head.appendChild(s); })()`);
  if (after) await run(after);
  await sleep(450);
}
let shotN = 0;
async function shot(name) { const r = await cdp('Page.captureScreenshot', { format: 'png' }); writeFileSync(join(OUT, name + '.png'), Buffer.from(r.data, 'base64')); console.log('shot', name); }

// ---- common setup used by several scenes
const SETUP_APPS = `appsCfg={enabled:true};window.__apps=[{id:'brave-browser',name:'Brave'},{id:'org.gnome.Nautilus',name:'Files'},{id:'kitty',name:'kitty'},{id:'spotify',name:'Spotify'},{id:'org.telegram.desktop',name:'Telegram'},{id:'code',name:'Visual Studio Code'},{id:'vlc',name:'VLC media player'},{id:'obs',name:'OBS Studio'}];
  rpc=(t,d,cb)=>setTimeout(()=>cb(t==='apps'&&d.cmd==='list'?{t:'apps',data:{apps:window.__apps}}:t==='apps'?{t:'apps',data:{state:'launched',name:'App',workspace:4}}:{t:'ack',data:{ok:true}}),60);renderWs();`;
const SHOW_SCREEN = `(() => { const src = ${MOCK}; const cv = $('sc-cv'); cv.width = 960; cv.height = 540; cv.getContext('2d').drawImage(src, 0, 0); cv.hidden = false; $('sc-empty').hidden = true; window.__mock = src; scrFps = 8; const st = $('sc-stat'); st.hidden = false; st.textContent = '8 fps'; })()`;
const FLOAT_ON = `(() => { scrOn = true; const m = $('sc-mcv'); m.width = 960; m.height = 540; m.getContext('2d').drawImage(window.__mock || ${MOCK}, 0, 0); updateMini(); })()`;

const which = process.argv[2] || 'all';

if (which === 'all' || which === 'stills') {
  for (const theme of ['light', 'dark']) {
    for (const v of ['desk', 'pad', 'keys', 'media', 'lock', 'settings']) { await goto(`demo=${v}&theme=${theme}`); await shot(`${v}-${theme}`); }
    await goto(`demo=screen&theme=${theme}`, SHOW_SCREEN); await run(`scrOn=true;renderScreen();$('sc-cv').hidden=false;$('sc-empty').hidden=true`); await shot(`screen-${theme}`);
    await goto(`demo=desk&theme=${theme}`, SETUP_APPS); await run(`openApps('empty')`); await sleep(500); await run(`(()=>{const q=$('ap-q');q.value='';})()`); await shot(`launcher-${theme}`);
    await goto(`demo=desk&theme=${theme}`, SETUP_APPS); await run(`openMove(state.windows[1].address)`); await sleep(400); await shot(`move-${theme}`);
    await goto(`demo=pad&theme=${theme}`, SHOW_SCREEN); await run(`setTab('screen');${SHOW_SCREEN};setTab('pad')`); await run(FLOAT_ON); await run(`prefs.mini={w:240,x:innerWidth-254,y:330};miniPlace()`); await sleep(500); await shot(`floating-${theme}`);
  }
}

if (which === 'all' || which === 'video') {
  scale = 1; await metrics();
  const dir = join(OUT, 'frames'); const durs = []; let fi = 0, last = Date.now();
  const frame = async () => { const r = await cdp('Page.captureScreenshot', { format: 'jpeg', quality: 88 }); const now = Date.now(); const name = `f${String(fi++).padStart(4, '0')}.jpg`; writeFileSync(join(dir, name), Buffer.from(r.data, 'base64')); durs.push([name, Math.max(0.04, (now - last) / 1000)]); last = now; };
  const hold = async ms => { const end = Date.now() + ms; while (Date.now() < end) await frame(); };
  const tapEl = async sel => { await run(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); if (!e) throw new Error('no ${sel}'); const r = e.getBoundingClientRect(); const d = document.createElement('i'); d.className = 'zv-tap'; d.style.left = (r.x + r.width / 2) + 'px'; d.style.top = (r.y + r.height / 2) + 'px'; document.body.appendChild(d); setTimeout(() => d.remove(), 600); })()`); await hold(260); await run(`document.querySelector(${JSON.stringify(sel)}).click()`); };
  const tapAt = async (x, y) => { await run(`(() => { const d = document.createElement('i'); d.className = 'zv-tap'; d.style.left = '${x}px'; d.style.top = '${y}px'; document.body.appendChild(d); setTimeout(() => d.remove(), 600); })()`); await hold(240); };
  const pe = (el, type, x, y) => run(`document.querySelector(${JSON.stringify(el)}).dispatchEvent(new PointerEvent('${type}',{bubbles:true,cancelable:true,pointerId:7,clientX:${x},clientY:${y},isPrimary:true,button:0}))`);
  const type = async (sel, text) => { for (let i = 1; i <= text.length; i++) { await run(`(() => { const q = document.querySelector(${JSON.stringify(sel)}); q.value = ${JSON.stringify(text)}.slice(0, ${i}); q.dispatchEvent(new Event('input')); })()`); await hold(140); } };

  await goto('demo=desk&theme=light', SETUP_APPS + SHOW_SCREEN + `;1`);
  await run(`window.__mock=${MOCK};`);
  await hold(1300);
  // 1. launcher: open an app on a new empty workspace
  await tapEl('#ws-add'); await hold(900);
  await type('#ap-q', 'spo'); await hold(500);
  await tapEl('[data-app=spotify]'); await hold(1100);
  // 2. move a window to another workspace
  await tapEl('[data-move]'); await hold(900);
  await tapEl('[data-mvto="1"]'); await hold(1200);
  // 3. touchpad with a finger trail
  await tapEl('#nav [data-tab=pad]'); await hold(700);
  const pad = await run(`(() => { const r = document.getElementById('padwrap').getBoundingClientRect(); return [r.x, r.y, r.width, r.height]; })()`);
  const [px, py, pw, ph] = pad;
  for (let t = 0; t <= 14; t++) { const x = px + pw * (0.25 + 0.5 * t / 14), y = py + ph * (0.55 - 0.25 * Math.sin(Math.PI * t / 14)); await pe('#padwrap', t === 0 ? 'pointerdown' : 'pointermove', x, y); await hold(55); }
  await pe('#padwrap', 'pointerup', px + pw * 0.75, py + ph * 0.55); await hold(500);
  // 4. keyboard and media
  await tapEl('#nav [data-tab=keys]'); await hold(1200);
  await tapEl('#nav [data-tab=media]'); await hold(1400);
  // 5. screen preview, brightness, tap to click
  await tapEl('#nav [data-tab=screen]'); await run(SHOW_SCREEN + `;scrOn=true;renderScreen();$('sc-cv').hidden=false;$('sc-empty').hidden=true;$('sc-stat').hidden=false;$('sc-stat').textContent='8 fps'`); await hold(1500);
  const cv = await run(`(() => { const r = $('sc-cv').getBoundingClientRect(); return [r.x, r.y, r.width, r.height]; })()`);
  await tapAt(cv[0] + cv[2] * 0.55, cv[1] + cv[3] * 0.4); await hold(700);
  // 6. the floating screen window follows you to other tabs, and can be dragged
  await tapEl('#nav [data-tab=pad]'); await run(FLOAT_ON); await hold(1200);
  const g = await run(`(() => { const r = $('sc-grip').getBoundingClientRect(); return [r.x + 10, r.y + 14]; })()`);
  for (let t = 1; t <= 10; t++) { await pe('#sc-grip', t === 1 ? 'pointerdown' : 'pointermove', g[0] - t * 6, g[1] - t * 38); await hold(60); }
  await pe('#sc-mh', 'pointerup', g[0] - 60, g[1] - 380); await hold(1000);
  // 7. dark theme
  await run(`themeForce='dark';applyTheme()`); await hold(1300);
  await run(`scrOn=false;updateMini()`);
  await tapEl('#nav [data-tab=desk]'); await hold(1400);
  writeFileSync(join(OUT, 'frames.txt'), durs.map(([f, d]) => `file '${join('frames', f).replace(/^.*?frames/, 'frames')}'\nduration ${d.toFixed(3)}`).join('\n') + `\nfile 'frames/${durs.at(-1)[0]}'\n`);
  console.log('video frames', durs.length, 'seconds', durs.reduce((a, b) => a + b[1], 0).toFixed(1));
}

cleanup(); await sleep(300); process.exit(0);
