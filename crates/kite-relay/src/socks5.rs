//! SOCKS5 (RFC 1928) CONNECT without authentication; the tunnel carrying it is authenticated.

use crate::{udpgw, Config};
use std::future::Future;
use std::io::{self, ErrorKind};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio::time::{timeout, timeout_at, Instant, Sleep};

/// Copy buffer per direction, so two per relayed connection.
const COPY_BUF: usize = 8 * 1024;

const VER: u8 = 5;
const NO_AUTH: u8 = 0;
const NO_ACCEPTABLE: u8 = 0xff;
const CMD_CONNECT: u8 = 1;
const ATYP_V4: u8 = 1;
const ATYP_DOMAIN: u8 = 3;
const ATYP_V6: u8 = 4;

const REP_OK: u8 = 0;
const REP_FAILURE: u8 = 1;
const REP_NOT_ALLOWED: u8 = 2;
const REP_HOST_UNREACHABLE: u8 = 4;
const REP_REFUSED: u8 = 5;
const REP_CMD_UNSUPPORTED: u8 = 7;
const REP_ATYP_UNSUPPORTED: u8 = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    Ip(SocketAddr),
    Domain(String, u16),
}

/// Serves one SOCKS5 session on `client`: handshake, connect from this process, then relay
/// until both sides close, an error occurs or the connection goes idle.
pub async fn serve<S>(mut client: S, cfg: &Config) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let deadline = Instant::now() + cfg.connect_timeout;
    let target = timeout_at(deadline, handshake(&mut client)).await??;
    if target == Target::Ip(udpgw::ADDR) {
        timeout(cfg.connect_timeout, reply(&mut client, REP_OK, None)).await??;
        return udpgw::serve(client, cfg).await;
    }
    let (rep, outbound) = match timeout_at(deadline, connect(&target, cfg)).await {
        Ok(Ok(s)) => (REP_OK, Ok(s)),
        Ok(Err(e)) => (rep_code(&e), Err(e)),
        Err(elapsed) => (REP_HOST_UNREACHABLE, Err(elapsed.into())),
    };
    let bound = outbound.as_ref().ok().and_then(|s| s.local_addr().ok());
    // A client that never reads the reply must not hold the session.
    timeout(cfg.connect_timeout, reply(&mut client, rep, bound)).await??;
    let outbound = outbound?;

    // Both sides see every byte, and whichever direction is still open polls one of them.
    let mut client = Idle::new(client, cfg.idle_timeout);
    let mut outbound = Idle::new(outbound, cfg.idle_timeout);
    tokio::io::copy_bidirectional_with_sizes(&mut client, &mut outbound, COPY_BUF, COPY_BUF)
        .await?;
    Ok(())
}

async fn handshake<S>(s: &mut S) -> io::Result<Target>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut head = [0u8; 2];
    s.read_exact(&mut head).await?;
    if head[0] != VER {
        return Err(invalid("not a SOCKS5 client"));
    }
    let mut methods = [0u8; 255];
    let methods = &mut methods[..head[1] as usize];
    s.read_exact(methods).await?;
    if !methods.contains(&NO_AUTH) {
        s.write_all(&[VER, NO_ACCEPTABLE]).await?;
        return Err(invalid("client does not offer no-auth"));
    }
    s.write_all(&[VER, NO_AUTH]).await?;

    let mut req = [0u8; 4];
    s.read_exact(&mut req).await?;
    if req[0] != VER {
        return Err(invalid("bad request version"));
    }
    if !matches!(req[3], ATYP_V4 | ATYP_V6 | ATYP_DOMAIN) {
        reply(s, REP_ATYP_UNSUPPORTED, None).await?;
        return Err(invalid("unsupported address type"));
    }
    let target = read_address(s, req[3]).await?;
    if req[1] != CMD_CONNECT {
        reply(s, REP_CMD_UNSUPPORTED, None).await?;
        return Err(invalid("unsupported command"));
    }
    Ok(target)
}

