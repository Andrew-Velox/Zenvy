# Zenvy for Android

A thin native shell around the Zenvy web UI: certificate-pinned to the PC you paired with, locked to that one origin, installable like any app.

Build and install (JDK 17 + Android SDK, both already set up on the reference machine):

```bash
cd android
JAVA_HOME=/usr/lib/jvm/java-17-openjdk ./gradlew testDebugUnitTest assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

Pair: on the PC run `zenvyd serve`, then `zenvyd pair`, then in the app tap **Scan QR code** (or paste the link). Approve the phone on the PC after checking the 4-digit code matches.

Design and test results: see **Appendix F** of `../PLAN.md`.
