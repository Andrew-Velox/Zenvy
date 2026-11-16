// Captures the README screenshots (tabs, launcher, window move, floating screen, lock, settings) from the phone UI's built-in demo mode
// (sample data, no connection to any PC). Needs Brave or Chrome and Node 22.
//
//   python3 -m http.server 8765 --directory <dir containing index.html = crates/zenvy-net/src/page.html>
//   BROWSER=brave OUT=docs/media/raw node tools/screenshots.mjs stills
// The side-by-side demo animation is made by tools/demo.mjs.
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

cleanup(); await sleep(300); process.exit(0);
