//! TUN mode (Linux): every connection and DNS query of this computer goes through the relay.
//! tun2proxy turns the TUN device's packets into connections to the local proxy from proxy mode.

use crate::{proxy, route};
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
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
    cfg.tun_name(NAME)
        .address(ADDR)
        .netmask(Ipv4Addr::new(255, 255, 255, 252))
        .mtu(MTU)
        .up();
    // Not persistent: the kernel removes the device and its routes when this process ends. Creating
    // it first also fails while another kite owns it, before its rules could be touched.
    let device = tun::create_as_async(&cfg).map_err(io::Error::other)?;
    let link = device.tun_index().map_err(io::Error::other)?;
    let link = u32::try_from(link).map_err(io::Error::other)?;
    route::restore().await?;
    let result = serve(device, link, tunnel, listener, active, shutdown).await;
    result.and(route::restore().await)
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
/// routing rules cannot redirect. When it is in use, give the tunnel link a server and the "~."
/// routing domain so resolved sends every query there. It drops these settings with the link.
async fn resolved(link: &str) -> io::Result<()> {
    let conf = tokio::fs::read_to_string("/etc/resolv.conf")
        .await
        .unwrap_or_default();
    if !uses_resolved_stub(&conf) {
        return Ok(());
    }
    let dns = DNS.to_string();
    for args in [
        ["dns", link, dns.as_str()],
        ["domain", link, "~."],
        ["default-route", link, "true"],
    ] {
        let out = tokio::process::Command::new("/usr/bin/resolvectl")
            .args(args)
            .output()
            .await?;
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

fn uses_resolved_stub(resolv_conf: &str) -> bool {
    resolv_conf.lines().any(|line| {
        let mut words = line.split_whitespace();
        words.next() == Some("nameserver")
            && matches!(words.next(), Some("127.0.0.53" | "127.0.0.54"))
    })
}

#[cfg(test)]
mod tests {
    use super::uses_resolved_stub;

    #[test]
    fn detects_the_systemd_resolved_stub() {
        assert!(uses_resolved_stub(
            "# stub\nnameserver 127.0.0.53\noptions edns0 trust-ad\nsearch lan\n"
        ));
        assert!(!uses_resolved_stub("nameserver 192.168.1.1\n"));
        assert!(!uses_resolved_stub("# nameserver 127.0.0.53\n"));
        assert!(!uses_resolved_stub(""));
    }
}
