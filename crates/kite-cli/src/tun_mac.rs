//! TUN mode on macOS: every connection and DNS query of this computer goes through the relay.
//! A utun device feeds tun2proxy, which turns its packets into connections to the local proxy
//! from proxy mode, as on Linux and Windows. It needs root.
//!
//! What it changes, and `restore` takes all of it back:
//!
//! - The device `utunN` (the system picks N; 198.18.0.1 to 198.18.0.2). It goes away with the
//!   process, and the kernel removes its routes with it.
//! - Four routes on that device, `0.0.0.0/1`, `128.0.0.0/1`, `::/1` and `8000::/1`: together they
//!   take everything that has no more specific route, so the LAN stays as it is and the real
//!   default route is not touched.
//! - A host route for the relay when it is reached through the default route, through the gateway
//!   (or interface) it had before, so the socket to the phone does not enter its own tunnel. A
//!   phone on the LAN needs none. It is written to `tun.state` before it is added.
//! - A DNS resolver in the system configuration (`State:/Network/Service/kite/DNS`) that matches
//!   every name and sends it to 1.1.1.1, which the tunnel carries to the phone.
//!
//! There is no kill switch on macOS yet: a crash leaves the DNS resolver until the next start of
//! the daemon or `restore`, and the host route likewise.
//!
//! Every command runs by its full path with an empty environment, and the only values that reach a
//! command line are parsed IP addresses and interface names checked for their characters.

use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tun::AbstractDevice;
use tun2proxy::{ArgDns, Args, CancellationToken, ProxyParameters};

const MTU: u16 = 1500;
/// 198.18.0.0/15 is reserved for benchmarking, so it does not clash with a LAN.
const ADDR: Ipv4Addr = Ipv4Addr::new(198, 18, 0, 1);
const PEER: Ipv4Addr = Ipv4Addr::new(198, 18, 0, 2);
/// The relay serves udpgw on this reserved address (see docs/architecture.md).
const UDPGW: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 7300);
/// DNS queries go here, as DNS over TCP through the relay.
const DNS: IpAddr = IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1));
const DNS_KEY: &str = "State:/Network/Service/kite/DNS";
/// Where the lock and the state live: the daemon's directory, for root only.
const STATE_DIR: &str = "/var/db/kite";
const ROUTE: &str = "/sbin/route";
const SCUTIL: &str = "/usr/sbin/scutil";
const DSCACHEUTIL: &str = "/usr/bin/dscacheutil";
const PKILL: &str = "/usr/bin/pkill";
/// Longest wait for one command.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the tunnel's tasks get to finish once the tunnel is told to end. Past it they are
/// dropped: with many open connections, or a task that does not answer, a disconnect would
/// otherwise not end.
const TEARDOWN: Duration = Duration::from_secs(4);

/// The routes that take all traffic into the tunnel: each half of the address space.
const HALVES_V4: [&str; 2] = ["0.0.0.0/1", "128.0.0.0/1"];
const HALVES_V6: [&str; 2] = ["::/1", "8000::/1"];

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
            "the kill switch is not available on macOS yet",
        ));
    }
    let _lock = lock()?;
    // Whatever a kite that was killed left behind.
    undo().await?;
    let device = create()?;
    serve(device, proxy, relay, serve_link, shutdown).await
}

/// Deletes what a TUN mode run that was killed left behind. Fails while another one is running.
pub async fn restore() -> io::Result<()> {
    let _lock = lock()?;
    undo().await
}

/// The daemon's directory, made for root only if it is missing. One that somebody else owns or can
/// open is refused: the state in it says which routes to delete.
fn dir() -> io::Result<PathBuf> {
    let dir = PathBuf::from(STATE_DIR);
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            return Err(io::Error::new(e.kind(), "TUN mode needs root; try sudo"));
        }
        r => r?,
    }
    let meta = std::fs::metadata(&dir)?;
    if meta.uid() != 0 || meta.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::other(format!(
            "{STATE_DIR} must belong to root and be closed to other users"
        )));
    }
    Ok(dir)
}

