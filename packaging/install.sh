#!/usr/bin/env bash
# Installs Zenvy for the current user (never as root, nothing outside your home directory).
#
#   packaging/install.sh [--prefix DIR] [--no-service] [--no-gui] [--enable] [--build]
#
# It uses a prebuilt `zenvyd` next to this script (release tarball) or builds one from the source tree with cargo.
#   --prefix DIR   install under DIR/bin (default: ~/.local)
#   --no-service   do not install the systemd user unit
#   --no-gui       do not install the desktop window (`zenvy`), its launcher entry and icon
#   --enable       also enable and start the service now
#   --build        always build from source, even if a prebuilt binary is present
set -euo pipefail

prefix="${HOME}/.local"; service=1; gui=1; enable=0; build=0
while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) prefix="${2:?--prefix needs a directory}"; shift 2 ;;
    --no-service) service=0; shift ;;
    --no-gui) gui=0; shift ;;
    --enable) enable=1; shift ;;
    --build) build=1; shift ;;
    -h|--help) sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done
if [ "$(id -u)" -eq 0 ]; then echo "Run this as your normal user, not root." >&2; exit 1; fi

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ -x "$here/zenvyd" ] && [ "$build" -eq 0 ]; then
  bin="$here/zenvyd"
elif [ -f "$here/../Cargo.toml" ]; then
  command -v cargo >/dev/null || { echo "cargo is needed to build from source (https://rustup.rs)." >&2; exit 1; }
  echo "Building zenvyd (release)..."
  (cd "$here/.." && cargo build --release --locked -p zenvyd)
  bin="$here/../target/release/zenvyd"
  if [ "$gui" -eq 1 ]; then
    echo "Building the desktop window (release)..."
    (cd "$here/.." && cargo build --release --locked -p zenvy-gui) && gui_bin="$here/../target/release/zenvy" || echo "The desktop window did not build; continuing without it."
  fi
else
  echo "No prebuilt zenvyd next to this script and no source tree to build." >&2; exit 1
fi

install -d "$prefix/bin"
install -m 0755 "$bin" "$prefix/bin/zenvyd"
echo "Installed $prefix/bin/zenvyd"

# the desktop window: a prebuilt `zenvy` next to this script, or the one just built
[ -z "${gui_bin:-}" ] && [ -x "$here/zenvy" ] && gui_bin="$here/zenvy"
if [ "$gui" -eq 1 ] && [ -n "${gui_bin:-}" ]; then
  data="${XDG_DATA_HOME:-$HOME/.local/share}"
  install -m 0755 "$gui_bin" "$prefix/bin/zenvy"
  install -d "$data/applications" "$data/icons/hicolor/512x512/apps"
  sed "s#^Exec=.*#Exec=$prefix/bin/zenvy#" "$here/zenvy.desktop" > "$data/applications/zenvy.desktop"
  [ -f "$here/../assets/logo/zenvy-icon-512.png" ] && install -m 0644 "$here/../assets/logo/zenvy-icon-512.png" "$data/icons/hicolor/512x512/apps/zenvy.png"
  [ -f "$here/zenvy-icon-512.png" ] && install -m 0644 "$here/zenvy-icon-512.png" "$data/icons/hicolor/512x512/apps/zenvy.png"
  echo "Installed $prefix/bin/zenvy (the desktop window) and its launcher entry"
fi

if [ "$service" -eq 1 ]; then
  unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
  install -d "$unit_dir"
  # the packaged unit points at /usr/bin; a user install points at the chosen prefix
  sed "s#^ExecStart=.*#ExecStart=$prefix/bin/zenvyd serve#" "$here/systemd/zenvyd.service" > "$unit_dir/zenvyd.service"
  echo "Installed $unit_dir/zenvyd.service"
  if command -v systemctl >/dev/null; then
    systemctl --user daemon-reload || true
    if [ "$enable" -eq 1 ]; then
      systemctl --user enable --now zenvyd.service
      echo "Service enabled and started (logs: journalctl --user -u zenvyd)."
    else
      echo "Start it at every login with:  systemctl --user enable --now zenvyd"
    fi
  fi
fi

echo
"$prefix/bin/zenvyd" doctor 2>/dev/null | sed -n '1,40p' || true
cat <<MSG

Next: open Zenvy from your launcher (or run  zenvy ) and tap Pair a phone, or run  zenvyd pair  in a terminal,
then scan the QR code with the Zenvy Android app.
Optional tools (install with your package manager): grim (screen preview), wl-clipboard (clipboard),
pipewire-pulse / pulseaudio-utils (volume and phone speaker), libnotify (desktop notices), ddcutil (brightness).
Uninstall:  $here/uninstall.sh   (add --purge to also delete paired phones and settings)
MSG
