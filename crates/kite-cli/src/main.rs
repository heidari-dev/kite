//! `kite`: shares a phone's connection with this computer through the Kite relay.

#![forbid(unsafe_code)]

use kite_relay::pair;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;
use tokio::net::{TcpSocket, TcpStream};

#[cfg(unix)]
mod client;
#[cfg(target_os = "linux")]
mod daemon;
mod proxy;
#[cfg(target_os = "linux")]
mod route;
mod session;
#[cfg(unix)]
mod status;
mod store;
#[cfg(target_os = "linux")]
mod tun;

const USAGE: &str = "usage:
  kite daemon [--state-dir DIR]
      Run the background service that owns the tunnel (Linux). The commands below
      talk to it on /run/kite/kite.sock.
  kite pair (--tcp HOST:PORT | --usb [--serial SERIAL])
      Pair this computer with the phone: type the 6-digit code it shows.
  kite peers [remove KEY]
      List the paired phones, or forget the one whose key starts with KEY.
  kite connect (--tcp HOST:PORT | --usb [--serial SERIAL]) [--listen ADDR]
      Tunnel through the relay at HOST:PORT, or through a phone connected by USB
      (needs a running adb server), and serve a SOCKS5 proxy on ADDR
      (a loopback address, default 127.0.0.1:1080).
  kite connect (--tcp HOST:PORT | --usb [--serial SERIAL]) --tun [--kill-switch]
      Send all traffic and DNS of this computer through the relay (Linux, needs
      CAP_NET_ADMIN). With --kill-switch, block everything while the link is down
      and reconnect when the phone is back, until kite disconnect.
  kite disconnect
      Close the tunnel and lift the kill switch.
  kite devices
      List the phones connected by USB.
  kite status
      Show the running tunnel.
  kite restore
      Remove routing rules left behind by a TUN mode run that was killed.

pair, peers and connect run in the daemon; add --foreground to run them in this
process instead, with this user's own key and phones. --socket PATH picks another
daemon socket.";

const DEFAULT_LISTEN: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 1080);

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let foreground = take_flag(&mut args, "--foreground");
    let result = take_option(&mut args, "--socket").and_then(|socket| {
        let socket = socket.unwrap_or_else(|| PathBuf::from(kite_ipc::DEFAULT_SOCKET));
        run(&args, &socket, foreground)
    });
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

/// Removes `flag` from `args` and says whether it was there.
fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let before = args.len();
    args.retain(|a| a != flag);
    args.len() != before
}

/// Removes `flag VALUE` from `args`.
fn take_option(args: &mut Vec<String>, flag: &str) -> io::Result<Option<PathBuf>> {
    let Some(at) = args.iter().position(|a| a == flag) else {
        return Ok(None);
    };
    if at + 1 >= args.len() {
        return Err(usage(format!("{flag} needs a value")));
    }
    let value = args.remove(at + 1);
    args.remove(at);
    Ok(Some(PathBuf::from(value)))
}

#[cfg_attr(not(unix), allow(unused_variables))]
fn run(args: &[String], socket: &Path, foreground: bool) -> io::Result<()> {
    match args.first().map(String::as_str) {
        Some("daemon") => daemon_command(&args[1..], socket),
        Some("connect") => parse_connect(&args[1..]).and_then(|c| {
            #[cfg(unix)]
            if !foreground {
                return block_on(client::connect(socket, &c), false);
            }
            let tun = c.tun;
            block_on(connect(c), tun)
        }),
        Some("pair") => parse_connect(&args[1..]).and_then(|c| {
            if c.tun || c.kill_switch || c.listen != DEFAULT_LISTEN {
                return Err(usage("pair takes only --tcp, --usb and --serial"));
            }
            #[cfg(unix)]
            if !foreground {
                return block_on(client::pair(socket, &c.transport), false);
            }
            block_on(pair(c.transport), false)
        }),
        Some("peers") => {
            #[cfg(unix)]
            if !foreground {
                return block_on(client::peers(socket, &args[1..]), false);
            }
            peers(&args[1..])
        }
        Some("disconnect" | "devices") => daemon_only(args, socket),
        Some("status") => block_on(status(socket), false),
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
    }
}

#[cfg(target_os = "linux")]
fn daemon_command(args: &[String], socket: &Path) -> io::Result<()> {
    let mut args = args.to_vec();
    let state =
        take_option(&mut args, "--state-dir")?.unwrap_or_else(|| PathBuf::from("/var/lib/kite"));
    if !args.is_empty() {
        return Err(usage("daemon takes only --state-dir and --socket"));
    }
    let opts = daemon::Options {
        socket: socket.to_path_buf(),
        state,
    };
    block_on(daemon::run(opts, shutdown_signal()), false)
}

#[cfg(not(target_os = "linux"))]
fn daemon_command(_: &[String], _: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the daemon is only available on Linux",
    ))
}

