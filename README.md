# Kite

[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/heidari-dev/kite/badge)](https://scorecard.dev/viewer/?uri=github.com/heidari-dev/kite)
[![OpenSSF Best Practices](https://www.bestpractices.dev/projects/16570/badge)](https://www.bestpractices.dev/projects/16570)

Share your phone's internet connection with a computer over USB or WiFi, and keep the phone's VPN or
proxy exit IP while doing it.

Normal USB tethering is routed by the kernel and skips the phone's VPN. Kite instead opens a real
socket on the phone for every connection the computer makes, so traffic leaves through whatever the
phone itself is using: a VPN, a proxy, or the plain mobile network. It is being built for Linux, Windows, macOS,
Android and iPhone.

## Status

Early development. On Linux the `kite` command works in proxy mode and in TUN mode against the
development relay (`relay-dev`), which stands in for a phone. There is no phone app yet, no USB or
WiFi transport, and the link is not encrypted, so do not use it on a network you do not trust. See
the open issues and milestones for the roadmap.

## Try it from source

You need a Rust toolchain (see `rust-toolchain.toml`). On one machine, in two terminals:

```
cargo run --release -p kite-relay --example relay-dev -- 127.0.0.1:7000 --allow-loopback
cargo run --release -p kite-cli -- connect --tcp 127.0.0.1:7000
```

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
- Code and docs: see [CONTRIBUTING.md](CONTRIBUTING.md)
- Security problems: see [SECURITY.md](SECURITY.md), not a public issue

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
