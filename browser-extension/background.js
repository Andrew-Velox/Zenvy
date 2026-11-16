// Zenvy Browser Bridge: reports tab titles and site names to the Zenvy agent on this computer
// (ws://127.0.0.1, token required) and executes three commands from it: activate, close, new tab.
// It never sends full addresses, page content, cookies or history, and it sees no incognito
// windows unless you explicitly allow the extension there. Site icons are read from the browser's
// own icon cache (no network request is made for them) and sent once per site, as small images.
'use strict';

const DEFAULT_PORT = 7879;
let ws = null;
let sendTimer = null;
let retries = 0;
let browserName = 'chrome';

(async () => {
  try { if (navigator.brave && (await navigator.brave.isBrave())) browserName = 'brave'; } catch (_) {}
})();

async function config() {
  const c = await chrome.storage.local.get(['token', 'port', 'enabled']);
  return { token: (c.token || '').trim(), port: Number(c.port) || DEFAULT_PORT, enabled: c.enabled !== false };
}

async function setStatus(status) {
  await chrome.storage.local.set({ status, statusAt: Date.now() });
}

/** Host name only: no path, query, fragment or credentials ever leave the browser. */
function hostOf(url) {
  try {
    const u = new URL(url || '');
    if (u.protocol === 'http:' || u.protocol === 'https:') return u.hostname.replace(/^www\./, '');
    return u.protocol.replace(':', '') + ' page';
  } catch (_) { return ''; }
}

async function snapshot() {
  const [tabs, focused] = await Promise.all([chrome.tabs.query({}), chrome.windows.getLastFocused().catch(() => null)]);
  return {
    t: 'tabs',
    data: {
      focused_window: focused ? focused.id : null,
      tabs: tabs.filter(t => t.id !== undefined && t.id >= 0).map(t => ({
        id: t.id, windowId: t.windowId, title: (t.title || '').slice(0, 160), host: hostOf(t.url),
        active: !!t.active, pinned: !!t.pinned, audible: !!t.audible,
      })),
    },
  };
}

// ---- site icons: the browser's own favicon cache, via the `favicon` permission (no network request)
const FAV_MAX_BYTES = 12000;
const favSent = new Set(); // hosts already handled on this connection
let favBusy = false;

async function faviconData(origin) {
  const u = new URL(chrome.runtime.getURL('/_favicon/'));
  u.searchParams.set('pageUrl', origin);
  u.searchParams.set('size', '32');
  const r = await fetch(u);
  if (!r.ok) return null;
  const type = (r.headers.get('content-type') || '').split(';')[0].trim();
  if (!/^image\/(png|jpeg|webp|gif|x-icon|vnd\.microsoft\.icon)$/.test(type)) return null;
  const bytes = new Uint8Array(await r.arrayBuffer());
  if (!bytes.length || bytes.length > FAV_MAX_BYTES) return null;
  let bin = '';
  for (const b of bytes) bin += String.fromCharCode(b);
  return `data:${type};base64,${btoa(bin)}`;
}

async function sendFavicons() {
  if (favBusy || !ws || ws.readyState !== WebSocket.OPEN) return;
  favBusy = true;
  try {
    const tabs = await chrome.tabs.query({});
    const want = new Map(); // host -> origin (the page address itself never leaves the browser)
    for (const t of tabs) {
      let u; try { u = new URL(t.url || ''); } catch (_) { continue; }
      if (u.protocol !== 'http:' && u.protocol !== 'https:') continue;
      const host = hostOf(t.url);
      if (host && !favSent.has(host) && !want.has(host)) want.set(host, u.origin + '/');
    }
    const icons = {};
    let n = 0;
    for (const [host, origin] of want) {
      if (n++ >= 12) break;
      favSent.add(host);
      try { const d = await faviconData(origin); if (d) icons[host] = d; } catch (_) { /* no icon for this site */ }
    }
    if (Object.keys(icons).length && ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ t: 'favicons', data: { icons } }));
    if (want.size > 12) setTimeout(sendFavicons, 400); // the rest, in small batches
  } finally { favBusy = false; }
}

