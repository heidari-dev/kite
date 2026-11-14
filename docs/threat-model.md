# Threat model

What Kite protects, who might attack it, and which part of the code is responsible for stopping
them. It uses STRIDE (spoofing, tampering, repudiation, information disclosure, denial of service,
elevation of privilege) on each trust boundary. See [architecture.md](architecture.md) for the data
path and [SECURITY.md](../SECURITY.md) for reporting a problem.

Kite is early. Each mitigation below is marked **done** (in the repository today), **partial** (in,
with the gap stated in the row) or **planned** (designed, not written yet). Until the planned items for a boundary land, treat that boundary as
unprotected and do not rely on it.

## Assets

| Asset | Why it matters |
|-------|----------------|
| Phone network access | The computer gets to open sockets from the phone: its data plan, its VPN or proxy exit, and its view of the networks it is on. |
| Pairing keys | The long-term keys that decide which computer may use which phone. Whoever holds them is the paired computer. |
| User traffic | Payloads, destination names and addresses, and DNS queries that cross the tunnel. |
| Daemon privileges | On the computer the daemon creates the TUN device and changes routes and DNS. That needs `CAP_NET_ADMIN` on Linux and administrator rights on Windows. |

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
| S | A rogue device pretends to be the phone or the computer. | Noise XX handshake with both sides pinning the other's static key after pairing. The computer checks the phone's key before sending its own; the phone refuses unknown computers once one is paired. | `kite-relay`, `kite-cli` | done |
| S | Someone in the middle of the first connection pairs in place of one side. | The 6-digit code is derived from the handshake hash and both sides' nonces, with the phone committing to its nonce first, so a man in the middle cannot grind two sessions to the same code: one guess in a million per attempt. The computer compares the typed code in constant time and stops on a mismatch; the phone compares it again. Codes expire after 2 minutes; 5 failed attempts lock pairing for 60 s; one pairing at a time. | `kite-relay`, `kite-cli` | done |
| S | Another device on the link pairs with the phone while pairing is open, before the user's computer does. | Pairing is only open for 2 minutes after the user opens it in the app, even on a phone with no computer yet, and closes after one success. The peer can compute the code itself, so the phone does not show the code until the user has allowed that computer in a dialog that names it by a fingerprint of its key, the same one `kite pair` prints on the computer. A stranger that arrives first is declined, which counts as a failed attempt. The app lists paired computers so one can be removed. The user can still allow a stranger by not comparing the fingerprints. | `kite-relay`, app | done |
| T | Someone alters or replays bytes on the wire. | Noise XX (`snow`: 25519, ChaChaPoly, BLAKE2s) on every transport before yamux; no custom crypto. A bad, replayed or truncated frame ends the session; frame lengths are checked before reading and the handshake has a timeout. | `kite-relay`, `kite-cli` | done |
| R | A user denies having connected, or we cannot tell which device did. | Not a goal. Logs never hold hostnames, keys, pairing codes or payloads by default; the relay logs nothing today. | `kite-relay` | done |
| I | An eavesdropper reads traffic on WiFi. | Noise encrypts the whole stream. USB links are not exposed to the LAN but get the same encryption. | `kite-relay`, `kite-cli` | done |
| I | Parsers leak memory or crash on hostile bytes. | SOCKS5 and udpgw parsers check lengths first and have cargo-fuzz targets; CI runs a short fuzz smoke. | `kite-relay` | done |
| D | A peer opens endless streams or buffers to exhaust the phone's RAM. | At most 256 yamux streams per tunnel, fixed 256 KiB window per stream, receive window for the whole tunnel capped (64 MiB worst case), at most 16 UDP sessions per gateway. | `kite-relay` | done |
| D | A peer stalls a handshake or sits idle on a connection. | Timeouts on the SOCKS5 handshake, name resolution and connect; idle timeout on relayed connections and UDP sessions; an idle tunnel is closed. | `kite-relay` | done |
| E | A paired (or compromised) computer reaches services the phone exposes to its own networks. | The relay refuses the phone's loopback, any address assigned to a local interface, link-local, unspecified, broadcast and multicast addresses. The check runs on resolved addresses, after DNS, with IPv4-mapped, IPv4-compatible and NAT64 forms normalised, so DNS rebinding does not help. | `kite-relay` | done |
| S | Another local user binds the loopback port of the adb server (5037) when none runs, or a rogue app binds the abstract socket `kite` on the phone, and acts as the relay. It would see every destination and payload; in TUN mode that is all traffic. | Noise covers USB like TCP, and `kite connect` aborts the handshake unless the phone's key is a paired one, before sending its own key. `kite pair` over a rogue server fails on the code. | `kite-link`, `kite-cli` | done |
| E | The WiFi listener is reachable from a network it should not be: mobile data, a VPN, or a public address. | Off until the user turns on "Share over WiFi". The app binds only private IPv4 addresses of interfaces that are up and that the system lists as WiFi or ethernet, or that have a usual hotspot or tethering name, minus any it lists as mobile data or a VPN; an interface it cannot classify is left out. It never binds a wildcard address. It looks at the networks every 4 s, so a listener ends within about 10 s of its address going away, with the tunnels it carried. The relay checks again and refuses any address that is not private IPv4. | `kite-relay`, app | done |
| D | A device on the WiFi or hotspot floods the listener with connections or stalls handshakes. | A connection holds a ticket until its handshake and pairing end: 8 in total, 2 per source address, 10 new connections per source address per 10 s, 256 source addresses remembered (a full table refuses new ones until entries expire). Over the cap the connection is closed before any byte is read. The Noise handshake has a 10 s timeout. A connection counts as a tunnel, with the 4-tunnel cap shared with USB, only after it is paired, so strangers cannot use up the places of paired computers. They can still hold the tickets, see the known gaps. | `kite-relay`, `kite-ffi` | done |
| S | A stranger on the link tries to use the phone through the listener. | Same as USB: after the handshake an unknown key gets `DENIED` and is dropped unless the user opened pairing in the app, and then the user must allow its fingerprint and the code must match. Pairing rate limit and expiry are unchanged. | `kite-relay` | done |
| S | A device on the link answers the computer's mDNS question as the phone, or takes the phone's address. | The answer only gives a port; the address is the datagram's source, and only private IPv4 senders count. The computer then runs the Noise handshake and aborts unless the phone's key is a paired one, before sending its own key; `kite pair --wifi` against such a device fails on the code, but the device then has seen this computer's public key (a stable identifier, not a secret). A rogue answer, or a flood of answers, can make discovery fail with "several phones answered"; `--wifi ADDRESS` avoids it. If several phones answer, kite lists them and asks for an address instead of choosing. | `kite-link`, `kite-cli` | done |
| T | A hostile mDNS datagram crashes or exhausts the computer. | The parser checks every length before reading, follows at most 8 name pointers and only backwards, caps names at 255 bytes and records at 64, never panics, and has a cargo-fuzz target with a CI smoke run. At most 8 phones are remembered and the wait is 3 s. The question leaves from an ephemeral port; nothing joins a multicast group or listens on 5353. | `kite-link` | done |
| I | The announcement tells the network more than it should. | `_kite._tcp` is announced with the system's NsdManager only while the listener is up. The service name is `Kite`, not the phone's name; the TXT record has a version and nothing else, so a listener on the network cannot follow the phone from one network to the next by a key or a name. | app | done |

