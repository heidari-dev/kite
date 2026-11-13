#!/usr/bin/env bash
# Installs the .deb, checks what it set up, removes it and checks that nothing is left; then does it
# again with purge. With systemd running it also checks the daemon. Installs and removes packages and
# a user: run it as root on a disposable machine.
#
# Usage: sudo bash scripts/test-deb.sh path/to/kite.deb
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive

deb=$(realpath "${1:?usage: test-deb.sh path/to/kite.deb}")
person=${SUDO_USER:-}
systemd=false
[[ -d /run/systemd/system ]] && systemd=true

fail() { echo "FAIL: $*" >&2; exit 1; }

files=(
  /usr/bin/kite
  /usr/bin/kite-desktop
  /usr/lib/systemd/system/kite.service
  /usr/lib/sysusers.d/kite.conf
  /usr/share/polkit-1/rules.d/50-kite.rules
  /usr/share/applications/kite.desktop
  /usr/share/icons/hicolor/scalable/apps/kite.svg
  /etc/xdg/autostart/kite-desktop.desktop
)

check_installed() {
  for f in "${files[@]}"; do [[ -e $f ]] || fail "missing $f"; done
  getent passwd kite >/dev/null || fail "no kite user"
  getent group kite >/dev/null || fail "no kite group"
  for b in /usr/bin/kite /usr/bin/kite-desktop; do
    if ldd "$b" | grep 'not found'; then fail "$b needs a library the package does not depend on"; fi
  done
  [[ $(kite --version) == "kite $(dpkg-deb -f "$deb" Version)" ]] || fail "wrong version"
  if command -v desktop-file-validate >/dev/null; then
    desktop-file-validate /usr/share/applications/kite.desktop /etc/xdg/autostart/kite-desktop.desktop
  fi
  if [[ -n $person ]] && id "$person" >/dev/null 2>&1; then
    getent group kite | cut -d: -f4 | tr , '\n' | grep -qx "$person" || fail "$person is not in the kite group"
  fi
  if $systemd; then
    systemctl is-enabled --quiet kite.service || fail "daemon not enabled"
    for _ in $(seq 20); do systemctl is-active --quiet kite.service && [[ -S /run/kite/kite.sock ]] && break; sleep 0.5; done
    systemctl is-active --quiet kite.service || { journalctl -u kite --no-pager | tail -n 30; fail "daemon not running"; }
    [[ -S /run/kite/kite.sock ]] || fail "no daemon socket"
    # Root may talk to the daemon; with no tunnel it answers "not connected".
    answer=$(kite status 2>&1 || true)
    [[ $answer == *'not connected'* ]] || { journalctl -u kite --no-pager | tail -n 30; fail "kite status said: $answer"; }
  fi
}

check_removed() {
  for f in "${files[@]}"; do [[ ! -e $f ]] || fail "$f is still there"; done
  ! getent passwd kite >/dev/null || fail "kite user is still there"
  ! getent group kite >/dev/null || fail "kite group is still there"
  [[ ! -e /var/lib/kite && ! -e /run/kite ]] || fail "state or runtime directory is still there"
  [[ ! -e /etc/systemd/system/multi-user.target.wants/kite.service ]] || fail "service link is still there"
  if command -v ip >/dev/null; then
    ! ip link show kite0 >/dev/null 2>&1 || fail "kite0 is still there"
    ! ip rule | grep -q 'lookup 5300' || fail "routing rules are still there"
  fi
  [[ $(dpkg-query -W -f='${Status}' kite 2>/dev/null) != 'install ok installed' ]] || fail "still installed"
}

apt-get install -y "$deb"
check_installed
apt-get install -y --reinstall "$deb"
check_installed
apt-get remove -y kite
check_removed

apt-get install -y "$deb"
check_installed
apt-get purge -y kite
check_removed

if command -v xvfb-run >/dev/null; then
  apt-get install -y "$deb"
  # A window that opens and stays open for a few seconds has found all its libraries.
  status=0
  xvfb-run -a timeout 5 kite-desktop || status=$?
  [[ $status -eq 124 ]] || fail "kite-desktop exited with $status"
  apt-get purge -y kite
  check_removed
fi
echo "ok"