/// Held for as long as TUN mode runs; it is also what keeps one kite from undoing another one's
/// setup. The system releases it when the process ends, however it ends.
fn lock() -> io::Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(dir()?.join("tun.lock"))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => {
            Err(io::Error::other("another kite is already running TUN mode"))
        }
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// The utun device, with the name the system gives it. It is created already holding its address.
fn create() -> io::Result<tun::AsyncDevice> {
    let mut cfg = tun::Configuration::default();
    cfg.address(ADDR)
        .destination(PEER)
        .netmask(Ipv4Addr::new(255, 255, 255, 252))
        .mtu(MTU)
        .up();
    tun::create_as_async(&cfg)
        .map_err(|e| io::Error::other(format!("creating the utun device (needs root): {e}")))
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
    let name = device.tun_name().map_err(io::Error::other)?;
    if !valid_interface(&name) {
        return Err(io::Error::other(
            "the system gave the device a strange name",
        ));
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

    // Polled from the start, so a signal during setup still ends in a clean restore.
    tokio::pin!(shutdown);
    tokio::select! {
        r = setup(&name, relay) => {
            if let Err(e) = r {
                let _ = undo().await;
                return Err(e);
            }
        }
        () = &mut shutdown => return undo().await,
    }

    let token = CancellationToken::new();
    let work = async {
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
    };
    tokio::pin!(work);
    // From the moment the tunnel is told to end, the host route and the DNS resolver come off at
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
            undo().await
        },
    );
    result.and(cleaned)
}

async fn setup(name: &str, relay: Option<IpAddr>) -> io::Result<()> {
    // The relay's way out, before the tunnel's routes change it. A more specific route than the
    // halves (a LAN) keeps winning on its own; only the default route needs a host route beside it.
    let bypass = match relay.filter(|ip| !ip.is_loopback()) {
        Some(dest) => default_via(&route_get(dest).await?)?.map(|via| Bypass { dest, via }),
        None => None,
    };
    let state = State { bypass };
    state.save()?;
    if let Some(b) = &state.bypass {
        route(&b.args("add")).await?;
    }
    for half in HALVES_V4 {
        route(&["-n", "add", "-inet", half, "-interface", name]).await?;
    }
    for half in HALVES_V6 {
        // No IPv6 on this host is the usual reason; say so, so that a failure on a host that has
        // IPv6 is not silent.
        if let Err(e) = route(&["-n", "add", "-inet6", half, "-interface", name]).await {
            eprintln!("kite: IPv6 is not taken into the tunnel: {e}");
            break;
        }
    }
    let script = format!(
        "d.init\nd.add ServerAddresses * {DNS}\nd.add SupplementalMatchDomains * \"\"\nset {DNS_KEY}\n"
    );
    scutil(&script).await?;
    flush_dns().await;
    Ok(())
}

/// Everything TUN mode adds that outlives the device, taken away; what is not there is skipped, so
/// this runs again and again. The first error is returned after the rest has been tried.
async fn undo() -> io::Result<()> {
    let mut first = Ok(());
    let mut note = |r: io::Result<()>| {
        if first.is_ok() {
            first = r;
        }
    };
    match State::load() {
        Ok(State { bypass: Some(b) }) => note(route(&b.args("delete")).await),
        Ok(_) => {}
        Err(e) => note(Err(e)),
    }
    note(scutil(&format!("remove {DNS_KEY}\n")).await);
    flush_dns().await;
    if first.is_ok() {
        let _ = std::fs::remove_file(dir()?.join("tun.state"));
    }
    first
}

/// The host route that keeps the relay out of the tunnel.
#[derive(Debug, Clone, PartialEq)]
struct Bypass {
    dest: IpAddr,
    via: Via,
}

#[derive(Debug, Clone, PartialEq)]
enum Via {
    /// An IP address, with the interface after a `%` for a link-local IPv6 one.
    Gateway(String),
    Interface(String),
}

impl Bypass {
    /// The arguments of `route` that add or delete this host route.
    fn args(&self, verb: &str) -> Vec<String> {
        let mut args = vec!["-n".to_string(), verb.to_string()];
        if self.dest.is_ipv6() {
            args.push("-inet6".into());
        }
        args.extend(["-host".into(), self.dest.to_string()]);
        match &self.via {
            Via::Gateway(g) => args.push(g.clone()),
            Via::Interface(i) => args.extend(["-interface".into(), i.clone()]),
        }
        args
    }
}

