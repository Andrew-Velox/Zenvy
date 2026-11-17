# Changelog

## Unreleased

- **Zenvy window**: a small desktop app (`zenvy`) to pair a phone with a QR code and approve it, see which phones are connected, and switch each phone's permissions (sensitive ones ask first). It only talks to the agent's local admin socket. `zenvy --pair` opens straight to the QR code.
- The agent's admin socket reports connected phones and a status summary (`online`, `status`).
- Android: a quick reachability check, a "Connecting" screen and automatic retry instead of a stale browser error page, and redesigned connection and pairing screens.

## 0.1.0

First public release.

- Touchpad, keyboard (live typing, shortcuts, special keys), windows and workspaces, browser tabs (extension), media, volume, brightness, phone speaker, clipboard and links, file transfer, screen preview with tap to click (a floating window on every tab), custom actions, power and lock control with unlock from the phone, app launcher (open on a new empty workspace), move a window to another workspace.
- Pairing by QR code with explicit approval, per-device keys, certificate pinning in the Android app, per-feature permissions, locked-PC lockdown.
- Android shell (certificate pinned, file chooser, downloads, share target) and Brave/Chrome tab extension.
- Hardening for release: caps on simultaneous sockets (per address and in total), a header-read timeout against slow-loris clients, file names stripped of bidirectional and zero-width characters and limited to the file-system byte limit, randomized robustness tests for every phone-facing parser.
- Packaging: systemd user unit (stopping the agent does not close apps you started), user-level install and uninstall scripts, Arch PKGBUILD, reproducible agent builds, release workflow with checksums and signed provenance.
