//! UDP gateway, wire-compatible with the udpgw client in tun2proxy.
//!
//! Every frame is `LEN u16 | FLAGS u8 | CONN_ID u16 | [SOCKS5 address] | payload`, big-endian,
//! where LEN counts the bytes after itself and only DATA frames carry an address. Each
//! CONN_ID gets its own UDP socket on the phone, connected to the resolved target.

use crate::socks5::{put_address, read_address, resolve, Target};
use crate::Config;
use socket2::{Domain, Socket, Type};
use std::collections::hash_map::{Entry, HashMap};
use std::future::poll_fn;
use std::io::{self, ErrorKind};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::UdpSocket;
use tokio::time::{timeout, Instant};

/// SOCKS5 CONNECT target that selects this handler instead of a TCP connection. TEST-NET-1
/// (RFC 5737) never routes, and tun2proxy's virtual DNS already owns 198.18.0.0/15.
pub const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 7300);

const KEEPALIVE: u8 = 0x01;
const DATA: u8 = 0x02;
const CLOSE: u8 = 0x04;
const ERR: u8 = 0x20;

/// Largest frame: the length field plus a body of up to `u16::MAX` bytes.
const MAX_FRAME: usize = 2 + u16::MAX as usize;
/// UDP sockets per gateway stream. tun2proxy uses one stream per flow, so this is generous.
const MAX_SESSIONS: usize = 16;
/// A UDP socket with no traffic for this long is closed.
const UDP_IDLE: Duration = Duration::from_secs(60);

struct Session {
    sock: UdpSocket,
    target: Target,
    /// FLAGS, CONN_ID and the peer address: everything in a reply frame after LEN.
    head: Vec<u8>,
    last: Instant,
}

/// Serves udpgw frames on `stream` until it closes, sends a malformed frame, stops reading or
/// stays silent for the idle timeout. tun2proxy's keepalives count as traffic.
pub async fn serve<S>(mut stream: S, cfg: &Config) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut sessions = HashMap::new();
    let mut inbuf = Vec::new();
    // Zeroed, so pages are only touched by datagrams that need them.
    let mut out = vec![0u8; MAX_FRAME];
    let mut sweep = tokio::time::interval(UDP_IDLE);
    let mut active = Instant::now();
    loop {
        tokio::select! {
            n = read_more(&mut stream, &mut inbuf) => {
                if n? == 0 {
                    return Ok(());
                }
                active = Instant::now();
                while let Some(len) = frame_len(&inbuf)? {
                    let control = handle(&inbuf[2..len], &mut sessions, cfg).await;
                    inbuf.drain(..len);
                    if let Some(frame) = control {
                        timeout(cfg.idle_timeout, stream.write_all(&frame)).await??;
                    }
                }
                // tun2proxy keeps idle gateway streams pooled; don't let one keep a big frame's buffer.
                if inbuf.is_empty() && inbuf.capacity() > 8192 {
                    inbuf = Vec::new();
                }
            }
            (id, got) = poll_fn(|cx| poll_recv_any(&sessions, &mut out, cx)) => {
                let frame = match (got, sessions.get_mut(&id)) {
                    (Ok(n), Some(s)) => {
                        s.last = Instant::now();
                        let body = s.head.len() + n;
                        out[..2].copy_from_slice(&(body as u16).to_be_bytes());
                        out[2..2 + s.head.len()].copy_from_slice(&s.head);
                        &out[..2 + body]
                    }
                    _ => {
                        sessions.remove(&id);
                        out[..5].copy_from_slice(&control(ERR, id));
                        &out[..5]
                    }
                };
                timeout(cfg.idle_timeout, stream.write_all(frame)).await??;
                active = Instant::now();
            }
            _ = sweep.tick() => sessions.retain(|_, s| s.last.elapsed() < UDP_IDLE),
            _ = tokio::time::sleep_until(active + cfg.idle_timeout) => {
                return Err(io::Error::new(ErrorKind::TimedOut, "idle timeout"));
            }
        }
    }
}

/// Cancel safe: `read_buf` either reads into `buf` or leaves it untouched.
async fn read_more<R: AsyncRead + Unpin>(r: &mut R, buf: &mut Vec<u8>) -> io::Result<usize> {
    buf.reserve(4096);
    r.read_buf(buf).await
}

/// Length of the first complete frame in `buf`, if there is one.
fn frame_len(buf: &[u8]) -> io::Result<Option<usize>> {
    let [hi, lo, ..] = *buf else { return Ok(None) };
    let body = u16::from_be_bytes([hi, lo]) as usize;
    if body < 3 {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "udpgw frame too short",
        ));
    }
    Ok((buf.len() >= 2 + body).then_some(2 + body))
}

