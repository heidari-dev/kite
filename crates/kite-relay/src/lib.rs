//! Phone-side relay: encrypted multiplexed sessions that open real sockets on the phone.

#![forbid(unsafe_code)]

use std::future::poll_fn;
use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::task::JoinSet;
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};

pub mod socks5;
pub mod udpgw;

/// Most concurrent streams (TCP connections plus UDP gateways) per tunnel. Yamux closes the
/// whole tunnel when the peer opens more, so the desktop side must stay below this. Each stream
/// may buffer up to 256 KiB, so a tunnel holds at most 64 MiB in flight.
pub const MAX_STREAMS: usize = 256;

/// Serves one tunnel: a yamux server on `tunnel` where every inbound stream is a SOCKS5 session.
/// Returns when the tunnel closes, fails or carries no session for the idle timeout; its sessions
/// are dropped with it.
pub async fn serve<T>(tunnel: T, cfg: Config) -> io::Result<()>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let mut mux = yamux::Config::default();
    mux.set_max_num_streams(MAX_STREAMS);
    // Streams start at yamux's fixed 256 KiB window. Capping the tunnel at exactly that per
    // stream leaves no budget for auto-tuning, so windows never grow.
    mux.set_max_connection_receive_window(Some(MAX_STREAMS * yamux::DEFAULT_CREDIT as usize));
    let mut conn = yamux::Connection::new(tunnel.compat(), mux, yamux::Mode::Server);
    let cfg = Arc::new(cfg);
    let mut sessions = JoinSet::new();
    loop {
        tokio::select! {
            next = poll_fn(|cx| conn.poll_next_inbound(cx)) => match next {
                Some(Ok(stream)) => {
                    let cfg = cfg.clone();
                    sessions.spawn(async move {
                        let _ = socks5::serve(stream.compat(), &cfg).await;
                    });
                }
                Some(Err(e)) => return Err(io::Error::other(e)),
                None => return Ok(()),
            },
            Some(_) = sessions.join_next() => {}
            // Yamux answers pings internally, so they don't keep an empty tunnel alive.
            _ = tokio::time::sleep(cfg.idle_timeout), if sessions.is_empty() => {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "idle tunnel"));
            }
        }
    }
}

/// Limits shared by every session.
#[derive(Debug, Clone)]
pub struct Config {
    /// Time allowed for the SOCKS5 handshake, name resolution and the outbound connect together.
    pub connect_timeout: Duration,
    /// A relayed connection with no traffic in either direction for this long is closed.
    pub idle_timeout: Duration,
    /// Allow targets on the phone's own loopback. For tests and local development only.
    pub allow_loopback: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(300),
            allow_loopback: false,
        }
    }
}

impl Config {
    /// Whether the relay may open a socket to `ip`. The phone's own loopback, link-local,
    /// unspecified, broadcast and multicast addresses are refused.
    pub fn allows(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        if ip.is_loopback() {
            return self.allow_loopback;
        }
        if ip.is_unspecified() || ip.is_multicast() {
            return false;
        }
        match ip {
            // 0.0.0.0/8 means "this host" on most stacks.
            IpAddr::V4(v4) => !(v4.is_link_local() || v4.is_broadcast() || v4.octets()[0] == 0),
            IpAddr::V6(v6) => {
                let s = v6.segments();
                // IPv4-compatible (::/96) and NAT64 (64:ff9b::/96) forms carry an IPv4 target.
                if let Some(v4) = v6.to_ipv4() {
                    return self.allows(v4.into());
                }
                if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                    let v4 = Ipv4Addr::from((u32::from(s[6]) << 16) | u32::from(s[7]));
                    return self.allows(v4.into());
                }
                s[0] & 0xffc0 != 0xfe80
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn allows_public_and_private_addresses() {
        let cfg = Config::default();
        for ip in [
            "1.1.1.1",
            "192.168.1.10",
            "10.0.0.1",
            "2606:4700::1111",
            "64:ff9b::101:101",
        ] {
            assert!(cfg.allows(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn refuses_local_and_special_addresses() {
        let cfg = Config::default();
        for ip in [
            "127.0.0.1",
            "127.1.2.3",
            "::1",
            "::ffff:127.0.0.1",
            "169.254.1.1",
            "fe80::1",
            "febf::1",
            "0.0.0.0",
            "0.1.2.3",
            "::",
            "255.255.255.255",
            "224.0.0.1",
            "ff02::1",
            "::ffff:169.254.1.1",
            "::7f00:1",
            "::a9fe:101",
            "64:ff9b::7f00:1",
            "64:ff9b::a9fe:101",
        ] {
            assert!(!cfg.allows(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn loopback_only_when_enabled() {
        let cfg = Config {
            allow_loopback: true,
            ..Config::default()
        };
        assert!(cfg.allows("127.0.0.1".parse().unwrap()));
        assert!(cfg.allows("::1".parse().unwrap()));
        assert!(!cfg.allows("169.254.1.1".parse().unwrap()));
    }
}
