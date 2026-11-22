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
pref 32760  ipproto udp dport 53 -> table 5300   every DNS query, even to a LAN resolver
pref 32761  ipproto tcp dport 53 -> table 5300
pref 32762  table main, suppress_prefixlength 0  LAN and other specific routes stay as they are
pref 32763  not fwmark 5300 -> table 5300         everything else except the relay transport
```

The rules come last, right before the main table's (32766), so a VPN started on top puts its own
before them (see [A second VPN](#a-second-vpn-on-the-computer)). Table 5300 holds only default
routes through `kite0`, the IPv4 one through 198.18.0.2, and the socket to the relay carries
firewall mark 5300 so it never enters the tunnel it carries. `/etc/resolv.conf` is left alone. With
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

## A second VPN on the computer

Where the phone's VPN gives only a way out, a second VPN or proxy on the computer can run over Kite
for a clean address: Kite carries the second VPN's own connection to its server through the phone's
VPN, and the second VPN carries everything else. For that the second VPN's connection to its server
must take Kite's TUN device, and the second VPN's tunnel must win over Kite's for everything else.

VPN clients do not all follow the routing table for their own connection. They pick an uplink and
pin to it:

| Client | How it picks its way out |
|--------|--------------------------|
| WireGuard for Windows (wireguard-nt) | The routing table, its own adapter left out. Adds the AllowedIPs as routes (`0.0.0.0/0` as one route), interface metric 0. Its kill switch blocks everything outside its adapter, and DNS to any server but its own. |
| OpenVPN | A host route to its server through the gateway of the route the server is reached by, then `0.0.0.0/1` and `128.0.0.0/1` (or `0.0.0.0/0`) through its adapter. `block-outside-dns` blocks DNS outside its adapter. |
| sing-box (v2rayN and other v2ray/Xray clients in TUN mode) | The `0.0.0.0/0` route with the lowest metric, but it skips virtual adapters (`IF_TYPE_PROP_VIRTUAL`), and it binds its own sockets to the adapter it found. Kite's Wintun adapter is virtual, so it is never picked: set the outbound interface to `kite0` by hand. |
| Windows IKEv2, SSTP, L2TP | The routing table; Windows raises the other adapters' metrics while the VPN is up. |
| System proxy (v2ray/Xray, Psiphon) | No adapter: the proxy's own connections follow the routing table, so they enter Kite like any other. |

So Kite presents its TUN device the way a network card with the best metric looks: a real default
route, a gateway, and the lowest metric of all network cards, but not lower than a VPN's. A VPN that
reads the routing table for its server then goes through Kite, and its own default route (metric 0,
or the more specific halves) wins for everything else. Kite never punches through another program's
firewall: it adds blocks and soft permits only, so a second VPN's kill switch blocks what it blocks
whether Kite runs or not.

### Windows

- `0.0.0.0/0` through `198.18.0.2` (the other address of `kite0`'s /30) and `::/0`, route metric 0,
  interface metric 3. Automatic metrics of network cards start at 5, so Kite wins over them;
  WireGuard sets 0 and Windows' own IKEv2 connection 1, so they win over Kite. If a network card
  still wins, for IPv4 or IPv6, Kite refuses to start rather than leak.
  Up to 0.1.0-rc.2 Kite used the four halves (`0.0.0.0/1` and so on) instead, which won over a VPN's
  single default route by being more specific: traffic went around the VPN into Kite, and a
  WireGuard kill switch then blocked all of it.
- DNS: Windows asks the DNS servers of every adapter at once, each through its own adapter. The DNS
  guard, Windows Filtering Platform filters in a sublayer of the lowest weight, goes up before the
  routes and blocks DNS (UDP and TCP port 53) on every adapter but the loopback, `kite0` and other
  virtual adapters (`IF_TYPE_PROP_VIRTUAL`: Wintun, WireGuard, TAP-Windows 9, OpenVPN DCO, the kind
  a VPN makes), and `kite0` offers 1.1.1.1. So a network card's resolver (Ethernet, Wi-Fi, mobile
  broadband, USB tethering, Hyper-V and VMware switches) and a PPPoE connection's never see a name.
  Up to 0.1.0-rc.2 a catch-all NRPT rule sent every name to 1.1.1.1 instead, which also took DNS
  away from a second VPN; its kill switch then blocked it.
- While a second VPN with DNS servers of its own carries the default route, `kite0` offers no DNS
  server, so names go only to that VPN's servers and not around it through the phone. Kite looks
  every two seconds and gives 1.1.1.1 back when the VPN goes. A dial-up VPN (IKEv2, SSTP, L2TP) is
  a PPP adapter, the same type as PPPoE, so the guard lets its DNS through only while it carries the
  default route.
- The kill switch also permits virtual adapters (Wintun, WireGuard, TAP-Windows, OpenVPN DCO), so a
  VPN on top of the tunnel works with it on; the VPN's own connection to its server still has to go
  through `kite0` or the relay, since everything else on the network cards stays blocked. Dial-up
  VPNs (IKEv2, SSTP, L2TP) look like PPPoE to Windows and stay blocked: use them with Kite's kill
  switch off.
- A VPN's own kill switch also covers Kite's connection to the phone. Over USB that is on the
  loopback and passes; over WiFi it is a LAN address, so the VPN needs its "allow LAN" setting.
- If Kite stops while a VPN runs on top, the VPN's connection to its server falls back to the
  network card, around the phone, unless Kite's kill switch is on and holds its block.

### Linux

- Kite's rules sit at 32760 to 32763, right before the main table's. A VPN's rules come first:
  wg-quick and NetworkManager add theirs without a priority, which the kernel puts right before the
  first rule after `local` (so a VPN started after Kite lands before it), sing-box uses 9000 and
  Tailscale 5210 to 5270. The VPN's own packets carry its firewall mark, skip its rules, fall through
  to Kite's and go into `kite0`. Up to 0.1.0-rc.2 Kite's rules sat at 5300 to 5303, before sing-box's.
- Table 5300's IPv4 default route goes through 198.18.0.2, so OpenVPN, which looks up the route to
  its server through the rules, adds its server's host route through `kite0` with a gateway.
- With systemd-resolved, two links with the `~.` routing domain both get every query. While another
  link carries the default route and has DNS servers of its own, Kite takes `~.` and the default
  route flag off `kite0`, so names go to that VPN only, and puts them back within two seconds of the
  VPN going.
- Kite adds no firewall rules on Linux, so a VPN's nftables or iptables kill switch keeps blocking
  whatever it blocks. Kite's kill switch, the blackhole route in table 5300, also catches a VPN's
  own packets when Kite dies, so they do not go out by the network card.
- Start the VPN after Kite: one started before has its rules after Kite's, and Kite takes its
  traffic. sing-box finds its outbound interface in the main table, where Kite's routes are not:
  set `default_interface` to `kite0`. Without systemd-resolved, a VPN's DNS server in
  `/etc/resolv.conf` is caught by Kite's DNS rule (OpenVPN's, whose routes are in the main table),
  so names fail; wg-quick's rules come first and are not affected.

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
- **iPhone over USB** (usbmux): the computer asks usbmuxd, the system service that owns the USB link
  to Apple devices, for a connection to TCP port 47810 of the iPhone, where the app listens on its
  loopback address while it shares (`kite-link::usbmux`). After the reply the socket is the byte
  stream. The Noise handshake decides whether it is the right phone.

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
service that runs the relay. An iOS app (SwiftUI, foreground only) is sideloaded. The relay core is shared with the mobile apps
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
  `pair_start`, `pair`, `peers`, `remove_peer`, `set_auto_connect`. A request is capped at 4 KiB, parsed strictly
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