/// Reads a SOCKS5 address of type `atyp`: IPv4, IPv6 or domain name, then the port.
pub(crate) async fn read_address<R: AsyncRead + Unpin>(r: &mut R, atyp: u8) -> io::Result<Target> {
    match atyp {
        ATYP_V4 => {
            let mut ip = [0u8; 4];
            r.read_exact(&mut ip).await?;
            let ip = Ipv4Addr::from(ip).into();
            Ok(Target::Ip(SocketAddr::new(ip, r.read_u16().await?)))
        }
        ATYP_V6 => {
            let mut ip = [0u8; 16];
            r.read_exact(&mut ip).await?;
            let ip = Ipv6Addr::from(ip).into();
            Ok(Target::Ip(SocketAddr::new(ip, r.read_u16().await?)))
        }
        ATYP_DOMAIN => {
            let mut name = vec![0u8; r.read_u8().await? as usize];
            r.read_exact(&mut name).await?;
            let name = String::from_utf8(name).map_err(|_| invalid("domain is not UTF-8"))?;
            Ok(Target::Domain(name, r.read_u16().await?))
        }
        _ => Err(invalid("unsupported address type")),
    }
}

/// Resolves domain names on the phone. Callers apply the target policy.
pub(crate) async fn resolve(target: &Target) -> io::Result<Vec<SocketAddr>> {
    match target {
        Target::Ip(addr) => Ok(vec![*addr]),
        Target::Domain(host, port) => Ok(tokio::net::lookup_host((host.as_str(), *port))
            .await
            .map_err(|e| io::Error::new(ErrorKind::NotFound, e))?
            .collect()),
    }
}

/// Drops refused addresses and tries the rest in order.
async fn connect(target: &Target, cfg: &Config) -> io::Result<TcpStream> {
    let mut last = None;
    for addr in resolve(target).await? {
        let attempt = if cfg.allows(addr.ip()) {
            TcpStream::connect(addr).await
        } else {
            Err(io::Error::new(
                ErrorKind::PermissionDenied,
                "target not allowed",
            ))
        };
        match attempt {
            Ok(stream) => return Ok(stream),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::new(ErrorKind::NotFound, "name has no address")))
}

fn rep_code(e: &io::Error) -> u8 {
    match e.kind() {
        ErrorKind::PermissionDenied => REP_NOT_ALLOWED,
        ErrorKind::ConnectionRefused => REP_REFUSED,
        ErrorKind::NotFound | ErrorKind::TimedOut => REP_HOST_UNREACHABLE,
        _ => REP_FAILURE,
    }
}

async fn reply<S: AsyncWrite + Unpin>(
    s: &mut S,
    rep: u8,
    bound: Option<SocketAddr>,
) -> io::Result<()> {
    let bound = bound.unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 0)));
    let mut buf = Vec::with_capacity(22);
    buf.extend_from_slice(&[VER, rep, 0]);
    put_address(&mut buf, bound);
    s.write_all(&buf).await?;
    s.flush().await
}

/// Appends `addr` in SOCKS5 form: ATYP, address, port.
pub(crate) fn put_address(buf: &mut Vec<u8>, addr: SocketAddr) {
    match addr.ip() {
        IpAddr::V4(ip) => {
            buf.push(ATYP_V4);
            buf.extend_from_slice(&ip.octets());
        }
        IpAddr::V6(ip) => {
            buf.push(ATYP_V6);
            buf.extend_from_slice(&ip.octets());
        }
    }
    buf.extend_from_slice(&addr.port().to_be_bytes());
}

fn invalid(msg: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, msg)
}

/// Fails a pending read or write with `TimedOut` once no bytes moved for `limit`.
struct Idle<S> {
    inner: S,
    limit: Duration,
    sleep: Pin<Box<Sleep>>,
}

impl<S> Idle<S> {
    fn new(inner: S, limit: Duration) -> Self {
        let sleep = Box::pin(tokio::time::sleep(limit));
        Self {
            inner,
            limit,
            sleep,
        }
    }

    fn touch(&mut self) {
        let deadline = Instant::now() + self.limit;
        self.sleep.as_mut().reset(deadline);
    }

