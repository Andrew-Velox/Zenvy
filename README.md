<p align="center"><img src="assets/logo/zenvy-icon.svg" width="96" alt="Zenvy logo"></p>

# Zenvy

Adaptive remote control for Hyprland from your phone. **Early development (Phases 0–4 done).** See [PLAN.md](PLAN.md) for the full design and the verified-vs-assumed status of every claim.

```bash
cargo build
target/debug/zenvyd doctor        # probe capabilities
target/debug/zenvyd state         # monitors / workspaces / windows
target/debug/zenvyd watch         # live (coalesced) Hyprland events
target/debug/zenvyd serve         # HTTPS+WebSocket server (binds the default-route LAN address)
target/debug/zenvyd pair          # QR code + approve the phone on this PC
target/debug/zenvyd devices | revoke <id>
cargo test
```

**Android app:** see [android/README.md](android/README.md) (installed with `adb install`).

`spikes/` holds throwaway Phase 0 experiments (separate workspace); they are not part of the product.

## Install and run

```bash
cargo install --path crates/zenvyd --root ~/.local --locked   # puts `zenvyd` in ~/.local/bin
zenvyd serve                                                  # run in a terminal, or as a service:

mkdir -p ~/.config/systemd/user
cp packaging/systemd/zenvyd.service ~/.config/systemd/user/
systemctl --user enable --now zenvyd                          # starts at login; logs: journalctl --user -u zenvyd
```

Then pair: `zenvyd pair` and scan the QR code in the Android app. The service listens on your LAN address and only authenticated, approved phones can use it; stop it with `systemctl --user disable --now zenvyd`.

## Browser tabs (optional)

Run `zenvyd browser setup` and follow the steps to load `browser-extension/` in Brave or Chrome. The phone's Desk screen then shows a **tabs** chip on browser windows, where you can switch, close or open tabs. Only tab titles and site names are shared, with this PC only (loopback, token protected). Details: [browser-extension/README.md](browser-extension/README.md) and PLAN.md Appendix H.
