# Zenvy for Android

A thin native shell around the Zenvy web UI: certificate-pinned to the PC you paired with, locked to that one origin, installable like any app.

Build and install (JDK 17 + Android SDK; set `JAVA_HOME` to the JDK 17, for example `/usr/lib/jvm/java-17-openjdk` on Arch):

```bash
cd android
JAVA_HOME=/usr/lib/jvm/java-17-openjdk ./gradlew testDebugUnitTest assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

Pair: on the PC run `zenvyd serve`, then `zenvyd pair`, then in the app tap **Scan QR code** (or paste the link). Approve the phone on the PC after checking the 4-digit code matches.

Design and test results: see **Appendix F** of `../PLAN.md`.

## Release build

```bash
cd android
JAVA_HOME=/usr/lib/jvm/java-17-openjdk ./gradlew assembleRelease     # unsigned unless a keystore is given
```

The release build is minified (R8) and, without a keystore, produces `app-release-unsigned.apk`, which cannot be installed. To sign it, create **your own** keystore once (keep it and its passwords out of the repository and back it up: an app can only be updated with the key it was first signed with):

```bash
keytool -genkeypair -v -keystore ~/zenvy-release.jks -alias zenvy -keyalg RSA -keysize 4096 -validity 9125
```

then point the build at it with environment variables (nothing is written to a file in the repository):

```bash
export ZENVY_KEYSTORE=~/zenvy-release.jks
export ZENVY_KEYSTORE_PASSWORD=...        # also ZENVY_KEY_ALIAS (default "zenvy") and ZENVY_KEY_PASSWORD (default: the keystore password)
JAVA_HOME=/usr/lib/jvm/java-17-openjdk ./gradlew assembleRelease
adb install -r app/build/outputs/apk/release/app-release.apk
```

A release APK is signed with a different key than the debug one, so Android will ask you to uninstall the debug build first (you will have to pair again).

## What the app adds to the web UI

Pinned-certificate loading of the paired PC only, the QR scanner (Google code scanner: no camera permission), the "Share to Zenvy" target (text and links go to the PC), the file picker for "Send files", and a downloader for files the PC offers that trusts only the pinned certificate and saves into `Downloads/Zenvy` (no storage permission).
