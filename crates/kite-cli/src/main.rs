//! `kite`: shares a phone's connection with this computer through the Kite relay.

#![forbid(unsafe_code)]

use kite_relay::pair;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;
use tokio::net::{TcpSocket, TcpStream};

#[cfg(any(unix, windows))]
mod client;
#[cfg(any(target_os = "linux", windows))]
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
#[cfg(windows)]
#[path = "tun_win.rs"]
mod tun;

const USAGE: &str = "usage:
  kite daemon [--state-dir DIR]
      Run the background service that owns the tunnel (Linux and Windows). The
      commands below talk to it on /run/kite/kite.sock, or on \\\\.\\pipe\\kite on Windows.
  kite pair (--tcp HOST:PORT | --wifi [ADDR[:PORT]] | --usb [--serial SERIAL])
      Pair this computer with the phone: type the 6-digit code it shows.
  kite peers [remove KEY]
      List the paired phones, or forget the one whose key starts with KEY.
  kite connect (--tcp HOST:PORT | --wifi [ADDR[:PORT]] | --usb [--serial SERIAL]) [--listen ADDR]
      Tunnel through the relay at HOST:PORT, through the phone on the same WiFi or
      hotspot (its address, port 47810 unless given, or found with mDNS if left
      out; turn on Share over WiFi in the app), or through a phone connected by USB
      (needs a running adb server), and serve a SOCKS5 proxy on ADDR
      (a loopback address, default 127.0.0.1:1080).
  kite connect (--tcp HOST:PORT | --wifi [ADDR[:PORT]] | --usb [--serial SERIAL]) --tun [--kill-switch]
      Send all traffic and DNS of this computer through the relay (Linux, needs
      CAP_NET_ADMIN; Windows, needs an administrator). With --kill-switch (Linux
      only), block everything while the link is down and reconnect when the phone
      is back, until kite disconnect.
  kite disconnect
      Close the tunnel and lift the kill switch.
  kite devices
      List the phones connected by USB.
  kite status
      Show the running tunnel.
  kite restore
      Remove routes, rules and DNS settings left behind by a TUN mode run that was killed.

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

#[cfg_attr(not(any(unix, windows)), allow(unused_variables))]
fn run(args: &[String], socket: &Path, foreground: bool) -> io::Result<()> {
    match args.first().map(String::as_str) {
        Some("daemon") => daemon_command(&args[1..], socket),
        Some("connect") => parse_connect(&args[1..]).and_then(|c| {
            #[cfg(any(unix, windows))]
            if !foreground {
                return block_on(client::connect(socket, &c), false);
            }
            let tun = c.tun;
            block_on(connect(c), tun)
        }),
        Some("pair") => parse_connect(&args[1..]).and_then(|c| {
            if c.tun || c.kill_switch || c.listen != DEFAULT_LISTEN {
                return Err(usage("pair takes only --tcp, --wifi, --usb and --serial"));
            }
            #[cfg(any(unix, windows))]
            if !foreground {
                return block_on(client::pair(socket, &c.transport), false);
            }
            block_on(pair(c.transport), false)
        }),
        Some("peers") => {
            #[cfg(any(unix, windows))]
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

#[cfg(any(target_os = "linux", windows))]
fn daemon_command(args: &[String], socket: &Path) -> io::Result<()> {
    let mut args = args.to_vec();
    #[cfg(windows)]
    let service = take_flag(&mut args, "--service");
    let state = take_option(&mut args, "--state-dir")?.unwrap_or_else(|| {
        #[cfg(windows)]
        return kite_win::data_dir();
        #[cfg(not(windows))]
        PathBuf::from("/var/lib/kite")
    });
    if !args.is_empty() {
        return Err(usage("daemon takes only --state-dir and --socket"));
    }
    let opts = daemon::Options {
        socket: socket.to_path_buf(),
        state,
    };
    #[cfg(windows)]
    if service {
        return run_service(opts);
    }
    block_on(daemon::run(opts, shutdown_signal()), false)
}

/// `kite daemon --service`: what the service manager starts. The log goes to `kite.log` in the
/// state directory, since a service has no console.
#[cfg(windows)]
fn run_service(opts: daemon::Options) -> io::Result<()> {
    kite_win::private_dir(&opts.state)?;
    let log = opts.state.join("kite.log");
    kite_win::service::run(&log, move |control| {
        let kite_win::service::Control { stop, up } = control;
        let stop = async {
            let _ = stop.await;
        };
        block_on(
            async {
                // The DNS rule of a TUN run that was killed survives a reboot; take it away.
                if let Err(e) = tun::restore().await {
                    eprintln!("kite: cleaning up after an earlier run: {e}");
                }
                daemon::run_with(opts, stop, || {
                    let _ = up.running();
                })
                .await
            },
            false,
        )
    })
}

#[cfg(not(any(target_os = "linux", windows)))]
fn daemon_command(_: &[String], _: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the daemon is only available on Linux and Windows",
    ))
}

/// `disconnect` and `devices` only make sense against the daemon.
#[cfg(any(unix, windows))]
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

#[cfg(not(any(unix, windows)))]
fn daemon_only(_: &[String], _: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the daemon is only available on Linux and Windows",
    ))
}

