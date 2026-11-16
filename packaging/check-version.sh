#!/usr/bin/env bash
# Checks that every version number that ships agrees with the release tag, e.g.  packaging/check-version.sh v0.1.0
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
want="${1:?usage: check-version.sh vX.Y.Z}"; want="${want#v}"
ws="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
apk="$(sed -n 's/.*versionName = "\(.*\)"/\1/p' android/app/build.gradle.kts | head -1)"
pkg="$(sed -n 's/^pkgver=\(.*\)/\1/p' packaging/arch/PKGBUILD | head -1)"
bin="$(grep -m1 -A0 '^## ' CHANGELOG.md | sed 's/^## \([0-9.]*\).*/\1/')"
bad=0
for pair in "Cargo workspace:$ws" "Android versionName:$apk" "Arch PKGBUILD pkgver:$pkg" "CHANGELOG top entry:$bin"; do
  name="${pair%%:*}"; got="${pair#*:}"
  if [ "$got" = "$want" ]; then echo "ok   $name = $got"; else echo "FAIL $name = '$got', expected $want"; bad=1; fi
done
exit $bad
