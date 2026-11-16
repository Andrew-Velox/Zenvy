# Reproducible builds

The agent binary does not depend on the directory, user name or time it was built with: building the same commit twice in two different directories on the same machine gives byte-identical files (checked, same SHA-256).

```bash
export SOURCE_DATE_EPOCH=1700000000
export RUSTFLAGS="--remap-path-prefix=$PWD=/src --remap-path-prefix=$HOME/.cargo=/cargo --remap-path-prefix=$HOME/.rustup=/rustup -C strip=symbols"
cargo build --release --locked -p zenvyd
sha256sum target/release/zenvyd
```

`Cargo.lock` is committed and `--locked` makes the dependency versions exact. The Rust toolchain is pinned in `.github/workflows/release.yml` (currently 1.98.1).

## What is and is not reproducible

- **Same environment, different directory: yes.** This is what the flags above guarantee.
- **Different distribution: no.** The TLS library (`aws-lc`) is compiled from C with the system compiler and the binary links the system C library, so a build on Arch and a build on the Ubuntu release runner differ byte for byte even with the same Rust toolchain (checked for 0.1.0). To verify a release exactly, rebuild in the runner's environment (Ubuntu 24.04 with the packages listed in the workflow).
- **The Android APK: no.** It is signed.

For everyday integrity use `SHA256SUMS` from the release, and the signed build provenance once the repository is public.

## What the prebuilt binary needs

The release is built on Ubuntu 24.04, so `zenvyd` needs glibc 2.39 or newer and `libopus`. On an older system build from source instead (`packaging/install.sh` does that when no prebuilt binary is present). Wider compatibility is part of the distro matrix, which is not done yet.
