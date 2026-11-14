//! TUN mode on Windows: every connection and DNS query of this computer goes through the relay.
//! A Wintun adapter feeds tun2proxy, which turns its packets into connections to the local proxy
//! from proxy mode, as on Linux.
//!
//! What it changes, and `restore` takes all of it back:
//!
//! - The adapter `kite0` (198.18.0.1/30, interface metric 1, DNS 1.1.1.1). It goes away with the
//!   process, or stays without routes until `restore`.
//! - Four routes on that adapter, `0.0.0.0/1`, `128.0.0.0/1`, `::/1` and `8000::/1`: together they
//!   take everything that has no more specific route, so the LAN and the other adapters' own
//!   subnets stay as they are and the real default route is not touched.
//! - A host route for the relay when it is reached through the default route, with the next hop it
//!   had before, so the socket to the phone does not enter its own tunnel. A phone on the LAN or
//!   the phone's hotspot needs none. It is written to `tun.state` before it is added.
//! - A catch-all DNS rule (NRPT, comment `kite`) that sends every name lookup to 1.1.1.1, which
//!   the tunnel carries to the phone. Without it Windows asks the DNS servers of all adapters at
//!   once and the answer from the local one wins the race.
//!
//! There is no kill switch on Windows yet: a crash leaves the adapter's routes in place only while
//! the adapter exists, and the DNS rule until the next start or `restore`.

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
/// DNS queries go here, as DNS over TCP through the relay.
const DNS: IpAddr = IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1));
/// The adapter's identity, fixed so Windows keeps one network profile for it across runs.
const GUID: u128 = 0x6b69_7465_0000_4000_8000_0a11_2233_4455;
/// The comment that marks the DNS rule as ours.
const RULE: &str = "kite";
/// Longest wait for PowerShell.
const POWERSHELL_TIMEOUT: Duration = Duration::from_secs(20);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The routes that take all traffic into the tunnel: each half of the address space.
const HALVES: [(IpAddr, u8); 4] = [
    (IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 1),
    (IpAddr::V4(Ipv4Addr::new(128, 0, 0, 0)), 1),
    (IpAddr::V6(Ipv6Addr::UNSPECIFIED), 1),
    (IpAddr::V6(Ipv6Addr::new(0x8000, 0, 0, 0, 0, 0, 0, 0)), 1),
];

/// Runs TUN mode until `shutdown` completes or tun2proxy fails. `serve_link` carries the traffic
/// that reaches the proxy at `proxy` through a tunnel to the relay, until the token it is given is
/// cancelled. `relay` is the address that tunnel connects to, so it can stay out of the tunnel.
///
/// There is no kill switch here, see the module comment: `kill_switch` must be false.
pub async fn run<F, Fut>(
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
    if kill_switch {
        return Err(io::Error::other(
            "the kill switch is not available on Windows yet",
        ));
    }
    let _lock = lock()?;
    // Whatever a kite that was killed left behind.
    undo().await?;
    let device = create()?;
    let result = serve(device, proxy, relay, serve_link, shutdown).await;
    result.and(undo().await)
}

/// Deletes what a TUN mode run that was killed left behind. Fails while another one is running.
pub async fn restore() -> io::Result<()> {
    let _lock = lock()?;
    undo().await
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
    cfg.platform_config(|p| {
        p.wintun_file(dll.as_os_str());
        p.device_guid(GUID);
        p.dns_servers(&[DNS]);
    });
    tun::create_as_async(&cfg).map_err(|e| io::Error::other(format!("creating {NAME}: {e}")))
}

async fn serve<F, Fut>(
    device: tun::AsyncDevice,
    proxy: SocketAddr,
    relay: Option<IpAddr>,
    serve_link: F,
    shutdown: impl Future<Output = ()>,
) -> io::Result<()>
where
    F: FnOnce(CancellationToken) -> Fut,
    Fut: Future<Output = io::Result<()>>,
{
    let index =
        u32::try_from(device.tun_index().map_err(io::Error::other)?).map_err(io::Error::other)?;
    // Polled from the start, so a signal during setup still ends in a clean restore.
    tokio::pin!(shutdown);
    tokio::select! {
        r = setup(index, relay) => r?,
        () = &mut shutdown => return Ok(()),
    }

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
    let (proxied, tunneled, ()) = tokio::join!(
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
                () = shutdown => token.cancel(),
                () = token.cancelled() => {}
            }
        },
    );
    proxied.and(tunneled.map(drop).map_err(io::Error::from))
}

/// The routes of the tunnel on the adapter with `luid`.
fn tunnel_routes(luid: u64) -> impl Iterator<Item = Route> {
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

async fn setup(index: u32, relay: Option<IpAddr>) -> io::Result<()> {
    let luid = route::luid_of_index(index)?;
    // The relay's way out, before the tunnel's routes change it. A more specific route than the
    // halves (a LAN) keeps winning on its own; only the default route needs a host route beside it.
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
    State { bypass }.save()?;
    if let Some(r) = &bypass {
        route::add(r, 1)?;
    }
    for r in tunnel_routes(luid) {
        match route::add(&r, 0) {
            // No IPv6 on this adapter or this host: there is no IPv6 traffic to take.
            Err(_) if r.dest.is_ipv6() => {}
            r => r?,
        }
    }
    powershell(&format!(
        "Add-DnsClientNrptRule -Namespace '.' -NameServers '{DNS}' -Comment '{RULE}'"
    ))
    .await
}

/// Everything TUN mode adds, taken away; what is not there is skipped, so this runs again and
/// again. The first error is returned after the rest has been tried.
async fn undo() -> io::Result<()> {
    let mut first = Ok(());
    let mut note = |r: io::Result<()>| {
        if first.is_ok() {
            first = r;
        }
    };
    match route::luid_of_alias(NAME) {
        Ok(Some(luid)) => tunnel_routes(luid).for_each(|r| note(route::delete(&r))),
        Ok(None) => {}
        Err(e) => note(Err(e)),
    }
    match State::load() {
        Ok(State { bypass: Some(r) }) => note(route::delete(&r)),
        Ok(_) => {}
        Err(e) => note(Err(e)),
    }
    note(
        powershell(&format!(
            "Get-DnsClientNrptRule | Where-Object {{ $_.Comment -eq '{RULE}' }} | \
             Remove-DnsClientNrptRule -Force"
        ))
        .await,
    );
    if first.is_ok() {
        let _ = std::fs::remove_file(dir()?.join("tun.state"));
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

    fn save(&self) -> io::Result<()> {
        std::fs::write(dir()?.join("tun.state"), self.text())
    }

    /// A missing file is no state; a damaged one is an error, not a guess.
    fn load() -> io::Result<Self> {
        match std::fs::read_to_string(dir()?.join("tun.state")) {
            Ok(text) => Self::parse(&text)
                .ok_or_else(|| io::Error::other("tun.state is damaged; remove it")),
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
    fn the_halves_cover_each_family_without_a_default_route() {
        let routes: Vec<_> = tunnel_routes(7).collect();
        assert_eq!(routes.len(), 4);
        assert!(routes.iter().all(|r| r.prefix == 1 && r.luid == 7));
        assert_eq!(routes.iter().filter(|r| r.dest.is_ipv4()).count(), 2);
        assert!(routes.iter().all(|r| r.next_hop.is_unspecified()));
    }
}