After pairing each side accepts only the other's key. The Android app listens on the abstract
socket `kite` and, when the user turns it on, on TCP port 47810 of its WiFi, hotspot and ethernet
addresses. `relay-dev` keeps its key and pairings in memory,
allows every pairing request and is for development only.

## Android app

| | Threat | Mitigation | Owner | Status |
|---|--------|------------|-------|--------|
| E | Another app on the phone opens the abstract socket `kite` to borrow the phone's network access. | The relay checks the caller's uid on every connection: only adbd (shell, root) and the app itself, then the Noise handshake and pairing apply. | `kite-ffi` | done |
| E | Another app starts, stops or reads the app's service or screens. | The service is not exported; the launcher activity is the only exported component. | app | done |
| I | The pairing code or the notification leaks through screenshots, recents or logs. | The screen sets `FLAG_SECURE`. The notification shows counts and speeds only, no names or hosts. The app logs nothing. | app | done |
| I | Backups carry the keys off the phone. | Backups are off; the private key is encrypted with an AES key that stays in Android Keystore. | app | done |
| D | Android kills the service with the screen off and the computer loses its link. | The app asks once to run without battery limits (`REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`, a normal permission); the user decides in a system dialog. Sharing works without it. | app | done |


## B2: local IPC

The daemon owns the privileges. The CLI and the desktop app are unprivileged clients. The Linux
daemon, its socket and its systemd unit exist (`packaging/linux`), and so do the Windows service
and its named pipe (rows marked Windows); the macOS equivalent does not yet.