    fn expired<T>(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<T>> {
        ready!(self.sleep.as_mut().poll(cx));
        Poll::Ready(Err(io::Error::new(ErrorKind::TimedOut, "idle timeout")))
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Idle<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            Poll::Ready(r) => {
                this.touch();
                Poll::Ready(r)
            }
            Poll::Pending => this.expired(cx),
        }
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Idle<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        match Pin::new(&mut this.inner).poll_write(cx, buf) {
            Poll::Ready(r) => {
                this.touch();
                Poll::Ready(r)
            }
            Poll::Pending => this.expired(cx),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = &mut *self;
        match Pin::new(&mut this.inner).poll_flush(cx) {
            Poll::Ready(r) => Poll::Ready(r),
            Poll::Pending => this.expired(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = &mut *self;
        match Pin::new(&mut this.inner).poll_shutdown(cx) {
            Poll::Ready(r) => Poll::Ready(r),
            Poll::Pending => this.expired(cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, DuplexStream};
    use tokio::net::TcpListener;

    fn loopback_cfg() -> Config {
        Config {
            allow_loopback: true,
            ..Config::default()
        }
    }

    async fn echo_server(addr: &str) -> SocketAddr {
        let listener = TcpListener::bind(addr).await.unwrap();
        let local = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let (mut r, mut w) = s.split();
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                });
            }
        });
        local
    }

    fn start(cfg: Config) -> (DuplexStream, tokio::task::JoinHandle<io::Result<()>>) {
        let (client, server) = duplex(64 * 1024);
        let task = tokio::spawn(async move { serve(server, &cfg).await });
        (client, task)
    }

    fn ip_request(addr: SocketAddr) -> Vec<u8> {
        let mut req = vec![VER, CMD_CONNECT, 0];
        match addr.ip() {
            IpAddr::V4(ip) => {
                req.push(ATYP_V4);
                req.extend_from_slice(&ip.octets());
            }
            IpAddr::V6(ip) => {
                req.push(ATYP_V6);
                req.extend_from_slice(&ip.octets());
            }
        }
        req.extend_from_slice(&addr.port().to_be_bytes());
        req
    }

    fn domain_request(name: &str, port: u16) -> Vec<u8> {
        let mut req = vec![VER, CMD_CONNECT, 0, ATYP_DOMAIN, name.len() as u8];
        req.extend_from_slice(name.as_bytes());
        req.extend_from_slice(&port.to_be_bytes());
        req
    }

    /// Sends the greeting and `req`, returns the reply code.
    async fn request(client: &mut DuplexStream, req: &[u8]) -> u8 {
        client.write_all(&[VER, 1, NO_AUTH]).await.unwrap();
        let mut choice = [0u8; 2];
        client.read_exact(&mut choice).await.unwrap();
        assert_eq!(choice, [VER, NO_AUTH]);
        client.write_all(req).await.unwrap();
        let mut head = [0u8; 4];
        client.read_exact(&mut head).await.unwrap();
        let rest = if head[3] == ATYP_V6 { 18 } else { 6 };
        let mut bound = vec![0u8; rest];
        client.read_exact(&mut bound).await.unwrap();
        head[1]
    }

    async fn assert_echo(client: &mut DuplexStream) {
        client.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
    }

    #[tokio::test]
    async fn connects_ipv4() {
        let echo = echo_server("127.0.0.1:0").await;
        let (mut client, _task) = start(loopback_cfg());
        assert_eq!(request(&mut client, &ip_request(echo)).await, REP_OK);
        assert_echo(&mut client).await;
    }

    #[tokio::test]
    async fn connects_ipv6() {
        let echo = echo_server("[::1]:0").await;
        let (mut client, _task) = start(loopback_cfg());
        assert_eq!(request(&mut client, &ip_request(echo)).await, REP_OK);
        assert_echo(&mut client).await;
    }

    #[tokio::test]
    async fn resolves_domain_on_the_relay() {
        let echo = echo_server("127.0.0.1:0").await;
        let (mut client, _task) = start(loopback_cfg());
        let req = domain_request("localhost", echo.port());
        assert_eq!(request(&mut client, &req).await, REP_OK);
        assert_echo(&mut client).await;
    }

