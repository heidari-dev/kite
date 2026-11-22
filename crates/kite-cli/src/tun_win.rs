//! TUN mode on Windows: every connection and DNS query of this computer goes through the relay.
//! A Wintun adapter feeds tun2proxy, which turns its packets into connections to the local proxy
//! from proxy mode, as on Linux.
//!
//! The adapter is set up the way a network card with the best metric would be, so that a second VPN
//! started on top of it treats it as the computer's way out and sends its own connection to its
//! server through it (see docs/architecture.md, "A second VPN"). What it changes, and `restore` takes
//! all of it back:
//!
//! - The adapter `kite0` (198.18.0.1/30, interface metric 1, DNS 1.1.1.1). It goes away with the
//!   process, or stays without routes until `restore`.
//! - Default routes on that adapter, `0.0.0.0/0` through 198.18.0.2 (an address in its /30, so a
//!   VPN that copies the default gateway gets one) and `::/0`, metric 0. With the interface metric of
//!   1 they win over every network card (automatic metrics start at 5) and lose to a second VPN that
//!   sets 0, as WireGuard does; LAN and other specific routes stay as they are. The network card's own
//!   default route is not touched. If a network card still wins, the start fails rather than leak.
//! - A host route for the relay when it is reached through the default route, with the next hop it
//!   had before, so the socket to the phone does not enter its own tunnel. A phone on the LAN or
//!   the phone's hotspot needs none. It is written to `tunnel.state` before it is added.
//! - The DNS guard, `kite_win::firewall::guard_dns`: Windows asks the DNS servers of every adapter
//!   at once, each through its own adapter, so DNS out of a network card is blocked and only the
//!   tunnel's server answers. While a second VPN with DNS servers of its own carries the default
//!   route the adapter offers none, so names go to that VPN's servers alone; it gets 1.1.1.1 back
//!   within two seconds when the VPN goes.
//!
//! With the kill switch (opt-in) Windows Filtering Platform filters also block every connection that
//! does not go through the adapter (or another virtual adapter: a second VPN), to this program's
//! relay or on the loopback, see `kite_win::firewall`. They go up before the routes do and come off
//! after them, and while the phone is away and the session redials they stay. They belong to no
//! session, so they outlive this process: after a crash or an error of the tunnel traffic stays
//! blocked until `restore`, the next connect or a reboot. Without the kill switch a crash leaves the
//! adapter's routes in place only while the adapter exists, and the DNS guard until the next start
//! or `restore`.
//!
//! Every filter Kite adds only blocks or softly permits, so a second VPN's kill switch keeps
//! blocking whatever it blocks, whether Kite runs or not.

use kite_win::firewall;
use kite_win::route::{self, Route};
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tun::AbstractDevice;
use tun2proxy::{ArgDns, Args, CancellationToken, ProxyParameters};

pub const NAME: &str = "kite0";
const MTU: u16 = 1500;
/// 198.18.0.0/15 is reserved for benchmarking, so it does not clash with a LAN.
const ADDR: Ipv4Addr = Ipv4Addr::new(198, 18, 0, 1);
/// The relay serves udpgw on this reserved address (see docs/architecture.md).
const UDPGW: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 7300);
/// The default route's next hop: the other address of the adapter's /30.
const GATEWAY: Ipv4Addr = Ipv4Addr::new(198, 18, 0, 2);
/// DNS queries go here, as DNS over TCP through the relay.
const DNS: IpAddr = IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1));
/// The adapter's identity, fixed so Windows keeps one network profile for it across runs.
const GUID: u128 = 0x6b69_7465_0000_4000_8000_0a11_2233_4455;
/// The comment of the catch-all DNS rule that versions up to 0.1.0-rc.2 added.
const RULE: &str = "kite";
/// What `undo` needs to know: the host route for the relay.
const STATE: &str = "tunnel.state";
/// The state versions up to 0.1.0-rc.2 wrote; it also means their DNS rule may still be there.
const LEGACY_STATE: &str = "tun.state";
/// Addresses whose route tells which adapter carries the default route now (documentation ranges,
/// never sent anywhere).
const PROBES: [IpAddr; 2] = [
    IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1)),
    IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
];
/// How often the adapter's DNS server follows a second VPN coming and going.
const FOLLOW: Duration = Duration::from_secs(2);
/// Longest wait for PowerShell.
const POWERSHELL_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the tunnel's tasks get to finish once the tunnel is told to end. Past it they are
/// dropped: with many open connections, or a task that does not answer, a disconnect would
/// otherwise not end.
const TEARDOWN: Duration = Duration::from_secs(4);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The default routes, IPv4 through the gateway and IPv6 on the link.
const DEFAULTS: [(IpAddr, IpAddr); 2] = [
    (IpAddr::V4(Ipv4Addr::UNSPECIFIED), IpAddr::V4(GATEWAY)),
    (
        IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    ),
];

