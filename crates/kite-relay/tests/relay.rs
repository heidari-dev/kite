//! A yamux client talks to the relay over an in-memory tunnel, the way the desktop will.

use kite_relay::{serve, Config};
use std::future::poll_fn;
use std::net::SocketAddr;
use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::compat::{Compat, FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};

type Stream = Compat<yamux::Stream>;

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

/// Starts a relay on one end of an in-memory tunnel and opens `n` streams from the other.
async fn open_streams(n: usize) -> Vec<Stream> {
    let (client, server) = duplex(64 * 1024);
    let cfg = Config {
        allow_loopback: true,
        ..Config::default()
    };
    tokio::spawn(serve(server, cfg));
    let mut conn = yamux::Connection::new(
        client.compat(),
        yamux::Config::default(),
        yamux::Mode::Client,
    );
    let mut streams = Vec::new();
    for _ in 0..n {
        let stream = poll_fn(|cx| conn.poll_new_outbound(cx)).await.unwrap();
        streams.push(stream.compat());
    }
    // The connection only moves bytes while it is polled.
    tokio::spawn(async move { while poll_fn(|cx| conn.poll_next_inbound(cx)).await.is_some() {} });
    streams
}

async fn socks_connect(s: &mut Stream, to: SocketAddr) -> u8 {
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

#[tokio::test]
async fn relays_tcp_through_yamux_and_socks5() {
    let echo = echo_server().await;
    let mut s = open_streams(1).await.remove(0);
    assert_eq!(socks_connect(&mut s, echo).await, 0);
    let data: Vec<u8> = (0..300_000u32).map(|i| i as u8).collect();
    let (mut r, mut w) = tokio::io::split(s);
    let sent = data.clone();
    let writer = tokio::spawn(async move { w.write_all(&sent).await.unwrap() });
    let mut back = vec![0u8; data.len()];
    r.read_exact(&mut back).await.unwrap();
    writer.await.unwrap();
    assert!(back == data, "echoed bytes differ");
}

#[tokio::test]
async fn serves_100_concurrent_connections() {
    let echo = echo_server().await;
    let mut tasks = Vec::new();
    for (i, mut s) in open_streams(100).await.into_iter().enumerate() {
        tasks.push(tokio::spawn(async move {
            assert_eq!(socks_connect(&mut s, echo).await, 0);
            let msg = format!("stream {i}");
            s.write_all(msg.as_bytes()).await.unwrap();
            let mut back = vec![0u8; msg.len()];
            s.read_exact(&mut back).await.unwrap();
            assert_eq!(back, msg.as_bytes());
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
}

#[tokio::test]
async fn serves_udpgw_through_yamux() {
    let mut s = open_streams(1).await.remove(0);
    assert_eq!(socks_connect(&mut s, kite_relay::udpgw::ADDR).await, 0);
    s.write_all(&[0, 3, 0x01, 0, 42]).await.unwrap();
    let mut keepalive = [0u8; 5];
    s.read_exact(&mut keepalive).await.unwrap();
    assert_eq!(keepalive, [0, 3, 0x01, 0, 42]);
}

#[tokio::test]
async fn closes_a_tunnel_without_sessions() {
    let (_client, server) = duplex(1024);
    let cfg = Config {
        idle_timeout: std::time::Duration::from_millis(100),
        ..Config::default()
    };
    let err = serve(server, cfg).await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
}
