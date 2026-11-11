# Architecture

Kite keeps one idea: every connection the computer makes is re-created as a real socket on the phone.
Android routes app sockets through an active VPN or proxy, so the computer exits with the phone's
address. Plain USB tethering is forwarded in the kernel and skips the VPN, which is what Kite avoids.

## Data path

```
 computer                                                       phone
 apps -> TUN (Linux tun / macOS utun / Wintun)
      -> local SOCKS5 + UDP gateway on 127.0.0.1
      -> each local TCP connection = one yamux stream
      -> Noise-encrypted session
      -> one byte stream: ADB local socket | usbmux TCP (iPhone) | TCP over WiFi
                                                                -> Noise -> yamux accept
                                                                -> SOCKS5 / UDP handler
                                                                -> real socket on the phone
                                                                -> phone VPN or proxy -> Internet
```

- The computer side never parses SOCKS itself; it pipes local connections into new yamux streams.
- DNS goes over the tunnel (DNS over TCP or virtual DNS), so queries leave through the phone.
- UDP is carried by a UDP gateway protocol (udpgw) served by the phone relay.
- The phone refuses targets on its own loopback and link-local ranges by default.
- A proxy-only mode stops at the local SOCKS5 listener and needs no administrator rights.

## Crates

| Crate | Role |
|-------|------|
| `kite-relay` | Phone side: Noise and yamux server, SOCKS5 and UDP handling per stream (tokio) |
| `kite-link` | Transports that yield one async byte stream: ADB, usbmux, TCP |
| `kite-cli` | The `kite` binary: daemon, proxy mode, TUN mode, status |
| `kite-ffi` | Planned: bindings of the relay for Kotlin and Swift |

Apps live under `apps/`: a Slint desktop app, a Kotlin Views Android app with a foreground service, and
a SwiftUI iOS app. The relay core is shared with the mobile apps through the bindings.

## Desktop delivery

A system daemon owns the TUN device and the transports. The desktop app (with tray icon) and the `kite`
command line are thin clients of that daemon over local IPC, so closing the window costs no connection
and frees the GUI's memory. Installers set up the app, CLI, daemon service and autostart with at most
one administrator prompt: `.deb` and AppImage for Linux first, then an MSI for Windows and a `.dmg` for
macOS.

## Prior art

PdaNet+ pioneered sharing a phone connection while keeping the phone's VPN. Kite is an independent,
open-source, cross-platform design with encrypted sessions.