fn control(flags: u8, id: u16) -> [u8; 5] {
    let [hi, lo] = id.to_be_bytes();
    [0, 3, flags, hi, lo]
}

/// Handles one frame body and returns the control frame to send back, if any.
async fn handle(
    body: &[u8],
    sessions: &mut HashMap<u16, Session>,
    cfg: &Config,
) -> Option<[u8; 5]> {
    let id = u16::from_be_bytes([body[1], body[2]]);
    match body[0] {
        KEEPALIVE => Some(control(KEEPALIVE, id)),
        CLOSE => {
            sessions.remove(&id);
            None
        }
        DATA => match send(id, &body[3..], sessions, cfg).await {
            Ok(()) => None,
            Err(_) => {
                sessions.remove(&id);
                Some(control(ERR, id))
            }
        },
        _ => Some(control(ERR, id)),
    }
}

async fn send(
    id: u16,
    mut rest: &[u8],
    sessions: &mut HashMap<u16, Session>,
    cfg: &Config,
) -> io::Result<()> {
    let atyp = rest.read_u8().await?;
    let target = read_address(&mut rest, atyp).await?;
    let full = sessions.len() >= MAX_SESSIONS;
    let session = match sessions.entry(id) {
        Entry::Occupied(e) if e.get().target == target => e.into_mut(),
        Entry::Occupied(mut e) => {
            e.insert(open(id, target, cfg).await?);
            e.into_mut()
        }
        Entry::Vacant(_) if full => return Err(io::Error::other("too many UDP sessions")),
        Entry::Vacant(e) => e.insert(open(id, target, cfg).await?),
    };
    session.last = Instant::now();
    session.sock.send(rest).await?;
    Ok(())
}

async fn open(id: u16, target: Target, cfg: &Config) -> io::Result<Session> {
    let peer = timeout(cfg.connect_timeout, resolve(&target))
        .await??
        .into_iter()
        .find(|a| cfg.allows(a.ip()))
        .ok_or_else(|| io::Error::new(ErrorKind::PermissionDenied, "target not allowed"))?;
    let sock = Socket::new(Domain::for_address(peer), Type::DGRAM, None)?;
    // iOS and macOS cap datagrams at the send buffer size, 9216 bytes by default.
    sock.set_send_buffer_size(u16::MAX as usize)?;
    sock.set_nonblocking(true)?;
    let any: IpAddr = match peer {
        SocketAddr::V4(_) => Ipv4Addr::UNSPECIFIED.into(),
        SocketAddr::V6(_) => Ipv6Addr::UNSPECIFIED.into(),
    };
    sock.bind(&SocketAddr::new(any, 0).into())?;
    let sock = UdpSocket::from_std(sock.into())?;
    sock.connect(peer).await?;

    let mut head = vec![DATA];
    head.extend_from_slice(&id.to_be_bytes());
    put_address(&mut head, peer);
    Ok(Session {
        sock,
        target,
        head,
        last: Instant::now(),
    })
}

