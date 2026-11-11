//! Proxy mode: one yamux tunnel to the relay and a local listener whose every connection becomes a
//! new stream. The relay speaks SOCKS5 on each stream, so this side never parses it.

use std::future::{poll_fn, Future};
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};

/// Most concurrent streams. The relay closes the whole tunnel past 256 and may keep a stream for
/// up to its connect timeout after this side dropped it, hence the headroom.
pub const MAX_STREAMS: usize = 224;

/// Copy buffer per direction per connection.
const BUF: usize = 16 * 1024;

/// A local client must send its first bytes within this time, or it gives up its slot.
const FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(10);

type OpenRequest = oneshot::Sender<yamux::Stream>;

/// Serves local connections from `listener` through a tunnel to the relay until the tunnel fails
/// or `shutdown` completes. `active` tracks the number of open streams.
pub async fn run<T>(
    tunnel: T,
    listener: TcpListener,
    active: Arc<AtomicUsize>,
    shutdown: impl Future<Output = ()>,
) -> io::Result<()>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let (opener, requests) = mpsc::channel::<OpenRequest>(1);
    let driver = drive(tunnel, requests);
    tokio::pin!(driver, shutdown);
    let mut conns = JoinSet::new();
    loop {
        active.store(conns.len(), Ordering::Relaxed);
        tokio::select! {
            r = &mut driver => return r,
            Some(_) = conns.join_next() => {}
            // At the cap new connections wait in the kernel's accept queue.
            accepted = listener.accept(), if conns.len() < MAX_STREAMS => match accepted {
                Ok((local, _)) => {
                    conns.spawn(pipe(local, opener.clone()));
                }
                Err(e) => {
                    eprintln!("kite: accept: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
            () = &mut shutdown => {
                conns.shutdown().await;
                drop(opener);
                // The relay gets a clean close unless it stopped answering.
                let _ = tokio::time::timeout(Duration::from_secs(2), driver).await;
                return Ok(());
            }
        }
    }
}

/// Pipes one local connection into a new stream until both sides are done.
async fn pipe(mut local: tokio::net::TcpStream, opener: mpsc::Sender<OpenRequest>) {
    let _ = local.set_nodelay(true);
    // No stream is opened before the client speaks, so idle connections cannot hold slots forever.
    let mut first = [0u8; 1];
    match tokio::time::timeout(FIRST_BYTE_TIMEOUT, local.peek(&mut first)).await {
        Ok(Ok(n)) if n > 0 => {}
        _ => return,
    }
    let (tx, rx) = oneshot::channel();
    if opener.send(tx).await.is_err() {
        return;
    }
    let Ok(stream) = rx.await else { return };
    let mut stream = stream.compat();
    let _ = tokio::io::copy_bidirectional_with_sizes(&mut local, &mut stream, BUF, BUF).await;
}

/// Runs the yamux client on `tunnel` and opens a stream for each request. Closes the tunnel cleanly
/// once every request sender is gone; an error if the relay closed it or it failed.
async fn drive<T>(tunnel: T, mut requests: mpsc::Receiver<OpenRequest>) -> io::Result<()>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let mut cfg = yamux::Config::default();
    cfg.set_max_num_streams(MAX_STREAMS);
    // Same per-tunnel limit as the relay: every stream keeps yamux's fixed 256 KiB window.
    cfg.set_max_connection_receive_window(Some(MAX_STREAMS * yamux::DEFAULT_CREDIT as usize));
    let mut conn = yamux::Connection::new(tunnel.compat(), cfg, yamux::Mode::Client);
    let mut waiting: Option<OpenRequest> = None;
    poll_fn(|cx| loop {
        if waiting.is_none() {
            match requests.poll_recv(cx) {
                Poll::Ready(Some(tx)) => waiting = Some(tx),
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => {}
            }
        }
        if let Some(tx) = waiting.take() {
            match conn.poll_new_outbound(cx) {
                Poll::Ready(Ok(stream)) => {
                    let _ = tx.send(stream);
                    continue;
                }
                // The stream cap keeps this below yamux's own limit, so any error is fatal.
                Poll::Ready(Err(e)) => return Poll::Ready(Err(io::Error::other(e))),
                Poll::Pending => waiting = Some(tx),
            }
        }
        // Polling inbound is what moves bytes. The relay never opens streams; any it does is dropped.
        match conn.poll_next_inbound(cx) {
            Poll::Ready(Some(Ok(_))) => {}
            Poll::Ready(Some(Err(e))) => return Poll::Ready(Err(io::Error::other(e))),
            Poll::Ready(None) => {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the relay closed the tunnel",
                )))
            }
            Poll::Pending => return Poll::Pending,
        }
    })
    .await?;
    // shortcut: a relay that hangs but keeps TCP open is not noticed (yamux 0.14 has no keepalive
    // ping); add a liveness check with the encrypted session layer (#9).
    poll_fn(|cx| conn.poll_close(cx))
        .await
        .map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt, DuplexStream};
    use tokio::net::TcpStream;
    use tokio::task::JoinHandle;

    async fn echo_server() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let (mut r, mut w) = s.split();
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                });
            }
        });
        addr
    }

    /// A relay on one end of an in-memory tunnel; the other end is for the client.
    fn relay() -> (DuplexStream, JoinHandle<io::Result<()>>) {
        let (client, server) = duplex(64 * 1024);
        let cfg = kite_relay::Config {
            allow_loopback: true,
            ..kite_relay::Config::default()
        };
        (client, tokio::spawn(kite_relay::serve(server, cfg)))
    }

    struct Proxy {
        addr: SocketAddr,
        active: Arc<AtomicUsize>,
        stop: oneshot::Sender<()>,
        task: JoinHandle<io::Result<()>>,
    }

    async fn proxy<T>(tunnel: T) -> Proxy
    where
        T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let active = Arc::new(AtomicUsize::new(0));
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(run(tunnel, listener, active.clone(), async {
            let _ = stopped.await;
        }));
        Proxy {
            addr,
            active,
            stop,
            task,
        }
    }

    /// SOCKS5 greeting and CONNECT to `to` in one write; returns the reply code.
    async fn socks_connect(s: &mut TcpStream, to: SocketAddr) -> u8 {
        let SocketAddr::V4(to) = to else {
            panic!("IPv4 only")
        };
        let mut req = vec![5, 1, 0, 5, 1, 0, 1];
        req.extend_from_slice(&to.ip().octets());
        req.extend_from_slice(&to.port().to_be_bytes());
        s.write_all(&req).await.unwrap();
        let mut resp = [0u8; 12];
        s.read_exact(&mut resp).await.unwrap();
        resp[1]
    }

    async fn echoes(s: &mut TcpStream, msg: &[u8]) {
        s.write_all(msg).await.unwrap();
        let mut back = vec![0u8; msg.len()];
        s.read_exact(&mut back).await.unwrap();
        assert_eq!(back, msg);
    }

    #[tokio::test]
    async fn pipes_a_local_connection_through_the_relay() {
        let echo = echo_server().await;
        let (tunnel, _relay) = relay();
        let p = proxy(tunnel).await;
        let mut s = TcpStream::connect(p.addr).await.unwrap();
        assert_eq!(socks_connect(&mut s, echo).await, 0);
        let data: Vec<u8> = (0..300_000u32).map(|i| i as u8).collect();
        let (mut r, mut w) = s.into_split();
        let sent = data.clone();
        let writer = tokio::spawn(async move { w.write_all(&sent).await.unwrap() });
        let mut back = vec![0u8; data.len()];
        r.read_exact(&mut back).await.unwrap();
        writer.await.unwrap();
        assert!(back == data, "echoed bytes differ");
    }

    #[tokio::test]
    async fn holds_connections_past_the_cap_until_a_stream_frees() {
        let echo = echo_server().await;
        let (tunnel, relay) = relay();
        let p = proxy(tunnel).await;
        let mut open = Vec::new();
        for _ in 0..MAX_STREAMS {
            let mut s = TcpStream::connect(p.addr).await.unwrap();
            assert_eq!(socks_connect(&mut s, echo).await, 0);
            open.push(s);
        }
        assert_eq!(p.active.load(Ordering::Relaxed), MAX_STREAMS);

        // One more connects in the kernel but gets no stream yet.
        let mut extra = TcpStream::connect(p.addr).await.unwrap();
        let waiting =
            tokio::time::timeout(Duration::from_millis(300), socks_connect(&mut extra, echo));
        assert!(waiting.await.is_err(), "served past the cap");

        // Its request is already sent; closing one stream lets it through.
        let mut first = open.remove(0);
        first.shutdown().await.unwrap();
        let mut resp = [0u8; 12];
        extra.read_exact(&mut resp).await.unwrap();
        assert_eq!(resp[1], 0);
        echoes(&mut extra, b"after the cap").await;
        echoes(&mut open[0], b"tunnel still up").await;
        assert!(!relay.is_finished(), "relay closed the tunnel");
    }

    #[tokio::test(start_paused = true)]
    async fn drops_a_silent_local_client() {
        let (tunnel, _relay) = relay();
        let p = proxy(tunnel).await;
        let mut s = TcpStream::connect(p.addr).await.unwrap();
        let mut buf = [0u8; 1];
        // The paused clock jumps past the first-byte timeout while nothing else can run.
        assert!(matches!(s.read(&mut buf).await, Ok(0) | Err(_)));
        assert_eq!(p.active.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn ends_when_the_relay_closes_the_tunnel() {
        let (tunnel, relay) = duplex(1024);
        let p = proxy(tunnel).await;
        drop(relay);
        assert!(p.task.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn shutdown_closes_the_tunnel_cleanly() {
        let echo = echo_server().await;
        let (tunnel, relay) = relay();
        let p = proxy(tunnel).await;
        let mut s = TcpStream::connect(p.addr).await.unwrap();
        assert_eq!(socks_connect(&mut s, echo).await, 0);
        p.stop.send(()).unwrap();
        p.task.await.unwrap().unwrap();
        // The relay sees the tunnel end and the local connection is closed.
        relay.await.unwrap().unwrap();
        let mut buf = [0u8; 1];
        assert!(matches!(s.read(&mut buf).await, Ok(0) | Err(_)));
    }
}
