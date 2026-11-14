# kite command line

`kite` connects this computer to a Kite relay and shares the phone's connection with it. Today the
relay is the `relay-dev` example (see the README) and the phone app does not exist yet, so only
`relay-dev` can be reached; `--tcp` reaches it over the network and `--usb` is ready for the phone app.

```
kite daemon [--state-dir DIR]
kite pair (--tcp HOST:PORT | --wifi [ADDR[:PORT]] | --usb [--serial SERIAL])
kite peers [remove KEY]
kite connect (--tcp HOST:PORT | --wifi [ADDR[:PORT]] | --usb [--serial SERIAL]) [--listen ADDR]
kite connect (--tcp HOST:PORT | --wifi [ADDR[:PORT]] | --usb [--serial SERIAL]) --tun [--kill-switch]
kite disconnect
kite devices
kite status
kite restore
kite --version | -V
kite --help | -h | help
```

On Linux and Windows the tunnel is owned by the [daemon](#kite-daemon) (a systemd unit, a Windows
service), and `pair`, `peers`, `connect`, `disconnect`, `devices` and `status` talk to it. `pair`,
`peers` and `connect` also take `--foreground`: they then run in the `kite` process itself, with the
key and the paired phones of the user who runs it, and need no daemon. Every command takes `--socket
PATH` to reach a daemon that listens somewhere other than `/run/kite/kite.sock` (on Windows the pipe
`\\.\pipe\kite`). Everything below that says "kite" without saying more works the same either way.
Elsewhere there is no daemon and the commands always run in the process.

## kite daemon

Runs the background service. It owns the transports, the TUN device and the tunnel, so closing a
terminal or the desktop app does not drop the connection, and it is the only part that needs
`CAP_NET_ADMIN`. It keeps its own key and the paired phones in `--state-dir` (default
`/var/lib/kite`, mode 0700), separate from the ones of any user's `--foreground` runs.

It listens on `/run/kite/kite.sock`, mode 0660, group `kite`. Each caller's credentials are checked
as well: only root, the daemon's own user and members of the daemon's group get an answer; the
socket's mode is what enforces this, the check is a second layer. On a system that mounts `/proc`
with `hidepid`, a caller whose groups the daemon cannot read is refused. Members of the group can
connect, disconnect and read status, but pairing and forgetting phones (`pair`, `peers remove`) are
for root and the daemon's user only, because the phones decide where all of the computer's traffic
can go: run them with `sudo`.
On Windows the daemon is the service `Kite` (started by `kite daemon --service`, which only the
service manager can run); `kite daemon` in an administrator's console runs the same daemon by hand.
It listens on the named pipe `\\.\pipe\kite`. The pipe is closed to remote clients, and its access
list lets in SYSTEM, the administrators and the members of the local group `kite`; the daemon also
asks the system who is on the other end when a request would change the paired phones, and only an
elevated administrator (or SYSTEM) may do that: run `kite pair` and `kite peers remove` from an
administrator prompt. The daemon keeps its key, the paired phones, its log (`kite.log`) and the TUN
state in `%ProgramData%\Kite`, a directory that only SYSTEM and the administrators can use (with
`--state-dir` it keeps the key and the phones elsewhere; kite replaces that directory's permissions
with the same ones, so give it a directory of its own). Add a user to the group with `net localgroup kite USER /add`; the person has to sign in again. The
service is installed by the MSI; by hand, with an administrator prompt:

```
sc create Kite binPath= "C:\Program Files\Kite\kite.exe daemon --service" start= auto
sc start Kite
```

The protocol is one JSON object per line (`crates/kite-ipc`): `status`, `devices`, `connect`,
`disconnect`, `stats`, `pair_start`, `pair`, `peers` and `remove_peer`. Requests over 4 KiB, with
unknown commands or unknown fields, or that take over 10 s to arrive, are refused; at most 8 clients
are served at once. `connect` returns when the tunnel is starting, and `status` tells how it went.
State-changing requests are written to the system log, never with keys, codes, addresses or the
caller's identity. The unit file that starts the daemon at boot is `packaging/linux/kite.service`; the Linux packages
install it.

```
sudo -u kite kite daemon     # what the unit does; add --socket and --state-dir to try it by hand
kite connect --usb --tun
kite disconnect
```