function scheduleSend() {
  if (sendTimer) return;
  sendTimer = setTimeout(async () => {
    sendTimer = null;
    if (ws && ws.readyState === WebSocket.OPEN) { ws.send(JSON.stringify(await snapshot())); sendFavicons(); }
  }, 150);
}

const int = v => (Number.isInteger(v) && v >= 0 ? v : null);

async function handle(msg) {
  try {
    if (msg.t === 'activate') {
      const tab = int(msg.tab_id), win = int(msg.window_id);
      if (tab === null) return;
      await chrome.tabs.update(tab, { active: true });
      if (win !== null) await chrome.windows.update(win, { focused: true });
    } else if (msg.t === 'close') {
      const tab = int(msg.tab_id);
      if (tab !== null) await chrome.tabs.remove(tab);
    } else if (msg.t === 'new') {
      const win = int(msg.window_id);
      await chrome.tabs.create(win !== null ? { windowId: win } : {});
    }
  } catch (_) { /* the tab may already be gone */ }
}

async function connect() {
  if (ws && (ws.readyState === WebSocket.CONNECTING || ws.readyState === WebSocket.OPEN)) return;
  const c = await config();
  if (!c.enabled) { await setStatus('Turned off'); return; }
  if (!c.token) { await setStatus('Not set up: paste the token from `zenvyd browser setup`'); return; }
  let sock;
  try { sock = new WebSocket(`ws://127.0.0.1:${c.port}/browser`); } catch (_) { return; }
  ws = sock;
  sock.onopen = () => { favSent.clear(); sock.send(JSON.stringify({ t: 'hello', token: c.token, browser: browserName, version: chrome.runtime.getManifest().version })); };
  sock.onmessage = async e => {
    let m; try { m = JSON.parse(e.data); } catch (_) { return; }
    if (m.t === 'ready') { retries = 0; await setStatus('Connected'); scheduleSend(); }
    else if (m.t === 'error') { await setStatus('The agent refused the token'); }
    else await handle(m);
  };
  sock.onclose = async () => {
    if (ws === sock) ws = null;
    await setStatus('Waiting for the Zenvy agent');
    retries = Math.min(retries + 1, 5);
    setTimeout(connect, Math.min(1000 * 2 ** retries, 15000));
  };
  sock.onerror = () => {};
}

for (const ev of [chrome.tabs.onCreated, chrome.tabs.onRemoved, chrome.tabs.onActivated, chrome.tabs.onMoved,
                  chrome.tabs.onAttached, chrome.tabs.onDetached, chrome.tabs.onReplaced, chrome.windows.onFocusChanged,
                  chrome.windows.onRemoved]) ev.addListener(scheduleSend);
chrome.tabs.onUpdated.addListener((_id, info) => {
  if ('title' in info || 'url' in info || 'audible' in info || 'pinned' in info || info.status === 'complete') scheduleSend();
});
// Reconnect only when the *settings* change. The status line lives in the same storage area, and
// reacting to it would close and reopen the connection every time it connects (a reconnect loop).
chrome.storage.onChanged.addListener((changes, area) => {
  if (area !== 'local' || !('token' in changes || 'port' in changes || 'enabled' in changes)) return;
  if (ws) ws.close();
  connect();
});

// A Manifest V3 service worker can be stopped at any time: an alarm wakes it and reconnects.
chrome.alarms.create('zenvy-keepalive', { periodInMinutes: 0.5 });
chrome.alarms.onAlarm.addListener(a => { if (a.name === 'zenvy-keepalive') connect(); });
chrome.runtime.onStartup.addListener(connect);
chrome.runtime.onInstalled.addListener(connect);
chrome.action.onClicked.addListener(() => chrome.runtime.openOptionsPage());
connect();