/// The routes versions up to 0.1.0-rc.2 used, each half of the address space; `undo` still deletes
/// them.
const HALVES: [(IpAddr, u8); 4] = [
    (IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 1),
    (IpAddr::V4(Ipv4Addr::new(128, 0, 0, 0)), 1),
    (IpAddr::V6(Ipv6Addr::UNSPECIFIED), 1),
    (IpAddr::V6(Ipv6Addr::new(0x8000, 0, 0, 0, 0, 0, 0, 0)), 1),
];

/// Lets the kill switch pass a dial to `relay`, and with `discovery` this program's mDNS lookup of
/// the phone. The block is replaced in one step, so nothing leaks between the old rules and the new.
pub async fn allow(relay: Option<SocketAddr>, discovery: bool) -> io::Result<()> {
    let luid = route::luid_of_alias(NAME)?
        .ok_or_else(|| io::Error::other("the tunnel adapter is gone"))?;
    firewall::engage(luid, relay.map(|a| a.ip().to_canonical()), discovery)
}

/// Runs TUN mode until `shutdown` completes or tun2proxy fails. `serve_link` carries the traffic
/// that reaches the proxy at `proxy` through a tunnel to the relay, until the token it is given is
/// cancelled. `relay` is the address that tunnel connects to, so it can stay out of the tunnel.
///
/// With `kill_switch`, everything but the tunnel is blocked, and stays so when this ends for any
/// reason but `shutdown`, until `restore`.
pub async fn run<F, Fut>(
    proxy: SocketAddr,
    relay: Option<SocketAddr>,
    kill_switch: bool,
    serve_link: F,
    shutdown: impl Future<Output = ()>,
) -> io::Result<()>
where
    F: FnOnce(CancellationToken) -> Fut,
    Fut: Future<Output = io::Result<()>>,
{
    // A dual-stack connection reports the relay as ::ffff:a.b.c.d; it is a.b.c.d to routes and filters.
    let relay = relay.map(|a| a.ip().to_canonical());
    let _lock = lock()?;
    // Whatever a kite that was killed left behind. A block stays for the kill switch to replace
    // atomically; without the kill switch it goes.
    undo(!kill_switch).await?;
    let device = create()?;
    serve(device, proxy, relay, kill_switch, serve_link, shutdown).await
}

/// Deletes what a TUN mode run that was killed left behind. Fails while another one is running.
pub async fn restore() -> io::Result<()> {
    let _lock = lock()?;
    undo(true).await
}

/// As `restore`, but a kill switch's block stays: for the service that starts again after a crash,
/// which must not lift what the crashed run put up for the user.
pub async fn restore_keeping_block() -> io::Result<()> {
    let _lock = lock()?;
    undo(false).await
}

/// Where the lock and the state live: the daemon's private directory.
fn dir() -> io::Result<PathBuf> {
    let dir = kite_win::data_dir();
    kite_win::private_dir(&dir)?;
    Ok(dir)
}

/// Held for as long as TUN mode runs; it is also what keeps one kite from undoing another one's
/// setup. The system releases it when the process ends, however it ends.
fn lock() -> io::Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir()?.join("tun.lock"))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => {
            Err(io::Error::other("another kite is already running TUN mode"))
        }
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// `wintun.dll` next to `kite.exe`, never searched for anywhere else: a program that loads a DLL
/// into an administrator's process by a search path hands that process to whoever can write there.
fn dll() -> io::Result<PathBuf> {
    let dll = std::env::current_exe()?.with_file_name("wintun.dll");
    if dll.is_file() {
        Ok(dll)
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "wintun.dll is not next to kite.exe ({}); the installer puts it there, or run \
                 scripts/fetch-wintun.ps1 for a build from source",
                dll.display()
            ),
        ))
    }
}

/// The Wintun adapter. It is created already holding its address and DNS server.
fn create() -> io::Result<tun::AsyncDevice> {
    let dll = dll()?;
    let mut cfg = tun::Configuration::default();
    cfg.tun_name(NAME)
        .address(ADDR)
        .netmask(Ipv4Addr::new(255, 255, 255, 252))
        .mtu(MTU)
        .metric(1)
        .up();
    // No DNS server yet: it is set after the DNS guard is up.
    cfg.platform_config(|p| {
        p.wintun_file(dll.as_os_str());
        p.device_guid(GUID);
    });
    tun::create_as_async(&cfg).map_err(|e| io::Error::other(format!("creating {NAME}: {e}")))
}

