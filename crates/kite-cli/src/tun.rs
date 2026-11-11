//! TUN mode (Linux): every connection and DNS query of this computer goes through the relay.
//! tun2proxy turns the TUN device's packets into connections to the local proxy from proxy mode.

use crate::{proxy, route};
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tun::AbstractDevice;
use tun2proxy::{ArgDns, Args, CancellationToken, ProxyParameters};

pub const NAME: &str = "kite0";
const MTU: u16 = 1500;
/// 198.18.0.0/15 is reserved for benchmarking, so it does not clash with a LAN.
const ADDR: Ipv4Addr = Ipv4Addr::new(198, 18, 0, 1);
/// The relay serves udpgw on this reserved address (see docs/architecture.md).
const UDPGW: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 7300);
/// DNS queries to private resolvers (a LAN router, the systemd-resolved stub) are sent here,
/// as DNS over TCP through the relay.
const DNS: IpAddr = IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1));

/// Runs TUN mode until the tunnel fails or `shutdown` completes. `tunnel` must bypass the routes
/// set up here, see [`route::TABLE`].
pub async fn run(
    tunnel: TcpStream,
    listener: TcpListener,
    active: Arc<AtomicUsize>,
    shutdown: impl Future<Output = ()>,
) -> io::Result<()> {
    let mut cfg = tun::Configuration::default();
    cfg.address(ADDR)
        .netmask(Ipv4Addr::new(255, 255, 255, 252))
        .mtu(MTU)
        .up();
    let device = claim(cfg)?;
    let link = device.tun_index().map_err(io::Error::other)?;
    let link = u32::try_from(link).map_err(io::Error::other)?;
    route::restore().await?;
    let result = serve(device, link, tunnel, listener, active, shutdown).await;
    result.and(route::restore().await)
}

/// Creates the TUN device. It is not persistent, so the kernel removes it and its routes when this
/// process ends. Creating it fails while another kite holds it, so holding it is also the lock that
/// keeps one kite from touching another one's rules.
fn claim(mut cfg: tun::Configuration) -> io::Result<tun::AsyncDevice> {
    cfg.tun_name(NAME);
    tun::create_as_async(&cfg).map_err(|e| io::Error::other(format!("creating {NAME}: {e}")))
}

/// Deletes rules left behind by a TUN mode run that was killed.
pub async fn restore() -> io::Result<()> {
    let mut cfg = tun::Configuration::default();
    cfg.platform_config(|p| {
        p.ensure_root_privileges(false); // no addresses: the device only serves as the lock
    });
    let _lock = claim(cfg)?;
    route::restore().await
}

async fn serve(
    device: tun::AsyncDevice,
    link: u32,
    tunnel: TcpStream,
    listener: TcpListener,
    active: Arc<AtomicUsize>,
    shutdown: impl Future<Output = ()>,
) -> io::Result<()> {
    route::setup(link).await?;
    resolved(NAME).await?;

    let proxy_url = format!("socks5://{}", listener.local_addr()?);
    let mut args = Args::default();
    args.proxy(ProxyParameters::try_from(proxy_url.as_str()).map_err(io::Error::other)?)
        .dns(ArgDns::OverTcp)
        .dns_addr(DNS)
        .udpgw_server(UDPGW)
        .ipv6_enabled(true);

    let token = CancellationToken::new();
    let (proxied, tunneled, ()) = tokio::join!(
        async {
            let r = proxy::run(tunnel, listener, active, token.cancelled()).await;
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

/// systemd-resolved sends each query out of the link that supplied the server (IP_UNICAST_IF), which
/// routing rules cannot redirect. When it runs, give the tunnel link a server and the "~." routing
/// domain so resolved sends every query there. It drops these settings with the link.
async fn resolved(link: &str) -> io::Result<()> {
    // Present while resolved runs, whatever /etc/resolv.conf points at (apps may use nss-resolve).
    if !Path::new("/run/systemd/resolve/io.systemd.Resolve").exists() {
        return Ok(());
    }
    let dns = DNS.to_string();
    for args in [
        ["dns", link, dns.as_str()],
        ["domain", link, "~."],
        ["default-route", link, "true"],
    ] {
        let run = tokio::process::Command::new("/usr/bin/resolvectl")
            .args(args)
            .env_clear()
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output();
        let out = tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "resolvectl did not answer"))??;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "resolvectl {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
    }
    Ok(())
}