#[cfg_attr(not(any(target_os = "linux", windows)), allow(unused_variables))]
fn block_on<F: std::future::Future<Output = io::Result<()>>>(f: F, tun: bool) -> io::Result<()> {
    let mut rt = tokio::runtime::Builder::new_current_thread();
    // Name lookups; in TUN mode block_in_place also takes a thread from this pool.
    let mut blocking = 4;
    #[cfg(any(target_os = "linux", windows))]
    if tun {
        // tun2proxy's TCP stack calls block_in_place, which a current_thread runtime refuses.
        rt = tokio::runtime::Builder::new_multi_thread();
        rt.worker_threads(1);
        // Dropping a connection there blocks its thread until the connection's task is done, and
        // that task needs a worker: the worker moves to another thread of this pool for as long
        // as the first one waits. With the pool full the only worker is stuck for good and
        // nothing runs, timers and the stop included, so a flood of connections ending together
        // (a disconnect) could freeze the tunnel. Threads are made when needed and go when idle.
        blocking = 512;
    }
    let rt = rt.enable_all().max_blocking_threads(blocking).build()?;
    let result = rt.block_on(f);
    // A task that does not stop must not keep the tunnel from ending: dropping the runtime would
    // wait for it for ever.
    rt.shutdown_timeout(Duration::from_secs(2));
    result
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
    /// The phone on the same WiFi or hotspot: its address (port 47810 if left out), or None to
    /// find it with mDNS. Becomes a `Tcp` one in [`resolve`].
    Wifi(Option<String>),
}

impl std::fmt::Display for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Transport::Tcp(addr) | Transport::Wifi(Some(addr)) => f.write_str(addr),
            Transport::Usb(_) => f.write_str("usb"),
            Transport::Wifi(None) => f.write_str("wifi"),
        }
    }
}

/// An address that dials: a `Wifi` transport becomes the phone's `HOST:PORT`, found with mDNS if
/// none was given. A name is looked up and an mDNS question is asked once, here, so a link that is
/// down later (with the kill switch) never waits on either.
async fn resolve(t: &Transport) -> io::Result<Transport> {
    Ok(match t {
        Transport::Wifi(Some(addr)) => Transport::Tcp(with_default_port(addr)),
        Transport::Wifi(None) => Transport::Tcp(find_phone().await?.to_string()),
        t => t.clone(),
    })
}

fn with_default_port(addr: &str) -> String {
    let port = kite_relay::wifi::PORT;
    if addr.parse::<SocketAddr>().is_ok() {
        addr.into()
    } else if let Ok(ip) = addr.trim_matches(['[', ']']).parse::<std::net::IpAddr>() {
        SocketAddr::new(ip, port).to_string()
    } else if addr.contains(':') {
        addr.into()
    } else {
        format!("{addr}:{port}")
    }
}