/// `disconnect` and `devices` only make sense against the daemon.
#[cfg(unix)]
fn daemon_only(args: &[String], socket: &Path) -> io::Result<()> {
    if args.len() != 1 {
        return Err(usage(format!("{} takes no arguments", args[0])));
    }
    block_on(
        async {
            if args[0] == "devices" {
                client::devices(socket).await
            } else {
                client::disconnect(socket).await
            }
        },
        false,
    )
}

#[cfg(not(unix))]
fn daemon_only(_: &[String], _: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the daemon is only available on Linux",
    ))
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

/// How to reach the relay.
#[derive(Clone, Debug, PartialEq)]
enum Transport {
    Tcp(String),
    /// A phone connected by USB, found through the adb server; the serial picks one of several.
    Usb(Option<String>),
}

impl std::fmt::Display for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Transport::Tcp(addr) => f.write_str(addr),
            Transport::Usb(_) => f.write_str("usb"),
        }
    }
}

#[derive(Debug)]
struct Connect {
    transport: Transport,
    listen: SocketAddr,
    tun: bool,
    kill_switch: bool,
}

fn parse_connect(args: &[String]) -> io::Result<Connect> {
    let mut relay = None;
    let mut usb = false;
    let mut serial = None;
    let mut listen = DEFAULT_LISTEN;
    let mut tun = false;
    let mut kill_switch = false;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        if flag == "--kill-switch" {
            kill_switch = true;
            continue;
        }
        if flag == "--tun" {
            if !cfg!(target_os = "linux") {
                return Err(usage("--tun is only available on Linux"));
            }
            tun = true;
            continue;
        }
        if flag == "--usb" {
            usb = true;
            continue;
        }
        let value = it
            .next()
            .ok_or_else(|| usage(format!("{flag} needs a value")))?;
        match flag.as_str() {
            "--tcp" => relay = Some(value.clone()),
            "--serial" => serial = Some(value.clone()),
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
    let transport = match (relay, usb) {
        (Some(addr), false) if serial.is_none() => Transport::Tcp(addr),
        (None, true) => Transport::Usb(serial),
        (None, false) => return Err(usage("connect needs --tcp HOST:PORT or --usb")),
        (Some(_), true) => return Err(usage("use either --tcp or --usb")),
        (Some(_), false) => return Err(usage("--serial only applies to --usb")),
    };
    if tun && listen != DEFAULT_LISTEN {
        return Err(usage("--listen does not apply to --tun"));
    }
    if kill_switch && !tun {
        return Err(usage("--kill-switch needs --tun"));
    }
    if kill_switch && matches!(&transport, Transport::Tcp(a) if a.parse::<SocketAddr>().is_err()) {
        return Err(usage(
            "--kill-switch needs an IP address, not a name, for --tcp",
        ));
    }
    Ok(Connect {
        transport,
        listen,
        tun,
        kill_switch,
    })
}

/// `kite connect --foreground`: the tunnel runs in this process, with this user's key and phones.
async fn connect(c: Connect) -> io::Result<()> {
    let store = store::Store::open()?;
    let shared = session::Shared::default();
    let relay = c.transport.to_string();
    let params = session::Params {
        transport: c.transport,
        tun: c.tun,
        listen: c.listen,
        kill_switch: c.kill_switch,
    };
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    let started = tokio::select! {
        r = session::start(&params, &store) => r?,
        () = &mut shutdown => return Ok(()),
    };
    let mode = started.mode()?;
    #[cfg(unix)]
    let _status = status::serve(status::Info {
        relay: relay.clone(),
        mode: mode.clone(),
        active: shared.active.clone(),
        started: std::time::Instant::now(),
    })
    .map_err(|e| eprintln!("kite: status unavailable: {e}"));
    println!("connected to {relay}, {mode}");
    started.serve(&shared, shutdown).await
}

/// Connects to the relay. Over TCP, keepalive notices a peer that vanished without closing.
async fn dial(transport: &Transport, tun: bool) -> io::Result<TcpStream> {
    let s = match transport {
        Transport::Tcp(relay) => {
            let s = dial_tcp(relay, tun).await?;
            let keepalive = socket2::TcpKeepalive::new()
                .with_time(Duration::from_secs(30))
                .with_interval(Duration::from_secs(10));
            socket2::SockRef::from(&s).set_tcp_keepalive(&keepalive)?;
            s
        }
        // The adb server on loopback carries the USB link, so this socket needs no mark: loopback
        // is answered by the local routing table before any TUN rule, and there is no IP path to a
        // USB phone to keep out of the tunnel. Keepalive would only be answered by the local
        // kernel, so it is skipped; an unplug or an adb crash closes the socket.
        Transport::Usb(serial) => kite_link::adb::open(serial.as_deref()).await?,
    };
    s.set_nodelay(true)?;
    Ok(s)
}

/// The relay stream after the Noise handshake. yamux runs on this.
type Tunnel = kite_relay::noise::Session<TcpStream>;

/// Pairs with the phone on `transport`: the user types the code the phone shows.
async fn pair(transport: Transport) -> io::Result<()> {
    let store = store::Store::open()?;
    let key = store.key()?;
    let attempt = pair::start(dial(&transport, false).await?, &key).await?;
    let name = pair::fingerprint(&key.public()?).ok_or_else(|| io::Error::other("no BLAKE2s"))?;
    println!("This computer is {name}.");
    println!("Allow it on the phone, then type the 6-digit code the phone shows and press Enter:");
    let phone = attempt.finish(&read_code().await?).await?;
    store.add_phone(phone)?;
    println!("paired with phone {}", store::hex(&phone));
    Ok(())
}

/// One line from stdin, within the code's lifetime. A plain thread, so a timeout does not leave
/// the runtime waiting on a blocked read at exit.
async fn read_code() -> io::Result<String> {
    use std::io::BufRead;
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let read = std::io::Read::take(std::io::stdin().lock(), 64).read_line(&mut line);
        let _ = tx.send(read.map(|_| line));
    });
    match tokio::time::timeout(pair::CODE_LIFETIME, rx).await {
        Ok(Ok(line)) => line,
        Ok(Err(_)) => Err(io::Error::other("stdin closed")),
        Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "the code expired")),
    }
}

