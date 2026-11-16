'use strict';
const $ = id => document.getElementById(id);

async function load() {
  const c = await chrome.storage.local.get(['token', 'port', 'enabled', 'status']);
  $('token').value = c.token || '';
  $('port').value = c.port || 7879;
  $('enabled').checked = c.enabled !== false;
  show(c.status);
}

function show(s) {
  $('status').textContent = s || 'Not connected';
  $('status').className = s === 'Connected' ? 'ok' : '';
}

async function save() {
  await chrome.storage.local.set({ token: $('token').value.trim(), port: Number($('port').value) || 7879, enabled: $('enabled').checked });
}

$('save').onclick = save;
chrome.storage.onChanged.addListener((ch) => { if (ch.status) show(ch.status.newValue); });

// Convenience for setup scripts and tests: options.html#token=...&port=...
const h = new URLSearchParams(location.hash.slice(1));
if (h.get('token')) {
  chrome.storage.local.set({ token: h.get('token'), port: Number(h.get('port')) || 7879 }).then(() => history.replaceState(null, '', location.pathname));
}
load();