| | Threat | Mitigation | Owner | Status |
|---|--------|------------|-------|--------|
| S | Another local user connects to the daemon's socket or pipe. | Socket mode 0660, group `kite`. The daemon checks every caller's credentials (SO_PEERCRED, then the groups in `/proc/PID/status`) and answers only root, its own user and its group; a caller it cannot establish is refused. | `kite-cli` (daemon) | done (Linux) |
| T | A client sends malformed or oversized requests. | Every request is one line of at most 4 KiB, parsed strictly (known commands only, unknown fields rejected) and its values checked, with a cargo-fuzz target on the parser. | `kite-ipc`, `kite-cli` | done |
| R | An unprivileged action changes the network setup and nobody can tell. | The daemon logs each state-changing request to the system log, without keys, codes, payloads or hostnames. It does not log who asked: CodeQL treats peer credentials as secrets. | `kite-cli` (daemon) | done |
| I | Status output or logs leak keys, pairing codes or visited hosts. | Keys never travel over IPC. Status shows the transport, mode, counters and the last error. Logs follow the same rule. | `kite-cli` (daemon) | done |
| D | A client floods the daemon with connections or requests. | At most 8 open IPC connections, a 10 s wait for each request and a 30 s limit for handling it, bounded line buffer. Pairing runs one at a time, one try per start. | `kite-cli` (daemon) | done |
| E | A member of the `kite` group points the computer's traffic at a relay of their own. | Pairing decides which phones may be used, so `pair_start`, `pair` and `remove_peer` are refused for everyone but root and the daemon's user. The group can only use phones that are already paired. | `kite-cli` (daemon) | done |
| T | The daemon's answer to the desktop app is hostile, huge or never comes. | The app reads one line of at most 64 KiB under a timeout, parses it strictly and never panics on it. Text from it is cut to 256 characters and stripped of control and direction characters before it is shown, and lists are capped. The socket path is fixed (`/run/kite/kite.sock`, in a directory only root and the daemon's user can write), so no other user can plant one. | `apps/desktop` | done |
| T | The phone address typed into the window carries control or direction characters, or is huge. | The app trims it, refuses more than 255 bytes, control, whitespace and bidirectional or zero-width characters before it is saved or sent, ignores a saved one that fails the same check, and the daemon checks it again in `kite-ipc`. | `apps/desktop`, `kite-ipc` | done |
| S | The window shows a setting that is not what the daemon runs. | While a tunnel is up the window shows the daemon's own mode and kill switch and locks those controls; the saved choice only applies at the next connect. | `apps/desktop` | done |
| I | The app keeps or leaks the pairing code or a key. | The code is sent once and not stored, logged or printed (its `Debug` output hides it). The app never holds a key; it shows the computer's fingerprint, and pairing stops if the daemon gives none. The only file it writes is `kite/desktop.json` (mode, kill switch), owner-only, replaced through a temporary file and ignored if damaged. | `apps/desktop` | done |
| E | A local user turns the daemon into a way to run commands or touch files as it. | A fixed set of commands with typed arguments, no paths or command lines taken from clients. The daemon's state directory is 0700 and its own. On Linux the unit runs it as the `kite` user with `CAP_NET_ADMIN` and nothing else (bounding set and ambient), `NoNewPrivileges`, a read-only system, no home directories, a system call and address family filter and a device allow-list; CI checks the unit with `systemd-analyze security` and runs it in the netns test. The polkit rule lets that user change DNS on a link through systemd-resolved and nothing else. | `kite-cli` (daemon), `packaging/linux` | done (installed by the Linux packages) |
| S | Another local user or a remote machine connects to the daemon's pipe (Windows). | The pipe's access list lets in SYSTEM, the administrators, the user a console daemon runs as, and the members of the local group `kite` (a local group, looked up as this computer's group and by type: a user called `kite`, or a group of that name in the domain, does not count), and nobody else; `PIPE_REJECT_REMOTE_CLIENTS` refuses other machines. Whether a caller may change the paired phones is asked of the system when a request needs it: the daemon impersonates the client on a thread of its own and checks for the Administrators group in an elevated token, so only an elevated administrator or SYSTEM may pair, as only root may on Linux. | `kite-win`, `kite-cli` (daemon) | done (Windows) |
| E | A member of the group `kite` opens a second instance of the pipe and answers the other members' requests (Windows). | The members' rights leave out `FILE_CREATE_PIPE_INSTANCE` (the bit `FILE_APPEND_DATA` shares), so only the daemon creates instances, and a test checks that a second first instance cannot be made. | `kite-win` | done (Windows) |
| S | A program creates the pipe `\\.\pipe\kite` before the service does and poses as the daemon (Windows). | The service creates the pipe as its first instance, which fails when anybody else got there first, and starts at boot. The service then does not start and says so in `kite.log`. The clients open the pipe at the identification level, so a program that did get there first learns who connects but cannot act as them (an administrator running `kite` is not exposed). The clients do not check who serves the pipe, see the known gaps. | `kite-win`, `kite-cli` (daemon) | partial (Windows) |
| E | A local user turns the service into a way to run commands or touch files as SYSTEM (Windows). | Same fixed set of typed commands as on Linux. The service runs PowerShell only with fixed text (the DNS rule: constants and nothing from a client), by the full path of the system's own copy and without a module path from the environment; the only address that reaches the system is a parsed IP address, handed to the routing API and never to a command line. `wintun.dll` is loaded by full path from the directory of `kite.exe` (`Program Files`, which only administrators can write) and never searched for. The daemon's directory `%ProgramData%\Kite`, with the key, the phones, the log and the TUN state, has a protected access list for SYSTEM and the administrators; the daemon refuses a directory that somebody else owns. | `kite-win`, `kite-cli` | done (Windows) |
| E | The service has more rights than it needs (Windows). | It runs as LocalSystem. A narrower account does not work: creating the Wintun adapter installs a device and changing routes changes the machine's network, which needs administrator rights; LocalService, NetworkService and a virtual service account have neither, and putting one in the Administrators group would not be narrower. The installer cuts the SYSTEM token to four privileges (`SeChangeNotify`, `SeCreateGlobal`, `SeLoadDriver`, `SeImpersonate`) with the service's required-privileges setting, and CI runs the TUN test against the installed service with that token. | `packaging/windows` | done |
| E | The person who installs, or a user added later, gets more than use of the tunnel (Windows). | The installer adds the installing user to the local group `kite` and, so it works before the next sign-in, to a users file in `%ProgramData%\Kite` that the pipe's access list reads: one SID per line, at most 64, anything that is not a valid SID skipped, so the file can put nothing but SIDs into the list; only administrators can write it, and the service ignores a file that somebody else owns (the directory is made with ProgramData's permissions for a moment before it is closed). Both give use of the tunnel, not pairing. | `kite-win`, `packaging/windows` | done |
| E | A standard user pairs a phone of their own and points the computer's traffic at it (Windows). | Pairing stays administrator-only. The Pair button opens `kite pair` through the system's consent prompt (ShellExecute "runas"), so it needs the person to approve and, on a standard account, an administrator's password. The command line is built from fixed words and the phone's address or serial, which must be plain characters (letters, digits and `._:-[]%`, not starting with a dash), else nothing starts. Only the installed `kite.exe` under Program Files is started this way, so a copy in a folder that ordinary users can write is not offered the prompt. The prompt shows an unknown publisher until the programs are signed. | `apps/desktop`, `kite-win` | done |
| E | Installing or removing runs code as SYSTEM that does the wrong thing (Windows). | The installer runs only `net.exe`, `sc.exe`, `cmd.exe` from System32 and the installed `kite.exe`, with fixed arguments; the one value from the installing user is their name, passed quoted to `net localgroup` and `kite allow`, which looks it up as a user and refuses anything else. Uninstalling runs `kite restore`, removes the files, then `%ProgramData%\Kite`, and the group only if it is the one the installer made (it checks the comment), and keeps neither on an upgrade. The install folder is not a public property, so it cannot be moved with a command line. When someone installs by typing an administrator's password, the administrator is the person added. | `packaging/windows` | done |
| E | The package gives the wrong person access to the tunnel. | The install script adds one person to the `kite` group, once, on first install: the one `sudo` or `pkexec` names, or else the person at the active graphical session. Only ordinary accounts qualify (UID 1000 to 59999), never root, and it prints who it added. Pairing stays closed to the group (see above). Removing the package deletes the user, the group and the state. | `packaging/linux` | done |

