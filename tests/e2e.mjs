// End-to-end test: acts as the phone (WebCrypto Ed25519, like Chrome) and as the PC operator.
// Usage: ZENVY_PORT=7888 ZENVY_RUNTIME=/run/user/1000 node tests/e2e.mjs
import https from 'node:https';
import net from 'node:net';
import { spawn, execFileSync } from 'node:child_process';
import { webcrypto as crypto } from 'node:crypto';

const PORT = process.env.ZENVY_PORT || '7888';
const HOST = `127.0.0.1:${PORT}`;
const SOCK = `${process.env.ZENVY_RUNTIME || process.env.XDG_RUNTIME_DIR}/zenvy/admin.sock`;
process.env.NODE_TLS_REJECT_UNAUTHORIZED = '0'; // self-signed test cert on loopback
const enc = new TextEncoder();
const b64u = b => Buffer.from(b).toString('base64url');
let pass = 0, fail = 0;
const ok = (c, m) => { c ? pass++ : fail++; console.log(`${c ? 'PASS' : 'FAIL'}  ${m}`); };
const sleep = ms => new Promise(r => setTimeout(r, ms));

function req(path, { method = 'GET', headers = {}, body } = {}) {
  return new Promise((res, rej) => {
    const r = https.request({ host: '127.0.0.1', port: PORT, path, method, headers: { host: HOST, ...headers }, rejectUnauthorized: false }, x => {
      let d = ''; x.on('data', c => d += c); x.on('end', () => res({ status: x.statusCode, headers: x.headers, body: d }));
    });
    r.on('error', rej); if (body) r.write(body); r.end();
  });
}

function admin(msg, onLine, holder = {}) { // one admin connection; onLine may return a reply object
  return new Promise((res, rej) => {
    const s = net.connect(SOCK); holder.sock = s; let buf = ''; const lines = [];
    s.on('connect', () => s.write(JSON.stringify(msg) + '\n'));
    s.on('data', d => { buf += d; let i; while ((i = buf.indexOf('\n')) >= 0) { const l = JSON.parse(buf.slice(0, i)); buf = buf.slice(i + 1); lines.push(l); const r = onLine?.(l); if (r) s.write(JSON.stringify(r) + '\n'); if (l.type === 'done' || l.type === 'devices' || l.type === 'revoked' || l.type === 'scoped' || l.type === 'error') s.end(); } });
    s.on('close', () => res(lines)); s.on('error', rej);
  });
}

async function newPhone() {
  const kp = await crypto.subtle.generateKey({ name: 'Ed25519' }, false, ['sign', 'verify']);
  const raw = new Uint8Array(await crypto.subtle.exportKey('raw', kp.publicKey));
  const h = new Uint8Array(await crypto.subtle.digest('SHA-256', raw));
  const code = String((((h[0] * 16777216) + (h[1] << 16) + (h[2] << 8) + h[3]) % 10000)).padStart(4, '0');
  return { kp, raw, code };
}

async function wsConnect(phone, deviceId, { badSig = false } = {}) {
  const ws = new WebSocket(`wss://${HOST}/ws`); const msgs = []; let closed = null;
  const waiters = [];
  const bins = []; ws.binaryType = 'arraybuffer';
  ws.onmessage = async e => {
    if (typeof e.data !== 'string') { bins.push({ t: Date.now(), d: Buffer.from(e.data) }); return; }
    const m = JSON.parse(e.data); msgs.push(m); waiters.forEach(w => w());
    if (m.t === 'challenge') {
      const nonce = Buffer.from(m.data.nonce, 'base64url'), ctx = enc.encode('zenvy-auth-v1\0'), id = enc.encode(deviceId);
      const msg = new Uint8Array(ctx.length + id.length + 1 + nonce.length); msg.set(ctx); msg.set(id, ctx.length); msg.set(nonce, ctx.length + id.length + 1);
      let sig = new Uint8Array(await crypto.subtle.sign('Ed25519', phone.kp.privateKey, msg));
      if (badSig) sig[0] ^= 1;
      ws.send(JSON.stringify({ v: 1, t: 'auth', data: { device_id: deviceId, sig: b64u(sig) } }));
    }
  };
  ws.onclose = e => { closed = e.code; waiters.forEach(w => w()); };
  const until = async (pred, ms = 5000) => { const end = Date.now() + ms; while (Date.now() < end) { const f = msgs.find(pred); if (f) return f; if (closed !== null) return null; await new Promise(r => { waiters.push(r); setTimeout(r, 100); }); } return null; };
  return { ws, msgs, bins, until, closedCode: () => closed, send: o => ws.send(JSON.stringify({ v: 1, ...o })) };
}

async function startPairing(phone, { approve = true, name = 'Test Phone', secretOverride } = {}) {
  let seen = {}; let secret; const holder = {};
  const adminDone = admin({ op: 'pair' }, l => {
    if (l.type === 'pairing') { secret = new URL(l.url).hash.match(/s=([^&]+)/)[1]; seen.url = l.url; }
    if (l.type === 'approval') { seen.approval = l; return { approve }; }
  }, holder);
  while (!secret) await sleep(20);
  const r = await req('/api/pair', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ secret: secretOverride ?? secret, name, pubkey: b64u(phone.raw) }) });
  if (!seen.approval) holder.sock.end(); // operator walks away: server must cancel the session
  const lines = await adminDone;
  return { r, seen, secret, lines, json: (() => { try { return JSON.parse(r.body) } catch { return {} } })() };
}

// ---------------------------------------------------------------- tests
console.log('== HTTP surface');
let r = await req('/api/hello'); ok(r.status === 200 && JSON.parse(r.body).name === 'zenvy', 'hello answers');
ok(r.headers['x-content-type-options'] === 'nosniff' && r.headers['content-security-policy']?.includes("frame-ancestors 'none'"), 'security headers present');
const h2 = execFileSync('curl', ['-sk', '--http2', '-o', '/dev/null', '-w', '%{http_code} %{http_version}', `https://${HOST}/api/hello`]).toString();
ok(h2 === '200 2', `HTTP/2 clients (Chrome) are accepted: Host comes from :authority (got '${h2}')`);
r = await req('/audio-worklet.js'); ok(r.status === 200 && /javascript/.test(r.headers['content-type'] || '') && r.body.includes("registerProcessor('zenvy-pcm'"), 'the phone speaker player script is served as JavaScript');
r = await req('/audio-worklet.js', { headers: { host: 'evil.example:7888' } }); ok(r.status === 421, 'and only to the right Host');
r = await req('/api/hello', { headers: { host: 'evil.example:7888' } }); ok(r.status === 421, `foreign Host header rejected (got ${r.status})`);
r = await req('/'); ok(r.status === 200 && r.body.includes('Zenvy') && !/PRIVATE KEY|BEGIN CERT/.test(r.body), 'page served, embeds no key material');
r = await req('/api/pair', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ secret: 'x', name: 'x', pubkey: b64u(new Uint8Array(32)) }) });
ok(r.status === 403, 'pair without an active session is refused');
r = await req('/assets/geist.woff2'); ok(r.status === 404, 'no font asset is served any more (the UI uses the phone\'s own font)');
r = await req('/'); ok(r.headers['cache-control'] === 'no-store' && r.headers['content-security-policy'].includes("font-src 'self'") && !/https?:\/\/(?!127\.0\.0\.1)[a-z0-9.-]+\.[a-z]{2,}/i.test(r.body.replace(/https:\/\/\$\{/g, '').replace(/http:\/\/www\.w3\.org\/2000\/svg/g, '') /* the SVG XML namespace is an identifier, not a request */), 'page is no-store, CSP allows only own fonts, page references no external hosts');

console.log('== pairing');
const phone = await newPhone();
let p = await startPairing(phone, { secretOverride: 'definitely-not-the-secret' });
ok(p.r.status === 403, 'wrong secret refused');
const p2 = await startPairing(phone, { approve: false });
ok(p2.r.status === 403 && p2.json.error.includes('not approved'), 'operator denial refused');
const p3 = await startPairing(phone, { approve: true });
ok(p3.r.status === 200 && p3.json.device_id, 'pairing approved by operator succeeds');
ok(p3.seen.approval?.code === phone.code, `operator sees the same confirmation code as the phone (${phone.code})`);
ok(p3.json.scopes && !p3.json.scopes.includes('power') && p3.json.scopes.includes('windows'), 'default scopes exclude power/exec');
const dev = p3.json.device_id;
const reuse = await req('/api/pair', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ secret: p3.secret, name: 'x', pubkey: b64u(new Uint8Array(32).fill(1)) }) });
ok(reuse.status === 403, 'used pairing secret cannot be reused');

console.log('== websocket auth');
let c = await wsConnect(phone, dev);
const ready = await c.until(m => m.t === 'ready'); ok(!!ready, 'signed challenge accepted -> ready');
const snap = await c.until(m => m.t === 'snapshot');
ok(snap?.data.capabilities.capabilities['hyprland.dispatch']?.state === 'available' && Array.isArray(snap.data.desktop.workspaces) && snap.data.ui_manifest.length > 3, 'snapshot has capabilities, ui manifest and desktop state');
ok(!JSON.stringify(snap).includes('serial'), 'snapshot contains no hardware serial');

let bad = await wsConnect(phone, dev, { badSig: true }); await bad.until(m => false, 3000); ok(bad.closedCode() === 4401, `bad signature closes with 4401 (got ${bad.closedCode()})`);
const other = await newPhone(); let imp = await wsConnect(other, dev); await imp.until(m => false, 3000); ok(imp.closedCode() === 4401, 'another key claiming this device id is rejected');
let ghost = await wsConnect(phone, 'doesnotexist'); await ghost.until(m => false, 3000); ok(ghost.closedCode() === 4401, 'unknown device id rejected');

console.log('== origin check');
const up = await new Promise(res => { const q = https.request({ host: '127.0.0.1', port: PORT, path: '/ws', headers: { host: HOST, Connection: 'Upgrade', Upgrade: 'websocket', 'Sec-WebSocket-Version': '13', 'Sec-WebSocket-Key': 'dGhlIHNhbXBsZSBub25jZQ==', Origin: 'https://evil.example' }, rejectUnauthorized: false }); q.on('response', x => res(x.statusCode)); q.on('upgrade', () => res(101)); q.on('error', () => res(0)); q.end(); });
ok(up === 403, `cross-origin websocket upgrade refused (got ${up})`);