/// Interface names: letters and digits, as `en0`, `bridge100`, `utun4`.
fn valid_interface(name: &str) -> bool {
    (1..=15).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn valid_gateway(g: &str) -> bool {
    let (ip, scope) = g.split_once('%').map_or((g, None), |(ip, s)| (ip, Some(s)));
    ip.parse::<IpAddr>().is_ok() && scope.is_none_or(valid_interface)
}

/// Reads `route get`: how a packet to the address leaves now, or None when a more specific route
/// than the default one takes it (a LAN, the host itself). Only a gateway that is an address or
/// an interface that is a plain name is used.
fn default_via(text: &str) -> io::Result<Option<Via>> {
    let field = |name: &str| {
        text.lines()
            .find_map(|l| l.trim().strip_prefix(name))
            .map(str::trim)
    };
    if field("destination:") != Some("default") {
        return Ok(None);
    }
    if let Some(g) = field("gateway:").filter(|g| valid_gateway(g)) {
        return Ok(Some(Via::Gateway(g.to_string())));
    }
    match field("interface:").filter(|i| valid_interface(i)) {
        Some(i) => Ok(Some(Via::Interface(i.to_string()))),
        None => Err(io::Error::other(
            "cannot tell how the relay is reached from `route get`",
        )),
    }
}

/// What `undo` cannot work out from the system: the host route for the relay. One line,
/// `bypass DESTINATION via GATEWAY` or `bypass DESTINATION dev INTERFACE`, or an empty file.
#[derive(Debug, Default, PartialEq)]
struct State {
    bypass: Option<Bypass>,
}

impl State {
    fn text(&self) -> String {
        self.bypass
            .as_ref()
            .map_or_else(String::new, |b| match &b.via {
                Via::Gateway(g) => format!("bypass {} via {g}\n", b.dest),
                Via::Interface(i) => format!("bypass {} dev {i}\n", b.dest),
            })
    }

    fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        let Some(line) = lines.next() else {
            return Some(Self::default());
        };
        let mut words = line.split(' ');
        let ("bypass", Some(dest), Some(how), Some(value), None) = (
            words.next()?,
            words.next(),
            words.next(),
            words.next(),
            words.next(),
        ) else {
            return None;
        };
        if lines.next().is_some() {
            return None;
        }
        let via = match how {
            "via" if valid_gateway(value) => Via::Gateway(value.to_string()),
            "dev" if valid_interface(value) => Via::Interface(value.to_string()),
            _ => return None,
        };
        let dest: IpAddr = dest.parse().ok()?;
        // The gateway of an IPv4 route is an IPv4 address, and of an IPv6 route an IPv6 one.
        if let Via::Gateway(g) = &via {
            let hop: IpAddr = g.split('%').next()?.parse().ok()?;
            if hop.is_ipv4() != dest.is_ipv4() {
                return None;
            }
        }
        Some(Self {
            bypass: Some(Bypass { dest, via }),
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

/// Runs `exe` by its full path with an empty environment, feeding it `input`.
async fn exec(exe: &str, args: &[&str], input: &str) -> io::Result<Output> {
    let run = async {
        let mut child = tokio::process::Command::new(exe)
            .args(args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(input.as_bytes()).await?;
        }
        child.wait_with_output().await
    };
    tokio::time::timeout(COMMAND_TIMEOUT, run)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, format!("{exe} did not answer")))?
}

fn said(out: &Output) -> String {
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    text.trim().to_string()
}

/// Runs `route` with `args`; deleting what is not there is not an error.
async fn route<S: AsRef<str>>(args: &[S]) -> io::Result<()> {
    let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
    let out = exec(ROUTE, &args, "").await?;
    let text = said(&out);
    if out.status.success() || (args.contains(&"delete") && text.contains("not in table")) {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "route {} failed: {text}",
            args.join(" ")
        )))
    }
}

/// How the system would send a packet to `dest` now.
async fn route_get(dest: IpAddr) -> io::Result<String> {
    let dest = dest.to_string();
    let mut args = vec!["-n", "get"];
    if dest.contains(':') {
        args.push("-inet6");
    }
    args.push(&dest);
    let out = exec(ROUTE, &args, "").await?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(io::Error::other(format!(
            "there is no route to the relay: {}",
            said(&out)
        )))
    }
}

/// Feeds a script to `scutil`, which edits the dynamic store the resolver reads.
async fn scutil(script: &str) -> io::Result<()> {
    let out = exec(SCUTIL, &[], script).await?;
    if out.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "changing the DNS settings failed: {}",
            said(&out)
        )))
    }
}