## B3: phone and Internet

| | Threat | Mitigation | Owner | Status |
|---|--------|------------|-------|--------|
| S | Forged DNS answers send traffic to the wrong host. | Names resolve on the phone, so queries leave through the phone's VPN or network, not the computer's. | `kite-relay` | done |
| T | Someone alters traffic past the phone. | Out of our hands: Kite does not touch application TLS. | none | n/a |
| I | Traffic or DNS leaves the computer outside the tunnel, or outside the phone's VPN. | The relay opens a real socket on the phone for every connection, so the phone's VPN applies. In TUN mode policy rules send every packet to port 53 and everything not marked as the relay transport into the tunnel, and tun2proxy carries DNS over TCP through it; with systemd-resolved the tunnel link gets the `~.` routing domain. CI checks with packet counters that nothing else leaves. | `kite-relay`, `kite-cli` | done |
| I | Traffic or DNS leaves the computer outside the tunnel (Windows). | Four routes on the Wintun adapter (`0.0.0.0/1`, `128.0.0.0/1`, `::/1`, `8000::/1`) are more specific than any default route, so they take everything the local network does not; the relay's own address gets a host route past them when it needs one. A catch-all DNS rule (NRPT) sends every lookup to 1.1.1.1 through the tunnel, because Windows otherwise asks the DNS server of every adapter at once, and the adapter's own DNS server and metric say the same. Everything is removed on stop and by `kite restore`. The smoke test on a Windows runner checks that the routes, the rule and the adapter appear and that all of it is gone afterwards. | `kite-cli`, `kite-win` | done (Windows), see the known gaps |
| I | The link to the phone drops and traffic falls back to the computer's own network, with its real address and DNS. | With the kill switch (`--kill-switch`, or the switch in the desktop app; off until the user turns it on) the TUN device, its rules and DNS settings stay while the link is down, local connections are refused, and the relay is redialled with backoff; a blackhole route in the tunnel table keeps traffic blocked if kite itself dies, until `kite restore`. The netns CI test cuts the link mid-download and counts zero packets outside the tunnel, then checks that traffic resumes. Without the kill switch a dropped link restores normal routing. There is no kill switch on Windows yet (issue 88): `--kill-switch` is refused. | `kite-cli` | done (Linux), planned (Windows) |
| D | A remote server holds connections open or sends data without end. | Idle timeouts, the stream cap above, bounded copy buffers. | `kite-relay` | done |
| E | The relay is turned into a way to reach the phone's own network (see B1, elevation). | Same deny-list. By design other hosts on the phone's networks, such as other hotspot clients on private addresses, stay reachable, because the user's traffic needs them. The phone's public address as seen by its carrier is not an interface address and cannot be detected. | `kite-relay` | done |

