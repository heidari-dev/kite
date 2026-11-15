#!/usr/bin/env bash
# Installs the macOS package on this Mac, checks that the daemon runs and the files are where they
# belong, runs the uninstall script and checks that nothing is left.
#
# Usage: sudo bash scripts/test-pkg-macos.sh Kite-VERSION.pkg
# It changes this machine while it runs, and puts it back.
set -euo pipefail

[ "$(id -u)" -eq 0 ] || { echo "run this as root" >&2; exit 1; }
pkg=$1
label=io.github.heidari-dev.kite
support="/Library/Application Support/Kite"
user=${SUDO_USER:?run this with sudo, from a user account}

fail() {
  echo "FAIL: $*" >&2
  tail -n 20 /var/log/kite.log >&2 2>/dev/null || true
  exit 1
}

installer -pkg "$pkg" -target /

launchctl print "system/$label" >/dev/null || fail "the daemon is not loaded"
for _ in $(seq 1 40); do
  [ -S /var/run/kite/kite.sock ] && break
  sleep 0.5
done
[ -S /var/run/kite/kite.sock ] || fail "the daemon has no socket"
[ "$(stat -f '%Su' "$support/kite")" = root ] || fail "the kite binary is not owned by root"
for f in "$support" "$support/kite" /Library/LaunchDaemons/$label.plist /Library/LaunchAgents/$label.desktop.plist; do
  [ "$(stat -f '%Su:%Sg' "$f")" = root:wheel ] || fail "$f is not owned by root:wheel"
  mode=$(stat -f '%Lp' "$f")
  [ $((0$mode & 022)) -eq 0 ] || fail "$f can be written by others ($mode)"
done
[ "$(readlink /usr/local/bin/kite)" = "$support/kite" ] || fail "/usr/local/bin/kite is not the link to the install"
[ -d /Applications/Kite.app ] || fail "the app is not in /Applications"
[ -x /usr/local/bin/kite ] || fail "no kite command"
[ -x /Applications/Kite.app/Contents/MacOS/kite-desktop ] || fail "no desktop app"
codesign --verify --deep /Applications/Kite.app || fail "the app's signature is broken"
plutil -lint /Library/LaunchDaemons/$label.plist /Library/LaunchAgents/$label.desktop.plist
/usr/local/bin/kite --version

# An administrator may use the daemon: the user who ran sudo is one on a runner.
out=$(sudo -u "$user" /usr/local/bin/kite status 2>&1) || true
echo "$out"
grep -q 'state: idle' <<<"$out" || fail "an administrator cannot reach the daemon"

"$support/uninstall.sh"

if launchctl print "system/$label" >/dev/null 2>&1; then fail "the daemon is still loaded"; fi
for f in "$support" /Applications/Kite.app /usr/local/bin/kite /var/run/kite \
  /Library/LaunchDaemons/$label.plist /Library/LaunchAgents/$label.desktop.plist /var/log/kite.log; do
  if [ -e "$f" ] || [ -L "$f" ]; then fail "$f is still there"; fi
done
if pgrep -x kite >/dev/null; then fail "kite is still running"; fi
if pkgutil --pkg-info "$label.pkg" >/dev/null 2>&1; then fail "the package receipt is still there"; fi
echo "ok"