console.log('== commands');
const wsState = () => c.msgs.filter(m => m.t === 'snapshot').at(-1).data.desktop;
c.send({ t: 'cmd', id: 'bad1', data: { cmd: 'exec', argv: ['touch', '/tmp/pwned'] } });
let e = await c.until(m => m.id === 'bad1'); ok(e?.t === 'err' && e.data.code === 'invalid_command', 'unknown command ("exec") rejected');
c.send({ t: 'cmd', id: 'bad2', data: { cmd: 'close_window', address: '0x1 })\nhl.dsp.exec_cmd("touch /tmp/pwned")--' } });
e = await c.until(m => m.id === 'bad2'); ok(e?.t === 'err' && e.data.code === 'invalid_command', 'Lua injection via address rejected');
c.send({ t: 'cmd', id: 'bad3', data: { cmd: 'switch_workspace', id: 0 } });
e = await c.until(m => m.id === 'bad3'); ok(e?.t === 'err', 'out-of-range workspace rejected');
const cur = wsState().monitors[0].active_workspace;
c.send({ t: 'cmd', id: 'ws', data: { cmd: 'switch_workspace', id: cur } });
e = await c.until(m => m.id === 'ws'); ok(e?.t === 'ack', 'switch to the already-active workspace acknowledged');

console.log('== live events (scratch window)');
const kitty = spawn('kitty', ['--class', 'zenvy-e2e', 'sleep', '60'], { detached: true, stdio: 'ignore' }); kitty.unref();
const opened = await c.until(m => m.t === 'evt' && m.data.topic === 'desktop.state' && m.data.value.windows.some(w => w.class === 'zenvy-e2e'), 8000);
ok(!!opened, 'phone receives desktop.state event when a window opens (no polling)');
const win = opened?.data.value.windows.find(w => w.class === 'zenvy-e2e');
if (win) {
  c.send({ t: 'cmd', id: 'fl', data: { cmd: 'toggle_floating', address: win.address } });
  const fl = await c.until(m => m.t === 'evt' && m.data.topic === 'desktop.state' && m.data.value.windows.find(w => w.address === win.address)?.floating === true, 5000);
  ok(!!fl, 'floating toggle propagates as an event');
  const seqs = c.msgs.filter(m => m.t === 'evt').map(m => m.seq); ok(seqs.every((s, i) => i === 0 || s > seqs[i - 1]), 'event seq numbers strictly increase');
  c.send({ t: 'cmd', id: 'cl', data: { cmd: 'close_window', address: win.address } });
  const closedEv = await c.until(m => m.t === 'evt' && m.data.topic === 'desktop.state' && !m.data.value.windows.some(w => w.address === win.address), 5000);
  ok(!!closedEv, 'close command executed and observed');
}

console.log('== input injection (scratch window, cursor confined to it)');
const fsx = await import('node:fs');
const osx = await import('node:os');
const pathx = await import('node:path');
const sh = (cmd, args) => execFileSync(cmd, args).toString();
const lua = code => { try { sh('hyprctl', ['dispatch', code]); } catch {} };
const OUT = pathx.join(osx.tmpdir(), `zenvy-e2e-typed-${process.pid}.txt`);
try { fsx.unlinkSync(OUT); } catch {}

// Find the kernel event nodes of the virtual devices the agent created.
function devNode(name) {
  const t = fsx.readFileSync('/proc/bus/input/devices', 'utf8');
  for (const blk of t.split('\n\n')) if (blk.includes(`Name="${name}"`)) { const m = blk.match(/event(\d+)/); if (m) return `/dev/input/event${m[1]}`; }
  return null;
}
const ptrNode = devNode('zenvy-pointer'), kbdNode = devNode('zenvy-keyboard');
ok(!!ptrNode && !!kbdNode, `agent created virtual pointer and keyboard devices (${ptrNode}, ${kbdNode})`);

const evs = []; // {dev, type, code, value}
const tail = (node, dev) => { if (!node) return; const ch = spawn('cat', [node], { stdio: ['ignore', 'pipe', 'ignore'] }); let buf = Buffer.alloc(0);
  ch.stdout.on('data', d => { buf = Buffer.concat([buf, d]); while (buf.length >= 24) { evs.push({ dev, type: buf.readUInt16LE(16), code: buf.readUInt16LE(18), value: buf.readInt32LE(20) }); buf = buf.subarray(24); } }); return { destroy: () => ch.kill() }; };
const streams = [tail(ptrNode, 'p'), tail(kbdNode, 'k')];
const waitEv = async (pred, ms = 3000) => { const end = Date.now() + ms; while (Date.now() < end) { const f = evs.find(pred); if (f) return f; await sleep(25); } return null; };

