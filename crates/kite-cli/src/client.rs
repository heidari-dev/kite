//! The `kite` commands that talk to the daemon over its socket (protocol in `kite-ipc`).

use crate::{read_code, Connect, Transport};
use kite_ipc::{read_line, Mode, Request, Response, State, MAX_RESPONSE};
use std::io;
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::{sleep, timeout};

/// The daemon answers within 30 s; this is a little more.
const REPLY_TIMEOUT: Duration = Duration::from_secs(35);
/// How long `connect` waits for the tunnel to come up.
const CONNECT_WAIT: Duration = Duration::from_secs(60);

/// Sends one request. An error answer becomes an `Err` with the daemon's message.
async fn call(socket: &Path, req: &Request) -> io::Result<Response> {
    let mut s = UnixStream::connect(socket)
        .await
        .map_err(|e| match e.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => io::Error::new(
                e.kind(),
                "the kite daemon is not running; start it (systemctl start kite) or use --foreground",
            ),
            io::ErrorKind::PermissionDenied => io::Error::new(
                e.kind(),
                "no permission to use the kite daemon; join the kite group, or use --foreground",
            ),
            _ => e,
        })?;
    let answer = timeout(REPLY_TIMEOUT, async {
        s.write_all(&req.line()).await?;
        read_line(&mut BufReader::new(s), MAX_RESPONSE).await
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no answer from the kite daemon"))??
    .ok_or_else(|| io::Error::other("the kite daemon closed the connection"))?;
    let response = Response::parse(&answer)?;
    if response.ok {
        Ok(response)
    } else {
        Err(io::Error::other(
            response.error.unwrap_or_else(|| "refused".into()),
        ))
    }
}

fn transport(t: &Transport) -> kite_ipc::Transport {
    match t {
        Transport::Tcp(addr) => kite_ipc::Transport::Tcp { addr: addr.clone() },
        Transport::Usb(serial) => kite_ipc::Transport::Usb {
            serial: serial.clone(),
        },
    }
}

async fn state(socket: &Path) -> io::Result<kite_ipc::Status> {
    call(socket, &Request::Status {})
        .await?
        .status
        .ok_or_else(|| io::Error::other("no status in the answer"))
}

/// The first key of an answer that carries one.
fn first_key(r: Response) -> String {
    r.peers
        .and_then(|p| p.into_iter().next())
        .unwrap_or_default()
}

pub async fn connect(socket: &Path, c: &Connect) -> io::Result<()> {
    let mode = if c.tun { Mode::Tun } else { Mode::Proxy };
    let shown = if c.tun {
        "all traffic through the tunnel".to_string()
    } else {
        format!("SOCKS5 proxy on {}", c.listen)
    };
    call(
        socket,
        &Request::Connect {
            transport: transport(&c.transport),
            mode,
            kill_switch: c.kill_switch,
            listen: (!c.tun && c.listen != crate::DEFAULT_LISTEN).then_some(c.listen),
        },
    )
    .await?;
    let deadline = tokio::time::Instant::now() + CONNECT_WAIT;
    loop {
        let s = state(socket).await?;
        match s.state {
            State::Connected => {
                println!("connected to {}, {shown}", c.transport);
                return Ok(());
            }
            State::Idle => {
                return Err(io::Error::other(
                    s.error.unwrap_or_else(|| "the tunnel stopped".into()),
                ))
            }
            State::Connecting | State::Reconnecting => {}
        }
        if tokio::time::Instant::now() > deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "still connecting; see kite status",
            ));
        }
        sleep(Duration::from_millis(200)).await;
    }
}

pub async fn disconnect(socket: &Path) -> io::Result<()> {
    call(socket, &Request::Disconnect {}).await.map(drop)
}

pub async fn devices(socket: &Path) -> io::Result<()> {
    let devices = call(socket, &Request::Devices {})
        .await?
        .devices
        .unwrap_or_default();
    if devices.is_empty() {
        println!("no phones connected by USB");
    }
    for d in devices {
        println!("{}\t{}", d.serial, d.state);
    }
    Ok(())
}

pub async fn status(socket: &Path) -> io::Result<()> {
    let s = state(socket).await?;
    let (Some(relay), Some(mode)) = (s.transport, s.mode) else {
        return Err(io::Error::new(io::ErrorKind::NotFound, "not connected"));
    };
    let stats = call(socket, &Request::Stats {})
        .await?
        .stats
        .ok_or_else(|| io::Error::other("no stats in the answer"))?;
    let mode = match mode {
        Mode::Tun => "all traffic through the tunnel",
        Mode::Proxy => "SOCKS5 proxy",
    };
    println!("state: {}", format!("{:?}", s.state).to_lowercase());
    println!("relay: {relay}");
    println!("mode: {mode}");
    if s.kill_switch {
        println!("kill switch: on");
    }
    println!("streams: {} of {}", stats.streams, stats.max_streams);
    println!("reconnects: {}", stats.reconnects);
    println!("uptime: {}s", stats.uptime_secs);
    if let Some(error) = s.error {
        println!("last error: {error}");
    }
    Ok(())
}

pub async fn pair(socket: &Path, t: &Transport) -> io::Result<()> {
    call(
        socket,
        &Request::PairStart {
            transport: transport(t),
        },
    )
    .await?;
    println!("Type the 6-digit code shown on the phone and press Enter:");
    let code = read_code().await?;
    let r = call(
        socket,
        &Request::Pair {
            code: code.trim().into(),
        },
    )
    .await?;
    println!("paired with phone {}", first_key(r));
    Ok(())
}

pub async fn peers(socket: &Path, args: &[String]) -> io::Result<()> {
    match args {
        [] => {
            let peers = call(socket, &Request::Peers {})
                .await?
                .peers
                .unwrap_or_default();
            if peers.is_empty() {
                println!("no paired phones; run kite pair");
            }
            for key in peers {
                println!("{key}");
            }
            Ok(())
        }
        [cmd, key] if cmd == "remove" => {
            let r = call(socket, &Request::RemovePeer { key: key.clone() }).await?;
            println!("removed phone {}", first_key(r));
            Ok(())
        }
        _ => Err(crate::usage("peers takes no arguments, or remove KEY")),
    }
}
