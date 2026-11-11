# Threat model

What Kite protects, who might attack it, and which part of the code is responsible for stopping
them. It uses STRIDE (spoofing, tampering, repudiation, information disclosure, denial of service,
elevation of privilege) on each trust boundary. See [architecture.md](architecture.md) for the data
path and [SECURITY.md](../SECURITY.md) for reporting a problem.

Kite is early. Each mitigation below is marked **done** (in the repository today) or **planned**
(designed, not written yet). Until the planned items for a boundary land, treat that boundary as
unprotected and do not rely on it.

## Assets

| Asset | Why it matters |
|-------|----------------|
| Phone network access | The computer gets to open sockets from the phone: its data plan, its VPN or proxy exit, and its view of the networks it is on. |
| Pairing keys | The long-term keys that decide which computer may use which phone. Whoever holds them is the paired computer. |
| User traffic | Payloads, destination names and addresses, and DNS queries that cross the tunnel. |
| Daemon privileges | On the computer the daemon creates the TUN device and changes routes and DNS. That needs `CAP_NET_ADMIN` on Linux and administrator rights elsewhere. |

## Who we worry about

- Another device on the same WiFi or hotspot, or someone who can sniff or inject on the link.
- Another user or program on the computer, without the daemon's privileges.
- A server on the Internet that the phone connects to on the computer's behalf.
- A paired computer that gets compromised. It should not be able to reach more of the phone than
  the relay is meant to offer.
- A malicious or hijacked dependency, or a tampered release.

Out of scope: someone holding an unlocked phone or a root shell on either device, a malicious VPN
provider, traffic analysis from volume and timing, and bugs in the phone's operating system.

## Trust boundaries

```
 computer                                                phone
 apps -> TUN -> kite daemon  ==[ B1: transport ]==  kite relay -> real sockets -> VPN -> Internet
                  ^                                      |                          [ B3 ]
          [ B2: local IPC ]                              +-> phone's own services
          GUI / CLI                                         (must stay unreachable)
```

- **B1** computer and phone: ADB, usbmux or TCP over WiFi. The wire is untrusted.
- **B2** the daemon and its local clients (the `kite` CLI and the desktop app).
- **B3** the phone and the Internet, plus everything else the phone can reach.

Everything that crosses a boundary is untrusted input: lengths are checked before allocating, every
stream, session and queue has a cap, every wait for a peer has a timeout, and peer input never panics.

## B1: computer and phone

| | Threat | Mitigation | Owner | Status |
|---|--------|------------|-------|--------|
| S | A rogue device pretends to be the phone or the computer. | Noise XX handshake with both sides pinning the other's static key after pairing. | `kite-relay`, `kite-cli` | planned |
| S | Someone guesses or intercepts the pairing code and pairs their own device. | Short-lived code, attempts rate-limited, constant-time comparison, code bound to the handshake. | `kite-relay`, `kite-cli` | planned |
| T | Someone alters or replays bytes on the wire. | Noise (`snow`: 25519, ChaChaPoly, BLAKE2s); no custom crypto. | `kite-relay`, `kite-link` | planned |
| R | A user denies having connected, or we cannot tell which device did. | Not a goal. Logs never hold hostnames, keys, pairing codes or payloads by default; the relay logs nothing today. | `kite-relay` | done |
| I | An eavesdropper reads traffic on WiFi. | Noise encrypts the whole stream. USB links are not exposed to the LAN but get the same encryption. | `kite-link` | planned |
| I | Parsers leak memory or crash on hostile bytes. | SOCKS5 and udpgw parsers check lengths first and have cargo-fuzz targets; CI runs a short fuzz smoke. | `kite-relay` | done |
| D | A peer opens endless streams or buffers to exhaust the phone's RAM. | At most 256 yamux streams per tunnel, fixed 256 KiB window per stream, receive window for the whole tunnel capped (64 MiB worst case), at most 16 UDP sessions per gateway. | `kite-relay` | done |
| D | A peer stalls a handshake or sits idle on a connection. | Timeouts on the SOCKS5 handshake, name resolution and connect; idle timeout on relayed connections and UDP sessions; an idle tunnel is closed. | `kite-relay` | done |
| E | A paired (or compromised) computer reaches services the phone exposes to its own networks. | The relay refuses the phone's loopback, any address assigned to a local interface, link-local, unspecified, broadcast and multicast addresses. The check runs on resolved addresses, after DNS, with IPv4-mapped, IPv4-compatible and NAT64 forms normalised, so DNS rebinding does not help. | `kite-relay` | done |
| S | Another local user binds the loopback port of the adb server (5037) when none runs, or a rogue app binds the abstract socket `kite` on the phone, and acts as the relay. It sees every destination and any unencrypted payload and can alter it; in TUN mode that is all traffic. | Not mitigated: USB is not a trusted link. Noise with a pinned phone key must cover USB too. `kite` prints a warning in USB mode. | `kite-link`, `kite-cli` | planned |

Until Noise is in, the relay trusts whatever is on the other end of the byte stream. Do not point
it at a network listener yet.