    #[tokio::test]
    async fn refuses_loopback_by_default() {
        let echo = echo_server("127.0.0.1:0").await;
        let (mut client, task) = start(Config::default());
        assert_eq!(
            request(&mut client, &ip_request(echo)).await,
            REP_NOT_ALLOWED
        );
        let err = task.await.unwrap().unwrap_err();
        assert_eq!(err.kind(), ErrorKind::PermissionDenied);
    }

    #[tokio::test]
    async fn refuses_names_that_resolve_to_loopback() {
        let echo = echo_server("127.0.0.1:0").await;
        let (mut client, _task) = start(Config::default());
        let req = domain_request("localhost", echo.port());
        assert_eq!(request(&mut client, &req).await, REP_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn refuses_link_local_without_connecting() {
        let (mut client, _task) = start(Config::default());
        let req = ip_request("169.254.1.1:80".parse().unwrap());
        assert_eq!(request(&mut client, &req).await, REP_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn reports_refused_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closed = listener.local_addr().unwrap();
        drop(listener);
        let (mut client, _task) = start(loopback_cfg());
        assert_eq!(request(&mut client, &ip_request(closed)).await, REP_REFUSED);
    }

    #[tokio::test]
    async fn serves_udpgw_on_its_reserved_address() {
        let (mut client, _task) = start(Config::default());
        assert_eq!(request(&mut client, &ip_request(udpgw::ADDR)).await, REP_OK);
        client.write_all(&[0, 3, 0x01, 0, 5]).await.unwrap();
        let mut keepalive = [0u8; 5];
        client.read_exact(&mut keepalive).await.unwrap();
        assert_eq!(keepalive, [0, 3, 0x01, 0, 5]);
    }

    #[tokio::test]
    async fn rejects_clients_without_no_auth() {
        let (mut client, task) = start(loopback_cfg());
        client.write_all(&[VER, 1, 2]).await.unwrap();
        let mut choice = [0u8; 2];
        client.read_exact(&mut choice).await.unwrap();
        assert_eq!(choice, [VER, NO_ACCEPTABLE]);
        assert!(task.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn rejects_bind_command() {
        let (mut client, _task) = start(loopback_cfg());
        let mut req = ip_request("1.1.1.1:80".parse().unwrap());
        req[1] = 2;
        assert_eq!(request(&mut client, &req).await, REP_CMD_UNSUPPORTED);
    }

    #[tokio::test]
    async fn rejects_unknown_address_type() {
        let (mut client, _task) = start(loopback_cfg());
        assert_eq!(
            request(&mut client, &[VER, CMD_CONNECT, 0, 9]).await,
            REP_ATYP_UNSUPPORTED
        );
    }

    #[tokio::test]
    async fn times_out_a_silent_handshake() {
        let cfg = Config {
            connect_timeout: Duration::from_millis(50),
            ..loopback_cfg()
        };
        let (_client, task) = start(cfg);
        let err = task.await.unwrap().unwrap_err();
        assert_eq!(err.kind(), ErrorKind::TimedOut);
    }

    #[tokio::test]
    async fn closes_idle_connections() {
        let echo = echo_server("127.0.0.1:0").await;
        let cfg = Config {
            idle_timeout: Duration::from_millis(100),
            ..loopback_cfg()
        };
        let (mut client, task) = start(cfg);
        assert_eq!(request(&mut client, &ip_request(echo)).await, REP_OK);
        assert_echo(&mut client).await;
        let err = task.await.unwrap().unwrap_err();
        assert_eq!(err.kind(), ErrorKind::TimedOut);
        let mut buf = [0u8; 1];
        assert_eq!(client.read(&mut buf).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn closes_half_closed_idle_connections() {
        // The target sends a greeting and closes; the client never closes its side.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            s.write_all(b"bye").await.unwrap();
            s.shutdown().await.unwrap();
            let mut sink = Vec::new();
            let _ = s.read_to_end(&mut sink).await;
        });
        let cfg = Config {
            idle_timeout: Duration::from_millis(100),
            ..loopback_cfg()
        };
        let (mut client, task) = start(cfg);
        assert_eq!(request(&mut client, &ip_request(addr)).await, REP_OK);
        let mut buf = Vec::new();
        client.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf, b"bye");
        let err = task.await.unwrap().unwrap_err();
        assert_eq!(err.kind(), ErrorKind::TimedOut);
    }
}
