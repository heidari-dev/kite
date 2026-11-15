#!/bin/sh
# Removes Kite from this Mac: stops the daemon, takes back what a TUN mode run left behind and
# deletes what the package installed. Run it with sudo. Your keys and paired phones stay (in
# /var/db/kite and ~/.config/kite) unless you pass --purge.
set -u
[ "$(/usr/bin/id -u)" -eq 0 ] || { echo "run this with sudo" >&2; exit 1; }
purge=no
case "${1:-}" in
  "") ;;
  --purge) purge=yes ;;
  *) echo "usage: sudo $0 [--purge]" >&2; exit 1 ;;
esac

label=io.github.heidari-dev.kite
support="/Library/Application Support/Kite"

# The tray icon of the user who is signed in, then the daemon.
uid=$(/usr/bin/stat -f %u /dev/console 2>/dev/null)
if [ -n "$uid" ] && [ "$uid" != 0 ]; then
  /bin/launchctl bootout "gui/$uid/$label.desktop" 2>/dev/null
fi
/usr/bin/pkill -x kite-desktop 2>/dev/null
/bin/launchctl bootout "system/$label" 2>/dev/null
# Only a program that root owns and nobody else can replace is run as root.
if [ -x "$support/kite" ] && [ ! -L "$support/kite" ] && [ "$(/usr/bin/stat -f %u "$support/kite")" = 0 ]; then
  "$support/kite" restore 2>/dev/null
fi

/bin/rm -f "/Library/LaunchDaemons/$label.plist" "/Library/LaunchAgents/$label.desktop.plist" /var/log/kite.log
# /usr/local/bin is Homebrew's on an Intel Mac: only the link the package made.
if [ "$(/usr/bin/readlink /usr/local/bin/kite 2>/dev/null)" = "$support/kite" ]; then
  /bin/rm -f /usr/local/bin/kite
fi
/bin/rm -rf /Applications/Kite.app /var/run/kite
if [ "$purge" = yes ]; then
  /bin/rm -rf /var/db/kite
else
  /bin/rm -f /var/db/kite/tun.lock /var/db/kite/tun.state
fi
/bin/rm -rf "$support"
/usr/sbin/pkgutil --forget "$label.pkg" >/dev/null 2>&1
echo "Kite is removed."
[ "$purge" = yes ] || echo "Your key and paired phones are kept; run this again with --purge to delete them too."
