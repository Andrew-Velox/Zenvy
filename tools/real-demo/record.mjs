// Records the README demo for real: the phone app next to the actual Hyprland desktop it controls.
//
//   node tools/real-demo/record.mjs            # records into $OUT (default ./real-demo-out), then run compose.mjs
//
// It takes over the desktop for about two minutes, so leave the mouse and keyboard alone. It only ever uses empty
// workspaces (5 to 7 by default, set DEMO_WS), types only into a throwaway terminal it started itself, and puts everything
// back afterwards (workspace, cursor, the app's settings). Needs wf-recorder, adb, Hyprland and a debug build of the app.
import { spawn, spawnSync } from 'node:child_process';
import { mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { adb, Phone } from './phone.mjs';

const OUT = resolve(process.env.OUT || 'real-demo-out');
const ZENVYD = process.env.ZENVYD || 'zenvyd';
const OUTPUT = process.env.MONITOR || 'HDMI-A-1';
const WS_A = +(process.env.DEMO_WS || 5);                 // the first demo workspace (a terminal)
const WS_B = WS_A + 1;                                     // a second one (another terminal)
const WS_NEW = WS_A + 2;                                   // the first empty workspace: where "open an app" goes
const sleep = ms => new Promise(r => setTimeout(r, ms));
mkdirSync(OUT, { recursive: true });

const hy = (...a) => spawnSync('hyprctl', a, { encoding: 'utf8' }).stdout || '';
const clients = () => JSON.parse(hy('clients', '-j'));
const activeWs = () => JSON.parse(hy('activeworkspace', '-j')).id;
const activeWin = () => { try { return JSON.parse(hy('activewindow', '-j')) || {}; } catch { return {}; } };
const cursor = () => hy('cursorpos').trim().split(',').map(Number);
const moveCursor = (x, y) => hy('dispatch', `hl.dsp.cursor.move({ x = ${Math.round(x)}, y = ${Math.round(y)} })`);
const zdev = (...a) => spawnSync(ZENVYD, ['dev', ...a], { encoding: 'utf8' });
const until = async (fn, ms, what) => { const end = Date.now() + ms; while (Date.now() < end) { if (fn()) return; await sleep(120); } throw new Error('timed out waiting for ' + what); };
const find = pred => clients().find(pred);

// ---- everything is put back whatever happens
const original = { ws: activeWs(), cursor: cursor() };
const mine = new Set();   // addresses of windows this script started
let pc, phone, pcSpawn = 0;
async function restore() {
  try { if (pc && pc.exitCode === null) { pc.kill('SIGINT'); await Promise.race([new Promise(r => pc.once('exit', r)), sleep(6000)]); } } catch {}
  try { if (phone) await phone.ev(`(() => { prefs.follow = ${phone.origFollow}; savePrefs(); if (scrOn) stopScreen(false); return 1; })()`); } catch {}
  for (const c of clients()) if (c.class === 'vlc' && c.workspace.id >= WS_A || mine.has(c.address)) zdev('close', c.address);
  await sleep(1200);
  zdev('switch-workspace', String(original.ws));
  moveCursor(...original.cursor);
}
process.on('SIGINT', async () => { await restore(); process.exit(1); });

const timeline = [];
let T0 = 0;
const at = () => (Date.now() - T0) / 1000;
const caption = (a, b = '') => timeline.push({ t: at(), a, b });
const mark = what => timeline.push({ t: at(), mark: what });

try {
  // ---------------------------------------------------------------- stage: two terminals of mine on empty workspaces
  if (clients().some(c => c.workspace.id >= WS_A)) throw new Error(`workspaces ${WS_A} and up are not empty: set DEMO_WS`);
  mkdirSync('/tmp/zv-demo', { recursive: true });
  writeFileSync('/tmp/zv-demo/rc', "PS1='\\[\\e[1;34m\\]zenvy\\[\\e[0m\\] \\$ '\n");
  const term = (title, ws, script) => {
    zdev('switch-workspace', String(ws));
    spawn('kitty', ['--title', title, '-o', 'font_size=21', '--directory', '/tmp/zv-demo', 'bash', '--norc', '--noprofile', '-c', script], { detached: true, stdio: 'ignore' }).unref();
  };
  term('zenvy-demo', WS_A, 'echo "  Zenvy"; echo "  controlled from my phone"; echo; exec bash --rcfile /tmp/zv-demo/rc -i');
  await until(() => find(c => c.title === 'zenvy-demo'), 8000, 'the first terminal'); mine.add(find(c => c.title === 'zenvy-demo').address);
  term('zenvy-demo-2', WS_B, 'echo; echo "  workspace ' + WS_B + '"; echo; ls /usr/share | head -8; exec bash --rcfile /tmp/zv-demo/rc -i');
  await until(() => find(c => c.title === 'zenvy-demo-2'), 8000, 'the second terminal'); mine.add(find(c => c.title === 'zenvy-demo-2').address);
  zdev('switch-workspace', String(WS_A));
  const termA = find(c => c.title === 'zenvy-demo').address;
  await sleep(1200);

  // ---------------------------------------------------------------- the phone
  phone = new Phone(); await phone.connect();
  phone.origFollow = await phone.ev('prefs.follow');
  await phone.ev(`(() => { prefs.scope = 'here'; savePrefs(); renderDesk(); return 1; })()`);
  await phone.tap('#nav [data-tab=desk]'); await sleep(800);
  moveCursor(960, 560);

  // ---------------------------------------------------------------- record both screens
  const pcFile = join(OUT, 'pc.mp4');
  pc = spawn('wf-recorder', ['-o', OUTPUT, '-f', pcFile, '-c', 'libx264', '-p', 'preset=veryfast', '-p', 'crf=21', '-r', '30'], { stdio: 'ignore' });
  pcSpawn = Date.now();
  phone.startScreencast(join(OUT, 'phone'));
  await sleep(1500);
  T0 = Date.now();
  caption('Control your Hyprland PC from your phone', 'Real phone, real desktop. Nothing here is simulated.');
  await sleep(2600);

  // 1. workspaces
  caption('Switch workspaces', 'Tap a workspace on the phone and the PC follows.');
  await phone.tap(`[data-ws="${WS_B}"]`); mark('switch to B'); await until(() => activeWs() === WS_B, 4000, 'workspace B'); await sleep(1700);
  await phone.tap(`[data-ws="${WS_A}"]`); mark('switch to A'); await until(() => activeWs() === WS_A, 4000, 'workspace A'); await sleep(1300);

  // 2. open an app on a new, empty workspace
  caption('Open an app on a new workspace', 'Search your installed apps, tap one: it opens on the first empty workspace.');
  await phone.tap('#ws-add'); await phone.waitFor('#ap-q'); await sleep(500);
  await phone.focusQuietly('#ap-q'); await sleep(500); await phone.type('vlc', 170); await sleep(900);
  await phone.tap('[data-app="vlc"]'); mark('launch vlc');
  await until(() => activeWs() === WS_NEW && find(c => c.class === 'vlc'), 20000, 'VLC on the new workspace'); await sleep(2600);

  // 3. move a window to another workspace (and follow it)
  caption('Move a window to another workspace', 'Choose where it goes, and whether to follow it.');
  await phone.ev(`(() => { prefs.follow = false; return 1; })()`);
  const ready = async () => phone.ev(`!!(state && state.windows.some(w => w.class === 'vlc') && state.monitors[0].active_workspace === ${WS_NEW})`);
  const end = Date.now() + 10000; while (Date.now() < end && !(await ready())) await sleep(200);
  await phone.tap('[data-move]'); await phone.waitFor('#sheet .set-row'); await sleep(900);
  await phone.tap('#sheet .set-row'); await sleep(600);
  if (!(await phone.ev('prefs.follow'))) { await phone.tap('#sheet .set-row'); await sleep(400); }
  await phone.tap(`[data-mvto="${WS_A}"]`); mark('move vlc');
  await until(() => activeWs() === WS_A && find(c => c.class === 'vlc')?.workspace.id === WS_A, 6000, 'VLC moved to workspace A'); await sleep(2800);

  // 4. touchpad: move the pointer, click to focus
  caption('Touchpad', 'Move, click and scroll, like a laptop trackpad.');
  await phone.tap('#nav [data-tab=pad]'); await sleep(900);
  const vlc = find(c => c.class === 'vlc');
  moveCursor(vlc.at[0] + vlc.size[0] * 0.6, vlc.at[1] + vlc.size[1] * 0.45);
  const pad = await phone.center('#padwrap');
  for (let i = 0; i < 4 && cursor()[0] > 820; i++) {   // swipe until the pointer is over the terminal (the left half)
    await phone.swipe(pad.x + pad.w * 0.3, pad.y + pad.h * 0.25, pad.x - pad.w * 0.3, pad.y + pad.h * 0.1, 800); mark('swipe'); await sleep(1000);
  }
  { const [cx, cy] = cursor(); if (cx > 900 || cy < 90 || cy > 1030) { moveCursor(520, 520); await sleep(200); } }
  await phone.ripple(pad.x, pad.y); phone.tapXY(pad.x, pad.y); mark('click'); await sleep(900);
  if (!activeWin().title?.startsWith('zenvy-demo')) { zdev('focus', termA); await sleep(500); }

  // 5. keyboard: type on the phone, it types on the PC
  caption('Keyboard', 'Type on the phone and it types on the PC.');
  await phone.tap('#nav [data-tab=keys]'); await sleep(900);
  if (!activeWin().title?.startsWith('zenvy-demo')) throw new Error('the throwaway terminal is not focused: refusing to type');
  await phone.focusQuietly('#kb'); await sleep(600);
  for (const line of ['echo Hello from my phone', 'ls']) {
    if (!activeWin().title?.startsWith('zenvy-demo')) throw new Error('focus left the throwaway terminal: refusing to type');
    await phone.type(line, 150); await sleep(350); phone.enter(); await sleep(1100);
  }
  await phone.ev('(document.activeElement && document.activeElement.blur(), 1)'); await sleep(700);

  // 6. the PC screen on the phone, tap to click
  caption('See the PC screen, and click it', 'A live picture of the monitor. Tap it to click.');
  await phone.tap('#nav [data-tab=screen]'); await sleep(900);
  await phone.tap('#sc-go'); await phone.waitFor('#sc-cv', 8000); await sleep(2400);
  { const v = find(c => c.class === 'vlc'); const cv = await phone.center('#sc-cv');
    const fx = (v.at[0] + v.size[0] * 0.62) / 1920, fy = (v.at[1] + v.size[1] * 0.5) / 1080;
    const px = cv.left + fx * cv.w, py = cv.top + fy * cv.h; await phone.ripple(px, py); phone.tapXY(px, py); mark('click through preview'); await sleep(1600); }

  // 7. the floating window
  caption('A floating screen window', 'It stays on top of every tab. Drag it, keep controlling.');
  await phone.tap('#nav [data-tab=pad]'); await phone.waitFor('#sc-mini'); await sleep(1500);
  { const g = await phone.center('#sc-grip'); await phone.swipe(g.x - 40, g.y, g.x - 330, g.y - 600, 800); mark('drag mini'); await sleep(1800); }
  await phone.tap('#sc-mx'); await sleep(900);

  caption('Zenvy', 'Open source. No cloud, no account, no telemetry.');
  await sleep(2400);
  mark('end');
} finally {
  const dur = at();
  if (phone) phone.stopScreencast();
  writeFileSync(join(OUT, 'timeline.json'), JSON.stringify({ duration: dur, t0: T0, pcSpawn, events: timeline, frames: phone ? phone.frames : [] }, null, 1));
  await restore();
  await sleep(1500);
  console.log('recorded', dur.toFixed(1), 's into', OUT);
}
process.exit(0);
