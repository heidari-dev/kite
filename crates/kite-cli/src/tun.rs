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
/// An address whose route tells which link carries the default route now (a documentation range,
/// never sent anywhere).
const PROBE: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 1);
/// How often kite0's DNS settings follow a second VPN coming and going.
const FOLLOW: Duration = Duration::from_secs(2);
const RESOLVECTL: &str = "/usr/bin/resolvectl";

/// Lets the kill switch pass a lookup of the phone and a dial to `relay`. Nothing to do here: the
/// sockets carry the firewall mark that keeps them out of the tunnel and its blackhole.
pub async fn allow(_relay: Option<SocketAddr>, _discovery: bool) -> io::Result<()> {
    Ok(())
}

/// Runs TUN mode until `shutdown` completes or tun2proxy fails. `serve_link` carries the traffic
/// that reaches the proxy at `proxy` through a tunnel to the relay, until the token it is given is
/// cancelled; that tunnel must bypass the routes set up here, see [`route::TABLE`].
///
/// The device and the rules live as long as this runs, whether or not a tunnel is up. Without the
/// kill switch they go away when it ends. With it they stay when it ends for any reason but
/// `shutdown`, so nothing leaks until the user runs `kite restore`.
pub async fn run<F, Fut>(
    proxy: SocketAddr,
    // The relay socket carries a firewall mark here; only Windows needs the address.
    _relay: Option<SocketAddr>,
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
    // Until setup is done an error is a failed start, not a lost tunnel: it restores as usual.
    let mut stopped = true;
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
    let mut uses_resolved = false;
    tokio::select! {
        r = async {
            route::setup(link, kill_switch).await?;
            uses_resolved = resolved_in_use().await?;
            if uses_resolved {
                resolved(NAME, true).await?;
            }
            io::Result::Ok(())
        } => r?,
        () = &mut shutdown => {
            *stopped = true;
            return Ok(());
        }
    }

    *stopped = false;
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
                    *stopped = true;
                    token.cancel();
                }
                () = token.cancelled() => {}
            }
        },
        async {
            if uses_resolved {
                tokio::select! {
                    () = follow_dns(link) => {}
                    () = token.cancelled() => {}
                }
            }
        },
    );
    proxied.and(tunneled.map(drop).map_err(io::Error::from))
}

/// Whether this host resolves through systemd-resolved.
async fn resolved_in_use() -> io::Result<bool> {
    let resolv_conf = match tokio::fs::read_to_string("/etc/resolv.conf").await {
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        r => r?,
    };
    let nsswitch = tokio::fs::read_to_string("/etc/nsswitch.conf")
        .await
        .unwrap_or_default();
    Ok(uses_resolved(&resolv_conf, &nsswitch))
}

/// systemd-resolved sends each query out of the link that supplied the server (IP_UNICAST_IF), which
/// routing rules cannot redirect. With `on`, give the tunnel link a server and the "~." routing
/// domain so resolved sends every query there; without, take the domain and the default route flag
/// away again, so that resolved sends nothing there. It drops these settings with the link. Without
/// root, resolved asks polkit (see docs/architecture.md).
async fn resolved(link: &str, on: bool) -> io::Result<()> {
    let dns = DNS.to_string();
    let steps: &[[&str; 3]] = if on {
        &[
            ["dns", link, dns.as_str()],
            ["domain", link, "~."],
            ["default-route", link, "true"],
        ]
    } else {
        // An empty string clears the list.
        &[["domain", link, ""], ["default-route", link, "false"]]
    };
    for args in steps {
        let out = resolvectl(args).await?;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "systemd-resolved refused DNS settings for {link} (run as root or add the polkit                  rule from docs/architecture.md): {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
    }
    Ok(())
}

async fn resolvectl(args: &[&str]) -> io::Result<std::process::Output> {
    let run = tokio::process::Command::new(RESOLVECTL)
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "resolvectl did not answer"))?
}

/// While the tunnel runs on a host that resolves through systemd-resolved: while another link (a
/// second VPN) carries the default route and takes every name itself (DNS servers and the `~.`
/// routing domain), kite0 leaves DNS to it, so resolved does not also send every name around it
/// through the phone; kite0 takes DNS back when the VPN goes. A link without `~.` would share the
/// names with the network card's link once kite0 stepped aside, so kite0 keeps DNS then. resolved
/// is asked only while the link that carries the default route is new or not a VPN yet. A failure
/// to look leaves the settings as they are, and is said once.
async fn follow_dns(link: u32) {
    let mut ours = true;
    // The link that carries the default route and was found to be a VPN.
    let mut vpn_link = None;
    let mut said = false;
    loop {
        tokio::time::sleep(FOLLOW).await;
        let r = async {
            let dev = route::device_to(PROBE).await?;
            let vpn = match dev {
                Some(d) if d == link => false,
                Some(d) if vpn_link == Some(d) => true,
                Some(d) => takes_every_name(d).await?,
                None => false,
            };
            vpn_link = dev.filter(|_| vpn);
            if vpn == ours {
                resolved(NAME, !vpn).await?;
                ours = !vpn;
            }
            io::Result::Ok(())
        }
        .await;
        match r {
            Ok(()) => said = false,
            Err(e) if !said => {
                eprintln!("kite: following a second VPN's DNS: {e}");
                said = true;
            }
            Err(_) => {}
        }
    }
}

/// Whether resolved sends every name to the link with index `dev` alone: it has DNS servers and the
/// `~.` routing domain.
async fn takes_every_name(dev: u32) -> io::Result<bool> {
    let dev = dev.to_string();
    let servers = resolvectl(&["dns", &dev]).await?;
    if !servers.status.success()
        || !after_link(&String::from_utf8_lossy(&servers.stdout))
            .next()
            .is_some()
    {
        return Ok(false);
    }
    let domains = resolvectl(&["domain", &dev]).await?;
    Ok(domains.status.success()
        && after_link(&String::from_utf8_lossy(&domains.stdout)).any(|d| d == "~."))
}

/// The words after the link in what `resolvectl dns LINK` or `resolvectl domain LINK` prints:
/// `Link 5 (wg0): 10.64.0.1 fd00::1`, `Link 5 (wg0): ~.`, nothing after the colon when there are
/// none.
fn after_link(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .find_map(|l| l.split_once("):"))
        .map_or("", |(_, rest)| rest)
        .split_whitespace()
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
    use super::{after_link, uses_resolved};

    #[test]
    fn reads_the_dns_settings_of_a_link() {
        let words = |text| after_link(text).collect::<Vec<_>>();
        assert_eq!(
            words("Link 5 (wg0): 10.64.0.1 fd00::1\n"),
            ["10.64.0.1", "fd00::1"]
        );
        assert_eq!(
            words("Link 5 (wg0): ~. corp.example\n"),
            ["~.", "corp.example"]
        );
        assert!(words("Link 5 (wg0):\n").is_empty());
        assert!(words("Link 5 (wg0):   \n").is_empty());
        assert!(words("").is_empty());
    }

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
