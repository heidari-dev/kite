# Kite on macOS

The computer side runs on macOS: `kite`, the daemon, TUN mode and the desktop app. It needs testers
(there is no Mac in the project's own hands; CI on GitHub's macOS runners is the only test).

## The daemon

The daemon is a launchd daemon, `io.github.heidari-dev.kite`
(`packaging/macos/io.github.heidari-dev.kite.plist`), started as root: a utun device and routes
need it. It keeps its key and the paired phones in `/var/db/kite` (root only) and listens on
`/var/run/kite/kite.sock`, mode 0660, group `admin`; it makes `/var/run/kite` itself (0750, group
`admin`) because `/var/run` is empty after a boot.

Each caller is checked again with `getpeereid`: only root, the daemon's own user and members of the
group `admin` get an answer. The groups of the caller's user are looked up in the directory service,
under a 2 s timeout; a caller that cannot be named is refused. Unlike Linux, members of `admin` may
also pair and forget phones, because they can become root anyway.

```
sudo launchctl bootstrap system /Library/LaunchDaemons/io.github.heidari-dev.kite.plist
sudo launchctl kickstart system/io.github.heidari-dev.kite
sudo launchctl bootout system/io.github.heidari-dev.kite     # stop it
```

When the daemon starts it first runs `kite restore`, so what a killed run left behind is gone.

## TUN mode

`kite connect --usb --tun` (or the desktop app) sends all traffic and DNS of the computer through
the relay. Run by hand with `--foreground` it needs `sudo`. It changes, and nothing else:

- The device `utunN` (the system picks N; 198.18.0.1 to 198.18.0.2). It goes away with the process.
- Four routes on that device: `0.0.0.0/1`, `128.0.0.0/1`, `::/1` and `8000::/1`. They take everything
  that has no more specific route, so the local network keeps working and the default route is not
  touched.
- A host route for the relay's address when the system reaches it through the default route (a
  `--tcp` address on the Internet), through the gateway it had, so the connection to the phone does
  not enter its own tunnel. A phone on the same WiFi or on its hotspot, or USB, needs none.
- A DNS resolver in the system configuration, `State:/Network/Service/kite/DNS`, that matches every
  name and sends it to 1.1.1.1, which the tunnel carries to the phone as DNS over TCP. Names that
  only the local network knows stop resolving while the tunnel is up.
- `tun.lock` and `tun.state` in `/var/db/kite`.

A normal stop removes the host route, the DNS resolver and the state file. If kite was killed, the
device and its routes go away with the process; the DNS resolver and the host route stay until the
daemon starts again or `sudo kite restore`. `kite` runs `route`, `scutil`, `dscacheutil` and `pkill`
by full path with an empty environment.

There is no kill switch on macOS yet (issue 101): `--kill-switch` is refused, and the desktop app
does not show the switch.

## Desktop app

The app has a menu bar icon, as on Linux and Windows, and a window. `kite-desktop --hidden` starts
with the icon only; `packaging/macos/io.github.heidari-dev.kite.desktop.plist` is a LaunchAgent that
does that at login.

## Not done yet

The package (.pkg) that installs all of this; for now build from source (`cargo build --release`)
and copy the plist.

## USB

Direct USB to the phone's ADB interface goes through IOKit and needs no driver and no rule. Only
one program can hold the interface: stop `adb` (`adb kill-server`) or use the adb path.
