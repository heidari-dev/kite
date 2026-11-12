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
