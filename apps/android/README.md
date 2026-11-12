# Kite for Android

The phone side of Kite: a small app that runs the relay (`crates/kite-relay`, through
`crates/kite-ffi`) in a foreground service. It listens on the abstract socket `kite`, which
`kite connect --usb` reaches through adb. Plain Android Views, no Compose, minSdk 26.

## Build

You need a JDK 17 or newer, the Android SDK with an NDK, Gradle 9.9, Rust with the Android
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

Install the APK, open Kite and tap Start sharing. On the computer, with USB debugging allowed:

```
kite connect --usb
```
