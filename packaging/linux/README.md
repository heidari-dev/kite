# Linux packaging

Files for the .deb and the AppImage, built by `scripts/package-linux.sh VERSION OUT_DIR` from the
release binaries (CI builds them on Ubuntu 22.04, so they run on Debian 12 and Ubuntu 22.04 or newer).
`scripts/test-deb.sh` installs, checks and removes the .deb; CI runs it on a machine with systemd and
in a Debian 12 container.

| File | Install as |
|------|------------|
| `kite.service` | `/usr/lib/systemd/system/kite.service` |
| `kite.sysusers` | `/usr/lib/sysusers.d/kite.conf` |
| `50-kite.rules` | `/usr/share/polkit-1/rules.d/50-kite.rules` |
| `kite.desktop` | `/usr/share/applications/kite.desktop` |
| `kite-autostart.desktop` | `/etc/xdg/autostart/kite-desktop.desktop` (tray icon at login) |
| `deb/postinst`, `prerm`, `postrm` | the package's maintainer scripts |
| `appimage/AppRun` | the AppImage's entry point |

## The .deb

Installing it needs no questions. It creates the `kite` user, adds the person who installed it to the
`kite` group (the one `sudo` or `pkexec` names, or else the person at the active graphical session;
they log out and in once), enables and starts the daemon, and puts Kite in the application menu and
the tray at login. It depends on `adb`. Removing it, with or without purge, stops the daemon, takes
its routing rules out, and deletes the user, the group and `/var/lib/kite`, so the paired phones are
forgotten.

Only amd64 is built. An rpm is not: its scriptlets differ from the deb's and nobody has asked yet.

## The AppImage

The desktop app and the `kite` command in one file, without the daemon: run
`Kite-VERSION-x86_64.AppImage --cli COMMAND` for the command line, for instance
`--cli connect --usb --foreground` for proxy mode. The window talks to the daemon and TUN mode needs
it, so both need the .deb. The AppImage uses the system's libraries.

The unit runs `kite daemon` as the `kite` user with `CAP_NET_ADMIN` and nothing else, in a
sandbox that allows only what the daemon does: unix, IP and netlink sockets, the TUN device, its
own `/run/kite` and `/var/lib/kite`. CI runs `systemd-analyze security` on it and fails if the
exposure score gets worse than the threshold in `.github/workflows/ci.yml`.

The daemon reads `/proc/PID/status` of its callers to check their groups. If `/proc` is mounted with
`hidepid=1` or `2`, add `gid=` of a group the `kite` user is in, or non-root callers are refused.

Members of the `kite` group can use the tunnel. Pairing phones is for root and the `kite` user
(see `docs/cli.md`).
