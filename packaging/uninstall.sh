#!/usr/bin/env bash
# Removes what packaging/install.sh installed. Add --purge to also delete Zenvy's data: paired phones, the
# certificate, settings and the audit log. Your files in ~/Downloads/Zenvy are never touched.
set -euo pipefail

prefix="${HOME}/.local"; purge=0
while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) prefix="${2:?--prefix needs a directory}"; shift 2 ;;
    --purge) purge=1; shift ;;
    -h|--help) sed -n '2,4p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done
if [ "$(id -u)" -eq 0 ]; then echo "Run this as your normal user, not root." >&2; exit 1; fi

if command -v systemctl >/dev/null; then
  systemctl --user disable --now zenvyd.service 2>/dev/null || true
fi
unit="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/zenvyd.service"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
rm -f "$unit" "$prefix/bin/zenvyd" "$prefix/bin/zenvy" "$data/applications/zenvy.desktop" "$data/icons/hicolor/512x512/apps/zenvy.png"
command -v systemctl >/dev/null && systemctl --user daemon-reload 2>/dev/null || true
echo "Removed the zenvyd program, the desktop window and the service."

if [ "$purge" -eq 1 ]; then
  rm -rf "${XDG_DATA_HOME:-$HOME/.local/share}/zenvy" \
         "${XDG_STATE_HOME:-$HOME/.local/state}/zenvy" \
         "${XDG_CONFIG_HOME:-$HOME/.config}/zenvy"
  [ -n "${XDG_RUNTIME_DIR:-}" ] && rm -rf "$XDG_RUNTIME_DIR/zenvy"
  echo "Deleted paired phones, certificate, settings and audit log."
else
  echo "Kept your paired phones and settings (run again with --purge to delete them)."
fi
echo "Not touched: ~/Downloads/Zenvy (received files), the Android app and the browser extension (remove those yourself)."
