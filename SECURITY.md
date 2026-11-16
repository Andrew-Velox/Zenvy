# Security policy

Zenvy lets a paired phone type, click, close windows, start programs, hear and see the PC, move files and, if you allow it, unlock the screen. A flaw in it can be serious, so please report problems privately.

## Reporting a vulnerability

Use GitHub's private reporting: open the repository's **Security** tab and choose **Report a vulnerability**. Please do not open a public issue or pull request for a security problem before it is fixed.

Include what you did, what you expected and what happened, the Zenvy version (`zenvyd --version`) and your compositor. A short proof of concept helps. You will get an answer within a week; fixes for confirmed problems are released as soon as they are tested, and you are credited in the changelog unless you prefer not to be.

## Supported versions

Only the latest release gets security fixes.

## What Zenvy promises

- **Phones are paired, not trusted by network position.** A phone holds its own Ed25519 key (it never leaves the phone). Pairing needs a one-time secret from the QR code **and** your approval on the PC after comparing a 4-digit code. Every connection then signs a fresh challenge, so there is no password or token to steal off the network.
- **The phone pins the PC.** The Android app only talks to the certificate fingerprint it paired with and stays on that one origin.
- **Least privilege per phone.** A new phone only gets `view`, `input`, `windows` and `system`. Listening, screen preview, clipboard, files, launching apps, running configured actions, power and unlock are separate permissions you grant on the PC (`zenvyd grant`), and you can remove a phone at any time (`zenvyd revoke`).
- **A locked PC stays locked.** While the screen is locked the agent refuses input, window, browser, clipboard, file, app and screen requests and hides window titles. The password is only typed (never logged, stored or echoed) when you ask for it and the PC is locked.
- **No shell, no free-form commands.** Phones send typed commands or ids; the agent looks the real command line up in its own configuration or desktop entries and runs it as an argument vector.
- **Small, local footprint.** No telemetry, no account, no cloud. The agent runs as your user (never root), listens on one LAN address and applies size, rate and Host/Origin limits.
- **Sensitive actions leave a trace.** Pairing, permission changes, power actions, launching apps and screen or speaker sessions are audit-logged and shown as a desktop notification where relevant. Frames, files, clipboard contents and passwords are never written to the log.

## Known limits

- Anyone on your network can reach the pairing page, but cannot do anything without the one-time secret and your approval. Pair on a network you trust and stop `zenvyd pair` when you are done.
- Any program running as your user can already inject input through `/dev/uinput`; Zenvy does not widen that, and it never edits udev rules for you.
- The agent is a user-session service, so it cannot reach the login screen before you log in.
- Release builds are signed by the maintainer's key only when you install a release APK; a build you made yourself is signed with your key.

The full threat model and the verified-versus-assumed status of each claim are in [PLAN.md](PLAN.md) (sections 9 and 14 and the appendices).
