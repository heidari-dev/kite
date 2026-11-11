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
- UDP is carried by a UDP gateway protocol (udpgw) served by the phone relay. See below.
- The phone refuses targets on its own loopback and link-local ranges by default, plus unspecified,
  broadcast and multicast addresses. Names resolve on the phone and are checked after resolution.

## UDP

tun2proxy's udpgw client does not dial the gateway directly. It opens a TCP connection through the
same SOCKS5 proxy as everything else and sends a CONNECT for the gateway address it was given. So the
phone needs no second protocol on a yamux stream: its SOCKS5 handler treats a CONNECT to the reserved
address `192.0.2.1:7300` as "speak udpgw on this stream" and serves it in-process. That address is in
TEST-NET-1 (RFC 5737) and never routes; 198.18.0.0/15 was avoided because tun2proxy's virtual DNS
hands out addresses from it. The desktop side runs tun2proxy with `--udpgw-server 192.0.2.1:7300`.

The relay implements the udpgw wire format itself instead of depending on tun2proxy. tun2proxy
exports only the frame types as a library; the server lives in its `udpgw-server` binary, and the
crate pulls in the TUN device, a TCP/IP stack, a DNS library and a CLI parser, none of which belong
on the phone. The format is small:

```
LEN u16 | FLAGS u8 | CONN_ID u16 | ATYP, address, port (DATA only) | payload
```

All fields are big-endian and LEN counts the bytes after itself. FLAGS is KEEPALIVE (0x01, echoed),
DATA (0x02), CLOSE (0x04, drops the socket for CONN_ID) or ERR (0x20, sent back when a datagram cannot
be delivered). Each CONN_ID gets its own UDP socket on the phone, connected to the resolved target,
so replies come back as DATA frames with the same CONN_ID. A frame body is at most 65535 bytes, which
fits the largest IPv4 datagram (65507 bytes).
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

## Security

See the [threat model](threat-model.md) for assets, trust boundaries and which mitigations exist today.

## Prior art

PdaNet+ pioneered sharing a phone connection while keeping the phone's VPN. Kite is an independent,
open-source, cross-platform design with encrypted sessions.
