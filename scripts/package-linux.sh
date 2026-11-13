#!/usr/bin/env bash
# Builds the Linux packages from the release binaries: a .deb and an AppImage, for this machine's
# architecture. Build the binaries on the oldest system you want to support: they need its glibc or newer.
#
# Usage: scripts/package-linux.sh VERSION OUT_DIR
# Needs: dpkg-deb, mksquashfs (squashfs-tools), curl, and target/release/{kite,kite-desktop}.
set -euo pipefail
umask 022

version=${1:?usage: package-linux.sh VERSION OUT_DIR}
out=${2:?usage: package-linux.sh VERSION OUT_DIR}
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+([-~+.][0-9A-Za-z.~+-]*)?$ ]] || { echo "bad version: $version" >&2; exit 1; }

root=$(cd "$(dirname "$0")/.." && pwd)
pkg=$root/packaging/linux
bin=$root/target/release
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --format=%ct)}
arch=$(dpkg --print-architecture)
[[ $arch == amd64 ]] || { echo "only amd64 is packaged for now, not $arch" >&2; exit 1; }

# The AppImage runtime, pinned by hash. It is the part of every AppImage that mounts the image.
runtime_url=https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64
runtime_sha256=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$out"

# --- .deb -------------------------------------------------------------------------------------
d=$work/deb
install -D -m 0755 "$bin/kite" "$d/usr/bin/kite"
install -D -m 0755 "$bin/kite-desktop" "$d/usr/bin/kite-desktop"
install -D -m 0644 "$pkg/kite.service" "$d/usr/lib/systemd/system/kite.service"
install -D -m 0644 "$pkg/kite.sysusers" "$d/usr/lib/sysusers.d/kite.conf"
install -D -m 0644 "$pkg/50-kite.rules" "$d/usr/share/polkit-1/rules.d/50-kite.rules"
install -D -m 0644 "$pkg/kite.desktop" "$d/usr/share/applications/kite.desktop"
install -D -m 0644 "$pkg/kite-autostart.desktop" "$d/etc/xdg/autostart/kite-desktop.desktop"
install -D -m 0644 "$root/assets/brand/app-icon.svg" "$d/usr/share/icons/hicolor/scalable/apps/kite.svg"
install -D -m 0644 "$pkg/copyright" "$d/usr/share/doc/kite/copyright"
for s in postinst prerm postrm; do install -D -m 0755 "$pkg/deb/$s" "$d/DEBIAN/$s"; done

(cd "$d" && find . -type f ! -path './DEBIAN/*' -printf '%P\0' | LC_ALL=C sort -z | xargs -0 md5sum) >"$d/DEBIAN/md5sums"
size=$(du -sk --exclude=DEBIAN "$d" | cut -f1)
cat >"$d/DEBIAN/control" <<CONTROL
Package: kite
Version: $version
Architecture: $arch
Maintainer: heidari-dev <333728610+heidari-dev@users.noreply.github.com>
Installed-Size: $size
Depends: adb, libc6 (>= 2.35), libfontconfig1, systemd, libx11-6, libxcursor1, libxi6, libxrandr2, libxcb1, libxkbcommon0, libxkbcommon-x11-0, libwayland-client0, libwayland-cursor0
Recommends: polkitd | policykit-1
Section: net
Priority: optional
Homepage: https://github.com/heidari-dev/kite
Description: share a phone's internet connection with this computer
 Kite sends this computer's traffic through the phone over USB, so it leaves the way the phone's
 own apps do: through its VPN or proxy. The package installs the kite command, the desktop app
 with its tray icon, and the daemon that owns the tunnel.
CONTROL
find "$d" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +
dpkg-deb --root-owner-group -Zxz --build "$d" "$out/kite_${version}_$arch.deb"

# --- AppImage ---------------------------------------------------------------------------------
a=$work/Kite.AppDir
install -D -m 0755 "$bin/kite" "$a/usr/bin/kite"
install -D -m 0755 "$bin/kite-desktop" "$a/usr/bin/kite-desktop"
install -m 0755 "$pkg/appimage/AppRun" "$a/AppRun"
install -m 0644 "$pkg/kite.desktop" "$a/kite.desktop"
install -m 0644 "$root/assets/brand/app-icon.svg" "$a/kite.svg"
ln -s kite.svg "$a/.DirIcon"

curl --proto '=https' --tlsv1.2 -fsSL -o "$work/runtime" "$runtime_url"
echo "$runtime_sha256  $work/runtime" | sha256sum -c --quiet -
mksquashfs "$a" "$work/image.squashfs" -root-owned -noappend -no-progress -quiet \
  -comp zstd -Xcompression-level 19
cat "$work/runtime" "$work/image.squashfs" >"$out/Kite-$version-x86_64.AppImage"
chmod 0755 "$out/Kite-$version-x86_64.AppImage"

ls -l "$out"