const origCursor = sh('hyprctl', ['cursorpos']).trim().split(/,\s*/).map(Number);
const kit = spawn('kitty', ['--class', 'zenvy-e2e-in', 'sh', '-c', `stty -echo; cat > ${OUT}`], { detached: true, stdio: 'ignore' }); kit.unref();
const op = await c.until(m => m.t === 'evt' && m.data.topic === 'desktop.state' && m.data.value.windows.some(w => w.class === 'zenvy-e2e-in'), 8000);
const w2 = op?.data.value.windows.find(w => w.class === 'zenvy-e2e-in');
ok(!!w2, 'scratch window for input tests appeared');
if (w2) {
  c.send({ t: 'cmd', id: 'in-fl', data: { cmd: 'toggle_floating', address: w2.address } }); await c.until(m => m.id === 'in-fl');
  lua(`hl.dsp.window.resize({ x = 600, y = 360, "exact", window = "address:${w2.address}" })`); await sleep(300);
  const geo = JSON.parse(sh('hyprctl', ['clients', '-j'])).find(w => w.address === w2.address);
  const cx = geo.at[0] + Math.floor(geo.size[0] / 2), cy = geo.at[1] + Math.floor(geo.size[1] / 2);
  lua(`hl.dsp.cursor.move({ x = ${cx}, y = ${cy} })`); await sleep(200);
  const cur = sh('hyprctl', ['cursorpos']).trim().split(/,\s*/).map(Number);
  const inside = cur[0] >= geo.at[0] && cur[0] <= geo.at[0] + geo.size[0] && cur[1] >= geo.at[1] && cur[1] <= geo.at[1] + geo.size[1];
  ok(inside, `cursor confined inside the scratch window before any click (${cur} in ${geo.at} ${geo.size})`);
  c.send({ t: 'cmd', id: 'in-fo', data: { cmd: 'focus_window', address: w2.address } }); await c.until(m => m.id === 'in-fo');

  await sleep(200);
  const focused = JSON.parse(sh('hyprctl', ['activewindow', '-j'])).address === w2.address;
  ok(focused, 'scratch window has focus before any key or text is sent');
  scratchBlock: if (inside && focused) {
    const scratchFocused = () => { try { return JSON.parse(sh('hyprctl', ['activewindow', '-j'])).address === w2.address; } catch { return false; } };
    // The desktop is shared with a person whose mouse moves: put the cursor and focus back before each typing step
    // (directly through hyprctl, because a locked agent refuses window commands), and give up rather than type elsewhere.
    const ensureFocus = async () => {
      for (let i = 0; i < 5; i++) {
        if (scratchFocused()) return true;
        lua(`hl.dsp.cursor.move({ x = ${cx}, y = ${cy} })`); lua(`hl.dsp.focus({ window = "address:${w2.address}" })`); await sleep(200);
      }
      return scratchFocused();
    };
    const refocus = async what => { if (await ensureFocus()) return true; ok(false, `scratch window lost focus before ${what}: the rest of the input tests were skipped rather than typing anywhere else`); return false; };
    const inp = (data, id) => c.send({ t: 'input', ...(id ? { id } : {}), data });
    // --- raw kernel events
    inp({ cmd: 'pointer_move', dx: 7, dy: -3 });
    ok(!!(await waitEv(e => e.dev === 'p' && e.type === 2 && e.code === 0 && e.value === 7)) && !!(await waitEv(e => e.dev === 'p' && e.type === 2 && e.code === 1 && e.value === -3)), 'pointer_move emits REL_X=7 and REL_Y=-3');
    inp({ cmd: 'pointer_move', dx: -7, dy: 3 });
    inp({ cmd: 'scroll', dx: 0, dy: 2 });
    ok(!!(await waitEv(e => e.dev === 'p' && e.type === 2 && e.code === 8 && e.value === -2)), 'scroll down emits REL_WHEEL=-2 (protocol dy>0 = content moves down)');
    const before = evs.length; inp({ cmd: 'button', button: 'left', state: 'click' }, 'clk');
    const ackc = await c.until(m => m.id === 'clk'); await sleep(150);
    const btn = evs.slice(before).filter(e => e.dev === 'p' && e.type === 1 && e.code === 0x110).map(e => e.value);
    ok(ackc?.t === 'ack' && btn.join() === '1,0', `left click = BTN_LEFT down then up (${btn})`);
    inp({ cmd: 'pointer_move', dx: 4, dy: 0 }); await sleep(100);
    ok(!evs.slice(-5).some(e => e.type === 1 && e.dev === 'p' && e.value === 1 && e.code !== 0x110), 'no unexpected button events');

    if (!(await refocus('the shortcut test'))) break scratchBlock;
    // --- shortcut: modifiers pressed then released in reverse
    const k0 = evs.length; inp({ cmd: 'key', key: 'f12', mods: ['ctrl', 'shift'] }, 'sc'); await c.until(m => m.id === 'sc'); await sleep(200);
    const ks = evs.slice(k0).filter(e => e.dev === 'k' && e.type === 1).map(e => `${e.code}:${e.value}`).join(' ');
    ok(ks === '29:1 42:1 88:1 88:0 42:0 29:0', `ctrl+shift+F12 sequence, mods released in reverse (${ks})`);

    // --- rejected input
    for (const [id, data, why] of [['r1', { cmd: 'key', key: 'power' }, 'power key not in table'], ['r2', { cmd: 'text', text: 'a\nb' }, 'newline in text'], ['r3', { cmd: 'pointer_move', dx: 99999, dy: 0 }, 'huge pointer step'], ['r4', { cmd: 'raw', code: 116 }, 'raw keycode'], ['r5', { cmd: 'text', text: '\u001b[31m' }, 'escape sequence']]) {
      inp(data, id); const r = await c.until(m => m.id === id);
      ok(r?.t === 'err' && r.data.code === 'invalid_command', `rejected: ${why}`);
    }

    // --- unlock from the phone: the password is typed through the virtual keyboard into the scratch window
    { const FD = process.env.ZENVY_FAKE_DIR, lockedPath = `${FD}/locked`;
      try { fsx.unlinkSync(lockedPath); } catch {}
      const au = () => (process.env.ZENVY_AUDIT && fsx.existsSync(process.env.ZENVY_AUDIT)) ? fsx.readFileSync(process.env.ZENVY_AUDIT, 'utf8') : '';
      const ul = async (id, password) => { c.send({ t: 'unlock', id, data: { password } }); return c.until(m => m.id === id, 12000); };
      const nsc = await ul('u0', 'x');
      ok(nsc?.t === 'err' && nsc.data.code === 'forbidden' && nsc.data.message.includes(`zenvyd grant ${dev} unlock`), 'unlocking needs its own permission, and the refusal shows the command');
      await admin({ op: 'grant', id: dev, scope: 'unlock' }); await sleep(300);
      const k0 = evs.length;
      const nl = await ul('u1', 'whatever');
      await sleep(400);
      ok(nl?.t === 'err' && nl.data.code === 'not_locked' && evs.length === k0, 'while the PC is not locked nothing is typed at all');
      const bad = await ul('u2', 'café');
      ok(bad?.t === 'err' && bad.data.code === 'invalid_command' && !JSON.stringify(bad).includes('caf'), 'a password with characters the lock screen cannot be typed with is refused, and not echoed');

      fsx.writeFileSync(lockedPath, ''); await sleep(2000);
      if (!(await ensureFocus())) { ok(false, 'scratch window lost focus: the password tests were skipped rather than typing anywhere else'); try { fsx.unlinkSync(lockedPath); } catch {} } else {
      const k1 = evs.length;
      c.send({ t: 'unlock', id: 'u3', data: { password: 'zv-Test9!' } });
      const enter = await waitEv(e => e.dev === 'k' && e.code === 28 && e.value === 1 && evs.indexOf(e) >= k1, 6000);
      fsx.unlinkSync(lockedPath); // the lock screen "accepted" it
      const ok3 = await c.until(m => m.id === 'u3', 10000);
      ok(!!enter && ok3?.t === 'ack', 'the password is typed and Enter pressed; once the lock is gone the phone gets success');
      const downs = evs.slice(k1).filter(e => e.dev === 'k' && e.value === 1 && e.code !== 42).map(e => e.code);
      ok(JSON.stringify(downs) === JSON.stringify([44, 47, 12, 20, 18, 31, 20, 10, 2, 28]), `exactly the right keys in the right order (${downs})`);
      const shiftD = evs.slice(k1).filter(e => e.dev === 'k' && e.code === 42 && e.value === 1).length, shiftU = evs.slice(k1).filter(e => e.dev === 'k' && e.code === 42 && e.value === 0).length;
      ok(shiftD === shiftU && shiftD >= 3, `Shift is released after every use (${shiftD} down, ${shiftU} up)`);
      await sleep(300);
      const got = fsx.readFileSync(OUT, 'latin1');
      ok(got.includes('zv-Test9!\n'), `the scratch window received exactly the password and a newline (file tail: ${JSON.stringify(got.slice(-40))})`);
      ok(au().includes('"unlock_attempt"') && au().includes('"unlock_ok"'), 'the attempt and its result are audit-logged');
      ok(!au().includes('zv-Test9') && !au().includes('Test9'), 'the password appears nowhere in the audit log');

      // wrong password: the lock stays, three failures lock the device out for a while
      fsx.writeFileSync(lockedPath, ''); await sleep(2000);
      for (const n of [1, 2, 3]) { if (!(await ensureFocus())) { ok(false, 'scratch window lost focus: wrong-password test skipped'); break; } const w = await ul(`uw${n}`, `wrong${n}`); ok(w?.t === 'err' && w.data.code === 'wrong_password', `wrong password ${n}: reported as still locked`); }
      const k2 = evs.length;
      const lim = await ul('uw4', 'wrong4'); await sleep(300);
      ok(lim?.t === 'err' && lim.data.code === 'rate_limited' && /\d+ s/.test(lim.data.message) && evs.length === k2, 'after three wrong passwords the device is locked out, and nothing is typed');
      ok(au().includes('"unlock_failed"') && au().includes('"unlock_blocked"'), 'failures and the lockout are audit-logged');
      }
      { // the agent samples the lock once a second: wait for an "unlocked" event that arrives AFTER the lock file is removed
        const n0 = c.msgs.length; try { fsx.unlinkSync(lockedPath); } catch {}
        const end = Date.now() + 6000; while (Date.now() < end && !c.msgs.slice(n0).some(m => m.t === 'evt' && m.data.topic === 'power.state' && m.data.value.lock === 'unlocked')) await sleep(100);
      }
    }

    if (!(await refocus('the typing test'))) break scratchBlock;
    // --- real typing into the scratch window (ASCII + Unicode via wtype), then EOF to flush
    inp({ cmd: 'text', text: 'Hello 42' }, 't1'); ok((await c.until(m => m.id === 't1'))?.t === 'ack', 'text command acknowledged');
    inp({ cmd: 'key', key: 'enter' }); await sleep(150);
    inp({ cmd: 'text', text: 'héllo wörld 日本 😀 -x' }, 't2'); await c.until(m => m.id === 't2'); inp({ cmd: 'key', key: 'enter' }); await sleep(250);
    inp({ cmd: 'key', key: 'd', mods: ['ctrl'] }); await sleep(800);
    const typed = (fsx.existsSync(OUT) ? fsx.readFileSync(OUT, 'utf8') : '').replace(/^\u001b\[24;6~/, ''); // kitty's own report of the earlier Ctrl+Shift+F12
    const UNLOCK_TYPED = 'zv-Test9!\nwrong1\nwrong2\nwrong3\n'; // typed by the unlock tests above, into the same window
    ok(typed === UNLOCK_TYPED + 'Hello 42\nhéllo wörld 日本 😀 -x\n', `typed text arrived intact incl. Unicode and a leading-dash word (${JSON.stringify(typed)})`);

    // --- disconnect while a button is held must not leave it stuck
    const c2 = await wsConnect(phone, dev); await c2.until(m => m.t === 'snapshot');
    const b0 = evs.length; c2.send({ t: 'input', data: { cmd: 'button', button: 'left', state: 'down' } });
    ok(!!(await waitEv(e => e.dev === 'p' && e.code === 0x110 && e.value === 1 && evs.indexOf(e) >= b0)), 'second session holds the left button');
    c2.ws.close(); await sleep(1200);
    const lastBtn = evs.slice(b0).filter(e => e.dev === 'p' && e.code === 0x110).at(-1);
    ok(lastBtn?.value === 0, 'button released automatically when that phone disconnected (no stuck button)');

  }
  const gone = JSON.parse(sh('hyprctl', ['clients', '-j'])).find(w => w.address === w2.address);
  if (gone) { c.send({ t: 'cmd', id: 'in-cl', data: { cmd: 'close_window', address: w2.address } }); await sleep(600); }
}
lua(`hl.dsp.cursor.move({ x = ${origCursor[0]}, y = ${origCursor[1]} })`);
streams.forEach(s => s?.destroy());
try { fsx.unlinkSync(OUT); } catch {}

console.log('== browser bridge (protocol, with a fake extension)');
const cb = await wsConnect(phone, dev); await cb.until(m => m.t === 'snapshot'); // own session: earlier sections used up this device's bad-message budget on purpose
const BPORT = process.env.ZENVY_BROWSER_PORT || '7879';
const DATA = process.env.ZENVY_DATA; // sandbox data dir, e.g. $XDG_DATA_HOME/zenvy
const BTOKEN = DATA && fsx.existsSync(pathx.join(DATA, 'browser.token')) ? fsx.readFileSync(pathx.join(DATA, 'browser.token'), 'utf8').trim() : null;
ok(!!BTOKEN, 'agent created the browser token file');
if (BTOKEN) {
  const tokMode = (fsx.statSync(pathx.join(DATA, 'browser.token')).mode & 0o777).toString(8);
  ok(tokMode === '600', `token file is private (mode ${tokMode})`);

  const rawUpgrade = (headers) => new Promise(res => {
    import('node:http').then(({ default: http }) => {
      const q = http.request({ host: '127.0.0.1', port: BPORT, path: '/browser', headers: { Connection: 'Upgrade', Upgrade: 'websocket', 'Sec-WebSocket-Version': '13', 'Sec-WebSocket-Key': 'dGhlIHNhbXBsZSBub25jZQ==', ...headers } });
      q.on('response', x => res(x.statusCode)); q.on('upgrade', () => res(101)); q.on('error', () => res(0)); q.end();
    });
  });
  ok(await rawUpgrade({ Host: `127.0.0.1:${BPORT}`, Origin: 'https://evil.example' }) === 403, 'a web page origin cannot use the bridge');
  ok(await rawUpgrade({ Host: `127.0.0.1:${BPORT}`, Origin: 'http://127.0.0.1:7888' }) === 403, 'even a page on this very machine is refused');
  ok(await rawUpgrade({ Host: `evil.example:${BPORT}`, Origin: 'chrome-extension://abc' }) === 421, 'DNS-rebinding style Host is refused');

  const ext = (token, name = 'testbrowser') => new Promise(res => {
    const ws = new WebSocket(`ws://127.0.0.1:${BPORT}/browser`); const msgs = []; let closed = null;
    ws.onopen = () => ws.send(JSON.stringify({ t: 'hello', token, browser: name }));
    const t0 = Date.now(); const trace = []; ws.onerror = e => trace.push(`error@${Date.now() - t0}ms`); ws.onmessage = e => msgs.push(JSON.parse(e.data)); ws.onclose = ev => { closed = true; trace.push(`close(${ev.code})@${Date.now() - t0}ms`); };
    const wait = async (pred, ms = 6000) => { const end = Date.now() + ms; while (Date.now() < end) { const f = msgs.find(pred); if (f) return f; await sleep(25); } return null; };
    res({ ws, msgs, wait, closed: () => closed, trace, state: () => ws.readyState });
  });

  const bad = await ext('definitely-wrong'); const err = await bad.wait(m => m.t === 'error'); for (let i = 0; i < 40 && !bad.closed(); i++) await sleep(50);
  ok(!!err && bad.closed(), `wrong token is refused and the connection closed${!!err && bad.closed() ? '' : ' [msgs=' + JSON.stringify(bad.msgs) + ' trace=' + bad.trace + ' readyState=' + bad.state() + ']'}`);

  for (let i = 0; i < 6; i++) { const x = await ext('stale-token-' + i); await x.wait(m => m.t === 'error', 3000); }
  const e = await ext(BTOKEN);
  ok(!!(await e.wait(m => m.t === 'ready')), 'the right token is accepted even right after six wrong ones (no lockout from a stale extension)');
  const capOn = await cb.until(m => m.t === 'evt' && m.data.topic === 'capability.changed' && m.data.value.id === 'browser.tabs' && m.data.value.record.state === 'available', 4000);
  ok(!!capOn, 'phone is told browser.tabs became available');

  e.ws.send(JSON.stringify({ t: 'tabs', data: { focused_window: 2, tabs: [
    { id: 5, windowId: 2, title: 'Docs \u001b[31m page', host: 'docs.rs', active: true, pinned: false, audible: false },
    { id: 6, windowId: 2, title: 'Secret', host: 'bank.example/account?token=abc', active: false, pinned: true, audible: true },
    { id: 7, windowId: 3, title: 'Other window', host: 'example.org', active: true },
    { id: -4, windowId: 3, title: 'negative id is dropped' },
  ] } }));
  const view = await cb.until(m => m.t === 'evt' && m.data.topic === 'browser.tabs' && m.data.value.browsers[0]?.windows.length === 2, 4000);
  const br = view?.data.value.browsers[0];
  ok(!!br && br.name === 'testbrowser', 'phone receives the browser and its windows');
  const tabs = br ? br.windows.flatMap(w => w.tabs) : [];
  ok(tabs.length === 3 && !tabs.some(t => t.id < 0), 'invalid tab entries are dropped');
  ok(!JSON.stringify(view).includes('\u001b') && !JSON.stringify(view).includes('token=abc') && tabs.find(t => t.id === 6)?.host === '', 'control characters and URL paths/queries never reach the phone');
  const bid = br?.id;

  const got = async (pred) => e.wait(pred, 3000);
  cb.send({ t: 'browser', id: 'b1', data: { cmd: 'activate_tab', browser: bid, tab_id: 6, window_id: 2 } });
  ok((await cb.until(m => m.id === 'b1'))?.t === 'ack' && !!(await got(m => m.t === 'activate' && m.tab_id === 6 && m.window_id === 2)), 'activate_tab reaches the extension with fixed-shape integers');
  cb.send({ t: 'browser', id: 'b2', data: { cmd: 'close_tab', browser: bid, tab_id: 7 } });
  ok(!!(await got(m => m.t === 'close' && m.tab_id === 7)), 'close_tab reaches the extension');
  cb.send({ t: 'browser', id: 'b3', data: { cmd: 'new_tab', browser: bid, window_id: 3 } });
  ok(!!(await got(m => m.t === 'new' && m.window_id === 3)), 'new_tab reaches the extension');
  for (const [id, data, why] of [['b4', { cmd: 'eval', code: 'alert(1)' }, 'arbitrary command'], ['b5', { cmd: 'close_tab', browser: bid, tab_id: '1;drop' }, 'non-integer id'], ['b6', { cmd: 'close_tab', browser: bid, tab_id: -1 }, 'negative id']]) {
    cb.send({ t: 'browser', id, data }); const r = await cb.until(m => m.id === id);
    ok(r?.t === 'err' && r.data.code === 'invalid_command', `rejected: ${why}`);
  }
  cb.send({ t: 'browser', id: 'b7', data: { cmd: 'close_tab', browser: 9999, tab_id: 1 } });
  ok((await cb.until(m => m.id === 'b7'))?.data.code === 'browser_unavailable', 'unknown browser gives a clean error');

  // --- site icons: validated on the way in, served on request, never as SVG, bounded
  const PNG1 = 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAA';
  e.ws.send(JSON.stringify({ t: 'favicons', data: { icons: { 'docs.rs': PNG1, 'evil.example': 'data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=', 'a/b': PNG1, 'big.example': 'data:image/png;base64,' + 'A'.repeat(21000) } } }));
  const favEvt = await cb.until(m => m.t === 'evt' && m.data.topic === 'browser.favicons', 4000);
  ok(!!favEvt && JSON.stringify(favEvt.data.value.hosts) === '["docs.rs"]' && !JSON.stringify(favEvt).includes('base64'), 'phones are told which hosts got an icon (host names only, not the images)');
  const askF = async (id, host) => { cb.send({ t: 'favicon', id, data: { host } }); return cb.until(m => m.id === id); };
  ok((await askF('f1', 'docs.rs'))?.data.url === PNG1, 'a phone can fetch a site icon');
  ok((await askF('f2', 'evil.example'))?.data.url === null && (await askF('f3', 'big.example'))?.data.url === null && (await askF('f4', 'nothing.example'))?.data.url === null, 'an SVG, an oversize image and an unknown host all have no icon');
  ok((await askF('f5', 'a/b'))?.data.code === 'invalid_command', 'a path-like host in a request is rejected');

  const c3 = await wsConnect(phone, dev); const snap3 = await c3.until(m => m.t === 'snapshot');
  ok(snap3?.data.browser.browsers.length === 1, 'a phone that connects later gets the current tabs in its snapshot'); c3.ws.close();

  e.ws.close();
  const gone = await cb.until(m => m.t === 'evt' && m.data.topic === 'browser.tabs' && m.data.value.browsers.length === 0, 4000);
  ok(!!gone, 'tabs disappear from the phone when the browser disconnects');
}

console.log('== browser bridge (real Brave, headless, throwaway profile)');
const BRAVE_OK = (() => { try { sh('which', ['brave']); return true; } catch { return false; } })();
if (!BTOKEN || !BRAVE_OK || process.env.ZENVY_SKIP_BRAVE) console.log('SKIP  no brave binary or token');
else {
  const EXT = pathx.resolve(process.env.ZENVY_EXT_DIR || 'browser-extension');
  const PROF = fsx.mkdtempSync(pathx.join(osx.tmpdir(), 'zenvy-brave-'));
  const CDP = '9333';
  const brave = spawn('brave', ['--headless=new', '--no-sandbox', '--disable-gpu', '--no-first-run', '--no-default-browser-check', `--user-data-dir=${PROF}`, `--remote-debugging-port=${CDP}`,
    `--load-extension=${EXT}`, '--disable-features=DisableLoadExtensionCommandLineSwitch', 'about:blank'], { stdio: 'ignore', detached: true });
  const targets = async () => { try { return await (await fetch(`http://127.0.0.1:${CDP}/json/list`)).json(); } catch { return []; } };
  let sw = null;
  for (let i = 0; i < 60 && !sw; i++) { sw = (await targets()).find(t => t.type === 'service_worker' && t.url.startsWith('chrome-extension://') && t.url.endsWith('/background.js')); if (!sw) await sleep(250); }
  if (!sw) { console.log('SKIP  this Brave did not load the unpacked extension (command-line loading may be disabled)'); }
  else {
    const evalSW = async (expr) => new Promise((res, rej) => {
      const w = new WebSocket(sw.webSocketDebuggerUrl); const timer = setTimeout(() => { w.close(); rej(new Error('cdp timeout')); }, 8000);
      w.onopen = () => w.send(JSON.stringify({ id: 1, method: 'Runtime.evaluate', params: { expression: expr, awaitPromise: true, returnByValue: true } }));
      w.onmessage = ev => { const m = JSON.parse(ev.data); if (m.id === 1) { clearTimeout(timer); w.close(); res(m.result?.result?.value); } };
    });
    await evalSW(`chrome.storage.local.set({ token: ${JSON.stringify(BTOKEN)}, port: ${BPORT} })`);
    const newTab = (html) => fetch(`http://127.0.0.1:${CDP}/json/new?${encodeURIComponent('data:text/html,<title>' + html + '</title>' + html)}`, { method: 'PUT' });
    await newTab('Alpha'); await newTab('Beta'); await newTab('Gamma');
    const view = await cb.until(m => m.t === 'evt' && m.data.topic === 'browser.tabs' && m.data.value.browsers.some(b => b.name === 'brave' || b.name === 'chrome') && JSON.stringify(m.data.value).includes('Gamma'), 15000);
    const b = view?.data.value.browsers.find(x => x.windows.some(w => w.tabs.some(t => t.title === 'Gamma')));
    ok(!!b, `real extension reported its tabs to the phone (browser name: ${b?.name})`);
    if (b) {
      const all = b.windows.flatMap(w => w.tabs); const find = t => all.find(x => x.title === t);
      ok(['Alpha', 'Beta', 'Gamma'].every(find), 'all three test tabs are listed with their titles');
      { // site icons end to end: a local site that has an icon, read from the browser's own cache by the real extension
        const httpm = (await import('node:http')).default;
        const png = fsx.readFileSync(pathx.resolve('browser-extension/icons/icon-48.png'));
        const site = httpm.createServer((q, s2) => { if (q.url === '/fav.png') { s2.setHeader('content-type', 'image/png'); s2.end(png); } else { s2.setHeader('content-type', 'text/html'); s2.end('<!doctype html><title>IconSite</title><link rel="icon" href="/fav.png">'); } }).listen(0, '127.0.0.1');
        await sleep(200);
        await fetch(`http://127.0.0.1:${CDP}/json/new?${encodeURIComponent('http://127.0.0.1:' + site.address().port + '/')}`, { method: 'PUT' });
        let url = null;
        for (let i = 0; i < 20 && !url; i++) { cb.send({ t: 'favicon', id: 'rf' + i, data: { host: '127.0.0.1' } }); const r = await cb.until(m => m.id === 'rf' + i, 3000); url = r?.data?.url; if (!url) await sleep(700); }
        ok(!!url && /^data:image\/png;base64,[A-Za-z0-9+\/=]+$/.test(url), `the real extension read a site icon from the browser's own cache (it sends no content-type header) and the phone can fetch it (${url ? url.length + ' chars' : 'none'})`);
        site.close();
      }
      ok(all.every(t => !/^data:/.test(t.host) && t.host.length < 40), `only host-like names are reported, never URLs (${all.map(t => JSON.stringify(t.host)).join(' ')})`);
      const beta = find('Beta'), gamma = find('Gamma');
      cb.send({ t: 'browser', id: 'r1', data: { cmd: 'activate_tab', browser: b.id, tab_id: beta.id, window_id: beta.window_id } });
      const r1 = await cb.until(m => m.id === 'r1'); ok(r1?.t === 'ack', 'agent acknowledged the activate command'); await sleep(600);
      const active = await evalSW(`chrome.tabs.query({ active: true }).then(ts => ts.map(t => t.title))`);
      ok(active?.includes('Beta'), `activating from the phone really switched the browser to Beta (active: ${JSON.stringify(active)})`);
      const after = await cb.until(m => m.t === 'evt' && m.data.topic === 'browser.tabs' && m.data.value.browsers.some(x => x.windows.some(w => w.tabs.some(t => t.title === 'Beta' && t.active))), 5000);
      ok(!!after, 'the phone sees the active tab change');
      // Poll the *latest* view: cb.until() scans old messages too, which would let a stale event satisfy these.
      const latest = () => cb.msgs.filter(m => m.t === 'evt' && m.data.topic === 'browser.tabs').at(-1)?.data.value.browsers[0];
      const poll = async (pred, ms = 6000) => { const end = Date.now() + ms; while (Date.now() < end) { if (await pred()) return true; await sleep(100); } return false; };
      cb.send({ t: 'browser', id: 'r2', data: { cmd: 'close_tab', browser: b.id, tab_id: gamma.id } });
      const closed = await poll(async () => !JSON.stringify(latest()).includes('Gamma') && !JSON.stringify(await targets()).includes('Gamma'));
      ok(closed, 'closing from the phone really closed the tab, and the phone sees it go');
      const count = () => latest()?.windows.flatMap(w => w.tabs).length;
      const before = count();
      cb.send({ t: 'browser', id: 'r3', data: { cmd: 'new_tab', browser: b.id, window_id: beta.window_id } });
      const grown = await poll(async () => count() === before + 1);
      ok(grown, `new tab from the phone opened a tab (${before} -> ${count()})`);
    }
  }
  try { process.kill(-brave.pid, 'SIGKILL'); } catch {}
  try { fsx.rmSync(PROF, { recursive: true, force: true }); } catch {}
}
cb.ws.close();
console.log('== audio and media (fake pactl, fake MPRIS player on a private bus)');
const FAKE = process.env.ZENVY_FAKE_DIR;
if (!FAKE) console.log('SKIP  run through tests/run-e2e.sh to get the fakes');
else {
  const ca = await wsConnect(phone, dev); // own session: earlier sections spent this device's bad-message budget on purpose
  const snap = await ca.until(m => m.t === 'snapshot');
  const audioFile = () => JSON.parse(fsx.readFileSync(`${FAKE}/audio.json`, 'utf8'));
  const lines = n => (fsx.existsSync(`${FAKE}/${n}`) ? fsx.readFileSync(`${FAKE}/${n}`, 'utf8').split('\n').filter(Boolean) : []);
  const pollA = async (pred, ms = 5000) => { const end = Date.now() + ms; while (Date.now() < end) { if (await pred()) return true; await sleep(60); } return false; };
  const lastEvt = topic => ca.msgs.filter(m => m.t === 'evt' && m.data.topic === topic).at(-1)?.data.value;
  const sendA = (id, data) => ca.send({ t: 'audio', id, data });
  const sendM = (id, data) => ca.send({ t: 'media', id, data });

  console.log('-- audio');
  ok(snap.data.ui_manifest.find(x => x.id === 'audio')?.enabled && snap.data.ui_manifest.find(x => x.id === 'media')?.enabled, 'the phone is told the Audio and Media modules are available');
  const au = snap.data.audio;
  ok(au?.output?.name === 'fake_speakers' && au.output.volume === 40 && au.mic?.volume === 70 && au.outputs.length === 2, 'snapshot carries output, microphone and the output list');
  ok(!JSON.stringify(snap).includes('secret-user') && !JSON.stringify(snap).includes('secret-host'), 'the host and user name that `pactl info` contains never reach the phone');

  sendA('a1', { cmd: 'set_volume', percent: 75 });
  ok((await ca.until(m => m.id === 'a1'))?.t === 'ack' && audioFile().sinks[0].volume === 75, 'set_volume reaches pactl and is acknowledged');
  ok(await pollA(() => lastEvt('audio.state')?.output?.volume === 75), 'the phone is pushed the new volume (event driven)');

  // a change made on the PC itself (keyboard volume key, another app): the phone must hear about it without asking
  const s = audioFile(); s.sinks[0].volume = 20; fsx.writeFileSync(`${FAKE}/audio.json`, JSON.stringify(s)); fsx.appendFileSync(`${FAKE}/events`, "Event 'change' on sink #1\n");
  ok(await pollA(() => lastEvt('audio.state')?.output?.volume === 20), 'a volume change made on the PC reaches the phone');

  sendA('a2', { cmd: 'set_mute', muted: true }); await ca.until(m => m.id === 'a2');
  ok(await pollA(() => lastEvt('audio.state')?.output?.muted === true) && audioFile().sinks[0].mute === true, 'mute');
  sendA('a3', { cmd: 'set_mic_mute', muted: true }); await ca.until(m => m.id === 'a3');
  ok(await pollA(() => lastEvt('audio.state')?.mic?.muted === true), 'microphone mute');
  sendA('a4', { cmd: 'set_default_output', name: 'fake_hdmi' }); await ca.until(m => m.id === 'a4');
  ok(await pollA(() => lastEvt('audio.state')?.output?.name === 'fake_hdmi' && lastEvt('audio.state').outputs.find(o => o.default)?.name === 'fake_hdmi'), 'switching the output device');

  const before = lines('pactl.log').length;
  for (const [id, data, code, why] of [['b1', { cmd: 'set_volume', percent: 101 }, 'invalid_command', 'volume above 100'], ['b2', { cmd: 'set_volume', percent: -1 }, 'invalid_command', 'negative volume'],
      ['b3', { cmd: 'set_default_output', name: 'x; rm -rf ~' }, 'invalid_command', 'shell-looking device name'], ['b4', { cmd: 'set_default_output', name: '$(id)' }, 'invalid_command', 'command substitution'],
      ['b5', { cmd: 'set_default_output', name: 'not_a_real_device' }, 'failed', 'well-formed but unknown device'], ['b6', { cmd: 'exec', argv: ['id'] }, 'invalid_command', 'arbitrary command']]) {
    sendA(id, data); const r = await ca.until(m => m.id === id);
    ok(r?.t === 'err' && r.data.code === code, `rejected: ${why}`);
  }
  const after = lines('pactl.log').slice(before).filter(l => l.startsWith('set-'));
  ok(after.length === 0, `none of the rejected commands reached pactl (${after.length} set-* calls)`);
  const allowed = /^(--format=json (info|list sinks|list sources)|subscribe|set-sink-volume @DEFAULT_SINK@ \d{1,3}%|set-sink-mute @DEFAULT_SINK@ [01]|set-source-mute @DEFAULT_SOURCE@ [01]|set-default-sink [A-Za-z0-9._:+@-]+)$/;
  ok(lines('pactl.log').every(l => allowed.test(l)), 'every pactl invocation had one of the fixed argv shapes');

  await sleep(300); const idle0 = lines('pactl.log').length; await sleep(2500);
  ok(lines('pactl.log').length === idle0, 'idle: no pactl calls at all while nothing changes (no polling)');



  console.log('-- application icons');
  { const ci = await wsConnect(phone, dev); await ci.until(m => m.t === 'snapshot');
    const ask = async (id, cls) => { ci.send({ t: 'icon', id, data: { class: cls } }); return ci.until(m => m.id === id); };
    const okI = await ask('i1', 'zenvy-fake-app');
    ok(okI?.t === 'icon' && okI.data.mime === 'image/png' && Buffer.from(okI.data.b64, 'base64').toString('latin1').endsWith('FAKE-ICON-BYTES'), 'an application icon is found through its desktop entry and sent as a PNG');
    ok((await ask('i2', 'ZENVY-FAKE-APP'))?.data.mime === 'image/png', 'the window class is matched case-insensitively');
    ok((await ask('i3', 'zenvy-no-such-app'))?.data.none === true, 'an unknown application gets "none" (the phone keeps its letter)');
    for (const [id, cls, why] of [['e1', 'zenvy-evil', 'an Icon= pointing at /etc/passwd'], ['e2', 'zenvy-evil2', 'an Icon= pointing at a file that is not an image'], ['e3', 'zenvy-evil3', 'an Icon= with ../ traversal']]) {
      const r = await ask(id, cls); ok(r?.t === 'icon' && r.data.none === true && !r.data.b64, `refused: ${why}`);
    }
    for (const [id, cls, why] of [['b1', '../../etc/passwd', 'a path as the class'], ['b2', 'a/b', 'a slash in the class'], ['b3', '', 'an empty class'], ['b4', 'x'.repeat(200), 'an oversize class']]) {
      const r = await ask(id, cls); ok(r?.t === 'err' && r.data.code === 'invalid_command', `rejected: ${why}`);
    }
    ci.ws.close(); }



  console.log('-- custom actions (config-defined fixed commands)');
  { const FD = process.env.ZENVY_FAKE_DIR, mark = `${FD}/action-mark`, danger = `${FD}/action-danger`;
    for (const f of [mark, danger]) { try { fsx.unlinkSync(f); } catch {} }
    const ca = await wsConnect(phone, dev); const snapA = await ca.until(m => m.t === 'snapshot');
    const ask = async (id, data) => { ca.send({ t: 'action', id, data }); return ca.until(m => m.id === id); };
    const au = () => (process.env.ZENVY_AUDIT && fsx.existsSync(process.env.ZENVY_AUDIT)) ? fsx.readFileSync(process.env.ZENVY_AUDIT, 'utf8') : '';
    const acts = snapA.data.actions;
    ok(Array.isArray(acts) && acts.map(a => a.id).join() === 'mark,danger,broken' && acts[1].confirm === true, 'the phone is told the configured actions (id, label, confirm)');
    ok(!JSON.stringify(acts).includes('touch') && !JSON.stringify(snapA.data).includes('action-mark'), 'the command line and paths never reach the phone');
    const no = await ask('a0', { cmd: 'run', id: 'mark' });
    ok(no?.t === 'err' && no.data.code === 'forbidden' && no.data.message.includes(`zenvyd grant ${dev} exec`) && !fsx.existsSync(mark), 'without the exec permission nothing runs and the grant command is shown');
    await admin({ op: 'grant', id: dev, scope: 'exec' }); await sleep(300);
    ok((await ask('a1', { cmd: 'run', id: 'mark' }))?.t === 'ack' && fsx.existsSync(mark), 'an action runs its fixed command');
    ok((await ask('a2', { cmd: 'run', id: 'nope' }))?.data.code === 'unknown_action', 'an unknown id is refused');
    ok((await ask('a3', { cmd: 'run', id: 'mark; rm -rf /' }))?.data.code === 'unknown_action', 'an id is only ever looked up, never executed');
    const pend = await ask('a4', { cmd: 'run', id: 'danger' });
    ok(pend?.t === 'action' && pend.data.state === 'pending' && /^[0-9a-f]{24}$/.test(pend.data.token) && !fsx.existsSync(danger), 'a confirm action asks first and has not run yet');
    ok((await ask('a5', { cmd: 'confirm', token: 'deadbeefdeadbeefdeadbeef' }))?.data.code === 'not_pending' && !fsx.existsSync(danger), 'a wrong token does not confirm');
    const pend2 = await ask('a6', { cmd: 'run', id: 'danger' });
    ok((await ask('a7', { cmd: 'confirm', token: pend2.data.token }))?.t === 'ack' && fsx.existsSync(danger), 'the right token runs it');
    const br = await ask('a8', { cmd: 'run', id: 'broken' });
    ok(br?.t === 'err' && br.data.code === 'failed' && /exited with 1/.test(br.data.message), 'a failing command is reported briefly');
    ok((await ask('a9', { cmd: 'zap' }))?.data.code === 'invalid_command', 'an unknown action command is rejected');
    ok(au().includes('"action_run"') && au().includes('mark') && !au().includes('touch') && !au().includes('action-mark'), 'runs are audit-logged by id, never by command line');
    ca.ws.close(); }


  console.log('-- clipboard and links (fake wl-copy/wl-paste/xdg-open: the real clipboard is never touched)');
  { const FD = process.env.ZENVY_FAKE_DIR, clip = `${FD}/clip`, opened = `${FD}/opened.log`;
    for (const f of [clip, opened, `${FD}/clip-types`]) { try { fsx.unlinkSync(f); } catch {} }
    const cc = await wsConnect(phone, dev); const snapC = await cc.until(m => m.t === 'snapshot');
    const ask = async (id, data) => { cc.send({ t: 'clipboard', id, data }); return cc.until(m => m.id === id); };
    const au = () => (process.env.ZENVY_AUDIT && fsx.existsSync(process.env.ZENVY_AUDIT)) ? fsx.readFileSync(process.env.ZENVY_AUDIT, 'utf8') : '';
    const opens = () => (fsx.existsSync(opened) ? fsx.readFileSync(opened, 'utf8').split('\n').filter(Boolean) : []);
    ok(snapC.data.share?.clipboard === true && snapC.data.share?.open_link === true, 'the phone is told clipboard transfer and link opening are available');
    const no = await ask('c0', { cmd: 'set', text: 'x' });
    ok(no?.t === 'err' && no.data.code === 'forbidden' && no.data.message.includes(`zenvyd grant ${dev} clipboard`) && !fsx.existsSync(clip), 'without the clipboard permission nothing happens and the grant command is shown');
    await admin({ op: 'grant', id: dev, scope: 'clipboard' }); await sleep(300);
    const secret = 'päss "wörd" $(touch /tmp/zv-no) `x` \\n 日本';
    ok((await ask('c1', { cmd: 'set', text: secret }))?.t === 'ack' && fsx.readFileSync(clip, 'utf8') === secret, 'text sent from the phone lands on the PC clipboard unchanged (no shell involved)');
    ok((await ask('c2', { cmd: 'get' }))?.data.text === secret, 'the PC clipboard can be read back on the phone');
    ok(!fsx.existsSync('/tmp/zv-no'), 'shell syntax in the text was never executed');
    fsx.writeFileSync(`${FD}/clip-types`, 'image/png\n');
    ok((await ask('c3', { cmd: 'get' }))?.data.text === null, 'an image on the PC clipboard gives no text');
    fsx.writeFileSync(`${FD}/clip-types`, 'text/plain\n'); fsx.writeFileSync(clip, 'a'.repeat(70000));
    ok((await ask('c4', { cmd: 'get' }))?.data.code === 'failed', 'an oversize clipboard is refused, not truncated');
    ok((await ask('c5', { cmd: 'set', text: 'a'.repeat(13000) }))?.data.code === 'failed', 'text over 12 KB from the phone is refused (bigger things belong in a file transfer)');
    ok((await ask('c6', { cmd: 'open', url: 'https://example.com/a?b=1' }))?.t === 'ack' && opens().join() === 'https://example.com/a?b=1', 'a link opens on the PC with exactly that address');
    for (const [id, url, why] of [['o1', 'javascript:alert(1)', 'javascript'], ['o2', 'file:///etc/passwd', 'a file link'], ['o3', 'https://user:pw@example.com/', 'credentials'], ['o4', 'https://exa mple.com', 'a space'], ['o5', 'ftp://example.com', 'ftp'], ['o6', 'https://example.com/' + 'a'.repeat(2100), 'a very long link']]) {
      const r = await ask(id, { cmd: 'open', url }); ok(r?.t === 'err' && r.data.code === 'invalid_command', `link refused: ${why}`);
    }
    ok(opens().length === 1, 'refused links never reach xdg-open');
    ok((await ask('c7', { cmd: 'set' }))?.data.code === 'invalid_command' && (await ask('c8', { cmd: 'paste-all' }))?.data.code === 'invalid_command', 'missing text and unknown commands are rejected');
    ok(au().includes('"clipboard_get"') && au().includes('"clipboard_set"') && au().includes('"link_open"') && !au().includes('päss') && !au().includes('example.com'), 'transfers are audit-logged without their content');
    ok(fsx.existsSync(`${FD}/notify.log`) && fsx.readFileSync(`${FD}/notify.log`, 'utf8').includes("read this PC's clipboard"), 'the person at the PC is notified when the clipboard is read');
    cc.ws.close(); }

  console.log('-- power and lock (fake systemctl/loginctl: nothing real is ever suspended, restarted or locked)');
  { const FD = process.env.ZENVY_FAKE_DIR;
    const plogPath = `${FD}/power.log`, lockedPath = `${FD}/locked`;
    for (const f of [plogPath, lockedPath]) { try { fsx.unlinkSync(f); } catch {} }
    const acts = () => (fsx.existsSync(plogPath) ? fsx.readFileSync(plogPath, 'utf8').split('\n').filter(Boolean) : []);
    const cp = await wsConnect(phone, dev); const snapPw = await cp.until(m => m.t === 'snapshot');
    const ask = async (id, data) => { cp.send({ t: 'power', id, data }); return cp.until(m => m.id === id); };
    const sleepMs = ms => new Promise(r => setTimeout(r, ms));
    const pw = snapPw.data.power;
    ok(pw?.lock === 'unlocked' && pw.can_lock && pw.can_suspend && pw.can_reboot && pw.can_poweroff && pw.can_unlock === true, `the phone is told what the PC allows (${JSON.stringify(pw)})`);
    ok(snapPw.data.ui_manifest.find(x => x.id === 'power')?.enabled === true, 'the Power module is enabled in the UI manifest');

    // without the permission: refused, told the command, nothing runs
    const noScope = await ask('p0', { cmd: 'request', action: 'poweroff' });
    ok(noScope?.t === 'err' && noScope.data.code === 'forbidden' && noScope.data.message.includes(`zenvyd grant ${dev} power`), 'a phone without the power permission is refused and shown the grant command');
    ok(acts().length === 0, 'nothing ran');
    await admin({ op: 'grant', id: dev, scope: 'power' }); await sleepMs(300);

    // lock: runs at once, and the phone learns the PC is locked
    const lk = await ask('p1', { cmd: 'request', action: 'lock' });
    ok(lk?.t === 'ack' && acts().includes('loginctl lock-session 5'), 'lock runs immediately through loginctl lock-session');
    ok(!!(await cp.until(m => m.t === 'evt' && m.data.topic === 'power.state' && m.data.value.lock === 'locked', 6000)), 'the phone is pushed "locked" (state is sampled while a phone is connected)');
    // --- while locked: lockdown (no windows, input or tabs; no titles leak), but media/volume/power still work
    const dsEvt = await cp.until(m => m.t === 'evt' && m.data.topic === 'desktop.state' && m.data.value.windows.length === 0, 4000);
    ok(!!dsEvt, 'the phone is pushed an empty desktop when the PC locks (no window titles stay on the phone)');
    const cl = await wsConnect(phone, dev); const snapL = await cl.until(m => m.t === 'snapshot');
    ok(snapL.data.power.lock === 'locked' && snapL.data.desktop.windows.length === 0 && snapL.data.desktop.workspaces.length === 0 && snapL.data.browser.browsers.length === 0, 'a phone that connects while the PC is locked is sent no windows, workspaces or tabs');
    const asked = async (type, id, data) => { cl.send({ t: type, id, data }); return cl.until(m => m.id === id); };
    for (const [type, id, data, why] of [['cmd', 'lk1', { cmd: 'switch_workspace', id: 1 }, 'switching workspaces'], ['input', 'lk2', { cmd: 'pointer_move', dx: 0, dy: 0 }, 'the pointer'], ['browser', 'lk3', { cmd: 'close_tab', browser: 1, tab: 1 }, 'closing browser tabs'], ['favicon', 'lk7', { host: 'docs.rs' }, 'site icons'], ['action', 'lk8', { cmd: 'run', id: 'mark' }, 'custom actions'], ['clipboard', 'lk9', { cmd: 'get' }, 'the clipboard']]) {
      const r = await asked(type, id, data); ok(r?.t === 'err' && r.data.code === 'pc_locked', `refused while locked: ${why}`);
    }
    ok((await asked('audio', 'lk4', { cmd: 'set_volume', percent: 33 }))?.t === 'ack', 'volume still works while locked');
    ok((await asked('power', 'lk5', { cmd: 'request', action: 'suspend' }))?.data.state === 'pending', 'power options still work while locked');
    cl.ws.close();
    fsx.unlinkSync(lockedPath);
    ok(!!(await cp.until(m => m.t === 'evt' && m.data.topic === 'power.state' && m.data.value.lock === 'unlocked', 6000)), 'and "unlocked" again when the lock goes away');
    ok(!!(await cp.until(m => m.t === 'evt' && m.data.topic === 'desktop.state' && m.data.value.windows.length > 0, 4000)), 'the real desktop is sent again after unlocking');

    // suspend: request -> token -> confirm -> countdown -> executes
    const rq = await ask('p2', { cmd: 'request', action: 'suspend' });
    ok(rq?.t === 'power' && rq.data.state === 'pending' && /^[0-9a-f]{24}$/.test(rq.data.token) && rq.data.delay_ms === 1000, 'suspend asks for a confirmation and returns a random token');
    ok(!acts().some(l => l.startsWith('systemctl')), 'asking alone runs nothing');
    const wrong = await ask('p3', { cmd: 'confirm', token: 'deadbeefdeadbeefdeadbeef' });
    ok(wrong?.t === 'err' && wrong.data.code === 'not_pending', 'a wrong token does not confirm');
    ok((await ask('p4', { cmd: 'confirm', token: rq.data.token }))?.data.code === 'not_pending', 'and it used up the pending request: no retries on one token');
    const rq2 = await ask('p5', { cmd: 'request', action: 'suspend' });
    const sc = await ask('p6', { cmd: 'confirm', token: rq2.data.token });
    ok(sc?.t === 'power' && sc.data.state === 'scheduled' && sc.data.delay_ms === 1000, 'the right token schedules it');
    ok(!!(await cp.until(m => m.t === 'evt' && m.data.topic === 'power.scheduled' && m.data.value.id === sc.data.id, 3000)), 'every phone is told a countdown started');
    ok((await ask('p7', { cmd: 'cancel', token: sc.data.id }))?.t === 'ack', 'it can be cancelled during the countdown');
    ok(!!(await cp.until(m => m.t === 'evt' && m.data.topic === 'power.cancelled', 3000)), 'and everyone is told');
    await sleepMs(1600);
    ok(!acts().some(l => l.startsWith('systemctl')), 'a cancelled suspend never reaches systemctl');
    const rq3 = await ask('p8', { cmd: 'request', action: 'suspend' }); await ask('p9', { cmd: 'confirm', token: rq3.data.token });
    ok(!!(await cp.until(m => m.t === 'evt' && m.data.topic === 'power.executing', 4000)), 'a confirmed suspend executes after the delay');
    await sleepMs(300);
    ok(acts().filter(l => l === 'systemctl suspend').length === 1, 'exactly one `systemctl suspend`');

    // reboot cancelled from the PC itself
    const rb = await ask('p10', { cmd: 'request', action: 'reboot' }); await ask('p11', { cmd: 'confirm', token: rb.data.token });
    ok((await admin({ op: 'power_cancel' })).find(l => l.type === 'scoped')?.ok === true, 'the person at the PC can stop a countdown (zenvyd power-cancel)');
    ok((await admin({ op: 'power_cancel' })).find(l => l.type === 'scoped')?.ok === false, 'and there is nothing left to cancel afterwards');
    await sleepMs(1500);
    ok(!acts().includes('systemctl reboot'), 'the stopped reboot never ran');

    // only one countdown at a time
    const b1 = await ask('p12', { cmd: 'request', action: 'poweroff' }); await ask('p13', { cmd: 'confirm', token: b1.data.token });
    const b2 = await ask('p14', { cmd: 'request', action: 'reboot' }); const busy = await ask('p15', { cmd: 'confirm', token: b2.data.token });
    ok(busy?.t === 'err' && busy.data.code === 'busy', 'a second countdown is refused while one is running');
    await sleepMs(1500);
    ok(acts().includes('systemctl poweroff') && !acts().includes('systemctl reboot'), 'only the first one ran');
    ok((await ask('p16', { cmd: 'confirm', token: 'abc' }))?.data.code === 'not_pending', 'confirming with nothing requested does nothing');

    // hostile or malformed
    for (const [id, data, why] of [['h1', { cmd: 'request', action: 'hibernate' }, 'an action that is not offered'], ['h2', { cmd: 'request', action: 'rm -rf /' }, 'a shell-looking action'], ['h3', { cmd: 'exec', argv: ['id'] }, 'a command'], ['h4', { cmd: 'confirm', token: 'x;y' }, 'a token with odd characters']]) {
      const r = await ask(id, data); ok(r?.t === 'err' && r.data.code === 'invalid_command', `rejected: ${why}`);
    }
    ok(acts().every(l => /^(loginctl lock-session 5|systemctl (suspend|poweroff))$/.test(l)), `only the fixed commands were ever run (${acts().join(' | ')})`);
    const au = (process.env.ZENVY_AUDIT && fsx.existsSync(process.env.ZENVY_AUDIT)) ? fsx.readFileSync(process.env.ZENVY_AUDIT, 'utf8').split('\n').filter(Boolean) : [];
    ok(['power_request', 'power_confirm', 'power_exec', 'power_cancel'].every(k => au.some(l => l.includes(`"${k}"`))), 'requests, confirmations, executions and cancellations are audit-logged');
    cp.ws.close(); }

  console.log('-- listening (PC audio to the phone)');
  ca.ws.close();
  const cl = await wsConnect(phone, dev); const snapL = await cl.until(m => m.t === 'snapshot');
  const auditLines = () => (process.env.ZENVY_AUDIT && fsx.existsSync(process.env.ZENVY_AUDIT)) ? fsx.readFileSync(process.env.ZENVY_AUDIT, 'utf8').split('\n').filter(Boolean) : [];
  const pids = () => lines('parec.pids');
  const isAlive = pid => { try { return !fsx.readFileSync(`/proc/${pid}/stat`, 'utf8').includes(') Z'); } catch { return false; } };
  // start from a clean audio state: default output unmuted (earlier audio tests changed the default sink and muted things)
  { const a = audioFile(); a.default_sink = 'fake_speakers'; a.sinks.forEach(x => { x.mute = false; }); fsx.writeFileSync(`${FAKE}/audio.json`, JSON.stringify(a)); }
  const sinkMuted = () => { const a = audioFile(); return a.sinks.find(x => x.name === a.default_sink).mute; };
  const sendL = (id, data) => cl.send({ t: 'listen', id, data });
  ok(snapL.data.capabilities.capabilities['audio.listen']?.state === 'available', 'the phone sees that listening is available');

  // 1. without the permission: refused, told exactly what to do, and nothing is captured or sent
  sendL('l0', { cmd: 'start' });
  const refused = await cl.until(m => m.id === 'l0');
  ok(refused?.t === 'err' && refused.data.code === 'forbidden' && refused.data.message.includes(`zenvyd grant ${dev} listen`), 'a phone without the listen permission is refused and shown the grant command');
  await sleep(600);
  ok(cl.bins.length === 0 && pids().length === 0, 'no audio is captured or sent to a phone that was refused');

  // 2. the owner grants it on the PC; the connected phone gets it live (no reconnect)
  const g = (await admin({ op: 'grant', id: dev, scope: 'listen' })).find(l => l.type === 'scoped');
  ok(g?.ok && g.scopes.includes('listen'), 'granting the permission on the PC');
  ok(!(await admin({ op: 'grant', id: dev, scope: 'root' })).find(l => l.type === 'scoped').ok, 'an unknown scope cannot be granted');
  await sleep(300);

  // 3. start, muting the PC speakers
  sendL('l1', { cmd: 'start', mute_pc: true });
  const started = await cl.until(m => m.id === 'l1' && m.t === 'listen', 5000);
  ok(started?.data.state === 'started' && started.data.rate === 48000 && started.data.channels === 2 && started.data.frame_ms === 20, 'start is answered with the stream format');
  await sleep(1500);
  const fr = cl.bins.map(b => ({ tag: b.d[0], seq: b.d.readUInt32LE(1), len: b.d.length - 5 }));
  ok(fr.length >= 55 && fr.length <= 95, `about 50 frames per second arrive (${fr.length} in ~1.5 s)`);
  ok(fr.every(f => f.tag === 1) && fr.slice(1).filter((f, i) => f.seq === fr[i].seq + 1).length >= fr.length - 3, 'frames carry the tag and consecutive sequence numbers');
  const avg = fr.reduce((a, f) => a + f.len, 0) / fr.length;
  ok(avg > 40 && avg < 400, `Opus packets carry a real signal at a sane size (average ${avg.toFixed(0)} bytes, ~${Math.round(avg * 8 * 50 / 1000)} kbit/s)`);
  ok(pids().length === 1 && isAlive(pids()[0]), 'exactly one capture process is running');
  ok(lines('parec.args').at(-1).includes('--device=@DEFAULT_MONITOR@') && lines('parec.args').at(-1).includes('--format=s16le'), 'capture uses the default output monitor with fixed arguments');
  ok(sinkMuted() === true, 'the PC speakers are muted while listening');
  ok(lines('notify.log').some(l => l.includes('is listening to this PC')), 'the person at the PC is notified');
  ok(auditLines().some(l => l.includes('"listen_start"')) && auditLines().some(l => l.includes('"grant"')), 'starting and the permission grant are audit-logged');

  // 4. a second phone session shares the same capture (one process, both get frames)
  const cl2 = await wsConnect(phone, dev); await cl2.until(m => m.t === 'snapshot');
  cl2.send({ t: 'listen', id: 'x1', data: { cmd: 'start' } }); await cl2.until(m => m.id === 'x1' && m.t === 'listen', 4000); await sleep(500);
  ok(pids().length === 1 && cl2.bins.length > 10, 'a second listener shares the one capture process');
  cl2.ws.close(); await sleep(600);
  ok(isAlive(pids()[0]), 'capture continues for the remaining listener');

  // 5. stop: frames stop, the process is killed, the PC is unmuted, the owner is told
  sendL('l2', { cmd: 'stop' });
  ok((await cl.until(m => m.id === 'l2' && m.t === 'listen'))?.data.state === 'stopped', 'stop is acknowledged');
  const n0 = cl.bins.length; await sleep(700);
  ok(cl.bins.length - n0 <= 2, 'no audio arrives after stopping');
  ok(!isAlive(pids()[0]), 'the capture process is gone once nobody listens');
  ok(sinkMuted() === false, 'the PC speakers are restored');
  ok(lines('notify.log').some(l => l.includes('stopped listening')) && auditLines().some(l => l.includes('"listen_stop"')), 'stopping is announced and audit-logged');

  // 6. a phone that vanishes mid-stream must not leave the PC capturing or muted
  const cl3 = await wsConnect(phone, dev); await cl3.until(m => m.t === 'snapshot');
  cl3.send({ t: 'listen', id: 'y1', data: { cmd: 'start', mute_pc: true } }); await cl3.until(m => m.id === 'y1' && m.t === 'listen', 4000); await sleep(400);
  const pid3 = pids().at(-1); ok(isAlive(pid3) && sinkMuted() === true, 'second session listening and muting');
  cl3.ws.close();
  ok(await pollA(() => !isAlive(pid3) && sinkMuted() === false, 6000), 'a phone that disconnects mid-stream stops the capture and unmutes the PC');

  // 7. the owner takes the permission away while a phone is listening: it stops at once
  const cl4 = await wsConnect(phone, dev); await cl4.until(m => m.t === 'snapshot');
  cl4.send({ t: 'listen', id: 'z1', data: { cmd: 'start', mute_pc: true } }); await cl4.until(m => m.id === 'z1' && m.t === 'listen', 4000); await sleep(400);
  const pid4 = pids().at(-1);
  await admin({ op: 'ungrant', id: dev, scope: 'listen' });
  const cut = await cl4.until(m => m.t === 'listen' && m.data.state === 'stopped' && m.data.reason === 'permission removed', 4000);
  ok(!!cut && await pollA(() => !isAlive(pid4) && sinkMuted() === false, 5000), 'removing the permission on the PC stops a live listener immediately');
  const nCut = cl4.bins.length; await sleep(500); ok(cl4.bins.length - nCut <= 1, 'and no more audio reaches it');

  // 8. malformed commands
  sendL('bad1', { cmd: 'record', path: '/tmp/x' }); const bad = await cl.until(m => m.id === 'bad1');
  ok(bad?.t === 'err' && bad.data.code === 'invalid_command', 'rejected: a command that is not start or stop');
  cl.ws.close(); cl4.ws.close();

  console.log('-- brightness');
  ca.ws.close();
  const cbr = await wsConnect(phone, dev); const snapB = await cbr.until(m => m.t === 'snapshot'); // fresh session: each one may send only 10 invalid messages
  const lastEvtB = topic => cbr.msgs.filter(m => m.t === 'evt' && m.data.topic === topic).at(-1)?.data.value;
  const bFile = () => JSON.parse(fsx.readFileSync(`${FAKE}/bright.json`, 'utf8'));
  const ddcCalls = () => lines('ddc.log');
  const br = snapB.data.brightness?.displays;
  ok(snapB.data.ui_manifest.find(x => x.id === 'brightness')?.enabled, 'the phone is told the Brightness module is available');
  ok(br?.length === 1 && br[0].id === 'ddc:2' && br[0].name === 'HDMI-A-1' && br[0].model === 'Fake Monitor' && br[0].percent === 50 && br[0].kind === 'ddc', 'snapshot lists the monitor by connector and model with its real brightness');
  ok(!JSON.stringify(snapB).includes('SERIAL-SECRET'), 'the monitor serial number from ddcutil never reaches the phone');
  const sendB = (id, data) => cbr.send({ t: 'brightness', id, data });

  sendB('c1', { cmd: 'set', id: 'ddc:2', percent: 70 });
  ok((await cbr.until(m => m.id === 'c1'))?.t === 'ack', 'set is acknowledged immediately (the slow DDC write happens in the background)');
  ok(await pollA(() => bFile().ddc[0].cur === 70, 6000), 'the value really reaches the monitor');
  ok(await pollA(() => lastEvtB('brightness.state')?.displays?.[0]?.percent === 70), 'the phone is pushed the new state after the write');

  // a slider drag: many values in a row. Only the newest must matter, and the monitor must not be flooded.
  const writesBefore = ddcCalls().filter(l => l.startsWith('setvcp')).length;
  for (const p of [10, 20, 30, 40, 50, 60]) { sendB('d' + p, { cmd: 'set', id: 'ddc:2', percent: p }); await sleep(15); }
  ok(await pollA(() => bFile().ddc[0].cur === 60, 6000), 'a burst ends on the newest value');
  const burstWrites = ddcCalls().filter(l => l.startsWith('setvcp')).length - writesBefore;
  ok(burstWrites <= 3, `six rapid values were coalesced into ${burstWrites} monitor writes (at most 3 allowed)`);

  // monitors never announce changes: a change on the monitor's own buttons is only seen on refresh, never by polling
  const s2 = bFile(); s2.ddc[0].cur = 33; fsx.writeFileSync(`${FAKE}/bright.json`, JSON.stringify(s2));
  await sleep(1200);
  ok(lastEvtB('brightness.state')?.displays?.[0]?.percent !== 33, 'a change made with the monitor buttons is not detected by background polling');
  sendB('r1', { cmd: 'refresh' }); await cbr.until(m => m.id === 'r1');
  ok(await pollA(() => lastEvtB('brightness.state')?.displays?.[0]?.percent === 33), 'refresh re-reads the real value');

  const w0 = ddcCalls().length;
  for (const [id, data, code, why] of [['e1', { cmd: 'set', id: 'ddc:2', percent: 101 }, 'invalid_command', 'brightness above 100'], ['e2', { cmd: 'set', id: 'ddc:2; rm -rf ~', percent: 5 }, 'invalid_command', 'a shell-looking id'],
      ['e3', { cmd: 'set', id: 'ddc:9', percent: 5 }, 'failed', 'a well-formed but unknown display'], ['e4', { cmd: 'set', id: '../../etc', percent: 5 }, 'invalid_command', 'a path as an id'], ['e5', { cmd: 'exec' }, 'invalid_command', 'an arbitrary command']]) {
    sendB(id, data); const r = await cbr.until(m => m.id === id);
    ok(r?.t === 'err' && r.data.code === code, `rejected: ${why}`);
  }
  await sleep(500);
  ok(!ddcCalls().slice(w0).some(l => l.startsWith('setvcp')), 'none of the rejected commands reached ddcutil');
  ok(ddcCalls().every(l => /^(detect --brief|getvcp 10 --bus \d+ --brief|setvcp 10 \d{1,3} --bus \d+ --noverify)$/.test(l)), 'every ddcutil invocation had one of the fixed argv shapes');
  await sleep(300); const quiet0 = ddcCalls().length; await sleep(2500);
  ok(ddcCalls().length === quiet0, 'idle: no ddcutil calls while nothing changes (no polling)');

  console.log('-- media');
  cbr.ws.close();
  const cm = await wsConnect(phone, dev); const snapM = await cm.until(m => m.t === 'snapshot');
  const sendMc = (id, data) => cm.send({ t: 'media', id, data });
  const lastEvtM = topic => cm.msgs.filter(m => m.t === 'evt' && m.data.topic === topic).at(-1)?.data.value;
  const me = snapM.data.media; const pl = me?.players?.[0];
  ok(pl?.id === 'org.mpris.MediaPlayer2.zenvyfake' && pl.title === 'Fake Track' && pl.artist === 'Fake Artist' && pl.length_ms === 180000 && pl.status === 'paused' && pl.can_seek, 'snapshot carries the MPRIS player with title, artist, length and capabilities');
  ok(me.current === pl.id && me.players.length === 1, 'only real MPRIS players are listed (no decoys, no playerctld duplicate)');
  const P = 'org.mpris.MediaPlayer2.zenvyfake';
  sendMc('m1', { cmd: 'play_pause', player: P });
  ok((await cm.until(m => m.id === 'm1'))?.t === 'ack' && await pollA(() => lines('player.log').includes('PlayPause')), 'play_pause reaches the player');
  ok(await pollA(() => lastEvtM('media.state')?.players?.[0]?.status === 'playing'), 'the phone is pushed the new playback status (event driven)');
  sendMc('m2', { cmd: 'next', player: P }); sendMc('m3', { cmd: 'previous', player: P }); sendMc('m4', { cmd: 'seek_by', player: P, offset_ms: -10000 });
  ok(await pollA(() => ['Next', 'Previous', 'Seek -10000000'].every(x => lines('player.log').includes(x))), 'next, previous and seek (milliseconds -> microseconds) reach the player');
  for (const [id, data, code, why] of [['n1', { cmd: 'play_pause', player: 'org.mpris.MediaPlayer2.notrunning' }, 'failed', 'a player that is not running'],
      ['n2', { cmd: 'play_pause', player: 'org.freedesktop.systemd1' }, 'invalid_command', 'a non-player bus name'], ['n3', { cmd: 'play_pause', player: 'org.mpris.MediaPlayer2.x;rm' }, 'invalid_command', 'a player name with shell characters'],
      ['n4', { cmd: 'seek_by', player: P, offset_ms: 99999999 }, 'invalid_command', 'an absurd seek'], ['n5', { cmd: 'call', method: 'Quit', player: P }, 'invalid_command', 'an arbitrary D-Bus method']]) {
    sendMc(id, data); const r = await cm.until(m => m.id === id);
    ok(r?.t === 'err' && r.data.code === code, `rejected: ${why}`);
  }
  ok(lines('player.log').length === 4, `nothing extra reached the player (${lines('player.log').join(', ')})`);
  cm.ws.close();
}

console.log('== resilience');
{
  const tlsx = await import('node:tls');
  const pidBefore = execFileSync('pgrep', ['-xn', 'zenvyd']).toString().trim();
  // Fire requests and vanish before the reply is written: the agent's writes then hit a closed socket.
  // If SIGPIPE were left at its default, the whole agent would be killed by the first of these.
  await Promise.all(Array.from({ length: 60 }, () => new Promise(res => {
    const s = tlsx.connect({ host: '127.0.0.1', port: +PORT, rejectUnauthorized: false }, () => {
      s.write(`GET / HTTP/1.1\r\nHost: ${HOST}\r\n\r\n`); s.destroy(); setTimeout(res, 20);
    }); s.on('error', () => res());
  })));
  await sleep(300);
  const alive = await req('/api/hello').then(x => x.status === 200).catch(() => false);
  const pidAfter = (() => { try { return execFileSync('pgrep', ['-xn', 'zenvyd']).toString().trim(); } catch { return ''; } })();
  ok(alive && pidBefore === pidAfter, 'the agent survives 60 clients that disconnect mid-response (SIGPIPE is ignored while serving)');
}

console.log('== revocation');
const list = (await admin({ op: 'devices' })).find(l => l.type === 'devices').devices;
ok(list.length >= 1 && !JSON.stringify(list).includes('pubkey'), 'device list does not expose key material');
const rv = (await admin({ op: 'revoke', id: dev })).find(l => l.type === 'revoked');
ok(rv.ok, 'revoke by id');
await c.until(m => false, 2500); ok(c.closedCode() === 4403, `live socket closed immediately on revoke (code ${c.closedCode()})`);
let again = await wsConnect(phone, dev); await again.until(m => false, 3000); ok(again.closedCode() === 4401, 'revoked device cannot reconnect');

console.log('== rate limiting');
let limited = false;
for (let i = 0; i < 12 && !limited; i++) { const rr = await req('/api/pair', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ secret: 'guess' + i, name: 'x', pubkey: b64u(new Uint8Array(32)) }) }); limited = rr.status === 429; }
ok(limited, 'repeated failures get 429 with lockout');
const retry = await req('/api/hello'); ok(retry.status === 200, 'unrelated endpoints stay reachable (lockout applies to pair/ws only)');

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
