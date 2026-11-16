#!/usr/bin/env bash
# Runs the end-to-end suite against a SANDBOX agent: loopback only, temporary data dirs, a private D-Bus, a fake
# `pactl` and a fake MPRIS player. It starts and stops only the processes it launched, so a real agent is never touched.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
if pgrep -x zenvyd >/dev/null; then
  echo "A zenvyd agent is already running. Stop it first (e.g. pkill -x zenvyd): it would share the browser-bridge port"
  echo "and the virtual input device names with the sandbox agent, and the results would be meaningless."
  exit 3
fi
TMP="$(mktemp -d)"; PIDS=()
cleanup() { for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done; sleep 0.4; for p in "${PIDS[@]}"; do kill -9 "$p" 2>/dev/null; done; rm -rf "$TMP"; }
trap cleanup EXIT

( cd "$ROOT" && cargo build -q -p zenvyd && cargo build -q -p zenvy-media --example fake_player ) || { echo "build failed"; exit 2; }

mkdir -p "$TMP"/{cfg/zenvy,data,state,fake,bin}
printf '[server]\nbind = ["127.0.0.1"]\nport = 7888\n[browser]\nport = 7889\n[lock]\nprocesses = []\nuse_compositor = false\npower_delay_seconds = 1\n' > "$TMP/cfg/zenvy/config.toml"   # its own bridge port, so a real browser extension (7879) can never interfere
cat > "$TMP/fake/audio.json" <<J
{"default_sink":"fake_speakers","default_source":"fake_mic",
 "sinks":[{"name":"fake_speakers","description":"Fake Speakers","mute":false,"volume":40},{"name":"fake_hdmi","description":"Fake HDMI","mute":false,"volume":100}],
 "sources":[{"name":"fake_mic","description":"Fake Microphone","mute":false,"volume":70}]}
J
: > "$TMP/fake/events"
cp "$ROOT/tests/fakes/pactl" "$TMP/bin/pactl"
cp "$ROOT/tests/fakes/ddcutil" "$TMP/bin/ddcutil"
for f in loginctl systemctl busctl wl-copy wl-paste xdg-open; do cp "$ROOT/tests/fakes/$f" "$TMP/bin/$f"; done
cp "$ROOT/tests/fakes/parec" "$TMP/bin/parec"; cp "$ROOT/tests/fakes/notify-send" "$TMP/bin/notify-send"
echo '{"ddc":[{"bus":2,"connector":"card1-HDMI-A-1","model":"Fake Monitor","serial":"SERIAL-SECRET-999","cur":50,"max":100}]}' > "$TMP/fake/bright.json"

# custom actions: harmless fixed commands (touch a marker file / fail), never a shell line
cat >> "$TMP/cfg/zenvy/config.toml" <<ACT
[[actions]]
id = "mark"
label = "Mark"
argv = ["touch", "$TMP/fake/action-mark"]
[[actions]]
id = "danger"
label = "Danger"
confirm = true
argv = ["touch", "$TMP/fake/action-danger"]
[[actions]]
id = "broken"
label = "Broken"
argv = ["false"]
ACT

# fake application entries and icons (the agent sees only these: XDG_DATA_DIRS is pointed at the sandbox)
mkdir -p "$TMP/data/applications" "$TMP/data/icons/hicolor/64x64/apps"
printf '\x89PNG\r\n\x1a\nFAKE-ICON-BYTES' > "$TMP/data/icons/hicolor/64x64/apps/zenvyfake.png"
printf '[Desktop Entry]\nName=Fake\nIcon=zenvyfake\n' > "$TMP/data/applications/zenvy-fake-app.desktop"
printf '[Desktop Entry]\nName=Evil\nIcon=/etc/passwd\n' > "$TMP/data/applications/zenvy-evil.desktop"
printf 'not an image, a secret' > "$TMP/fake/notimage.png"
printf '[Desktop Entry]\nName=Evil2\nIcon=%s\n' "$TMP/fake/notimage.png" > "$TMP/data/applications/zenvy-evil2.desktop"
printf '[Desktop Entry]\nName=Evil3\nIcon=../../../../../../etc/passwd\n' > "$TMP/data/applications/zenvy-evil3.desktop"

# private session bus (agent + fake player only)
dbus-daemon --session --nofork --print-address=3 3>"$TMP/dbus.addr" >/dev/null 2>&1 &
PIDS+=($!)
for _ in $(seq 1 50); do [ -s "$TMP/dbus.addr" ] && break; sleep 0.1; done
BUS="$(cat "$TMP/dbus.addr")"   # given only to the agent and the fake player, never exported to other processes

DBUS_SESSION_BUS_ADDRESS="$BUS" FAKE_PLAYER_LOG="$TMP/fake/player.log" "$ROOT/target/debug/examples/fake_player" >/dev/null 2>&1 &
PIDS+=($!)
sleep 1

DBUS_SESSION_BUS_ADDRESS="$BUS" PATH="$TMP/bin:$PATH" ZENVY_FAKE_DIR="$TMP/fake" XDG_CONFIG_HOME="$TMP/cfg" XDG_DATA_HOME="$TMP/data" XDG_DATA_DIRS="$TMP/data" XDG_STATE_HOME="$TMP/state" \
  "$ROOT/target/debug/zenvyd" serve >"$TMP/server.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 1 50); do curl -sk -m 1 https://127.0.0.1:7888/api/hello >/dev/null 2>&1 && break; sleep 0.2; done
for _ in $(seq 1 50); do (exec 3<>/dev/tcp/127.0.0.1/7889) 2>/dev/null && break; sleep 0.2; done   # the browser bridge too

ZENVY_AUDIT="$TMP/state/zenvy/audit.log" ZENVY_BROWSER_PORT=7889 ZENVY_DATA="$TMP/data/zenvy" ZENVY_FAKE_DIR="$TMP/fake" node "$ROOT/tests/e2e.mjs" 2>&1 | grep -v "NODE_TLS_REJECT_UNAUTHORIZED\|trace-warnings\|^(Use"
RC=${PIPESTATUS[0]}
echo "--- agent startup:"; grep -E "audio control|media control|input injection|browser bridge|power control|unlock from phone" "$TMP/server.log"
[ "$RC" -ne 0 ] && { echo "--- agent log (last lines):"; tail -15 "$TMP/server.log"; echo "--- audit log (browser events):"; grep -E "browser_" "$TMP/state/zenvy/audit.log" | cut -c1-160 | head -20; echo "--- brave processes using test profiles:"; ps -eo pid,etimes,args | grep -E "[z]envy-brave" | cut -c1-120 | head -5; }
exit "$RC"
