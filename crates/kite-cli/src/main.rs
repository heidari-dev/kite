//! `kite`: shares a phone's connection with this computer through the Kite relay.

#![forbid(unsafe_code)]

use std::io;
use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpSocket, TcpStream};

mod proxy;
#[cfg(target_os = "linux")]
mod route;
#[cfg(unix)]
mod status;
#[cfg(target_os = "linux")]
mod tun;

const USAGE: &str = "usage:
  kite connect --tcp HOST:PORT [--listen ADDR]
      Tunnel through the relay at HOST:PORT and serve a SOCKS5 proxy on ADDR
      (a loopback address, default 127.0.0.1:1080).
  kite connect --tcp HOST:PORT --tun
      Send all traffic and DNS of this computer through the relay (Linux, needs
      CAP_NET_ADMIN).
  kite status
      Show the running tunnel.
  kite restore
      Remove routing rules left behind by a TUN mode run that was killed.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("connect") => parse_connect(&args[1..]).and_then(|c| {
            let tun = c.tun;
            block_on(connect(c), tun)
        }),
        Some("status") => block_on(status(), false),
        Some("restore") => block_on(restore(), false),
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

#[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
fn block_on<F: std::future::Future<Output = io::Result<()>>>(f: F, tun: bool) -> io::Result<()> {
    let mut rt = tokio::runtime::Builder::new_current_thread();
    #[cfg(target_os = "linux")]
    if tun {
        // tun2proxy's TCP stack calls block_in_place, which a current_thread runtime refuses.
        rt = tokio::runtime::Builder::new_multi_thread();
        rt.worker_threads(1);
    }
    // Name lookups; block_in_place also borrows a thread from this pool.
    rt.enable_all().max_blocking_threads(4).build()?.block_on(f)
}

fn usage(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.into())
}

#[derive(Debug)]
struct Connect {
    relay: String,
    listen: SocketAddr,
    tun: bool,
}

fn parse_connect(args: &[String]) -> io::Result<Connect> {
    let mut relay = None;
    let mut listen = SocketAddr::from(([127, 0, 0, 1], 1080));
    let mut tun = false;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        if flag == "--tun" {
            if !cfg!(target_os = "linux") {
                return Err(usage("--tun is only available on Linux"));
            }
            tun = true;
            continue;
        }
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
    if tun && listen != SocketAddr::from(([127, 0, 0, 1], 1080)) {
        return Err(usage("--listen does not apply to --tun"));
    }
    Ok(Connect { relay, listen, tun })
}

async fn connect(c: Connect) -> io::Result<()> {
    // TUN mode feeds tun2proxy through a proxy on a free port.
    let listen = if c.tun {
        SocketAddr::from(([127, 0, 0, 1], 0))
    } else {
        c.listen
    };
    let listener = TcpListener::bind(listen).await?;
    let tunnel = dial(&c.relay, c.tun).await?;
    let active = Arc::new(AtomicUsize::new(0));
    let mode = if c.tun {
        "all traffic through the tunnel".to_string()
    } else {
        format!("SOCKS5 proxy on {}", listener.local_addr()?)
    };
    #[cfg(unix)]
    let _status = status::serve(status::Info {
        relay: c.relay.clone(),
        mode: mode.clone(),
        active: active.clone(),
        started: std::time::Instant::now(),
    })
    .map_err(|e| eprintln!("kite: status unavailable: {e}"));
    println!("connected to {}, {mode}", c.relay);
    #[cfg(target_os = "linux")]
    if c.tun {
        return tun::run(tunnel, listener, active, shutdown_signal()).await;
    }
    proxy::run(tunnel, listener, active, shutdown_signal()).await
}

/// Connects to the relay. Keepalive notices a peer that vanished without closing. In TUN mode the
/// socket carries the routing mark that keeps it out of the tunnel it carries.
#[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
async fn dial(relay: &str, tun: bool) -> io::Result<TcpStream> {
    let connect = async {
        let mut last = io::Error::new(io::ErrorKind::NotFound, "relay name did not resolve");
        for addr in tokio::net::lookup_host(relay).await? {
            let sock = if addr.is_ipv4() {
                TcpSocket::new_v4()?
            } else {
                TcpSocket::new_v6()?
            };
            #[cfg(target_os = "linux")]
            if tun {
                socket2::SockRef::from(&sock).set_mark(route::TABLE)?;
            }
            match sock.connect(addr).await {
                Ok(s) => return Ok(s),
                Err(e) => last = e,
            }
        }
        Err(last)
    };
    let s = tokio::time::timeout(Duration::from_secs(10), connect)
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

#[cfg(target_os = "linux")]
async fn restore() -> io::Result<()> {
    // While a TUN mode kite runs, its rules are live; removing them would leak its traffic.
    if std::path::Path::new("/sys/class/net")
        .join(tun::NAME)
        .exists()
    {
        return Err(io::Error::other("kite is running in TUN mode"));
    }
    route::restore().await
}

#[cfg(not(target_os = "linux"))]
async fn restore() -> io::Result<()> {
    Ok(())
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
        assert!(!c.tun);
        let c = parse_connect(&args("--tun --tcp 10.0.0.2:7000"));
        assert_eq!(c.is_ok_and(|c| c.tun), cfg!(target_os = "linux"));
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
            "--tcp a:1 --tun --listen 127.0.0.1:9050",
        ] {
            assert!(parse_connect(&args(bad)).is_err(), "{bad}");
        }
    }
}