/// Makes the system forget answers it got from the resolvers of the other side. Failing only
/// leaves old answers around for a while.
async fn flush_dns() {
    let short = Duration::from_secs(3);
    let _ = tokio::join!(
        tokio::time::timeout(short, exec(DSCACHEUTIL, &["-flushcache"], "")),
        tokio::time::timeout(short, exec(PKILL, &["-HUP", "-x", "mDNSResponder"], "")),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT: &str = "   route to: 203.0.113.9\ndestination: default\n       mask: default\n    gateway: 192.168.1.1\n  interface: en0\n      flags: <UP,GATEWAY,DONE,STATIC,PRCLONING,GLOBAL>\n";

    fn gateway(dest: &str, g: &str) -> Bypass {
        Bypass {
            dest: dest.parse().unwrap(),
            via: Via::Gateway(g.into()),
        }
    }

    #[test]
    fn the_state_survives_a_round_trip() {
        let on_link = Bypass {
            dest: "203.0.113.9".parse().unwrap(),
            via: Via::Interface("ppp0".into()),
        };
        for b in [
            gateway("203.0.113.9", "192.168.1.1"),
            gateway("2001:db8::9", "fe80::1%en0"),
            on_link,
        ] {
            let state = State { bypass: Some(b) };
            assert_eq!(State::parse(&state.text()), Some(state));
        }
        assert_eq!(State::parse(""), Some(State::default()));
        assert_eq!(State::default().text(), "");
    }

    #[test]
    fn a_damaged_state_is_refused() {
        for bad in [
            "bypass",
            "bypass 203.0.113.9",
            "bypass 203.0.113.9 via",
            "bypass not-an-ip via 192.168.1.1",
            "bypass 203.0.113.9 via fe80::1",
            "bypass 203.0.113.9 via -interface",
            "bypass 203.0.113.9 via 192.168.1.1%-x",
            "bypass 203.0.113.9 dev en0;reboot",
            "bypass 203.0.113.9 dev -interface",
            "bypass 203.0.113.9 by 192.168.1.1",
            "bypass 203.0.113.9 via 192.168.1.1 extra",
            "bypass 203.0.113.9 via 192.168.1.1\nbypass 203.0.113.9 via 192.168.1.1\n",
            "route 203.0.113.9 via 192.168.1.1",
        ] {
            assert_eq!(State::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_relay_behind_the_default_route_gets_a_host_route() {
        assert_eq!(
            default_via(DEFAULT).unwrap(),
            Some(Via::Gateway("192.168.1.1".into()))
        );
        // A LAN route, or the host's own address, stays as it is.
        let lan = DEFAULT
            .replace("default", "192.168.1.0")
            .replace("gateway: 192.168.1.1\n", "");
        assert_eq!(default_via(&lan).unwrap(), None);
        // A default route without a gateway, or whose gateway is no address (`link#4`).
        let want = Some(Via::Interface("en0".into()));
        let direct = DEFAULT.replace("gateway: 192.168.1.1\n", "");
        assert_eq!(default_via(&direct).unwrap(), want);
        let linked = DEFAULT.replace("192.168.1.1", "link#4");
        assert_eq!(default_via(&linked).unwrap(), want);
        let v6 = DEFAULT.replace("192.168.1.1", "fe80::1%en0");
        assert_eq!(
            default_via(&v6).unwrap(),
            Some(Via::Gateway("fe80::1%en0".into()))
        );
        // What route prints is not trusted either.
        let odd = DEFAULT.replace("en0", "-x").replace("192.168.1.1", "-y");
        assert!(default_via(&odd).is_err());
    }

    #[test]
    fn the_host_route_is_added_and_deleted_the_same_way() {
        let v4 = gateway("203.0.113.9", "192.168.1.1");
        assert_eq!(
            v4.args("add"),
            ["-n", "add", "-host", "203.0.113.9", "192.168.1.1"]
        );
        assert_eq!(v4.args("delete")[1], "delete");
        let v6 = gateway("2001:db8::9", "fe80::1%en0");
        assert_eq!(
            v6.args("add"),
            ["-n", "add", "-inet6", "-host", "2001:db8::9", "fe80::1%en0"]
        );
        let direct = Bypass {
            dest: "203.0.113.9".parse().unwrap(),
            via: Via::Interface("en0".into()),
        };
        assert_eq!(
            direct.args("add"),
            ["-n", "add", "-host", "203.0.113.9", "-interface", "en0"]
        );
    }

    #[test]
    fn interface_names_are_plain() {
        for ok in ["en0", "utun12", "bridge100"] {
            assert!(valid_interface(ok), "{ok}");
        }
        for bad in ["", "-x", "en 0", "en0;x", "0123456789abcdef", "é"] {
            assert!(!valid_interface(bad), "{bad:?}");
        }
    }
}
