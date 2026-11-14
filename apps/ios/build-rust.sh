#!/usr/bin/env bash
# Builds kite-ffi as a static library for iPhones and the Apple silicon simulator, packs both into
# build/KiteFFI.xcframework and generates the Swift bindings into Kite/FFI, where the Xcode project
# picks them up (they are not committed).
# Needs: macOS with Xcode; rustup targets aarch64-apple-ios and aarch64-apple-ios-sim.
set -euo pipefail
cd "$(dirname "$0")/../.."
ios=apps/ios
# The app's deployment target, so the Rust objects do not ask for a newer iOS than the app.
export IPHONEOS_DEPLOYMENT_TARGET=17.0
for target in aarch64-apple-ios aarch64-apple-ios-sim; do
  cargo rustc -q --release -p kite-ffi --lib --crate-type staticlib --target "$target"
done
# As in apps/android/build-rust.sh: the bindings come from a debug build for this machine, and the
# generator is installed outside the workspace at the version of the uniffi library.
cargo build -q -p kite-ffi --lib
host=target/debug/libkite_ffi.dylib
uniffi=$(sed -n '/^name = "uniffi"$/{n;s/^version = "\(.*\)"$/\1/p;q}' Cargo.lock)
[ -n "$uniffi" ] || { echo "uniffi is not in Cargo.lock" >&2; exit 1; }
root=target/uniffi-bindgen-$uniffi
[ -x "$root/bin/uniffi-bindgen" ] ||
  cargo install uniffi --version "=$uniffi" --locked --features cli --bin uniffi-bindgen --root "$root"
gen=target/ios-bindings
rm -rf "$gen"
mkdir -p "$gen/include" "$ios/Kite/FFI"
"$root/bin/uniffi-bindgen" generate --no-format --library "$host" --language swift --out-dir "$gen"
# An XCFramework's headers need the module map under this name for Swift to import it.
mv "$gen/kite_ffiFFI.h" "$gen/include/"
mv "$gen/kite_ffiFFI.modulemap" "$gen/include/module.modulemap"
mv "$gen/kite_ffi.swift" "$ios/Kite/FFI/"
rm -rf "$ios/build/KiteFFI.xcframework"
xcodebuild -create-xcframework \
  -library target/aarch64-apple-ios/release/libkite_ffi.a -headers "$gen/include" \
  -library target/aarch64-apple-ios-sim/release/libkite_ffi.a -headers "$gen/include" \
  -output "$ios/build/KiteFFI.xcframework"