async fn find_phone() -> io::Result<SocketAddr> {
    let found = kite_link::mdns::discover().await?;
    let list = || {
        found
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };
    match found.as_slice() {
        [one] => Ok(*one),
        [] => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no phone answered on this network: is Share over WiFi on, and are both on the same WiFi? \
             Or give the phone's address: --wifi ADDRESS",
        )),
        _ => Err(io::Error::other(format!(
            "several phones answered ({}); pick one with --wifi ADDRESS",
            list()
        ))),
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
    let mut wifi: Option<Option<String>> = None;
    let mut serial = None;
    let mut listen = DEFAULT_LISTEN;
    let mut tun = false;
    let mut kill_switch = false;
    let mut it = args.iter().peekable();
    while let Some(flag) = it.next() {
        if flag == "--kill-switch" {
            if cfg!(windows) {
                return Err(usage("--kill-switch is not available on Windows yet"));
            }
            kill_switch = true;
            continue;
        }
        if flag == "--tun" {
            if !cfg!(any(target_os = "linux", windows)) {
                return Err(usage("--tun is only available on Linux and Windows"));
            }
            tun = true;
            continue;
        }
        if flag == "--usb" {
            usb = true;
            continue;
        }
        if flag == "--wifi" {
            // The address is optional: it is whatever follows that is not another option.
            wifi = Some(it.next_if(|a| !a.starts_with("--")).cloned());
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
    let transport = match (relay, usb, wifi) {
        (Some(addr), false, None) if serial.is_none() => Transport::Tcp(addr),
        (None, true, None) => Transport::Usb(serial),
        (None, false, Some(addr)) if serial.is_none() => Transport::Wifi(addr),
        (None, false, None) => return Err(usage("connect needs --tcp HOST:PORT, --wifi or --usb")),
        (Some(_), false, None) => return Err(usage("--serial only applies to --usb")),
        (None, false, Some(_)) => return Err(usage("--serial only applies to --usb")),
        _ => return Err(usage("use only one of --tcp, --wifi and --usb")),
    };
    if tun && listen != DEFAULT_LISTEN {
        return Err(usage("--listen does not apply to --tun"));
    }
    if kill_switch && !tun {
        return Err(usage("--kill-switch needs --tun"));
    }
    if kill_switch
        && match &transport {
            Transport::Tcp(a) => a.parse::<SocketAddr>().is_err(),
            Transport::Wifi(Some(a)) => {
                a.parse::<SocketAddr>().is_err() && a.parse::<std::net::IpAddr>().is_err()
            }
            _ => false,
        }
    {
        return Err(usage(
            "--kill-switch needs an IP address, not a name, for --tcp and --wifi",
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
    let s = match &resolve(transport).await? {
        Transport::Wifi(_) => return Err(io::Error::other("the WiFi address was not resolved")),
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
#[cfg_attr(not(any(target_os = "linux", windows)), allow(unused_variables))]
async fn dial_tcp(relay: &str, tun: bool) -> io::Result<TcpStream> {
    let connect = async {
        let mut last = io::Error::new(io::ErrorKind::NotFound, "relay name did not resolve");
        for addr in tokio::net::lookup_host(relay).await? {
            let sock = if addr.is_ipv4() {
                TcpSocket::new_v4()?
            } else {
                TcpSocket::new_v6()?
            };
            #[cfg(any(target_os = "linux", windows))]
            if tun {
                // Every packet to port 53 is taken for DNS, so this socket would loop.
                if addr.port() == 53 {
                    return Err(io::Error::other("TUN mode cannot reach a relay on port 53"));
                }
                // The mark keeps this socket out of the tunnel (Windows: a host route does).
                #[cfg(target_os = "linux")]
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

#[cfg(any(target_os = "linux", windows))]
async fn restore() -> io::Result<()> {
    tun::restore().await
}

#[cfg(not(any(target_os = "linux", windows)))]
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

/// On Windows only the service answers; a `--foreground` tunnel does not.
#[cfg(windows)]
async fn status(socket: &Path) -> io::Result<()> {
    client::status(socket).await
}

#[cfg(not(any(unix, windows)))]
async fn status(_: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "status is only available on Linux and Windows",
    ))
}

#[cfg(test)]
mod tests {
    use super::{parse_connect, Transport};

    /// TUN mode exists on Linux and Windows; the kill switch on Linux only.
    const TUN: bool = cfg!(any(target_os = "linux", windows));
    const KILL_SWITCH: bool = cfg!(target_os = "linux");

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
        assert_eq!(c.is_ok_and(|c| c.tun && !c.kill_switch), TUN);
        let c = parse_connect(&args("--tun --kill-switch --usb"));
        assert_eq!(c.is_ok_and(|c| c.kill_switch), KILL_SWITCH);
    }

    #[test]
    fn a_task_that_does_not_stop_does_not_hold_the_runtime() {
        let started = std::time::Instant::now();
        super::block_on(
            async {
                tokio::task::spawn_blocking(|| {
                    std::thread::sleep(std::time::Duration::from_secs(20))
                });
                // Let the blocking task start.
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                Ok(())
            },
            false,
        )
        .unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn parses_usb() {
        let c = parse_connect(&args("--usb")).unwrap();
        assert_eq!(c.transport, Transport::Usb(None));
        let c = parse_connect(&args("--usb --serial R5CT1234")).unwrap();
        assert_eq!(c.transport, Transport::Usb(Some("R5CT1234".into())));
    }

    #[test]
    fn parses_wifi() {
        let wifi = |s: &str| parse_connect(&args(s)).map(|c| c.transport);
        assert_eq!(wifi("--wifi").unwrap(), Transport::Wifi(None));
        assert_eq!(
            wifi("--wifi 192.168.43.1").unwrap(),
            Transport::Wifi(Some("192.168.43.1".into()))
        );
        // The address is only what is not another option.
        let c = parse_connect(&args("--wifi --listen 127.0.0.1:9050")).unwrap();
        assert_eq!(c.transport, Transport::Wifi(None));
        assert_eq!(c.listen, "127.0.0.1:9050".parse().unwrap());
        assert!(parse_connect(&args("--wifi --tun --kill-switch")).is_ok() == KILL_SWITCH);
        for bad in [
            "--tcp 10.0.0.2 --tun --kill-switch",
            "--wifi --usb",
            "--wifi a:1 --tcp b:2",
            "--wifi --serial X",
            "--wifi phone.local --tun --kill-switch",
        ] {
            assert!(parse_connect(&args(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn gives_the_wifi_address_the_default_port() {
        use super::with_default_port as port;
        assert_eq!(port("192.168.43.1"), "192.168.43.1:47810");
        assert_eq!(port("192.168.43.1:9"), "192.168.43.1:9");
        assert_eq!(port("phone.local"), "phone.local:47810");
        assert_eq!(port("phone.local:9"), "phone.local:9");
        assert_eq!(port("[fd00::1]"), "[fd00::1]:47810");
        assert_eq!(port("fd00::1"), "[fd00::1]:47810");
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
