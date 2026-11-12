//! TUN mode (Linux): every connection and DNS query of this computer goes through the relay.
//! tun2proxy turns the TUN device's packets into connections to the local proxy from proxy mode.

use crate::route;
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
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
/// DNS queries to private resolvers (a LAN router, the systemd-resolved stub) are sent here,
/// as DNS over TCP through the relay.
const DNS: IpAddr = IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1));

/// Runs TUN mode until `shutdown` completes or tun2proxy fails. `serve_link` carries the traffic
/// that reaches the proxy at `proxy` through a tunnel to the relay, until the token it is given is
/// cancelled; that tunnel must bypass the routes set up here, see [`route::TABLE`].
///
/// The device and the rules live as long as this runs, whether or not a tunnel is up. Without the
/// kill switch they go away when it ends. With it they stay when it ends for any reason but
/// `shutdown`, so nothing leaks until the user runs `kite restore`.
pub async fn run<F, Fut>(
    proxy: SocketAddr,
    kill_switch: bool,
    serve_link: F,
    shutdown: impl Future<Output = ()>,
) -> io::Result<()>
where
    F: FnOnce(CancellationToken) -> Fut,
    Fut: Future<Output = io::Result<()>>,
{
    let mut cfg = tun::Configuration::default();
    cfg.address(ADDR)
        .netmask(Ipv4Addr::new(255, 255, 255, 252))
        .mtu(MTU)
        .up();
    let device = claim(cfg)?;
    let link = device.tun_index().map_err(io::Error::other)?;
    let link = u32::try_from(link).map_err(io::Error::other)?;
    route::restore().await?;
    let mut stopped = false;
    let result = serve(
        device,
        link,
        proxy,
        kill_switch,
        serve_link,
        shutdown,
        &mut stopped,
    )
    .await;
    if kill_switch && !stopped {
        return result.and(Err(io::Error::other(
            "the tunnel ended; traffic stays blocked until `kite restore`",
        )));
    }
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

async fn serve<F, Fut>(
    device: tun::AsyncDevice,
    link: u32,
    proxy: SocketAddr,
    kill_switch: bool,
    serve_link: F,
    shutdown: impl Future<Output = ()>,
    stopped: &mut bool,
) -> io::Result<()>
where
    F: FnOnce(CancellationToken) -> Fut,
    Fut: Future<Output = io::Result<()>>,
{
    // Polled from the start, so a signal during setup still ends in a clean restore.
    tokio::pin!(shutdown);
    tokio::select! {
        r = async { route::setup(link, kill_switch).await?; resolved(NAME).await } => r?,
        () = &mut shutdown => {
            *stopped = true;
            return Ok(());
        }
    }

    let proxy_url = format!("socks5://{proxy}");
    let mut args = Args::default();
    args.proxy(ProxyParameters::try_from(proxy_url.as_str()).map_err(io::Error::other)?)
        .dns(ArgDns::OverTcp)
        .dns_addr(DNS)
        .udpgw_server(UDPGW)
        // Drops AAAA answers. The TCP stack accepts every handshake before the relay connects, so an
        // IPv6 target the phone cannot reach would look connected and then fail, with no fallback.
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
                () = shutdown => {
                    *stopped = true;
                    token.cancel();
                }
                () = token.cancelled() => {}
            }
        },
    );
    proxied.and(tunneled.map(drop).map_err(io::Error::from))
}

/// systemd-resolved sends each query out of the link that supplied the server (IP_UNICAST_IF), which
/// routing rules cannot redirect. When this host resolves through it, give the tunnel link a server
/// and the "~." routing domain so resolved sends every query there. It drops these settings with
/// the link. Without root, resolved asks polkit (see docs/architecture.md).
async fn resolved(link: &str) -> io::Result<()> {
    let resolv_conf = match tokio::fs::read_to_string("/etc/resolv.conf").await {
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        r => r?,
    };
    let nsswitch = tokio::fs::read_to_string("/etc/nsswitch.conf")
        .await
        .unwrap_or_default();
    if !uses_resolved(&resolv_conf, &nsswitch) {
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
                "systemd-resolved refused DNS settings for {link} (run as root or add the polkit \
                 rule from docs/architecture.md): {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
    }
    Ok(())
}

/// Whether name lookups go through systemd-resolved: its stub is the nameserver, or nss-resolve
/// is in the hosts line. Both describe this network namespace, unlike resolved's socket in /run.
fn uses_resolved(resolv_conf: &str, nsswitch: &str) -> bool {
    let stub = resolv_conf.lines().any(|line| {
        let mut words = line.split_whitespace();
        words.next() == Some("nameserver")
            && matches!(words.next(), Some("127.0.0.53" | "127.0.0.54"))
    });
    let nss = nsswitch.lines().any(|line| {
        let mut words = line.split_whitespace();
        words.next() == Some("hosts:") && words.any(|w| w == "resolve")
    });
    stub || nss
}

#[cfg(test)]
mod tests {
    use super::uses_resolved;

    #[test]
    fn detects_systemd_resolved() {
        let stub = "# stub\nnameserver 127.0.0.53\noptions edns0 trust-ad\nsearch lan\n";
        let lan = "nameserver 192.168.1.1\n";
        let nss = "passwd: files\nhosts: files mymachines resolve [!UNAVAIL=return] dns\n";
        assert!(uses_resolved(stub, ""));
        assert!(uses_resolved(lan, nss));
        assert!(!uses_resolved(lan, "hosts: files dns\n"));
        assert!(!uses_resolved(
            "# nameserver 127.0.0.53\n",
            "# hosts: resolve\n"
        ));
        assert!(!uses_resolved("", ""));
    }
}