/// Receives one datagram from any session straight into `out`, after room for its header.
// shortcut: always scans sessions in map order, so one busy socket can starve the others on
// the same stream; tun2proxy opens one stream per flow, revisit if a client multiplexes more.
fn poll_recv_any(
    sessions: &HashMap<u16, Session>,
    out: &mut [u8],
    cx: &mut Context<'_>,
) -> Poll<(u16, io::Result<usize>)> {
    for (&id, s) in sessions {
        let mut buf = ReadBuf::new(&mut out[2 + s.head.len()..]);
        if let Poll::Ready(r) = s.sock.poll_recv(cx, &mut buf) {
            return Poll::Ready((id, r.map(|()| buf.filled().len())));
        }
    }
    Poll::Pending
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, DuplexStream};

    fn loopback_cfg() -> Config {
        Config {
            allow_loopback: true,
            ..Config::default()
        }
    }

    fn start(cfg: Config) -> DuplexStream {
        let (client, server) = duplex(MAX_FRAME * 2);
        tokio::spawn(async move { serve(server, &cfg).await });
        client
    }

    async fn udp_echo(addr: &str) -> SocketAddr {
        let addr: SocketAddr = addr.parse().unwrap();
        let sock = Socket::new(Domain::for_address(addr), Type::DGRAM, None).unwrap();
        sock.set_send_buffer_size(u16::MAX as usize).unwrap();
        sock.set_nonblocking(true).unwrap();
        sock.bind(&addr.into()).unwrap();
        let sock = UdpSocket::from_std(sock.into()).unwrap();
        let local = sock.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = vec![0u8; u16::MAX as usize];
            while let Ok((n, from)) = sock.recv_from(&mut buf).await {
                let _ = sock.send_to(&buf[..n], from).await;
            }
        });
        local
    }

    fn data_frame(id: u16, to: SocketAddr, payload: &[u8]) -> Vec<u8> {
        let mut body = vec![DATA];
        body.extend_from_slice(&id.to_be_bytes());
        put_address(&mut body, to);
        body.extend_from_slice(payload);
        let mut frame = (body.len() as u16).to_be_bytes().to_vec();
        frame.extend_from_slice(&body);
        frame
    }

    async fn read_frame(client: &mut DuplexStream) -> Vec<u8> {
        let len = client.read_u16().await.unwrap();
        let mut body = vec![0u8; len as usize];
        client.read_exact(&mut body).await.unwrap();
        body
    }

    async fn round_trip(payload_len: usize) {
        let echo = udp_echo("127.0.0.1:0").await;
        let mut client = start(loopback_cfg());
        let payload: Vec<u8> = (0..payload_len).map(|i| i as u8).collect();
        client
            .write_all(&data_frame(7, echo, &payload))
            .await
            .unwrap();
        let body = read_frame(&mut client).await;
        let mut expected = vec![DATA, 0, 7];
        put_address(&mut expected, echo);
        expected.extend_from_slice(&payload);
        assert_eq!(body.len(), expected.len());
        assert!(body == expected, "reply differs from the echoed datagram");
    }

    #[tokio::test]
    async fn relays_dns_sized_datagram() {
        round_trip(512).await;
    }

    #[tokio::test]
    async fn relays_max_sized_datagram() {
        // Largest IPv4 UDP payload; its frame body is 65517 of 65535 bytes.
        round_trip(65_507).await;
    }

    #[tokio::test]
    async fn relays_over_ipv6() {
        let echo = udp_echo("[::1]:0").await;
        let mut client = start(loopback_cfg());
        client.write_all(&data_frame(1, echo, b"q")).await.unwrap();
        let body = read_frame(&mut client).await;
        assert_eq!(body.last(), Some(&b'q'));
        assert_eq!(&body[..3], &[DATA, 0, 1]);
    }

    #[tokio::test]
    async fn answers_keepalive() {
        let mut client = start(loopback_cfg());
        client.write_all(&control(KEEPALIVE, 9)).await.unwrap();
        assert_eq!(read_frame(&mut client).await, [KEEPALIVE, 0, 9]);
    }

    #[tokio::test]
    async fn refuses_loopback_by_default() {
        let echo = udp_echo("127.0.0.1:0").await;
        let mut client = start(Config::default());
        client.write_all(&data_frame(3, echo, b"x")).await.unwrap();
        assert_eq!(read_frame(&mut client).await, [ERR, 0, 3]);
    }

    #[tokio::test]
    async fn reuses_the_stream_after_close_with_a_new_id() {
        let echo = udp_echo("127.0.0.1:0").await;
        let mut client = start(loopback_cfg());
        client.write_all(&data_frame(1, echo, b"a")).await.unwrap();
        assert_eq!(read_frame(&mut client).await.last(), Some(&b'a'));
        client.write_all(&control(CLOSE, 1)).await.unwrap();
        client.write_all(&data_frame(2, echo, b"b")).await.unwrap();
        let body = read_frame(&mut client).await;
        assert_eq!(&body[..3], &[DATA, 0, 2]);
        assert_eq!(body.last(), Some(&b'b'));
    }

    #[tokio::test]
    async fn closes_a_silent_stream() {
        let cfg = Config {
            idle_timeout: Duration::from_millis(100),
            ..loopback_cfg()
        };
        let mut client = start(cfg);
        client.write_all(&control(KEEPALIVE, 1)).await.unwrap();
        assert_eq!(read_frame(&mut client).await, [KEEPALIVE, 0, 1]);
        let mut rest = Vec::new();
        client.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty());
    }

    #[test]
    fn frame_len_waits_for_a_whole_frame() {
        assert_eq!(frame_len(&[]).unwrap(), None);
        assert_eq!(frame_len(&[0]).unwrap(), None);
        assert_eq!(frame_len(&[0, 3, KEEPALIVE, 0]).unwrap(), None);
        assert_eq!(frame_len(&[0, 3, KEEPALIVE, 0, 1, 0]).unwrap(), Some(5));
        assert!(frame_len(&[0, 2, 0, 0]).is_err());
    }
}
