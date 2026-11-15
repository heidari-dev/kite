#!/usr/bin/env bash
# Builds kite and kite-desktop for Apple silicon and Intel and joins each pair into one universal
# binary in target/universal. Run it on a Mac, from the repository root.
set -euo pipefail

targets=(aarch64-apple-darwin x86_64-apple-darwin)
rustup target add "${targets[@]}"
for t in "${targets[@]}"; do
  cargo build --release --locked -p kite-cli -p kite-desktop --target "$t"
done
mkdir -p target/universal
for b in kite kite-desktop; do
  lipo -create -output "target/universal/$b" \
    "target/${targets[0]}/release/$b" "target/${targets[1]}/release/$b"
done
lipo -info target/universal/kite target/universal/kite-desktop
