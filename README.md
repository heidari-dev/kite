# Kite

Share your phone's internet connection with a computer over USB or WiFi, and keep the phone's VPN or
proxy exit IP while doing it.

Normal USB tethering is routed by the kernel and skips the phone's VPN. Kite instead opens a real
socket on the phone for every connection the computer makes, so traffic leaves through whatever the
phone itself is using: a VPN, a proxy, or the plain mobile network. It is being built for Linux, Windows, macOS,
Android and iPhone.

## Status

Early development. Nothing is usable yet. See the open issues and milestones for the roadmap.

## Planned platforms

| Platform | Role | Milestone |
|----------|------|-----------|
| Android  | Phone (relay app) | v0.1.0 |
| Linux    | Computer (CLI, daemon, desktop app) | v0.1.0 |
| Windows  | Computer | v0.2.0 |
| iPhone   | Phone (relay app) | v0.3.0 |
| macOS    | Computer | not scheduled |

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
