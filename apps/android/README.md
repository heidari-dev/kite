# Kite for Android

The phone side of Kite: a small app that runs the relay (`crates/kite-relay`, through
`crates/kite-ffi`) in a foreground service. It listens on the abstract socket `kite`, which
`kite connect --usb` reaches through adb. Plain Android Views, no Compose, minSdk 26.

## Build

You need a JDK 17 or newer, the Android SDK with NDK r28 or newer (for 16 KB page alignment),
Gradle 9.9, Rust with the Android
targets, and cargo-ndk:

```
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
cargo install cargo-ndk --locked
apps/android/build-rust.sh
cd apps/android && gradle assembleDebug
```

`build-rust.sh` puts `libkite_ffi.so` for arm64-v8a, armeabi-v7a and x86_64 under
`app/src/main/jniLibs` and the generated Kotlin bindings under `app/src/main/java/.../ffi`. Both
are build outputs and ignored by git. CI builds the same debug APK and attaches it to every run.

## Try it

Install the APK, open Kite and tap Start sharing. The first time, tap Pair a computer and, on the
computer with USB debugging allowed, run `kite pair --usb`. The phone asks whether to allow the
computer and shows its fingerprint, which `kite pair` printed too; tap Allow, then type the code
the phone shows. Paired computers are listed on the screen and can be removed there. Then:

```
kite connect --usb
```

The relay's private key is stored in the app's no-backup directory, encrypted with an AES key that
stays in Android Keystore; the paired computers' public keys are stored next to it.

## The screen

One column: the kite, a line saying what it is doing, the live speed, Start or Stop, and pairing.
The kite follows [docs/design/motion.md](../../docs/design/motion.md): each layer is a vector
drawable in its own `ImageView` (`res/layout/view_kite.xml`), moved with property animators, and
the numbers live in `KiteMotion.kt`. It rests while sharing is off, leans back and wobbles while it
waits for a computer, flies upright when one is connected and flutters its tail faster with
throughput, and falls and shows a badge when the relay cannot start. The loops run from one frame
callback at most 30 times a second and stop when the app is not in front or the screen is off.
With animations turned off in the system settings (animator duration scale 0) it fades between
states instead of moving. Light and dark follow the system.

## Screenshots

The `Android screens` workflow (push to main, or run it by hand) boots an emulator, installs the debug
APK and saves screenshots of the main screen in light and dark: stopped, waiting for a computer and
pairing open. They are the `android-screens` artifact of the run. `scripts/android-screens.sh` does
the driving. Screenshots show a secure window black, so the script starts a debuggable build with
`--ez screenshots true`, which skips `FLAG_SECURE`; release builds never do.