Besides the abstract socket, the relay's only listener is the optional WiFi one in B1.

## Keys and secrets

- Computer: `key` and `phones` in the user's config directory, mode 0600 in a 0700 directory;
  kite refuses files other users can read or write. On Windows the directory (`%APPDATA%\kite`, or
  `%ProgramData%\Kite` for the service) gets a protected access list for the user, SYSTEM and the
  administrators each time kite opens it, and kite refuses a directory that somebody else owns.
  **Done.**
- Phone: the relay takes the key as bytes and wipes them; the app keeps it encrypted with an AES key
  in Android Keystore (iOS Keychain with `ThisDeviceOnly` is planned). **Done** on Android.
- Keys are zeroized after use and never logged. snow 0.10 keeps its own copies in the handshake
  and cipher state without wiping them. **Done**, with that limit.
- Pairing codes expire after 2 minutes, are compared in constant time and never logged by the
  relay. **Done.**
- The repository holds no secrets, signing keys or `.env` files. gitleaks runs on every pull request
  and GitHub secret scanning is on. **Done.**

## Supply chain and releases

| Threat | Mitigation | Status |
|--------|------------|--------|
| A dependency has a known vulnerability, is yanked, or comes from an unexpected source. | `cargo-deny` on every pull request and weekly: advisories, bans, licenses, and only crates.io allowed. Dependabot for crates and Actions. | done |
| A CI action is swapped for a malicious version. | Every action is pinned to a full commit SHA. Workflows default to `contents: read` and never use `pull_request_target`. | done |
| Unsafe or buggy code lands in a parser. | `#![forbid(unsafe_code)]` outside FFI and TUN glue, CodeQL, fuzz targets, review on every pull request. | done |
| `wintun.dll` is swapped for another one. | It is never committed. `scripts/fetch-wintun.ps1` downloads the zip from wintun.net over HTTPS and checks the zip and the DLL against SHA-256 values in the script; the DLL is signed by WireGuard LLC. kite loads it only from its own directory. | done |
| The WiX tool that builds the installer is swapped. | `scripts/build-msi.ps1` downloads WiX 3.14.1 from its release page and checks the zip against a SHA-256 before it runs anything. The MSI is built by CI only and goes through the same checksum, attestation and signature as the other release files. It is not code-signed yet, so Windows warns about an unknown publisher (docs/windows.md). | done |
| The AppImage runtime or a packaging tool is swapped. | The .deb is built with `dpkg-deb` and no other tool. The only download is the AppImage runtime, checked against a SHA-256 in `scripts/package-linux.sh`. | done |
| A release is built or altered somewhere untrusted. | Releases are built only by CI from tags, with SHA-256 checksums, GitHub artifact attestations and a CycloneDX SBOM. OpenSSF Scorecard tracks the repository's practices. | done (see [releasing.md](releasing.md)) |

