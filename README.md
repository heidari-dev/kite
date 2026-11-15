# Kite

[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/heidari-dev/kite/badge)](https://scorecard.dev/viewer/?uri=github.com/heidari-dev/kite)
[![OpenSSF Best Practices](https://www.bestpractices.dev/projects/16570/badge)](https://www.bestpractices.dev/projects/16570)

Share your phone's internet with your computer over USB or WiFi, through the phone's VPN or proxy.

<!-- Demo GIF goes here: the computer's address before and after pressing Connect.
     Add it as assets/demo.gif and replace this comment with an image tag. -->

Normal USB tethering is routed by the kernel and skips the phone's VPN. Kite instead opens a real
socket on the phone for every connection the computer makes, so traffic leaves through whatever the
phone itself is using: a VPN, a proxy, or the plain mobile network. The phone can be an Android
phone or an iPhone, and the computer runs Linux, Windows or macOS.

## Kite and native tethering

| | USB or WiFi tethering | Kite |
|---|---|---|
| Setup | Built into the phone, nothing to install | An app on the phone and one on the computer, paired once |
| The phone's VPN | Skipped on most phones | Used |
| DNS lookups | Often answered outside the VPN | Answered on the phone, through the VPN |
| Between phone and computer | Plain USB, or the hotspot's WiFi encryption | Encrypted with Noise |

Both use the phone's data plan. Use plain tethering when you do not need the VPN. More in the
[FAQ](docs/guides/faq.md).

## Install

Everything is on the [releases page](https://github.com/heidari-dev/kite/releases). Each guide walks
through the phone, the computer and the first pairing.

- **Android phone:** the `.apk`, installed as the first step of any Android guide below.
- **iPhone:** signed by you with SideStore or AltStore, see the [iPhone guide](docs/guides/iphone.md).
- **Linux:** a `.deb` or an AppImage: [Android and Linux](docs/guides/android-linux.md). Package
  details: [packaging/linux](packaging/linux/README.md).
- **Windows:** an MSI: [Android and Windows](docs/guides/android-windows.md), and
  [Kite on Windows](docs/windows.md).
- **macOS:** a package: [Android and macOS](docs/guides/android-macos.md), and
  [Kite on macOS](docs/macos.md). It needs testers.

Trouble: [troubleshooting](docs/guides/troubleshooting.md). Questions, including whether it is safe:
[FAQ](docs/guides/faq.md).

## Status

Heading for 0.1.0; the first release candidate is `0.1.0-rc.1`. See the [roadmap](ROADMAP.md).

What works: an Android phone or an iPhone and a Linux computer, over USB (through adb or directly
for Android, through usbmuxd for the iPhone) or WiFi. The computer can send everything through the
phone (TUN mode) or only what you point at a local SOCKS5 proxy (proxy mode). A kill switch that
blocks traffic while the phone is disconnected is there on every computer and off until you turn it on. The
link is encrypted with Noise, and both sides are paired once with a 6-digit code.

The same works with a Windows computer (MSI installer) and a Mac (package, needs testers). The iPhone app is sideloaded and has to stay open on the screen
while it shares.

## Try it from source

You need a Rust toolchain (see `rust-toolchain.toml`). On one machine, in two terminals:

```
cargo run --release -p kite-relay --example relay-dev -- 127.0.0.1:7000 --allow-loopback
cargo run --release -p kite-cli -- pair --tcp 127.0.0.1:7000
cargo run --release -p kite-cli -- connect --tcp 127.0.0.1:7000
```

`kite pair` asks for the code that relay-dev prints, the way the phone will show it; it is needed
once per relay-dev run, since relay-dev forgets its pairings when it stops.

`--allow-loopback` lets the relay reach servers on its own machine; leave it off to behave like a
phone. Kite then serves a SOCKS5 proxy on 127.0.0.1:1080:

```
curl --proxy socks5h://127.0.0.1:1080 https://ifconfig.me
```

On Linux, TUN mode sends everything through the relay. It needs `CAP_NET_ADMIN`, not full root:

```
sudo setcap cap_net_admin+ep target/release/kite
target/release/kite connect --tcp RELAY_HOST:7000 --tun
```

After a crash, `kite restore` removes leftover routing rules. See [docs/cli.md](docs/cli.md) for
every command and option, and [docs/architecture.md](docs/architecture.md) for how it works.

## Feedback and contributing

- Bugs and ideas: [issues](https://github.com/heidari-dev/kite/issues)
- Questions and discussion: [Discussions](https://github.com/heidari-dev/kite/discussions)
- Code and docs: see [CONTRIBUTING.md](CONTRIBUTING.md); how decisions are made: [GOVERNANCE.md](GOVERNANCE.md)
- Security problems: see [SECURITY.md](SECURITY.md), not a public issue

## Platforms

| Platform | Role | Milestone |
|----------|------|-----------|
| Android  | Phone (relay app) | v0.1.0 |
| Linux    | Computer (CLI, daemon, desktop app) | v0.1.0 |
| Windows  | Computer (CLI, service, desktop app, MSI) | needs testers |
| macOS    | Computer (CLI, daemon, desktop app, pkg) | needs testers |
| iPhone   | Phone (relay app), WiFi and hotspot | v0.3.0 |

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
