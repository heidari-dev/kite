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
- The phone refuses targets on its own loopback, any address assigned to one of its interfaces and
  link-local ranges by default, plus unspecified, broadcast and multicast addresses. Names resolve on the phone and are checked after resolution.

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

## TUN mode on Linux

`kite connect --tun` creates the TUN device `kite0` and runs tun2proxy on it as a library. tun2proxy
turns each TCP connection into a SOCKS5 connection to kite's own proxy on 127.0.0.1, sends DNS over
TCP, and carries other UDP through the relay's gateway. It drops AAAA answers: its TCP stack accepts
every handshake before the relay connects, so an IPv6 target the phone cannot reach would look
connected and then fail instead of falling back to IPv4. Routing follows wg-quick:

```
pref 5300  ipproto udp dport 53 -> table 5300   every DNS query, even to a LAN resolver
pref 5301  ipproto tcp dport 53 -> table 5300
pref 5302  table main, suppress_prefixlength 0  LAN and other specific routes stay as they are
pref 5303  not fwmark 5300 -> table 5300         everything else except the relay transport
```

Table 5300 holds only default routes through `kite0`, and the socket to the relay carries firewall
mark 5300 so it never enters the tunnel it carries. `/etc/resolv.conf` is left alone. With
systemd-resolved, which sends each query out of the link its server belongs to, `kite0` gets its own
DNS server and the `~.` routing domain.

Nothing here depends on kite exiting cleanly. The device is not persistent, so when the process ends
in any way the kernel removes it together with its routes and resolved forgets its DNS settings. The
rules left behind point at an empty table and fall through to the main table, so the network keeps
working. kite deletes them at the next start, on a clean exit, and with `kite restore`.

With the kill switch the device and the rules also outlive a lost link, not only a clean exit: the
proxy listener and tun2proxy keep running, the proxy refuses local connections while no tunnel is
up, and kite dials the relay again with backoff (1 s up to 15 s). Everything that enters the device
ends at a proxy with nothing behind it, so nothing leaves. A blackhole default route (metric 1) in
table 5300 outlives the device and keeps traffic blocked if kite dies, until `kite restore`.

Routes, rules and the device need only `CAP_NET_ADMIN`. systemd-resolved does not accept that
capability for link settings and asks polkit instead, so on a host that resolves through it a
non-root kite needs a polkit rule for the user it runs as. The Linux packages install
`packaging/linux/50-kite.rules`, which lets the `kite` user do the three things TUN mode asks of
resolved (`set-dns-servers`, `set-domains`, `set-default-route`) and nothing else. polkit cannot
name the link, so the rule covers any link, and the daemon only touches its own. Without it TUN mode
stops with an error instead of running with DNS that could leak.

## Transports

The session does not care what carries it, as long as it is one reliable byte stream. `kite-link`
opens it:

- **USB (Android)**: through a running adb server on the computer, which owns the USB device and the
  authorization. kite switches an adb connection to the phone's transport and opens the abstract
  socket `kite` that the app listens on. Talking to the phone without an adb server is
  [#52](https://github.com/heidari-dev/kite/issues/52).
- **WiFi**: a TCP connection to the phone's port 47810, on the phone's WiFi, hotspot or ethernet
  address. It is off until the user turns on "Share over WiFi". `kite connect --wifi` finds the
  phone with one mDNS question (`_kite._tcp`); the answer only gives a port, and the Noise
  handshake decides whether it is the right phone.
- **iPhone over USB** (usbmux) is planned, see the [roadmap](../ROADMAP.md).

## Session and pairing

Every transport carries the same thing: a Noise XX handshake (`snow`: 25519, ChaChaPoly, BLAKE2s),
then yamux inside the encrypted frames. After the handshake each side holds the other's static key. A
computer the phone has not paired is refused, unless the user has opened pairing in the app. Pairing
shows a 6-digit code derived from the handshake on the phone, which the user types on the computer.
Where the code comes from, why a man in the middle cannot match it, and the limits on attempts are in
[`kite-relay/src/pair.rs`](../crates/kite-relay/src/pair.rs) and the
[threat model](threat-model.md).

## Crates

| Crate | Role |
|-------|------|
| `kite-relay` | Phone side: Noise and yamux server, pairing, SOCKS5 and UDP handling per stream (tokio) |
| `kite-link` | Transports that yield one async byte stream: adb, TCP, and mDNS discovery of the phone |
| `kite-ipc` | The local protocol between the daemon and its clients: request and response types, strict parser |
| `kite-cli` | The `kite` binary: daemon, proxy mode, TUN mode, status |
| `kite-ffi` | Bindings of the relay (UniFFI), which the Android app calls through JNA |

Apps live under `apps/`: a Slint desktop app and a Kotlin Views Android app with a foreground
service that runs the relay. An iOS app is planned. The relay core is shared with the mobile apps
through the bindings.

## Daemon

`kite daemon` (Linux first) owns the TUN device and the transports. The desktop app (with tray icon)
and the `kite` command line are thin clients of that daemon over local IPC, so closing the window
costs no connection and frees the GUI's memory.

```
 kite CLI / desktop app ==[ /run/kite/kite.sock, JSON lines ]== kite daemon
                                                                  |-- main thread: accepts clients, one task each
                                                                  `-- tunnel thread: transport, Noise, yamux, TUN
```

- The protocol lives in the small `kite-ipc` crate, so a client links only the request and response
  types. One JSON object per line: `status`, `devices`, `connect`, `disconnect`, `stats`,
  `pair_start`, `pair`, `peers`, `remove_peer`. A request is capped at 4 KiB, parsed strictly
  (unknown commands and fields are refused) and checked again for its values before anything acts
  on it. Keys never travel over it.
- The socket is 0660 and belongs to the daemon's group. The daemon also checks each caller's
  credentials (SO_PEERCRED, then `/proc/PID/status` for supplementary groups) and answers only
  root, its own user and its group; only root and its own user may change the paired phones. At
  most 8 clients are served at once, each request has a timeout, and no request carries a path or a
  command line.
- The tunnel runs on its own thread with its own runtime, the same code that `kite connect
  --foreground` runs in the CLI process, so a slow request never stalls traffic. `connect` returns
  once the thread is started; `status` reports `connecting`, `connected` and, after the end, `idle`
  with the error.
- `packaging/linux/kite.service` runs it as the `kite` user with `CAP_NET_ADMIN` and nothing else,
  `NoNewPrivileges`, a read-only system, no home directories and a system call and address family
  filter. `systemd-analyze security` rates it 1.9 (OK), CI keeps it under 2.5, and the netns test
  runs the real unit.
- The daemon's key and the paired phones live in its state directory (`/var/lib/kite`, 0700),
  never in a user's home. Pairing is two requests, `pair_start` (dial the phone, which shows a
  code) and `pair` (the typed code), with one try per start and the same two-minute lifetime as on
  the phone.

## Desktop delivery

Installers set up the app, CLI, daemon service and autostart with at most one administrator prompt:
`.deb` and AppImage for Linux first, then an MSI for Windows and a `.dmg` for macOS.

## Security

See the [threat model](threat-model.md) for assets, trust boundaries and which mitigations exist today.

## Prior art

PdaNet+ pioneered sharing a phone connection while keeping the phone's VPN. Kite is an independent,
open-source, cross-platform design with encrypted sessions.
