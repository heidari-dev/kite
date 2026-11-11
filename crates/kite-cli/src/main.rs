//! `kite`: shares a phone's connection with this computer through the Kite relay.

#![forbid(unsafe_code)]

use std::io;
use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};

mod proxy;
#[cfg(unix)]
mod status;

const USAGE: &str = "usage:
  kite connect --tcp HOST:PORT [--listen ADDR]
      Tunnel through the relay at HOST:PORT and serve a SOCKS5 proxy on ADDR
      (a loopback address, default 127.0.0.1:1080).
  kite status
      Show the running tunnel.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("connect") => parse_connect(&args[1..]).and_then(|c| block_on(connect(c))),
        Some("status") => block_on(status()),
        Some("--version" | "-V") => {
            println!("kite {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--help" | "-h" | "help") => {
            println!("{USAGE}");
            Ok(())
        }
        _ => Err(usage("expected a command")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("kite: {e}");
            if e.kind() == io::ErrorKind::InvalidInput {
                eprintln!("{USAGE}");
            }
            ExitCode::FAILURE
        }
    }
}

fn block_on<F: std::future::Future<Output = io::Result<()>>>(f: F) -> io::Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        // Only the relay's name is looked up, once.
        .max_blocking_threads(1)
        .build()?
        .block_on(f)
}

fn usage(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.into())
}

#[derive(Debug)]
struct Connect {
    relay: String,
    listen: SocketAddr,
}

fn parse_connect(args: &[String]) -> io::Result<Connect> {
    let mut relay = None;
    let mut listen = SocketAddr::from(([127, 0, 0, 1], 1080));
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let value = it
            .next()
            .ok_or_else(|| usage(format!("{flag} needs a value")))?;
        match flag.as_str() {
            "--tcp" => relay = Some(value.clone()),
            "--listen" => {
                listen = value.parse().map_err(|_| usage("--listen takes IP:PORT"))?;
            }
            _ => return Err(usage(format!("unknown option {flag}"))),
        }
    }
    // Anyone who can reach the proxy uses the phone's connection.
    if !listen.ip().is_loopback() {
        return Err(usage("--listen must be a loopback address"));
    }
    let relay = relay.ok_or_else(|| usage("connect needs --tcp HOST:PORT"))?;
    Ok(Connect { relay, listen })
}

async fn connect(c: Connect) -> io::Result<()> {
    let listener = TcpListener::bind(c.listen).await?;
    let tunnel = dial(&c.relay).await?;
    let active = Arc::new(AtomicUsize::new(0));
    let proxy = listener.local_addr()?;
    #[cfg(unix)]
    let _status = status::serve(status::Info {
        relay: c.relay.clone(),
        proxy,
        active: active.clone(),
        started: std::time::Instant::now(),
    })
    .map_err(|e| eprintln!("kite: status unavailable: {e}"));
    println!("connected to {}, SOCKS5 proxy on {proxy}", c.relay);
    proxy::run(tunnel, listener, active, shutdown_signal()).await
}

/// Connects to the relay. Keepalive notices a peer that vanished without closing.
async fn dial(relay: &str) -> io::Result<TcpStream> {
    let s = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(relay))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "relay did not answer"))??;
    s.set_nodelay(true)?;
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(30))
        .with_interval(Duration::from_secs(10));
    socket2::SockRef::from(&s).set_tcp_keepalive(&keepalive)?;
    Ok(s)
}

async fn shutdown_signal() {
    #[cfg(unix)]
    if let Ok(mut term) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
        return;
    }
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(unix)]
async fn status() -> io::Result<()> {
    status::query().await
}

#[cfg(not(unix))]
async fn status() -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "status is only available on Linux",
    ))
}

#[cfg(test)]
mod tests {
    use super::parse_connect;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn parses_connect() {
        let c = parse_connect(&args("--tcp relay.local:7000")).unwrap();
        assert_eq!(c.relay, "relay.local:7000");
        assert_eq!(c.listen, "127.0.0.1:1080".parse().unwrap());
        let c = parse_connect(&args("--listen [::1]:9050 --tcp 10.0.0.2:7000")).unwrap();
        assert_eq!(c.listen, "[::1]:9050".parse().unwrap());
    }

    #[test]
    fn refuses_bad_connect_arguments() {
        for bad in [
            "",
            "--tcp",
            "--tcp a:1 --listen 0.0.0.0:1080",
            "--tcp a:1 --listen 192.168.1.2:1080",
            "--tcp a:1 --listen localhost:1080",
            "--tcp a:1 --udp b:2",
        ] {
            assert!(parse_connect(&args(bad)).is_err(), "{bad}");
        }
    }
}
