# Linux packaging

Files for the packages. Nothing installs them yet; the `.deb` and AppImage work does.

| File | Install as |
|------|------------|
| `kite.service` | `/usr/lib/systemd/system/kite.service` |
| `kite.sysusers` | `/usr/lib/sysusers.d/kite.conf` |
| `50-kite.rules` | `/usr/share/polkit-1/rules.d/50-kite.rules` |

The unit runs `kite daemon` as the `kite` user with `CAP_NET_ADMIN` and nothing else, in a
sandbox that allows only what the daemon does: unix, IP and netlink sockets, the TUN device, its
own `/run/kite` and `/var/lib/kite`. CI runs `systemd-analyze security` on it and fails if the
exposure score gets worse than the threshold in `.github/workflows/ci.yml`.

Members of the `kite` group can use the tunnel. Pairing phones is for root and the `kite` user
(see `docs/cli.md`).