`kite disconnect` closes the tunnel and, with it, the routes it set up. `kite devices` lists the
phones the adb server sees, as `serial state`.

## kite pair

Pairs this computer with a phone, once. kite prints a fingerprint of this computer's key and the
phone asks you to allow the computer, showing the same fingerprint. After you allow it the phone
shows a 6-digit code; type it when kite asks. Both sides then remember each other's key, and
`kite connect` only talks to a paired phone.

```
$ kite pair --usb
This computer is 3F2A 91C0 77D4 0B1E.
Allow it on the phone, then type the 6-digit code the phone shows and press Enter:
123456
paired with phone 3f9a...
```

If the fingerprints differ, tap Don't allow: some other device reached the phone first. The code comes from the encrypted session itself, so it only matches when nothing sits between the
two devices. A code that does not match stops pairing: if you typed it right, someone is
intercepting the link. Codes expire after 2 minutes. The phone accepts a new computer only for 2
minutes after you open pairing in its app, one at a time, and after 5 failed attempts it refuses
pairing for 60 seconds. `relay-dev` keeps pairing open until the first computer pairs.

This computer's key and the paired phones live in `$XDG_CONFIG_HOME/kite` (`~/.config/kite`;
`%APPDATA%\kite` on Windows): `key` and `phones`, readable only by you. kite refuses to use them
if other users can read or write them. With the daemon they live in its state directory instead
(`/var/lib/kite`), and `kite pair` asks the daemon to pair: it dials the phone, waits for the code
you type, and gives it one try per pairing.

## kite peers

Lists the paired phones by public key. `kite peers remove KEY` forgets the phone whose key starts
with `KEY`; give enough of it to match one phone. Remove this computer on the phone too.

## kite connect

Opens one tunnel to the relay. With the daemon it returns as soon as the tunnel is up (or says why it
could not come up) and the tunnel lives on in the daemon until `kite disconnect`. With
`--foreground` it keeps running until it is stopped or the tunnel fails.

