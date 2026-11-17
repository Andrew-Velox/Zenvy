// Records the demo on the maintainer's own, already prepared desktop (no staging): a terminal on workspace 1, a file manager
// showing a folder of wallpapers on 3 and VLC with a video on 4.
//
//   node tools/real-demo/record-live.mjs        # records into $OUT, then run compose.mjs
//
// It takes over the desktop for about two minutes. It never types anywhere but the terminal it identified up front, never
// opens a file, and puts back the workspace, the pointer, the volume and VLC's state afterwards. Needs wf-recorder, adb,
// playerctl, pactl, Hyprland and a debug build of the app.
import { spawn, spawnSync } from 'node:child_process';
import { mkdirSync, writeFileSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { adb, Phone } from './phone.mjs';

const OUT = resolve(process.env.OUT || 'real-demo-out');
const ZENVYD = process.env.ZENVYD || 'zenvyd';
const OUTPUT = process.env.MONITOR || 'HDMI-A-1';
const sleep = ms => new Promise(r => setTimeout(r, ms));
mkdirSync(OUT, { recursive: true });

const run = (cmd, args) => (spawnSync(cmd, args, { encoding: 'utf8' }).stdout || '').trim();
const hy = (...a) => run('hyprctl', a);
const clients = () => JSON.parse(hy('clients', '-j'));
const activeWs = () => JSON.parse(hy('activeworkspace', '-j')).id;
const activeWin = () => { try { return JSON.parse(hy('activewindow', '-j')) || {}; } catch { return {}; } };
const cursor = () => hy('cursorpos').split(',').map(Number);
const moveCursor = (x, y) => hy('dispatch', `hl.dsp.cursor.move({ x = ${Math.round(x)}, y = ${Math.round(y)} })`);
const zdev = (...a) => spawnSync(ZENVYD, ['dev', ...a], { encoding: 'utf8' });
const vlc = (...a) => run('playerctl', ['-p', 'vlc', ...a]);
const sysVol = () => +(run('pactl', ['get-sink-volume', '@DEFAULT_SINK@']).match(/(\d+)%/) || [])[1];
const until = async (fn, ms, what) => { const end = Date.now() + ms; while (Date.now() < end) { if (await fn()) return; await sleep(120); } throw new Error('timed out waiting for ' + what); };
const winOn = (ws, cls) => clients().find(c => c.workspace.id === ws && c.class === cls);

const WS_TERM = 1, WS_FILES = 3, WS_VIDEO = 4;
const term = winOn(WS_TERM, 'kitty'), files = winOn(WS_FILES, 'org.kde.dolphin'), video = winOn(WS_VIDEO, 'vlc');
if (!term || !files || !video) throw new Error('expected a terminal on 1, the file manager on 3 and VLC on 4');
if (/claude|adaptive hyprland/i.test(term.title)) throw new Error('the terminal on workspace 1 looks like the coding session: refusing');
if (clients().filter(c => c.workspace.id === WS_TERM && c.class === 'kitty').length !== 1) throw new Error('more than one terminal on workspace 1');

// ---- everything is put back whatever happens
const original = { ws: activeWs(), cursor: cursor(), volume: sysVol(), vlcStatus: vlc('status'), vlcPos: vlc('position') };
let pc, phone, imageWin = null;
async function restore() {
  try { if (pc && pc.exitCode === null) { pc.kill('SIGINT'); await Promise.race([new Promise(r => pc.once('exit', r)), sleep(6000)]); } } catch {}
  try { if (phone) await phone.ev(`(() => { if (scrOn) stopScreen(false); return 1; })()`); } catch {}
  for (const c of clients()) if (/swappy/i.test(c.class)) zdev('close', c.address);
  if (original.vlcStatus === 'Paused') vlc('pause');
  if (original.vlcPos) vlc('position', original.vlcPos);
  if (original.volume && sysVol() !== original.volume) run('pactl', ['set-sink-volume', '@DEFAULT_SINK@', original.volume + '%']);
  zdev('switch-workspace', String(original.ws));
  moveCursor(...original.cursor);
}
process.on('SIGINT', async () => { await restore(); process.exit(1); });

const timeline = [], blurs = [];
// on workspace 3: the address bar shows the user name, and a notes widget on the wallpaper shows through the translucent windows
const FILES_BLUR = [[200, 56, 430, 44], [1568, 562, 304, 270]];
let T0 = 0, pcSpawn = 0;
const at = () => (Date.now() - T0) / 1000;
const caption = (a, b = '') => timeline.push({ t: at(), a, b });
const mark = what => timeline.push({ t: at(), mark: what });
// the file manager's address bar shows the user name: blur it whenever workspace 3 is on screen
let in3 = null;
const goWs = async (phoneObj, n) => {
  await phoneObj.tap(`[data-ws="${n}"]`); await until(() => activeWs() === n, 5000, 'workspace ' + n);
  if (n === WS_FILES && in3 === null) in3 = at() - 0.6;
  if (n !== WS_FILES && in3 !== null) { for (const r of FILES_BLUR) blurs.push({ region: r, from: Math.max(0, in3), to: at() + 1.4 }); in3 = null; }
};

try {
  zdev('switch-workspace', String(WS_TERM)); await sleep(900);
  phone = new Phone(); await phone.connect();
  await phone.ev(`(() => { prefs.scope = 'here'; savePrefs(); renderDesk(); return 1; })()`);
  await phone.tap('#nav [data-tab=desk]'); await sleep(700);
  moveCursor(960, 700);

  pc = spawn('wf-recorder', ['-o', OUTPUT, '-f', join(OUT, 'pc.mp4'), '-c', 'libx264', '-p', 'preset=veryfast', '-p', 'crf=21', '-r', '30'], { stdio: 'ignore' });
  pcSpawn = Date.now();
  phone.startScreencast(join(OUT, 'phone'));
  await sleep(1500);
  T0 = Date.now();
  caption('Control your Hyprland PC from your phone', 'Real phone, real desktop. Nothing here is simulated.');
  await sleep(2800);

  // 1. workspaces
  caption('Switch workspaces', 'Tap a workspace on the phone and the PC follows.');
  await goWs(phone, WS_FILES); await sleep(1700);
  await goWs(phone, WS_VIDEO); await sleep(1700);
  await goWs(phone, WS_TERM); await sleep(1300);

  // 2. type on the phone, it types in the terminal
  caption('Type on the phone, it types on the PC', 'Any layout, any language, straight into the focused window.');
  await phone.tap('#nav [data-tab=keys]'); await sleep(900);
  if (activeWin().address !== term.address) { zdev('focus', term.address); await sleep(500); }
  if (activeWin().address !== term.address) throw new Error('the terminal is not focused: refusing to type');
  await phone.focusQuietly('#kb'); await sleep(600);
  for (const line of ['echo Hello from my phone', 'echo Typed on a phone, run on the PC', 'date']) {
    if (activeWin().address !== term.address) throw new Error('focus left the terminal: refusing to type');
    await phone.type(line, 150); await sleep(350); phone.enter(); await sleep(1100);
  }
  await phone.ev('(document.activeElement && document.activeElement.blur(), 1)'); await sleep(700);

  // 3. the pointer, in the file manager: select, double-click to open, close it from the phone, right-click for the menu
  caption('Touchpad: move, click, double-click', 'Select a wallpaper, double-click to open it, right-click for the menu.');
  await phone.tap('#nav [data-tab=pad]'); await sleep(700);
  await goWs(phone, WS_FILES); await sleep(1200);
  const pad = await phone.center('#padwrap');
  moveCursor(230, 430); await sleep(300);
  // learn how far the pointer travels for a swipe, then steer by looking where it is
  { const [x0] = cursor(); await phone.swipe(pad.x - pad.w * 0.15, pad.y, pad.x + pad.w * 0.15, pad.y, 600); await sleep(500); const [x1] = cursor(); var k = Math.max(0.3, (x1 - x0) / (pad.w * 0.3)); }
  const steerTo = async (tx, ty) => {
    for (let i = 0; i < 7; i++) {
      const [cx, cy] = cursor(); const dx = tx - cx, dy = ty - cy; if (Math.hypot(dx, dy) < 38) return;
      let sx = dx / k, sy = dy / k; const f = Math.min(1, (pad.w * 0.42) / Math.max(1, Math.abs(sx)), (pad.h * 0.38) / Math.max(1, Math.abs(sy)));
      sx *= f; sy *= f;
      await phone.swipe(pad.x - sx / 2, pad.y - sy / 2, pad.x + sx / 2, pad.y + sy / 2, 650); mark('swipe'); await sleep(750);
    }
  };
  const click = async () => { await phone.ripple(pad.x, pad.y); phone.tapXY(pad.x, pad.y); mark('click'); await sleep(900); };
  await steerTo(434, 266); await click(); await sleep(700);                         // select nature1
  await steerTo(993, 266); await sleep(500);                                          // purple-nature
  for (let tries = 0; tries < 3 && !clients().some(c => /swappy/i.test(c.class)); tries++) {
    if (tries) await steerTo(993, 266);                                               // a try that did not open: aim again
    await phone.doubleTap(pad.x, pad.y); mark('double-click');
    for (let i = 0; i < 25 && !clients().some(c => /swappy/i.test(c.class)); i++) await sleep(120);
  }
  await until(() => clients().some(c => /swappy/i.test(c.class)), 4000, 'the wallpaper to open');
  imageWin = clients().find(c => /swappy/i.test(c.class)); await sleep(3500);
  // close it from the phone's window list (tap the cross, tap again to confirm)
  await phone.tap('#nav [data-tab=desk]'); await sleep(900);
  await phone.tapTwice(`[data-close="${imageWin.address}"]`); mark('close window');
  await until(() => !clients().some(c => c.address === imageWin.address), 5000, 'the image window to close'); imageWin = null; await sleep(1200);
  await phone.tap('#nav [data-tab=pad]'); await sleep(900);
  await steerTo(1411, 266); await sleep(500);                                         // stay-vigil
  await phone.tap('#br'); mark('right-click'); await sleep(2800);                     // the context menu
  await steerTo(1500, 840); await click(); await sleep(800);                          // click empty space: the menu closes

  // 4. media: VLC on workspace 4
  caption('Media and volume', 'Play, skip around, set the volume.');
  await goWs(phone, WS_VIDEO); await sleep(1000);
  await phone.tap('#nav [data-tab=media]'); await sleep(1000);
  await phone.tap('.mplay'); mark('play'); await until(() => vlc('status') === 'Playing', 5000, 'VLC to play'); await sleep(3500);
  for (let i = 0; i < 2; i++) { await phone.tap('button[aria-label="Forward 10 seconds"]'); mark('seek'); await sleep(1500); }
  { const v = await phone.center('#vol'); const x = f => v.left + v.w * f;
    await phone.swipe(x(0.60), v.y, x(0.33), v.y, 800); mark('volume down'); await sleep(1500);
    await phone.swipe(x(0.33), v.y, x(0.60), v.y, 800); mark('volume up'); await sleep(1400); }

  // 5. mirror the PC screen on the phone, click through it
  caption('See the PC screen, and click it', 'A live picture of the monitor. Tap it to click.');
  await phone.tap('#nav [data-tab=screen]'); await sleep(900);
  await phone.tap('#sc-go'); await phone.waitFor('#sc-cv', 8000); await sleep(2600);
  { const cv = await phone.center('#sc-cv'); await phone.ripple(cv.x, cv.y); phone.tapXY(cv.x, cv.y); mark('pause through mirror'); await sleep(2000);
    await phone.ripple(cv.x, cv.y); phone.tapXY(cv.x, cv.y); mark('play through mirror'); await sleep(1800); }

  // 6. the mirror floats over every tab and follows the workspaces
  caption('A floating screen window', 'It stays on top of every tab and follows you between workspaces.');
  // the window remembers where it was dragged last; earlier runs may have left it up under the header, out of reach
  const resetMini = () => phone.ev('(() => { prefs.mini = { w: 200 }; savePrefs(); miniPlace(); })()');
  await resetMini();
  await phone.tap('#nav [data-tab=pad]'); await phone.waitFor('#sc-mini'); await sleep(1800);
  { // drag it to a spot well inside the touchpad area (never up under the header or the chips)
    const m = await phone.center('#sc-mini'), g = await phone.center('#sc-grip'), pd = await phone.center('#padwrap');
    const wantLeft = pd.left + pd.w * 0.08, wantTop = pd.top + pd.h * 0.3;
    await phone.swipe(g.x, g.y, g.x + (wantLeft - m.left), g.y + (wantTop - m.top), 900); mark('drag mini'); await sleep(2200); }
  await goWs(phone, WS_FILES); await sleep(2000);
  await goWs(phone, WS_TERM); await sleep(1800);
  await phone.tap('#sc-mx'); await sleep(900);
  await resetMini();

  caption('Zenvy', 'Open source. No cloud, no account, no telemetry.');
  await sleep(2600);
  mark('end');
} finally {
  const dur = at();
  if (in3 !== null) for (const r of FILES_BLUR) blurs.push({ region: r, from: Math.max(0, in3), to: dur });
  if (phone) phone.stopScreencast();
  writeFileSync(join(OUT, 'timeline.json'), JSON.stringify({ duration: dur, t0: T0, pcSpawn, events: timeline, blurs, frames: phone ? phone.frames : [] }, null, 1));
  await restore();
  await sleep(1500);
  console.log('recorded', dur.toFixed(1), 's into', OUT, '| volume now', sysVol(), 'vlc', vlc('status'), '| workspace', activeWs());
}
process.exit(0);