## Known gaps

- A member of the `kite` group can make the daemon dial any `host:port` and learn whether it
  answered, so the daemon user works as a port scanner for them. The group is trusted with the
  tunnel; keep it small.
- One member of the `kite` group can hold all 8 IPC slots for a few seconds at a time and starve the
  others; there is no per-user cap yet.
- On Windows the clients (`kite`, the desktop app) do not check that the pipe's server is the service. A program that makes the pipe first, while the service is stopped, would see the requests (phone addresses, the pairing code, which cannot be used without the phone's handshake) and could show a false status. Checking the server's process against the service's is planned.
- On Windows a program that sends DNS to a resolver on the local network itself (`nslookup NAME 192.168.1.1`) reaches it directly, because that network's own route is more specific than the tunnel's. Names looked up the usual way go through the rule. Names that only the local network knows stop resolving while the tunnel is up.
- On Windows a crash of the daemon leaves the DNS rule (and a host route for a relay on the Internet) until the service starts again, which removes them, or the next TUN start or `kite restore`. Until then names resolve through 1.1.1.1 without a tunnel, in the clear. A service that is not restarted (stopped by hand) leaves the rule.
- Removing a computer in the app does not end a tunnel it already has up; stop sharing to cut it off.
- No quota per paired computer: a paired computer can use all of the phone's data.
- Proxy mode and TUN mode listen on 127.0.0.1, so any local user can send traffic through the phone
  while kite runs.
- In TUN mode, names under a search domain that a LAN link supplies to systemd-resolved still go to
  that link's resolver, and so do link-local IPv6 resolvers.
- In TUN mode, DNS over TCP to a resolver on the computer's LAN enters the tunnel and fails there,
  because the phone cannot reach that resolver. Only UDP queries to private resolvers are redirected.
- With WiFi sharing on, anyone on the same network can see that a Kite phone is there and open
  a connection, within the caps above. A paired computer that connects while the phone already
  serves 4 computers is closed after the handshake.
- IPv6-only networks are not served by the WiFi listener or by `kite connect --wifi`.
- A device on the link that sees the mDNS question's source port can answer from a spoofed private
  address, such as a routed 10/8 host, and make the computer open one TCP connection to it and
  send an ephemeral Noise key, outside the tunnel in TUN mode. It reads nothing back. Give the
  address with `--wifi ADDRESS` on networks you do not trust.
- With `--wifi` and the kill switch the address is looked up once: if the phone's address changes
  while connected, the redial never succeeds and the block stays until `kite disconnect`.
- `kite pair --wifi` without an address, while a TUN session is up, sends its mDNS question into
  the tunnel and finds nothing; pair before connecting, or give the address.
- The mDNS question tells everyone on the link that this computer looks for a Kite phone. It
  leaves on the default route's interface only, and the phone's address is looked up once, when the
  tunnel starts. Give the address with `--wifi ADDRESS` to skip the question.
- Someone on the same link who can use several source addresses can hold the 8 handshake tickets
  or fill the table of 256 addresses, and a paired computer is refused until their connections
  time out (10 s); they can repeat it. They can also fail pairing attempts on purpose while pairing
  is open and lock it for 60 s. Neither gives them access; both stop the user from connecting
  until they leave the network.
- The switch stays on across networks: a café or hotel WiFi gets the listener as well while
  sharing runs. Turn the switch off there.
- On an open or shared WiFi the link to the phone is encrypted by Noise, but the pairing window
  is open to everyone there: pair on a network you trust.
- Timing and volume of traffic are visible to anyone who can watch the link.

## Keeping this current

Update this file in the same pull request that adds or changes network input, crypto, pairing, IPC or
privileges, and flip the status of a row when its mitigation lands.