async fn serve<F, Fut>(
    device: tun::AsyncDevice,
    proxy: SocketAddr,
    relay: Option<IpAddr>,
    kill_switch: bool,
    serve_link: F,
    shutdown: impl Future<Output = ()>,
) -> io::Result<()>
where
    F: FnOnce(CancellationToken) -> Fut,
    Fut: Future<Output = io::Result<()>>,
{
    let index =
        u32::try_from(device.tun_index().map_err(io::Error::other)?).map_err(io::Error::other)?;
    let luid = route::luid_of_index(index)?;
    // Polled from the start, so a signal during setup still ends in a clean restore.
    tokio::pin!(shutdown);
    tokio::select! {
        r = setup(luid, relay, kill_switch) => {
            if let Err(e) = r {
                // A failed start, not a lost tunnel: it restores as usual.
                let _ = undo(true).await;
                return Err(e);
            }
        }
        () = &mut shutdown => return undo(true).await,
    }
    // Set when the user ends the tunnel; any other end leaves the kill switch's block up.
    let stopped = std::cell::Cell::new(false);

    let proxy_url = format!("socks5://{proxy}");
    let mut args = Args::default();
    args.proxy(ProxyParameters::try_from(proxy_url.as_str()).map_err(io::Error::other)?)
        .dns(ArgDns::OverTcp)
        .dns_addr(DNS)
        .udpgw_server(UDPGW)
        // As on Linux: an IPv6 target the phone cannot reach would look connected and then fail.
        // IPv6 still enters the tunnel; it just does not leak.
        .ipv6_enabled(false);

    let token = CancellationToken::new();
    let work = async {
        let (proxied, tunneled, (), ()) = tokio::join!(
            async {
                let r = serve_link(token.clone()).await;
                token.cancel();
                r
            },
            async {
                let r = tun2proxy::run(device, MTU, args, token.clone()).await;
                token.cancel();
                r
            },
            async {
                tokio::select! {
                    () = shutdown => {
                    stopped.set(true);
                    token.cancel();
                }
                    () = token.cancelled() => {}
                }
            },
            async {
                tokio::select! {
                    () = follow_dns(luid) => {}
                    () = token.cancelled() => {}
                }
            },
        );
        proxied.and(tunneled.map(drop).map_err(io::Error::from))
    };
    tokio::pin!(work);
    // From the moment the tunnel is told to end, the routes, the DNS rule and the block come off at
    // once, beside the tasks winding down, so the computer is back on its own network whatever
    // those tasks do. The tasks get TEARDOWN, then they are dropped.
    let (result, cleaned) = tokio::join!(
        async {
            tokio::select! {
                r = &mut work => r,
                () = async {
                    token.cancelled().await;
                    tokio::time::sleep(TEARDOWN).await;
                } => Err(io::Error::other("the tunnel did not stop in time and was dropped")),
            }
        },
        async {
            token.cancelled().await;
            undo(stopped.get() || !kill_switch).await
        },
    );
    if kill_switch && !stopped.get() {
        return result.and(cleaned).and(Err(io::Error::other(
            "the tunnel ended; traffic stays blocked until `kite restore`",
        )));
    }
    result.and(cleaned)
}

/// The default routes of the tunnel on the adapter with `luid`.
fn tunnel_routes(luid: u64) -> impl Iterator<Item = Route> {
    DEFAULTS.into_iter().map(move |(dest, next_hop)| Route {
        luid,
        dest,
        prefix: 0,
        next_hop,
    })
}

/// The routes of versions up to 0.1.0-rc.2, on-link.
fn legacy_routes(luid: u64) -> impl Iterator<Item = Route> {
    HALVES.into_iter().map(move |(dest, prefix)| Route {
        luid,
        dest,
        prefix,
        next_hop: if dest.is_ipv4() {
            Ipv4Addr::UNSPECIFIED.into()
        } else {
            Ipv6Addr::UNSPECIFIED.into()
        },
    })
}

