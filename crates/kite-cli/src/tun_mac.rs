//! TUN mode on macOS: every connection and DNS query of this computer goes through the relay.
//! A utun device feeds tun2proxy, which turns its packets into connections to the local proxy
//! from proxy mode, as on Linux and Windows. It needs root.
//!
//! The device is set up as the computer's primary network service, the way a VPN's is, so that a
//! second VPN started on top of it treats it as the way out and sends its own connection to its
//! server through it (see docs/architecture.md, "A second VPN"). What it changes, and `restore`
//! takes all of it back:
//!
//! - The device `utunN` (the system picks N; 198.18.0.1 to 198.18.0.2). It goes away with the
//!   process, and the kernel removes its routes with it.
//! - A network service in the system configuration, `State:/Network/Service/kite/IPv4` with the
//!   router 198.18.0.2 and `OverridePrimary`, as VPNs publish theirs: configd makes it the primary
//!   service and points the default route at it, and a VPN that looks for the primary interface or
//!   the default gateway finds the tunnel. A VPN that publishes its own service later takes over.
//!   If the default route does not follow, the routes `0.0.0.0/1` and `128.0.0.0/1` on the device
//!   take everything instead, as up to 0.1.0-rc.2.
//! - Two routes on the device, `::/1` and `8000::/1`, which take all IPv6 that has no more specific
//!   route (the device has no IPv6 address to be a primary service with).
//! - A host route for the relay when it is reached through the default route, through the gateway
//!   (or interface) it had before, so the socket to the phone does not enter its own tunnel. A
//!   phone on the LAN needs none. It is written to `tun.state` before it is added.
//! - The service's DNS (`State:/Network/Service/kite/DNS`), 1.1.1.1, which the tunnel carries to the
//!   phone: the primary service's resolver answers every name. Under the fallback it is a resolver
//!   that matches every name.
//!
//! With the kill switch (opt-in) pf also blocks every packet that does not go out through the
//! device, to the relay (its address and port) or on the loopback, plus DHCP and IPv6 link
//! discovery, so a lost phone or a lost device never leaks, DNS included:
//!
//! - The rules live in the anchor `com.apple/kite`, loaded with `pfctl -a ... -f -`. Under `com.apple`
//!   because the main ruleset macOS ships has `anchor "com.apple/*"`, so Kite never rewrites the
//!   user's main ruleset; it checks that line is there and refuses to start without it, rather than
//!   run a block that does nothing.
//! - They are blocks only, never a pass: a `pass quick` in Kite's anchor would end pf's evaluation
//!   before another program's anchor, a second VPN's kill switch, could block. The anchor holds one
//!   rule, `anchor "block" out on ! lo0`, and the blocks in `com.apple/kite/block` each leave out the
//!   device and what the tunnel needs, so whatever they do not block is left to the rest of pf.
//! - pf is enabled with `pfctl -E`, which counts a reference (a token, kept in `tun.state`) instead
//!   of switching it on; `pfctl -X` gives it back, so a firewall the user had on stays on and one
//!   that was off goes off again.
//! - The block goes up before the routes and comes off after them. It is kernel state: it outlives
//!   a crash of kite until `restore`, `kite disconnect` or the next connect without the kill
//!   switch, and a reboot clears it. The daemon does not lift it when launchd starts it again.
//!
//! Without the kill switch a crash leaves the DNS resolver until the next start of the daemon or
//! `restore`, and the host route likewise.
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
const IPV4_KEY: &str = "State:/Network/Service/kite/IPv4";
/// An address whose route tells where the default route goes now (a documentation range, never
/// sent anywhere).
const PROBE: &str = "203.0.113.1";
/// How long configd gets to point the default route at the device.
const PRIMARY_WAIT: Duration = Duration::from_secs(3);
/// Where the lock and the state live: the daemon's directory, for root only.
const STATE_DIR: &str = "/var/db/kite";
const ROUTE: &str = "/sbin/route";
const SCUTIL: &str = "/usr/sbin/scutil";
const DSCACHEUTIL: &str = "/usr/bin/dscacheutil";
const PKILL: &str = "/usr/bin/pkill";
const PFCTL: &str = "/sbin/pfctl";
const ANCHOR: &str = "com.apple/kite";
/// Where the blocks are; the anchor above only sends what does not leave on the loopback here.
const BLOCK_ANCHOR: &str = "com.apple/kite/block";
/// Longest wait for one command.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the tunnel's tasks get to finish once the tunnel is told to end. Past it they are
/// dropped: with many open connections, or a task that does not answer, a disconnect would
/// otherwise not end.
const TEARDOWN: Duration = Duration::from_secs(4);

