#!/usr/bin/env bash
# Builds the macOS installer, an unsigned .pkg, from the release binaries.
#
# Usage: scripts/package-macos.sh VERSION BIN_DIR OUT_DIR
# BIN_DIR holds kite and kite-desktop, built for the architectures to support (a universal
# binary: see the release workflow). Needs the tools of macOS: pkgbuild, productbuild, codesign.
set -euo pipefail

version=${1:?usage: package-macos.sh VERSION BIN_DIR OUT_DIR}
bin=${2:?usage: package-macos.sh VERSION BIN_DIR OUT_DIR}
out=${3:?usage: package-macos.sh VERSION BIN_DIR OUT_DIR}
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] || { echo "bad version: $version" >&2; exit 1; }

root=$(cd "$(dirname "$0")/.." && pwd)
pkg=$root/packaging/macos
id=io.github.heidari-dev.kite
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$out"
out=$(cd "$out" && pwd)

p=$work/payload
support="$p/Library/Application Support/Kite"
app=$p/Applications/Kite.app
install -d -m 0755 "$support" "$app/Contents/MacOS" "$p/Library/LaunchDaemons" "$p/Library/LaunchAgents" "$p/usr/local/bin"
install -m 0755 "$bin/kite" "$support/kite"
install -m 0755 "$pkg/uninstall.sh" "$support/uninstall.sh"
ln -s "/Library/Application Support/Kite/kite" "$p/usr/local/bin/kite"
install -m 0755 "$bin/kite-desktop" "$app/Contents/MacOS/kite-desktop"
# CFBundleShortVersionString is numbers and dots only.
sed "s/@VERSION@/${version%%-*}/" "$pkg/Info.plist" >"$app/Contents/Info.plist"
install -m 0644 "$pkg/$id.plist" "$p/Library/LaunchDaemons/$id.plist"
install -m 0644 "$pkg/$id.desktop.plist" "$p/Library/LaunchAgents/$id.desktop.plist"

# Apple silicon refuses code with no signature at all, and lipo drops the one the linker made. An
# ad-hoc signature says nothing about who made it; it only lets the code run.
codesign --force --sign - "$support/kite"
codesign --force --sign - "$app"

scripts=$work/scripts
mkdir "$scripts"
install -m 0755 "$pkg/preinstall" "$pkg/postinstall" "$scripts/"
pkgbuild --root "$p" --scripts "$scripts" --identifier "$id.pkg" --version "$version" \
  --ownership recommended --install-location / "$work/component.pkg"
productbuild --package "$work/component.pkg" "$out/Kite-$version.pkg"
ls -l "$out"