async fn setup(luid: u64, relay: Option<IpAddr>, kill_switch: bool) -> io::Result<()> {
    // Before the routes change, so nothing leaks between the two.
    if kill_switch {
        firewall::engage(luid, relay, false)?;
    }
    // The relay's way out, before the tunnel's routes change it. A more specific route (a LAN)
    // keeps winning on its own; a default route, or the halves another VPN put over it, need a host
    // route beside them.
    let bypass = match relay.filter(|ip| !ip.is_loopback()) {
        Some(ip) => {
            let best = route::best_to(ip)?;
            (best.prefix < 2).then(|| Route {
                luid: best.luid,
                dest: ip,
                prefix: if ip.is_ipv4() { 32 } else { 128 },
                next_hop: best.next_hop,
            })
        }
        None => None,
    };
    State { bypass }.save(STATE)?;
    if let Some(r) = &bypass {
        route::add(r, 1)?;
    }
    let mut ipv6 = true;
    for r in tunnel_routes(luid) {
        match route::add(&r, 0) {
            // No IPv6 on this adapter or this host is the usual reason; say so, so that a failure
            // on a host that has IPv6 is not silent.
            Err(e) if r.dest.is_ipv6() => {
                eprintln!("kite: IPv6 is not taken into the tunnel: {e}");
                ipv6 = false;
            }
            r => r?,
        }
    }
    for probe in PROBES.into_iter().filter(|p| ipv6 || p.is_ipv4()) {
        // No route at all (no IPv6 on this host) is nothing to win against.
        let Ok(best) = route::best_to(probe) else {
            continue;
        };
        if best.luid != luid && route::is_card(best.luid)? {
            return Err(io::Error::other(
                "a network card's default route has a better metric than the tunnel's, so traffic \
                 would not enter the tunnel; set that card's interface metric to automatic or above 1",
            ));
        }
    }
    firewall::guard_dns()?;
    route::set_dns(GUID, &DNS.to_string())
}

/// While the tunnel runs: the adapter offers no DNS server while a second VPN that has DNS servers
/// of its own carries the default route, so that names go to that VPN's servers, not around it. A
/// route that ends on a network card means no second VPN, and a failure to look leaves the setting
/// as it is.
async fn follow_dns(luid: u64) {
    let mut offered = true;
    loop {
        tokio::time::sleep(FOLLOW).await;
        let Ok(best) = route::best_to(PROBES[0]) else {
            continue;
        };
        let second_vpn = best.luid != luid
            && route::is_card(best.luid).is_ok_and(|card| !card)
            && route::has_dns_servers(best.luid).unwrap_or(false);
        let offer = !second_vpn;
        if offer != offered {
            let servers = if offer {
                DNS.to_string()
            } else {
                String::new()
            };
            match route::set_dns(GUID, &servers) {
                Ok(()) => offered = offer,
                Err(e) => eprintln!("kite: changing the tunnel's DNS server: {e}"),
            }
        }
    }
}

/// Everything TUN mode adds, taken away; what is not there is skipped, so this runs again and
/// again. The first error is returned after the rest has been tried. The kill switch's block, if
/// there is one, is lifted last, and only with `lift`.
async fn undo(lift: bool) -> io::Result<()> {
    let mut first = Ok(());
    let mut note = |r: io::Result<()>| {
        if first.is_ok() {
            first = r;
        }
    };
    match route::luid_of_alias(NAME) {
        Ok(Some(luid)) => tunnel_routes(luid)
            .chain(legacy_routes(luid))
            .for_each(|r| note(route::delete(&r))),
        Ok(None) => {}
        Err(e) => note(Err(e)),
    }
    for name in [STATE, LEGACY_STATE] {
        match State::load(name) {
            Ok(State { bypass: Some(r) }) => note(route::delete(&r)),
            Ok(_) => {}
            Err(e) => note(Err(e)),
        }
    }
    // Only an earlier version adds the catch-all DNS rule, after it writes its state, and its state
    // goes only once the rule is gone: without the file there is no rule, and no reason to start
    // PowerShell. If the file cannot be checked, look anyway.
    let legacy = dir().and_then(|d| d.join(LEGACY_STATE).try_exists());
    if legacy.unwrap_or(true) {
        note(
            powershell(&format!(
                "Get-DnsClientNrptRule | Where-Object {{ $_.Comment -eq '{RULE}' }} | \
                 Remove-DnsClientNrptRule -Force"
            ))
            .await,
        );
    }
    note(firewall::unguard_dns());
    if lift {
        note(firewall::lift());
    }
    if first.is_ok() {
        let dir = dir()?;
        for name in [STATE, LEGACY_STATE] {
            let _ = std::fs::remove_file(dir.join(name));
        }
    }
    first
}

/// What `undo` cannot work out from the system: the host route for the relay. One line,
/// `bypass LUID DESTINATION NEXT_HOP`, or an empty file.
#[derive(Debug, Default, PartialEq)]
struct State {
    bypass: Option<Route>,
}