| Option | Meaning | Default |
|--------|---------|---------|
| `--tcp HOST:PORT` | Relay to connect to over the network. A name is resolved once; every address is tried for up to 10 s in total. One of `--tcp` and `--usb` is required. | none |
| `--wifi [ADDR[:PORT]]` | Reach the phone on the same WiFi or on its hotspot. Without an address kite looks for the phone with mDNS. See [WiFi](#wifi). | off |
| `--usb` | Reach the relay on a phone connected by USB, through a running adb server (`adb.exe` on Windows). | off |
| `--serial SERIAL` | With `--usb`: the phone to use when several are connected. | the only phone |
| `--listen ADDR` | Proxy mode only. `IP:PORT` of the local SOCKS5 proxy. It must be a loopback address. | `127.0.0.1:1080` |
| `--tun` | TUN mode (Linux and Windows): send all traffic and DNS of this computer through the relay. Cannot be combined with `--listen`. | off |
| `--kill-switch` | Linux only. With `--tun`: while the link to the phone is down, block all traffic and reconnect, until `kite disconnect`. With `--tcp` or `--wifi ADDR` the relay must be an IP address: a name would be looked up again at every redial, into the block. See [Kill switch](#kill-switch). | off |

Without `--tun` kite runs in proxy mode: it serves a SOCKS5 proxy on `--listen`, needs no special
privileges, and only programs that use the proxy go through the phone. Use `socks5h://` (proxy
resolves names) so DNS goes through the phone too:

```
curl --proxy socks5h://127.0.0.1:1080 https://ifconfig.me
```

Every connection to the proxy becomes one stream to the relay. At most 224 run at once (the relay
closes the tunnel past 256); further connections wait. A client that sends nothing for 10 s is dropped.

Stop kite with Ctrl+C or SIGTERM. It closes local connections, closes the tunnel and exits with 0.
If the relay closes the tunnel or it fails, kite prints the error and exits with 1.

### USB

`--usb` asks the adb server on 127.0.0.1:5037 for the connected phones, picks one, and opens the
abstract socket `kite` on it; that stream carries the same tunnel as `--tcp`, so proxy and TUN mode
work the same. It needs `adb` installed with its server running (`adb start-server`; on Windows
Android's platform-tools, with a driver for the phone), USB debugging turned on, and the Kite app
listening on the phone. Only phones connected by USB count; adb over WiFi
is ignored. Kite explains what is missing:

- no phone found, or the adb server is not running;
- several phones connected: choose one with `--serial` (the serial is the first column of `adb devices`);
- the phone is `unauthorized`: accept the USB debugging prompt on the phone and run kite again;
- the phone is `offline`: unplug and replug it;
- the Kite app is not listening: open it on the phone and start sharing.

Unplugging the phone, or stopping the adb server, closes the tunnel: kite prints the error and exits
with 1, and in TUN mode removes its rules as usual.

Every link, TCP or USB, is encrypted with Noise and kite checks the phone's key against the paired
phones before sending its own, so a program that pretends to be the relay, such as a fake adb
server started by another local user, gets nothing.
Without `--tun` nothing needs special privileges.

### WiFi

Turn on Share over WiFi in the phone app, put the computer on the same WiFi or on the phone's
hotspot, and run:

```
kite pair --wifi            # once; the phone shows a code, as with --usb
kite connect --wifi         # finds the phone with mDNS
kite connect --wifi 192.168.43.1 --tun --kill-switch
```

The phone listens on port 47810, so `--wifi ADDR` is `--tcp ADDR:47810`. Without an address kite
sends one mDNS question for `_kite._tcp.local` from an ephemeral port and waits 3 s for answers;
the phone's address is the one the answer came from. If several phones answer, kite lists them
and asks for `--wifi ADDRESS`; if none does, check that the switch is on and that both devices are
on the same network (some routers block multicast between clients; give the address then). The
question leaves on the default route's interface only. The address is looked up once, when the
tunnel starts, so the kill switch never waits for a lookup; if the phone's address changes
while connected, run `kite connect` again.

Over WiFi the tunnel is the same Noise session as over USB and kite checks the phone's key before
sending its own, so another device that answers the question or takes the phone's address gets
nothing. In TUN mode the socket to the phone carries the same firewall mark as `--tcp`: the
policy rules leave it out of the tunnel, so the tunnel does not carry itself, and no route is
added that `kite restore` would have to remove.

### TUN mode

On Windows, see [TUN mode on Windows](#tun-mode-on-windows). On Linux
`kite connect --tcp HOST:PORT --tun` needs `CAP_NET_ADMIN`, not full root:

```
sudo setcap cap_net_admin+ep ./kite     # once, or run kite as root
./kite connect --tcp 192.168.1.50:7000 --tun
```

What it changes on the system, and nothing else:

- The TUN device `kite0` (198.18.0.1/30), created without persistence.
- Four policy rules per address family, at priorities 5300 to 5303, and one routing table, 5300, that
  holds only the default routes through `kite0`:

  ```
  5300  ipproto udp dport 53 -> table 5300
  5301  ipproto tcp dport 53 -> table 5300
  5302  table main, suppress_prefixlength 0
  5303  not fwmark 5300 -> table 5300
  ```

  The socket to the relay carries firewall mark 5300, so it never enters its own tunnel. Specific
  routes, such as your LAN, still win over the tunnel.
- If this host resolves through systemd-resolved (its stub in `/etc/resolv.conf`, or `resolve` in the
  `hosts:` line of `/etc/nsswitch.conf`), the link `kite0` gets the DNS server 1.1.1.1, the `~.`
  routing domain and the default-route flag, through `resolvectl`. Without root, resolved asks
  polkit; see [architecture.md](architecture.md) for a rule. Otherwise `/etc/resolv.conf` is not touched.

DNS queries go to the relay as DNS over TCP, other UDP through the relay's UDP gateway. AAAA answers
are dropped, so clients use IPv4.

How it is undone:

- Normal stop (signal, relay or phone lost, error): kite deletes the rules, and the kernel removes `kite0`,
  its routes and resolved's settings for that link.
- Kite killed or crashed: the kernel still removes `kite0`, its routes and resolved's settings. The
  four rules stay but point at an empty table, so traffic falls through to your normal routes and the
  network keeps working. They are removed the next time kite starts in TUN mode, or by `kite restore`.

Without `--kill-switch`, kite does not block traffic if the tunnel drops; it ends the tunnel and
normal routing returns.

### TUN mode on Windows

`kite connect --usb --tun` needs an administrator, which the service is, so the usual way is the
service (`kite connect` talks to it) or the desktop app. With `--foreground` run it from an
administrator prompt. It needs `wintun.dll` next to `kite.exe`: the installer puts it there; for a
build from source run `scripts\fetch-wintun.ps1 target\release`. Kite loads it from that directory
only.

What it changes on the system, and nothing else:

- The Wintun adapter `kite0` (198.18.0.1/30, interface metric 1, DNS server 1.1.1.1), which goes away
  with the process.
- Four routes on that adapter: `0.0.0.0/1`, `128.0.0.0/1`, `::/1` and `8000::/1`. Together they take
  everything that has no more specific route, so the local network and the other adapters' own
  subnets keep working and the real default route is not touched.
- A host route for the relay's address when the system reaches it through the default route (a
  `--tcp` address on the Internet), with the next hop it had, so the connection to the phone does
  not enter its own tunnel. A phone on the same WiFi or on its hotspot needs none. USB needs none:
  adb is on the loopback address.
- A catch-all DNS rule (NRPT, comment `kite`) that sends every name lookup to 1.1.1.1, which the
  tunnel carries to the phone as DNS over TCP. Windows asks the DNS servers of all its adapters at
  once; without the rule the local resolver would see the names and could answer first. Names that
  only the local network knows (a router's `.lan` name) stop resolving while the tunnel is up.
- `tun.lock` and `tun.state` in `%ProgramData%\Kite`: the first keeps two kites from setting up at
  once, the second remembers the relay's host route.

How it is undone: a normal stop (Ctrl+C, `kite disconnect`, the link lost, an error) removes the
routes, the DNS rule and the state file. If kite was killed, the adapter goes away with the process
and takes its routes with it; the DNS rule and the host route stay until the next TUN start or
`kite restore`. Nothing else is touched.

There is no kill switch on Windows yet (issue 88): `--kill-switch` is refused, and when the link to
the phone is lost the tunnel ends and normal routing returns.

### Kill switch

This is Linux only. With `--kill-switch` a lost link (phone unplugged, WiFi gone, the app stopped) does not end the
tunnel. `kite0`, its rules and the DNS settings stay, so everything that is not loopback, the local
network or the link to the phone keeps going into the tunnel and stops there: new connections are
refused at once and nothing leaves the computer. kite dials the phone again after 1 s, then backs
off to every 15 s, and traffic resumes as soon as the phone answers. `kite status` says
`reconnecting` meanwhile, with the last error, and counts the reconnects. `kite disconnect` (or
stopping a `--foreground` kite with Ctrl+C) is the only way out, and it restores normal routing.

The kill switch also holds if kite itself dies: table 5300 then gets a blackhole default route that
outlives the device, so traffic stays blocked after a crash or SIGKILL until `kite restore` or the
next `kite connect`. It is opt-in everywhere: off on the command line unless `--kill-switch` is
given, and off in the desktop app until the user turns it on.

## kite status

Prints the tunnel of the daemon:

```
state: connected
relay: 192.168.1.50:7000
mode: SOCKS5 proxy
streams: 3 of 224
reconnects: 0
uptime: 42s
```

`state` is `connecting`, `connected`, `reconnecting` or, once the tunnel has ended, `idle`; then
`last error` says why. `mode` is `all traffic through the tunnel` in TUN mode. With no daemon, it
shows a tunnel started with `--foreground` instead: that kite listens on
`$XDG_RUNTIME_DIR/kite.sock` (a Unix socket, mode 0600) and only writes `relay`, `mode`, `streams`
and `uptime`. If `XDG_RUNTIME_DIR` is unset or open to other users, such a kite still runs but
prints `status unavailable`. `kite status` exits with 1 and prints `not connected` when nothing is
running. On Windows only the service answers, not a `--foreground` tunnel.

## kite restore

Removes what a TUN mode kite that was killed left behind. Needs `CAP_NET_ADMIN` on Linux, an
administrator on Windows. It fails while a TUN mode kite is running. On Linux it only deletes rules
that exactly match the ones kite adds, so rules from other programs stay. On Windows it deletes the
routes on the adapter `kite0`, the host route in `tun.state` and the DNS rule with the comment
`kite`.

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Normal stop, or `--help` and `--version` |
| 1 | Any error: bad arguments (usage is printed), relay unreachable, phone not paired or code wrong, tunnel lost, status not available |
