# Releasing Zenvy

A release is a tag. The `Release` workflow (`.github/workflows/release.yml`) builds everything from that tag and makes a **draft** GitHub release, so nothing is public until you review it and press **Publish**.

## One-time setup: your signing key

An Android app can only ever be updated by an APK signed with the same key, so the key is yours alone: create it once, keep a backup somewhere safe (not in the repository), and never share it.

```bash
keytool -genkeypair -v -keystore ~/zenvy-release.jks -alias zenvy -keyalg RSA -keysize 4096 -validity 9125
base64 -w0 ~/zenvy-release.jks > /tmp/zenvy-release.jks.b64      # for the GitHub secret below; delete it afterwards
```

In the GitHub repository open **Settings → Secrets and variables → Actions** and add:

| Secret | Value |
| --- | --- |
| `ZENVY_KEYSTORE_BASE64` | the contents of `zenvy-release.jks.b64` |
| `ZENVY_KEYSTORE_PASSWORD` | the keystore password |
| `ZENVY_KEY_ALIAS` | `zenvy` (optional, this is the default) |
| `ZENVY_KEY_PASSWORD` | the key password (optional, defaults to the keystore password) |

Without these secrets the workflow still builds and releases the agent; it just leaves the Android app out.

Also turn on **Settings → Code security → Private vulnerability reporting**, which is where `SECURITY.md` sends reporters.

## Cutting a release

1. Update the version in `Cargo.toml` (`[workspace.package]` and every crate that sets its own), `android/app/build.gradle.kts` (`versionName`, and raise `versionCode`), `packaging/arch/PKGBUILD` (`pkgver`) and the top entry of `CHANGELOG.md`.
2. `packaging/check-version.sh vX.Y.Z` must print only `ok` lines.
3. `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`, then the end-to-end suite on your own desktop (`tests/run-e2e.sh`).
4. Commit, then `git tag vX.Y.Z && git push origin main vX.Y.Z`.
5. Wait for the workflow, open the draft release, check the files and notes, and publish.

## What a release contains

- `zenvy-X.Y.Z-x86_64-linux.tar.gz`: `zenvyd`, `packaging/` (install and uninstall scripts, systemd unit), the browser extension, licence and docs.
- `zenvy-X.Y.Z.apk`: the Android app signed with your key.
- `SHA256SUMS`, and signed build provenance (verify with `gh attestation verify <file> -R <owner>/Zenvy`) once the repository is public; GitHub does not offer attestations for private repositories on free accounts, so the step is skipped while it is private.

The agent is built reproducibly (see [REPRODUCIBLE.md](REPRODUCIBLE.md)): anyone can rebuild the tag and get the same bytes.

## Arch (AUR)

After publishing the tag, set `sha256sums` in `packaging/arch/PKGBUILD` to the checksum of the GitHub source tarball (`updpkgsums`), test with `makepkg -f`, and push it to the AUR as `zenvy`.
