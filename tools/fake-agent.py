#!/usr/bin/env python3
"""A pretend agent for the desktop window: answers the admin socket with demo data, so screenshots and development need no
real agent and show no real devices.   ZENVY_ADMIN_SOCKET=/tmp/zv.sock tools/fake-agent.py /tmp/zv.sock"""
import json, os, socket, sys, threading, time

path = sys.argv[1] if len(sys.argv) > 1 else "/tmp/zenvy-fake.sock"
now = int(time.time())
devices = [
    {"id": "d1", "name": "Pixel 8", "scopes": ["view", "input", "windows", "system", "screen", "launch", "clipboard"], "last_seen": now, "online": True},
    {"id": "d2", "name": "Galaxy Tab S9", "scopes": ["view", "input", "windows", "system", "listen"], "last_seen": now - 5400, "online": False},
]
lock = threading.Lock()

def handle(c):
    f = c.makefile("rw")
    req = json.loads(f.readline() or "{}")
    op = req.get("op")
    out = lambda o: (f.write(json.dumps(o) + "\n"), f.flush())
    if op == "status":
        out({"type": "status", "version": "0.1.0", "address": "https://192.168.1.20:7878", "locked": False, "phones_online": 1, "fingerprint": "AA"})
    elif op == "devices":
        with lock:
            out({"type": "devices", "devices": devices})
    elif op in ("grant", "ungrant"):
        with lock:
            d = next((d for d in devices if d["id"] == req.get("id")), None)
            if d:
                sc = req["scope"]
                if op == "grant" and sc not in d["scopes"]: d["scopes"].append(sc)
                if op == "ungrant" and sc in d["scopes"]: d["scopes"].remove(sc)
        out({"type": "scoped", "ok": d is not None})
    elif op == "revoke":
        with lock:
            n = len(devices); devices[:] = [d for d in devices if d["id"] != req.get("id")]
        out({"type": "revoked", "ok": len(devices) < n})
    elif op == "pair":
        url = "https://192.168.1.20:7878/#s=demo0123456789abcdef&fp=" + "ab" * 32
        out({"type": "pairing", "url": url, "ttl": 300, "fingerprint": "AA"})
        delay = float(os.environ.get("FAKE_APPROVAL_AFTER", "0"))
        if delay:
            time.sleep(delay)
            twin = [{"id": "d0", "last_seen": now - 300, "online": False}] if os.environ.get("FAKE_TWIN") else []
            out({"type": "approval", "name": "Pixel 8", "code": "4815", "same_name": twin})
            ans = json.loads(f.readline() or "{}")
            out({"type": "done", "result": "approved" if ans.get("approve") else "denied"})
        else:
            f.readline()  # wait until the window cancels
    else:
        out({"type": "error", "message": "unknown op"})

try: os.unlink(path)
except FileNotFoundError: pass
s = socket.socket(socket.AF_UNIX); s.bind(path); s.listen(8)
while True:
    c, _ = s.accept()
    threading.Thread(target=lambda c=c: (handle(c), c.close()), daemon=True).start()