/// The routes that take all traffic into the tunnel when the device is not the primary service:
/// each half of the address space.
const HALVES_V4: [&str; 2] = ["0.0.0.0/1", "128.0.0.0/1"];
const HALVES_V6: [&str; 2] = ["::/1", "8000::/1"];

/// Lets the kill switch pass a lookup of the phone and a dial to a new relay address. Not done on
/// macOS yet: the pf block keeps the address it started with, so the caller does not look again.
pub async fn allow(_relay: Option<SocketAddr>, _discovery: bool) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the kill switch cannot follow a new relay address on macOS",
    ))
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
    // A dual-stack connection reports the relay as ::ffff:a.b.c.d; it is a.b.c.d to routes and pf.
    let relay = relay.map(|a| SocketAddr::new(a.ip().to_canonical(), a.port()));
    let _lock = lock()?;
    // Whatever a kite that was killed left behind. A block stays for the kill switch to replace;
    // without the kill switch it goes.
    undo(!kill_switch).await?;
    let device = create()?;
    serve(device, proxy, relay, kill_switch, serve_link, shutdown).await
}

/// Deletes what a TUN mode run that was killed left behind. Fails while another one is running.
pub async fn restore() -> io::Result<()> {
    let _lock = lock()?;
    undo(true).await
}

