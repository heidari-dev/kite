#!/usr/bin/env bash
# Builds libkite_ffi.so for the app's ABIs and generates its Kotlin bindings, both into
# app/src/main where the Android build picks them up (they are not committed).
# Needs: rustup targets aarch64-linux-android, armv7-linux-androideabi, x86_64-linux-android;
# cargo-ndk; ANDROID_NDK_HOME pointing at an NDK.
set -euo pipefail
cd "$(dirname "$0")/../.."
main=apps/android/app/src/main
cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 --platform 26 -o "$main/jniLibs" \
  build --release -p kite-ffi --lib
# The release libraries are stripped, which removes the metadata uniffi reads, so the bindings come
# from a debug build for this machine. The interface is the same on every target.
cargo build -q -p kite-ffi --lib
for host in target/debug/libkite_ffi.so target/debug/libkite_ffi.dylib target/debug/kite_ffi.dll; do
  [ -f "$host" ] && break
done
# The generator is installed outside the workspace so that its dependencies stay out of Cargo.lock.
# Its version must be the one of the uniffi library, so it is read from Cargo.lock.
uniffi=$(sed -n '/^name = "uniffi"$/{n;s/^version = "\(.*\)"$/\1/p;q}' Cargo.lock)
[ -n "$uniffi" ] || { echo "uniffi is not in Cargo.lock" >&2; exit 1; }
root=target/uniffi-bindgen-$uniffi
[ -x "$root/bin/uniffi-bindgen" ] ||
  cargo install uniffi --version "=$uniffi" --locked --features cli --bin uniffi-bindgen --root "$root"
"$root/bin/uniffi-bindgen" generate --no-format \
  --library "$host" --language kotlin --out-dir "$main/java"
