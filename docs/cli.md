# kite command line

`kite` connects this computer to a Kite relay and shares the phone's connection with it. Today the
relay is the `relay-dev` example (see the README) and the phone app does not exist yet, so only
`relay-dev` can be reached; `--tcp` reaches it over the network and `--usb` is ready for the phone app.

```
kite connect (--tcp HOST:PORT | --usb [--serial SERIAL]) [--listen ADDR]
kite connect (--tcp HOST:PORT | --usb [--serial SERIAL]) --tun
kite status
kite restore
kite --version | -V
kite --help | -h | help
```

## kite connect

Opens one tunnel to the relay and keeps running until it is stopped or the tunnel fails.

| Option | Meaning | Default |
|--------|---------|---------|
| `--tcp HOST:PORT` | Relay to connect to over the network. A name is resolved once; every address is tried for up to 10 s in total. One of `--tcp` and `--usb` is required. | none |
| `--usb` | Reach the relay on a phone connected by USB, through a running adb server. | off |
| `--serial SERIAL` | With `--usb`: the phone to use when several are connected. | the only phone |
| `--listen ADDR` | Proxy mode only. `IP:PORT` of the local SOCKS5 proxy. It must be a loopback address. | `127.0.0.1:1080` |
| `--tun` | TUN mode (Linux only): send all traffic and DNS of this computer through the relay. Cannot be combined with `--listen`. | off |

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
work the same. It needs `adb` installed with its server running (`adb start-server`), USB debugging
turned on, and the Kite app listening on the phone. Only phones connected by USB count; adb over WiFi
is ignored. Kite explains what is missing:

- no phone found, or the adb server is not running;
- several phones connected: choose one with `--serial` (the serial is the first column of `adb devices`);
- the phone is `unauthorized`: accept the USB debugging prompt on the phone and run kite again;
- the phone is `offline`: unplug and replug it;
- the Kite app is not listening: open it on the phone and start sharing.

Unplugging the phone, or stopping the adb server, closes the tunnel: kite prints the error and exits
with 1, and in TUN mode removes its rules as usual. The link is not authenticated yet (see the
[threat model](threat-model.md)), so kite prints a warning; any local user can run a program that
pretends to be the adb server when none is running. Without `--tun` nothing needs special privileges.

### TUN mode

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

Kite does not block traffic if the tunnel drops; it exits and normal routing returns.

## kite status

Prints the running tunnel:

```
relay: 192.168.1.50:7000
mode: SOCKS5 proxy on 127.0.0.1:1080
streams: 3 of 224
uptime: 42s
```

`mode` is `all traffic through the tunnel` in TUN mode. The running kite listens on
`$XDG_RUNTIME_DIR/kite.sock` (a Unix socket, mode 0600) and only writes this text. If
`XDG_RUNTIME_DIR` is unset or open to other users, kite still runs but prints
`status unavailable`. `kite status` exits with 1 and prints `not connected` when no kite is running.
Linux and other Unix systems only.

## kite restore

Removes policy rules left behind by a TUN mode kite that was killed. Linux only, needs
`CAP_NET_ADMIN`. It fails while a TUN mode kite is running, and it only deletes rules that exactly
match the ones kite adds, so rules from other programs stay.

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Normal stop, or `--help` and `--version` |
| 1 | Any error: bad arguments (usage is printed), relay unreachable, tunnel lost, status not available |