/// As `restore`, but a kill switch's block stays: for the daemon that launchd starts again after a
/// crash, which must not lift what the crashed run put up for the user.
pub async fn restore_keeping_block() -> io::Result<()> {
    let _lock = lock()?;
    undo(false).await
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
    relay: Option<SocketAddr>,
    kill_switch: bool,
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
        r = setup(&name, relay, kill_switch) => {
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
                    () = shutdown => {
                        stopped.set(true);
                        token.cancel();
                    }
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

async fn setup(name: &str, relay: Option<SocketAddr>, kill_switch: bool) -> io::Result<()> {
    // The relay's way out, before the tunnel's routes change it. A more specific route than the
    // halves (a LAN) keeps winning on its own; only the default route needs a host route beside it.
    let bypass = match relay.map(|a| a.ip()).filter(|ip| !ip.is_loopback()) {
        Some(dest) => default_via(&route_get(dest).await?)?.map(|via| Bypass { dest, via }),
        None => None,
    };
    let old = State::load()?.pf;
    let mut state = State { bypass, pf: old };
    state.save()?;
    if kill_switch {
        // Before the routes change, so nothing leaks between the two. What was there before the
        // first reference stays in the state, for the release.
        let was_on = match old {
            Some(old) => old.was_on,
            None => pf_is_on().await,
        };
        let new = Pf {
            token: pf_enable().await?,
            was_on,
        };
        state.pf = Some(new);
        if let Err(e) = state.save() {
            pf_release(new).await;
            return Err(e);
        }
        if let Some(old) = old {
            pf_release(old).await;
        }
        pf_load(&blocks(name, relay)).await?;
    }
    if let Some(b) = &state.bypass {
        route(&b.args("add")).await?;
    }
    scutil(&service(name)).await?;
    scutil(&dns(true)).await?;
    if !became_primary(name).await? {
        eprintln!(
            "kite: the system did not make the tunnel its primary service; a VPN started on top \
             may not find it"
        );
        for half in HALVES_V4 {
            route(&["-n", "add", "-inet", half, "-interface", name]).await?;
        }
        scutil(&dns(false)).await?;
    }
    for half in HALVES_V6 {
        // No IPv6 on this host is the usual reason; say so, so that a failure on a host that has
        // IPv6 is not silent.
        if let Err(e) = route(&["-n", "add", "-inet6", half, "-interface", name]).await {
            eprintln!("kite: IPv6 is not taken into the tunnel: {e}");
            break;
        }
    }
    flush_dns().await;
    Ok(())
}

/// The scutil script that publishes the device as a network service that overrides the primary one.
fn service(utun: &str) -> String {
    format!(
        "d.init\nd.add Addresses * {ADDR}\nd.add DestAddresses * {PEER}\nd.add Router {PEER}\n\
         d.add InterfaceName {utun}\nd.add OverridePrimary # 1\nset {IPV4_KEY}\n"
    )
}

/// The scutil script for the service's DNS: as the primary service's, or, `primary` false, as a
/// resolver that matches every name.
fn dns(primary: bool) -> String {
    let every_name = if primary {
        ""
    } else {
        "d.add SupplementalMatchDomains * \"\"\n"
    };
    format!("d.init\nd.add ServerAddresses * {DNS}\n{every_name}set {DNS_KEY}\n")
}

/// Waits for configd to point the default route at the device.
async fn became_primary(utun: &str) -> io::Result<bool> {
    let deadline = tokio::time::Instant::now() + PRIMARY_WAIT;
    loop {
        let out = exec(ROUTE, &["-n", "get", PROBE], "").await?;
        if route_interface(&String::from_utf8_lossy(&out.stdout)) == Some(utun) {
            return Ok(true);
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The `interface:` line of `route get`.
fn route_interface(text: &str) -> Option<&str> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix("interface:"))
        .map(str::trim)
}

/// Everything TUN mode adds that outlives the device, taken away; what is not there is skipped, so
/// this runs again and again. The first error is returned after the rest has been tried. The kill
/// switch's block, if there is one, goes only with `lift`, and then last.
async fn undo(lift: bool) -> io::Result<()> {
    let mut first = Ok(());
    let mut note = |r: io::Result<()>| {
        if first.is_ok() {
            first = r;
        }
    };
    let mut pf = None;
    match State::load() {
        Ok(State { bypass, pf: held }) => {
            if let Some(b) = bypass {
                note(route(&b.args("delete")).await);
            }
            pf = held;
        }
        Err(e) => note(Err(e)),
    }
    note(scutil(&format!("remove {IPV4_KEY}\nremove {DNS_KEY}\n")).await);
    flush_dns().await;
    if lift {
        // Flushing also works when the rules are not there; the token is given back either way.
        note(pf_flush().await);
        if let Some(held) = pf {
            pf_release(held).await;
        }
    }
    if first.is_ok() {
        let keep = State {
            bypass: None,
            pf: pf.filter(|_| !lift),
        };
        if keep.text().is_empty() {
            let _ = std::fs::remove_file(dir()?.join("tun.state"));
        } else {
            keep.save()?;
        }
    }
    first
}

/// The blocks of the kill switch, for the anchor `com.apple/kite/block`, which only sees what does
/// not leave on the loopback. Each block leaves out the device, and together they leave out TCP to
/// the relay (its address and port), DHCP and IPv6 link discovery, which keep the link to the router
/// or the phone up. Everything else going out is refused, with a reset or an unreachable so that
/// programs fail at once. Blocks only: what they do not block, other rules in pf still decide.
fn blocks(utun: &str, relay: Option<SocketAddr>) -> String {
    let out = format!("block return out quick on ! {utun}");
    let mut r = String::new();
    match relay.filter(|a| !a.ip().is_loopback()) {
        Some(a) => {
            let (ip, port) = (a.ip(), a.port());
            let (own, other) = if ip.is_ipv4() {
                ("inet", "inet6")
            } else {
                ("inet6", "inet")
            };
            r += &format!("{out} {own} proto tcp from any to ! {ip}\n");
            r += &format!("{out} {own} proto tcp from any to {ip} port != {port}\n");
            r += &format!("{out} {other} proto tcp all\n");
        }
        // A relay on the loopback is not filtered here.
        None => r += &format!("{out} proto tcp all\n"),
    }
    r += &format!(
        "{out} inet proto udp from any port != 68\n\
         {out} inet proto udp from any port 68 to any port != 67\n\
         {out} inet6 proto udp from any port != 546\n\
         {out} inet6 proto udp from any port 546 to any port != 547\n\
         {out} inet proto icmp all\n\
         {out} inet6 proto icmp6 all icmp6-type {{ 128, 129, 139, 140 }}\n\
         {out} proto {{ 4, 41, 47, 50, 51, 103, 112, 115, 132 }} all\n"
    );
    r
}

/// The rule of the anchor `com.apple/kite`: what does not leave on the loopback goes on to the blocks.
const SEND_TO_BLOCKS: &str = "anchor \"block\" out on ! lo0 all\n";

/// Takes a reference on pf being enabled; the token gives it back.
async fn pf_enable() -> io::Result<u64> {
    let out = exec(PFCTL, &["-E"], "").await?;
    let text = said(&out);
    match parse_token(&text) {
        Some(t) if out.status.success() => Ok(t),
        _ => Err(io::Error::other(format!("pfctl -E failed: {text}"))),
    }
}

/// `pfctl -E` says `Token : 1234567890`.
fn parse_token(text: &str) -> Option<u64> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix("Token"))
        .and_then(|rest| rest.trim().strip_prefix(':'))
        .and_then(|n| n.trim().parse().ok())
}

async fn pf_is_on() -> bool {
    exec(PFCTL, &["-s", "info"], "").await.is_ok_and(|out| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|l| l.trim_start().starts_with("Status: Enabled"))
    })
}