fn peers(args: &[String]) -> io::Result<()> {
    let store = store::Store::open()?;
    match args {
        [] => {
            let phones = store.phones()?;
            if phones.is_empty() {
                println!("no paired phones; run kite pair");
            }
            for phone in phones {
                println!("{}", store::hex(&phone));
            }
            Ok(())
        }
        [cmd, key] if cmd == "remove" => {
            let phone = store.remove_phone(key)?;
            println!("removed phone {}", store::hex(&phone));
            Ok(())
        }
        _ => Err(usage("peers takes no arguments, or remove KEY")),
    }
}

/// In TUN mode the socket carries the routing mark that keeps it out of the tunnel it carries.
#[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
async fn dial_tcp(relay: &str, tun: bool) -> io::Result<TcpStream> {
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
                // The DNS rules come before the mark check and would loop this socket.
                if addr.port() == 53 {
                    return Err(io::Error::other("TUN mode cannot reach a relay on port 53"));
                }
                socket2::SockRef::from(&sock).set_mark(route::TABLE)?;
            }
            match sock.connect(addr).await {
                Ok(s) => return Ok(s),
                Err(e) => last = e,
            }
        }
        Err(last)
    };
    tokio::time::timeout(Duration::from_secs(10), connect)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "relay did not answer"))?
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
    tun::restore().await
}

#[cfg(not(target_os = "linux"))]
async fn restore() -> io::Result<()> {
    Ok(())
}

/// The daemon's tunnel, or else a `kite connect --foreground` one.
#[cfg(unix)]
async fn status(socket: &Path) -> io::Result<()> {
    match client::status(socket).await {
        Err(e) if e.to_string().contains("daemon is not running") => status::query().await,
        r => r,
    }
}

#[cfg(not(unix))]
async fn status(_: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "status is only available on Linux",
    ))
}

#[cfg(test)]
mod tests {
    use super::{parse_connect, Transport};

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn parses_connect() {
        let c = parse_connect(&args("--tcp relay.local:7000")).unwrap();
        assert_eq!(c.transport, Transport::Tcp("relay.local:7000".into()));
        assert_eq!(c.listen, "127.0.0.1:1080".parse().unwrap());
        let c = parse_connect(&args("--listen [::1]:9050 --tcp 10.0.0.2:7000")).unwrap();
        assert_eq!(c.listen, "[::1]:9050".parse().unwrap());
        assert!(!c.tun);
        let c = parse_connect(&args("--tun --tcp 10.0.0.2:7000"));
        assert_eq!(
            c.is_ok_and(|c| c.tun && !c.kill_switch),
            cfg!(target_os = "linux")
        );
        let c = parse_connect(&args("--tun --kill-switch --usb"));
        assert_eq!(c.is_ok_and(|c| c.kill_switch), cfg!(target_os = "linux"));
    }

    #[test]
    fn parses_usb() {
        let c = parse_connect(&args("--usb")).unwrap();
        assert_eq!(c.transport, Transport::Usb(None));
        let c = parse_connect(&args("--usb --serial R5CT1234")).unwrap();
        assert_eq!(c.transport, Transport::Usb(Some("R5CT1234".into())));
    }

    #[test]
    fn takes_global_options_out_of_the_arguments() {
        let mut a = args("connect --socket /tmp/k.sock --foreground --usb");
        assert!(super::take_flag(&mut a, "--foreground"));
        assert_eq!(
            super::take_option(&mut a, "--socket").unwrap(),
            Some("/tmp/k.sock".into())
        );
        assert_eq!(a, args("connect --usb"));
        assert!(!super::take_flag(&mut a, "--foreground"));
        let mut a = args("status --socket");
        assert!(super::take_option(&mut a, "--socket").is_err());
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
            "--tcp a:1 --usb",
            "--tcp a:1 --serial X",
            "--usb --serial",
            "--tcp a:1 --kill-switch",
            "--serial X",
        ] {
            assert!(parse_connect(&args(bad)).is_err(), "{bad}");
        }
    }
}