impl State {
    fn text(&self) -> String {
        self.bypass.map_or_else(String::new, |r| {
            format!("bypass {} {} {}\n", r.luid, r.dest, r.next_hop)
        })
    }

    fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        let Some(line) = lines.next() else {
            return Some(Self::default());
        };
        let mut words = line.split(' ');
        let ("bypass", Some(luid), Some(dest), Some(hop), None) = (
            words.next()?,
            words.next(),
            words.next(),
            words.next(),
            words.next(),
        ) else {
            return None;
        };
        let (dest, next_hop): (IpAddr, IpAddr) = (dest.parse().ok()?, hop.parse().ok()?);
        if lines.next().is_some() || dest.is_ipv4() != next_hop.is_ipv4() {
            return None;
        }
        Some(Self {
            bypass: Some(Route {
                luid: luid.parse().ok()?,
                dest,
                prefix: if dest.is_ipv4() { 32 } else { 128 },
                next_hop,
            }),
        })
    }

    fn save(&self, name: &str) -> io::Result<()> {
        std::fs::write(dir()?.join(name), self.text())
    }

    /// A missing file is no state; a damaged one is an error, not a guess.
    fn load(name: &str) -> io::Result<Self> {
        match std::fs::read_to_string(dir()?.join(name)) {
            Ok(text) => Self::parse(&text)
                .ok_or_else(|| io::Error::other(format!("{name} is damaged; remove it"))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }
}

/// Runs a PowerShell command, with the path of the system's own copy and the default module path,
/// so nothing in this process's environment decides what code runs.
async fn powershell(script: &str) -> io::Result<()> {
    let root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| io::Error::other("SystemRoot is not set"))?;
    let exe = root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let run = tokio::process::Command::new(exe)
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(format!(
            "$ErrorActionPreference = 'Stop'; {script}; Clear-DnsClientCache"
        ))
        .env_remove("PSModulePath")
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(POWERSHELL_TIMEOUT, run)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "PowerShell did not answer"))??;
    if out.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "changing the DNS settings failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_survives_a_round_trip() {
        let v4 = Route {
            luid: 1_689_399_632_855_040,
            dest: "203.0.113.9".parse().unwrap(),
            prefix: 32,
            next_hop: "192.168.1.1".parse().unwrap(),
        };
        let v6 = Route {
            dest: "2001:db8::9".parse().unwrap(),
            prefix: 128,
            next_hop: "fe80::1".parse().unwrap(),
            ..v4
        };
        for r in [v4, v6] {
            let state = State { bypass: Some(r) };
            assert_eq!(State::parse(&state.text()), Some(state));
        }
        assert_eq!(State::parse(""), Some(State::default()));
        assert_eq!(State::default().text(), "");
    }

    #[test]
    fn a_damaged_state_is_refused() {
        for bad in [
            "bypass",
            "bypass 1 203.0.113.9",
            "bypass x 203.0.113.9 192.168.1.1",
            "bypass 1 not-an-ip 192.168.1.1",
            "bypass 1 203.0.113.9 fe80::1",
            "bypass 1 203.0.113.9 192.168.1.1 extra",
            "bypass 1 203.0.113.9 192.168.1.1\nbypass 2 203.0.113.9 192.168.1.1\n",
            "route 1 203.0.113.9 192.168.1.1",
        ] {
            assert_eq!(State::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_tunnel_is_the_default_route_of_each_family() {
        let routes: Vec<_> = tunnel_routes(7).collect();
        assert_eq!(routes.len(), 2);
        assert!(routes
            .iter()
            .all(|r| r.prefix == 0 && r.luid == 7 && r.dest.is_unspecified()));
        // A VPN that copies the default gateway for its server's host route needs an address, and
        // one that is on the adapter's /30.
        let v4 = routes.iter().find(|r| r.dest.is_ipv4()).unwrap();
        assert_eq!(v4.next_hop, IpAddr::V4(GATEWAY));
        assert_eq!(GATEWAY.octets()[..3], ADDR.octets()[..3]);
        assert_eq!(GATEWAY.octets()[3] & !3, ADDR.octets()[3] & !3);
        assert_ne!(GATEWAY, ADDR);
        let v6 = routes.iter().find(|r| r.dest.is_ipv6()).unwrap();
        assert!(v6.next_hop.is_unspecified() && v6.next_hop.is_ipv6());
        // The routes of earlier versions are still known, for `undo`.
        assert!(legacy_routes(7).all(|r| r.prefix == 1));
    }
}
