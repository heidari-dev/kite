# Kite on macOS

Kite shares your phone's connection with a Mac, with the phone's VPN or proxy intact. This page is
about installing it, using it and removing it. The command line is in [cli.md](cli.md), what Kite
does to the system and why in [threat-model.md](threat-model.md).

**macOS needs testers.** The project has no Mac of its own; the code is built and tested on
GitHub's macOS runners only. If you have one, please run the [acceptance test](#acceptance-test)
and tell us how it went.

macOS 12 or newer, Apple silicon or Intel.

## Install

Download `Kite-VERSION.pkg` from the [releases page](https://github.com/heidari-dev/kite/releases)
and open it. The installer asks for an administrator's password once. It puts

- the `kite` command in `/Library/Application Support/Kite` (and `/usr/local/bin/kite`),
- the app in `/Applications/Kite.app`, which starts with a menu bar icon when you log in,
- the daemon `io.github.heidari-dev.kite` in `/Library/LaunchDaemons`, and starts it.

### The Gatekeeper warning

The package is not signed with an Apple developer certificate, because that costs money the project
does not have. macOS therefore says it cannot verify the package. This is expected. Open System
Settings, Privacy & Security, scroll to the message about `Kite-VERSION.pkg` and click **Open
Anyway** (or right-click the package in Finder and choose Open).

You do not have to trust the download on faith. Compare it with what the project published:

```
shasum -a 256 Kite-VERSION.pkg                      # compare with SHA256SUMS on the release
gh attestation verify Kite-VERSION.pkg --repo heidari-dev/kite
```

The release also holds a Sigstore bundle for the file (`Kite-VERSION.pkg.sigstore.json`), see
[releasing.md](releasing.md). The package is built by the release workflow from the tagged commit
and by nothing else. The programs inside are ad-hoc signed, which only lets them run on Apple
silicon and says nothing about who made them.

## Uninstall

```
sudo "/Library/Application Support/Kite/uninstall.sh"
```

It stops the daemon, runs `kite restore`, and deletes everything the package installed. Your key and
paired phones stay (in `/var/db/kite` and `~/.config/kite`); add `--purge` to delete the daemon's
too.

## Acceptance test

1. Turn on a VPN on the phone and connect Kite (USB or WiFi).
2. On the Mac, `curl https://ifconfig.me` prints the VPN's address, not your own.
3. A DNS leak test (for example dnsleaktest.com, extended test) lists only the VPN's resolvers.


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

## USB

Direct USB to the phone's ADB interface goes through IOKit and needs no driver and no rule. Only
one program can hold the interface: stop `adb` (`adb kill-server`) or use the adb path.
