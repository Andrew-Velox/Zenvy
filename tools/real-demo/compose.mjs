// Puts the recording from record.mjs together: the real desktop on the left, the real app on the right, captions below.
//
//   node tools/real-demo/compose.mjs            # reads $OUT (default ./real-demo-out), writes docs/media/demo.webp
//
// Tunables (environment): PC_LATENCY (seconds the screen recorder needs to start, default 0.6), PHONE_LATENCY, WIDTH, FPS, QUALITY,
// CROP_TOP (pixels of the desktop's top bar to cut off, it can show names and tray icons; default 56), BLUR (x:y:w:h of a part of
// the desktop to blur in every frame; the default covers a notes widget in this author's wallpaper, set it for your own desktop).
import { execFileSync, spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync, mkdirSync, existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const OUT = resolve(process.env.OUT || 'real-demo-out');
const FINAL = resolve(process.env.FINAL || join(root, 'docs/media/demo.webp'));
const tl = JSON.parse(readFileSync(join(OUT, 'timeline.json'), 'utf8'));
const FONT_B = process.env.FONT_BOLD || '/usr/share/fonts/noto/NotoSans-Bold.ttf';
const FONT_R = process.env.FONT_REGULAR || '/usr/share/fonts/noto/NotoSans-Regular.ttf';
const CROP_TOP = +(process.env.CROP_TOP || 56);
const LAT = +(process.env.PC_LATENCY || 0.6);
// regions of the desktop picture (x:y:w:h in the cropped frame) blurred in every frame (BLUR, ";"-separated), plus the timed
// ones record-live.mjs asks for (the file manager's address bar shows the user name)
const regions = [...(process.env.BLUR || '').split(';').filter(Boolean).map(s => ({ region: s.split(':').map(Number) })), ...(tl.blurs || [])];
let blurChain = '', cur = '[c0]';
regions.forEach((r, i) => {
  const [x, y, w, h] = r.region, en = r.from !== undefined ? `:enable='between(t,${r.from.toFixed(2)},${r.to.toFixed(2)})'` : '';
  blurChain += `${cur}split[a${i}][b${i}];[b${i}]crop=${w}:${h}:${x}:${y},boxblur=${Math.max(2, Math.floor(Math.min(w, h) / 4) - 1)}:6[bl${i}];[a${i}][bl${i}]overlay=${x}:${y}${en}[c${i + 1}];`;
  cur = `[c${i + 1}]`;
});
blurChain = cur === '[c0]' ? '[c0]null[bo];' : blurChain.replace(/\[c\d+\];$/, '[bo];');
const dur = tl.duration;

// ---- the phone's frames as a timed list
// the phone's frames reach the computer over USB a moment after they were drawn: shift them back by PHONE_LATENCY seconds
const PHONE_LAT = +(process.env.PHONE_LATENCY || 0.55);
const rel = tl.frames.map(f => ({ t: (f.t - tl.t0) / 1000 - PHONE_LAT, name: f.name }));
let start = rel.findLastIndex(f => f.t <= 0); if (start < 0) start = 0;
const list = rel.slice(start); list[0] = { ...list[0], t: 0 };
let concat = '';
list.forEach((f, i) => { const next = i + 1 < list.length ? list[i + 1].t : dur; concat += `file '${join(OUT, 'phone', f.name)}'\nduration ${Math.max(0.03, next - f.t).toFixed(3)}\n`; });
concat += `file '${join(OUT, 'phone', list.at(-1).name)}'\n`;
writeFileSync(join(OUT, 'phone.txt'), concat);

// ---- captions as files (no escaping trouble in the filter)
const caps = tl.events.filter(e => e.a).map((e, i, a) => ({ ...e, end: i + 1 < a.length ? a[i + 1].t : dur }));
caps.forEach((c, i) => { writeFileSync(join(OUT, `cap${i}a.txt`), c.a); writeFileSync(join(OUT, `cap${i}b.txt`), c.b || ''); });

// ---- the bezels, drawn once
const PW = 880, PH = Math.round(PW * (1080 - CROP_TOP) / 1920), PX = 24, PY = 66, PHX = 948, PHY = 34, PHH = 650, PHW = Math.round(PHH * 540 / 1170);
const frame = join(OUT, 'frame.png');
const m = (...a) => execFileSync('magick', a);
m('-size', '1280x720', 'xc:none', '-fill', '#1D1D1F',
  '-draw', `roundrectangle ${PX - 9},${PY - 9} ${PX + PW + 8},${PY + PH + 8} 18,18`,
  '-draw', `roundrectangle ${PHX - 11},${PHY - 11} ${PHX + PHW + 10},${PHY + PHH + 10} 40,40`,
  '-fill', '#c9c9ce', '-draw', `rectangle ${PX + PW / 2 - 30},${PY + PH + 9} ${PX + PW / 2 + 30},${PY + PH + 30}`,
  '-fill', '#b9b9be', '-draw', `roundrectangle ${PX + PW / 2 - 80},${PY + PH + 29} ${PX + PW / 2 + 80},${PY + PH + 38} 4,4`,
  '(', '-size', '1280x720', 'xc:none', '-fill', 'white',
  '-draw', `roundrectangle ${PX},${PY} ${PX + PW - 1},${PY + PH - 1} 10,10`,
  '-draw', `roundrectangle ${PHX},${PHY} ${PHX + PHW - 1},${PHY + PHH - 1} 30,30`, ')',
  '-compose', 'DstOut', '-composite', frame);

// ---- zoom insets (a magnified patch of the desktop over the picture, for text too small to read) and speed-ups,
// from extras.json next to the recording: { zooms: [{ region: [x, y, w, h], size: [w, h], at: [x, y], from, to }], speeds: [{ from, to, factor }] }
const extras = existsSync(join(OUT, 'extras.json')) ? JSON.parse(readFileSync(join(OUT, 'extras.json'), 'utf8')) : {};
const zooms = extras.zooms || [];
let pcChain = '';
if (zooms.length) {
  pcChain += `[bo]split=${zooms.length + 1}[pbase]${zooms.map((_, i) => `[zs${i}]`).join('')};`;
  zooms.forEach((z, i) => { pcChain += `[zs${i}]crop=${z.region[2]}:${z.region[3]}:${z.region[0]}:${z.region[1]},scale=${z.size[0]}:${z.size[1]}:flags=lanczos,drawbox=x=0:y=0:w=iw:h=ih:color=0x007AFF:t=4[z${i}];`; });
  pcChain += `[pbase]scale=${PW}:${PH}:flags=lanczos[pb0];`;
  zooms.forEach((z, i) => { pcChain += `[pb${i}][z${i}]overlay=${z.at[0]}:${z.at[1]}:enable='between(t,${z.from},${z.to})'[pb${i + 1}];`; });
  pcChain += `[pb${zooms.length}]null[pc];`;
} else pcChain = `[bo]scale=${PW}:${PH}:flags=lanczos[pc];`;
const speeds = (extras.speeds || []).slice().sort((a, b) => a.from - b.from);
let speedChain = '[v0]null[v1];';
if (speeds.length) {
  const pts = [0]; const fac = [];
  for (const s of speeds) { if (s.from > pts.at(-1)) { fac.push(1); pts.push(s.from); } fac.push(s.factor); pts.push(s.to); }
  if (pts.at(-1) < dur) { fac.push(1); pts.push(dur); }
  speedChain = `[v0]split=${fac.length}${fac.map((_, i) => `[sp${i}]`).join('')};`;
  fac.forEach((x, i) => { speedChain += `[sp${i}]trim=start=${pts[i].toFixed(2)}:end=${pts[i + 1].toFixed(2)},setpts=(PTS-STARTPTS)/${x}[st${i}];`; });
  speedChain += `${fac.map((_, i) => `[st${i}]`).join('')}concat=n=${fac.length}:v=1:a=0[v1];`;
}

// ---- one ffmpeg run
const seek = ((tl.t0 - tl.pcSpawn) / 1000 - LAT).toFixed(3);
let f = `color=c=0xF5F5F7:s=1280x720:r=15:d=${dur.toFixed(2)}[bg];` +
  // BLUR is x:y:w:h in the cropped desktop picture: a corner of the wallpaper that holds a notes widget, blurred in every frame
  `[0:v]fps=15,crop=1920:${1080 - CROP_TOP}:0:${CROP_TOP}[c0];${blurChain}${pcChain}` +
  `[1:v]fps=15,scale=${PHW}:${PHH}:flags=lanczos[ph];` +
  `[bg][pc]overlay=${PX}:${PY}[a];[a][ph]overlay=${PHX}:${PHY}[b];[b][2:v]overlay=0:0[c];[3:v]scale=34:34[lg];[c][lg]overlay=30:20[d];` +
  `[d]drawtext=fontfile=${FONT_B}:text=Zenvy:fontcolor=0x1D1D1F:fontsize=24:x=74:y=22,` +
  `drawtext=fontfile=${FONT_R}:text='your phone controls your Hyprland PC':fontcolor=0x6e6e73:fontsize=17:x=156:y=27`;
caps.forEach((c, i) => {
  const en = `enable='between(t,${c.t.toFixed(2)},${c.end.toFixed(2)})'`;
  f += `,drawtext=fontfile=${FONT_B}:textfile=${join(OUT, `cap${i}a.txt`)}:fontcolor=0x1D1D1F:fontsize=30:x=${PX}:y=610:${en}`;
  f += `,drawtext=fontfile=${FONT_R}:textfile=${join(OUT, `cap${i}b.txt`)}:fontcolor=0x6e6e73:fontsize=19:x=${PX}:y=654:${en}`;
});
f += `,drawtext=fontfile=${FONT_R}:text='Recorded on a real phone and a real Hyprland desktop, at the same time':fontcolor=0xa1a1a6:fontsize=13:x=${PX}:y=690[v0];${speedChain}[v1]fps=${process.env.FPS || 12},scale=${process.env.WIDTH || 1000}:-2:flags=lanczos[v]`;
mkdirSync(dirname(FINAL), { recursive: true });
const r = spawnSync('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-y',
  '-ss', seek, '-t', dur.toFixed(2), '-i', join(OUT, 'pc.mp4'),
  '-f', 'concat', '-safe', '0', '-i', join(OUT, 'phone.txt'),
  '-loop', '1', '-i', frame, '-loop', '1', '-i', join(root, 'assets/logo/zenvy-icon-512.png'),
  '-filter_complex', f, '-map', '[v]',
  ...(FINAL.endsWith('.mp4') ? ['-c:v', 'libx264', '-crf', '20', '-pix_fmt', 'yuv420p'] : ['-c:v', 'libwebp_anim', '-quality', process.env.QUALITY || '70', '-compression_level', '6', '-loop', '0']), '-an', FINAL], { encoding: 'utf8' });
if (r.status !== 0) { console.error(r.stderr); process.exit(1); }
console.log('wrote', FINAL);
