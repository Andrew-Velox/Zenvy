# Reproducible builds

The agent binary does not depend on the directory, user name or time it was built with. Building the same commit twice in two different directories gives byte-identical files (checked: same SHA-256).

```bash
export SOURCE_DATE_EPOCH=1700000000
export RUSTFLAGS="--remap-path-prefix=$PWD=/src --remap-path-prefix=$HOME/.cargo=/cargo --remap-path-prefix=$HOME/.rustup=/rustup -C strip=symbols"
cargo build --release --locked -p zenvyd
sha256sum target/release/zenvyd
```

To verify a release, check out its tag, build as above with the same Rust toolchain version as the release workflow (stable at the time of the release; the version is printed in the workflow log), and compare the hash with the binary inside the release tarball. `Cargo.lock` is committed and `--locked` makes the dependency versions exact.

Limits: the same toolchain and system C compiler are needed (the crypto library `aws-lc` is compiled from C), and the Android APK is not reproducible because it is signed.