/// Gives the reference back. Failing leaves pf enabled, which is no leak, so it only says so.
/// `-X` switches pf off with the last reference, also when the user had switched it on with
/// `pfctl -e`; then it is switched on again.
async fn pf_release(pf: Pf) {
    let arg = pf.token.to_string();
    match exec(PFCTL, &["-X", &arg], "").await {
        Ok(out) if out.status.success() => {}
        Ok(out) => eprintln!("kite: releasing pf: {}", said(&out)),
        Err(e) => eprintln!("kite: releasing pf: {e}"),
    }
    if pf.was_on && !pf_is_on().await {
        let _ = exec(PFCTL, &["-e"], "").await;
    }
}

/// Loads the blocks, then the rule that sends packets to them, and checks that the main ruleset
/// evaluates the anchor it is in.
async fn pf_load(blocks: &str) -> io::Result<()> {
    for (anchor, rules) in [(BLOCK_ANCHOR, blocks), (ANCHOR, SEND_TO_BLOCKS)] {
        let out = exec(PFCTL, &["-a", anchor, "-f", "-"], rules).await?;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "loading the kill switch into pf failed: {}",
                said(&out)
            )));
        }
    }
    let main = exec(PFCTL, &["-s", "rules"], "").await?;
    if !String::from_utf8_lossy(&main.stdout)
        .lines()
        .any(|l| l.trim() == "anchor \"com.apple/*\" all")
    {
        return Err(io::Error::other(
            "pf's main ruleset has no `anchor \"com.apple/*\" all` line, so the kill switch would not \
             apply; restore /etc/pf.conf or leave the kill switch off",
        ));
    }
    Ok(())
}

/// Takes the kill switch out of pf: first the rule that sends packets to the blocks, then the blocks.
async fn pf_flush() -> io::Result<()> {
    for anchor in [ANCHOR, BLOCK_ANCHOR] {
        let out = exec(PFCTL, &["-a", anchor, "-F", "rules"], "").await?;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "removing the kill switch from pf failed: {}",
                said(&out)
            )));
        }
    }
    Ok(())
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

/// What `undo` cannot work out from the system: the host route for the relay and the pf token.
/// Up to two lines, in this order: `bypass DESTINATION via GATEWAY` or `bypass DESTINATION dev
/// INTERFACE`, and `pf TOKEN WAS_ON` (`1` when pf was enabled before the token); or an empty file.
#[derive(Debug, Default, PartialEq)]
struct State {
    bypass: Option<Bypass>,
    pf: Option<Pf>,
}

/// The reference on pf being enabled that the kill switch holds.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Pf {
    token: u64,
    was_on: bool,
}

impl State {
    fn text(&self) -> String {
        let mut text = self
            .bypass
            .as_ref()
            .map_or_else(String::new, |b| match &b.via {
                Via::Gateway(g) => format!("bypass {} via {g}\n", b.dest),
                Via::Interface(i) => format!("bypass {} dev {i}\n", b.dest),
            });
        if let Some(pf) = self.pf {
            text += &format!("pf {} {}\n", pf.token, u8::from(pf.was_on));
        }
        text
    }

    fn parse(text: &str) -> Option<Self> {
        let mut state = Self::default();
        let mut lines = text.lines().peekable();
        if lines.peek().is_some_and(|l| l.starts_with("bypass")) {
            state.bypass = Some(parse_bypass(lines.next()?)?);
        }
        if let Some(line) = lines.next() {
            let mut words = line.split(' ');
            let (Some("pf"), Some(token), Some(was_on), None) =
                (words.next(), words.next(), words.next(), words.next())
            else {
                return None;
            };
            state.pf = Some(Pf {
                token: token.parse().ok()?,
                was_on: match was_on {
                    "0" => false,
                    "1" => true,
                    _ => return None,
                },
            });
        }
        lines.next().is_none().then_some(state)
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

fn parse_bypass(line: &str) -> Option<Bypass> {
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
    Some(Bypass { dest, via })
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
            let state = State {
                bypass: Some(b),
                pf: None,
            };
            assert_eq!(State::parse(&state.text()), Some(state));
        }
        assert_eq!(State::parse(""), Some(State::default()));
        assert_eq!(State::default().text(), "");
    }

