# Contributing to Zenvy

Thanks for helping. Zenvy is a Rust agent (`zenvyd`), a phone UI served by the agent, an Android shell and an optional browser extension. [PLAN.md](PLAN.md) is the design and records what was verified; read the relevant appendix before changing a feature.

## Build and test

```bash
cargo build
cargo fmt --all && cargo clippy --workspace --all-targets
cargo test --workspace                       # unit tests, no desktop needed
node tests/typing.mjs && node tests/worklet.mjs
(cd android && ./gradlew testDebugUnitTest)  # JDK 17 + Android SDK
```

The end-to-end suite (`tests/run-e2e.sh`) starts a sandboxed agent against a **real Hyprland session** and uses throwaway scratch windows, fake `systemctl`, `loginctl`, `grim`, `wl-copy` and friends, so it never locks, suspends, captures or types into your real desktop. Run it only through the script, close your own agent first, and do not run it while you are typing: it sends keystrokes to its own scratch window.

## Ground rules

- **Security first.** New message types must check the device's scope, be refused while the PC is locked when they touch the desktop, validate every field, and be added to the e2e tests (including a malformed-input case on a throwaway connection). Never build a shell command from phone input. Never log passwords, clipboard text, file contents or frames.
- **Privacy.** No telemetry, no hardware serials or machine ids in anything sent to the phone or written to a profile.
- **Tests.** A change comes with tests: unit tests for logic, an e2e check for anything the phone can trigger, and a preview check (`crates/zenvy-net/src/page.html` in a browser with `#demo`) for UI.
- **UI.** Icon-led, short labels, check light and dark, keep every control at least 40 px, give every control an accessible name.
- **Small commits** with a message that says what changed and why. Do not commit keys, keystores, `local.properties`, APKs or personal identifiers.

## Reporting bugs

Open an issue with your compositor and version, distro, `zenvyd doctor` output and the steps. For security problems follow [SECURITY.md](SECURITY.md) instead.