## B2: local IPC

The daemon owns the privileges. The CLI and the desktop app are unprivileged clients. None of this
exists yet.

| | Threat | Mitigation | Owner | Status |
|---|--------|------------|-------|--------|
| S | Another local user connects to the daemon's socket or pipe. | Socket or pipe limited to the owner or the `kite` group. The daemon checks the peer's credentials where the OS offers them. | `kite-cli` (daemon) | planned |
| T | A client sends malformed or oversized requests. | Every request is validated: size cap, known commands only, unknown fields rejected. | `kite-cli` (daemon) | planned |
| R | An unprivileged action changes the network setup and nobody can tell. | The daemon logs each state-changing request to the system log, without payloads or hostnames. | `kite-cli` (daemon) | planned |
| I | Status output or logs leak keys, pairing codes or visited hosts. | Keys never travel over IPC. Status shows device names and counters only. Logs follow the same rule by default. | `kite-cli` (daemon) | planned |
| D | A client floods the daemon with connections or requests. | Cap on open IPC connections, per-request timeout, bounded buffers. | `kite-cli` (daemon) | planned |
| E | A local user turns the daemon into a way to run commands or touch files as it. | A fixed set of commands with typed arguments, no paths or command lines taken from clients. On Linux the daemon gets `CAP_NET_ADMIN` from systemd instead of running as full root. | `kite-cli` (daemon) | planned |

## B3: phone and Internet

| | Threat | Mitigation | Owner | Status |
|---|--------|------------|-------|--------|
| S | Forged DNS answers send traffic to the wrong host. | Names resolve on the phone, so queries leave through the phone's VPN or network, not the computer's. | `kite-relay` | done |
| T | Someone alters traffic past the phone. | Out of our hands: Kite does not touch application TLS. | none | n/a |
| I | Traffic or DNS leaves the computer outside the tunnel, or outside the phone's VPN. | The relay opens a real socket on the phone for every connection, so the phone's VPN applies. In TUN mode policy rules send every packet to port 53 and everything not marked as the relay transport into the tunnel, and tun2proxy carries DNS over TCP through it; with systemd-resolved the tunnel link gets the `~.` routing domain. CI checks with packet counters that nothing else leaves. | `kite-relay`, `kite-cli` | done |
| D | A remote server holds connections open or sends data without end. | Idle timeouts, the stream cap above, bounded copy buffers. | `kite-relay` | done |
| E | The relay is turned into a way to reach the phone's own network (see B1, elevation). | Same deny-list. By design other hosts on the phone's networks, such as other hotspot clients on private addresses, stay reachable, because the user's traffic needs them. The phone's public address as seen by its carrier is not an interface address and cannot be detected. | `kite-relay` | done |

The relay opens only outbound connections. The one thing it will listen on, the transport for WiFi
pairing, is a planned part of B1.

## Keys and secrets

- Phone: Android Keystore or iOS Keychain (`ThisDeviceOnly`). Computer: an owner-only file or the OS
  keyring. Keys are zeroized after use and never logged. **Planned.**
- Pairing codes expire and are compared in constant time. **Planned.**
- The repository holds no secrets, signing keys or `.env` files. gitleaks runs on every pull request
  and GitHub secret scanning is on. **Done.**

## Supply chain and releases

| Threat | Mitigation | Status |
|--------|------------|--------|
| A dependency has a known vulnerability, is yanked, or comes from an unexpected source. | `cargo-deny` on every pull request and weekly: advisories, bans, licenses, and only crates.io allowed. Dependabot for crates and Actions. | done |
| A CI action is swapped for a malicious version. | Every action is pinned to a full commit SHA. Workflows default to `contents: read` and never use `pull_request_target`. | done |
| Unsafe or buggy code lands in a parser. | `#![forbid(unsafe_code)]` outside FFI and TUN glue, CodeQL, fuzz targets, review on every pull request. | done |
| A release is built or altered somewhere untrusted. | Releases are built only by CI from tags, with SHA-256 checksums, GitHub artifact attestations and a CycloneDX SBOM. OpenSSF Scorecard tracks the repository's practices. | planned (releases), done (Scorecard) |

## Known gaps

- No encryption or authentication on the transport until the Noise work lands.
- No quota per paired computer: a paired computer can use all of the phone's data.
- No kill switch: when the tunnel drops, kite exits and the computer's own routes take over again.
- Proxy mode and TUN mode listen on 127.0.0.1, so any local user can send traffic through the phone
  while kite runs.
- In TUN mode, names under a search domain that a LAN link supplies to systemd-resolved still go to
  that link's resolver, and so do link-local IPv6 resolvers.
- In TUN mode, DNS over TCP to a resolver on the computer's LAN enters the tunnel and fails there,
  because the phone cannot reach that resolver. Only UDP queries to private resolvers are redirected.
- Timing and volume of traffic are visible to anyone who can watch the link.

## Keeping this current

Update this file in the same pull request that adds or changes network input, crypto, pairing, IPC or
privileges, and flip the status of a row when its mitigation lands.