    #[test]
    fn the_pf_token_is_part_of_the_state() {
        let via = Bypass {
            dest: "203.0.113.9".parse().unwrap(),
            via: Via::Gateway("192.168.1.1".into()),
        };
        for state in [
            State {
                bypass: Some(via.clone()),
                pf: Some(Pf {
                    token: 1_234_567_890,
                    was_on: true,
                }),
            },
            State {
                bypass: None,
                pf: Some(Pf {
                    token: 7,
                    was_on: false,
                }),
            },
        ] {
            assert_eq!(State::parse(&state.text()), Some(state));
        }
        assert_eq!(
            State::parse("pf 7 0\n").map(|s| s.pf.map(|p| p.token)),
            Some(Some(7))
        );
        for bad in [
            "pf",
            "pf x",
            "pf -1",
            "pf 7",
            "pf 7 2",
            "pf 7 0 1",
            "pf 7\npf 8\n",
            "pf 7 0\nbypass 203.0.113.9 via 192.168.1.1\n",
            "bypass 203.0.113.9 via 192.168.1.1\npf 7 0\nextra\n",
        ] {
            assert_eq!(State::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn pfctl_says_its_token() {
        let said = "No ALTQ support in kernel\nALTQ related functions disabled\npf enabled\n\nToken : 3846205941\n";
        assert_eq!(parse_token(said), Some(3_846_205_941));
        for bad in ["", "Token", "Token : x", "pf enabled", "Token 5"] {
            assert_eq!(parse_token(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_kill_switch_only_blocks_and_leaves_out_the_tunnel() {
        let relay: SocketAddr = "203.0.113.9:47810".parse().unwrap();
        let text = blocks("utun4", Some(relay));
        // A pass would end pf's evaluation before another program's anchor could block.
        for line in text.lines().chain(SEND_TO_BLOCKS.lines()) {
            assert!(!line.contains("pass"), "{line}");
        }
        assert!(text
            .lines()
            .all(|l| l.starts_with("block return out quick on ! utun4 ")));
        let lines: Vec<_> = text.lines().collect();
        assert!(lines.contains(
            &"block return out quick on ! utun4 inet proto tcp from any to ! 203.0.113.9"
        ));
        assert!(lines.contains(
            &"block return out quick on ! utun4 inet proto tcp from any to 203.0.113.9 port != 47810"
        ));
        // TCP of the other family has no relay to go to.
        assert!(lines.contains(&"block return out quick on ! utun4 inet6 proto tcp all"));
        assert!(SEND_TO_BLOCKS.contains("on ! lo0"));
        let v6: SocketAddr = "[2001:db8::9]:47810".parse().unwrap();
        let text = blocks("utun4", Some(v6));
        assert!(text.contains("inet6 proto tcp from any to ! 2001:db8::9\n"));
        assert!(text.contains("on ! utun4 inet proto tcp all\n"));
    }

    #[test]
    fn no_relay_exception_without_a_relay_or_for_loopback() {
        let loopback: SocketAddr = "127.0.0.1:7000".parse().unwrap();
        for relay in [None, Some(loopback)] {
            let text = blocks("utun4", relay);
            assert!(text.contains("on ! utun4 proto tcp all\n"));
            assert!(!text.contains("port != 7000"));
        }
    }

    #[test]
    fn the_service_overrides_the_primary_one_with_the_peer_as_router() {
        let s = service("utun4");
        for want in [
            "d.add Router 198.18.0.2",
            "d.add InterfaceName utun4",
            "d.add OverridePrimary # 1",
            "set State:/Network/Service/kite/IPv4",
        ] {
            assert!(s.contains(want), "{want}");
        }
        assert!(!dns(true).contains("SupplementalMatchDomains"));
        assert!(dns(false).contains("SupplementalMatchDomains * \"\""));
        assert_eq!(
            route_interface("   route to: 203.0.113.1\n  interface: utun4\n"),
            Some("utun4")
        );
        assert_eq!(route_interface("route: writing to routing socket"), None);
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
