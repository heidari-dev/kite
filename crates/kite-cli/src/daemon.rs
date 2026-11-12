//! `kite daemon`: owns the transports, the TUN device and the tunnel, and answers the `kite`
//! command and the desktop app on a Unix socket (protocol in `kite-ipc`).
//!
//! Who may talk to it: the socket is 0660 and belongs to the daemon's group, and each caller's
//! credentials (SO_PEERCRED) are checked again: root, the daemon's own user, or a member of the
//! daemon's group. Every request is a fixed command with typed arguments; no path or command line
//! of a client is ever used. The tunnel runs on its own thread with its own runtime, so a slow
//! request never stalls it and it never stalls a request.

use crate::session::{self, Params, Shared};
use crate::store::{hex, Store};
use crate::Transport;
use kite_ipc::{read_line, Device, Mode, Request, Response, State, Stats, Status, MAX_REQUEST};
use kite_relay::pair::{self, PairAttempt};
use std::net::SocketAddr;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;
use std::{future::Future, io};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{TcpStream, UnixListener, UnixStream};
use tokio::sync::{oneshot, Semaphore};
use tokio::time::{timeout, Instant};

/// Open client connections at once; more are closed on arrival.
const MAX_CLIENTS: usize = 8;
/// A client has this long to send its request.
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest time one request may take, pairing's dial and handshake included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// Dialling the phone and its side of the pairing handshake.
const PAIR_START_TIMEOUT: Duration = Duration::from_secs(25);
/// How long a disconnect waits for the tunnel to come down.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Options {
    pub socket: PathBuf,
    /// Holds the daemon's key and the paired phones.
    pub state: PathBuf,
}

/// Runs the daemon until `shutdown` completes.
pub async fn run(opts: Options, shutdown: impl Future<Output = ()>) -> io::Result<()> {
    let store = Store::at(opts.state)?;
    let me = Ids::of_pid("self")?;
    let (listener, _socket) = bind(opts.socket).await?;
    let daemon = Arc::new(Daemon {
        store,
        inner: Mutex::new(Inner::default()),
    });
    let slots = Arc::new(Semaphore::new(MAX_CLIENTS));
    eprintln!("kite: daemon ready");
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let Ok(slot) = slots.clone().try_acquire_owned() else { continue };
                    let (daemon, me) = (daemon.clone(), me.clone());
                    tokio::spawn(async move {
                        client(stream, &daemon, &me).await;
                        drop(slot);
                    });
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            },
        }
    }
    daemon.inner().closing = true;
    daemon.disconnect().await;
    Ok(())
}

/// Binds the socket owner and group only, replacing one left by a daemon that did not exit
/// cleanly. A socket that answers belongs to a running daemon. The guard removes the file again.
async fn bind(path: PathBuf) -> io::Result<(UnixListener, crate::status::SocketFile)> {
    let listener = match UnixListener::bind(&path) {
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
            let alive = timeout(Duration::from_secs(1), UnixStream::connect(&path)).await;
            if !matches!(alive, Ok(Err(_))) {
                return Err(io::Error::other("another kite daemon is already running"));
            }
            // Only ever a stale socket, whatever `--socket` points at.
            if !std::fs::symlink_metadata(&path)?.file_type().is_socket() {
                return Err(io::Error::other("the socket path is not a socket"));
            }
            std::fs::remove_file(&path)?;
            UnixListener::bind(&path)?
        }
        bound => bound?,
    };
    let guard = crate::status::SocketFile(path);
    // Until this, the umask decides; a socket needs write permission to be connected to, and the
    // usual umask gives that to the owner only.
    std::fs::set_permissions(&guard.0, std::fs::Permissions::from_mode(0o660))?;
    Ok((listener, guard))
}

/// User and groups of a process, from `/proc/PID/status`.
#[derive(Clone, Debug, PartialEq)]
struct Ids {
    uid: u32,
    gid: u32,
    groups: Vec<u32>,
}

impl Ids {
    fn of_pid(pid: impl std::fmt::Display) -> io::Result<Self> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/status"))?;
        Self::parse(&text).ok_or_else(|| io::Error::other("unreadable /proc status"))
    }

    /// The effective user and group and the supplementary groups.
    fn parse(status: &str) -> Option<Self> {
        let field = |name: &str| status.lines().find_map(|l| l.strip_prefix(name));
        let effective = |name: &str| field(name)?.split_whitespace().nth(1)?.parse().ok();
        Some(Self {
            uid: effective("Uid:")?,
            gid: effective("Gid:")?,
            groups: field("Groups:")?
                .split_whitespace()
                .map(|g| g.parse().ok())
                .collect::<Option<_>>()?,
        })
    }

    /// Root or the daemon's own user. Pairing decides which phones the tunnel, and in TUN mode all
    /// traffic of the computer, may go to, so the group does not get it.
    fn may_admin(&self, daemon: &Self) -> bool {
        self.uid == 0 || self.uid == daemon.uid
    }

    /// Root, the daemon's own user, or a member of the daemon's group.
    fn may_talk_to(&self, daemon: &Self) -> bool {
        self.uid == 0
            || self.uid == daemon.uid
            || self.gid == daemon.gid
            || self.groups.contains(&daemon.gid)
    }
}

/// Serves one client: requests are answered in order until it hangs up or breaks the protocol.
async fn client(stream: UnixStream, daemon: &Daemon, me: &Ids) {
    // A caller whose credentials cannot be read or whose group cannot be established is refused
    // (a /proc mounted with hidepid hides other users' processes, and so their groups).
    let Ok(caller) = stream.peer_cred() else {
        return;
    };
    let peer = caller.pid().and_then(|pid| Ids::of_pid(pid).ok());
    let Some(peer) = peer.filter(|p| p.uid == caller.uid() && p.may_talk_to(me)) else {
        eprintln!("kite: refused a connection from outside root and the daemon's group");
        return;
    };
    let admin = peer.may_admin(me);
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::with_capacity(MAX_REQUEST, read);
    loop {
        let line = match timeout(READ_TIMEOUT, read_line(&mut read, MAX_REQUEST)).await {
            Ok(Ok(Some(line))) => line,
            // Hung up, too slow or too long: the connection is not worth answering.
            _ => return,
        };
        let response = match Request::parse(&line) {
            Ok(req) => {
                log(&req);
                timeout(REQUEST_TIMEOUT, daemon.handle(req, admin))
                    .await
                    .unwrap_or_else(|_| Response::err("timed out"))
            }
            Err(e) => Response::err(e),
        };
        match timeout(WRITE_TIMEOUT, write.write_all(&response.line())).await {
            Ok(Ok(())) => {}
            _ => return,
        }
    }
}

/// Requests that change something go to the system log, without keys, codes, addresses or the
/// caller's identity (CodeQL treats peer credentials as secrets).
fn log(req: &Request) {
    let what = match req {
        Request::Connect {
            mode, kill_switch, ..
        } => format!("connect {mode:?} kill_switch={kill_switch}"),
        Request::Disconnect {} => "disconnect".into(),
        Request::PairStart { .. } => "pair_start".into(),
        Request::Pair { .. } => "pair".into(),
        Request::RemovePeer { .. } => "remove_peer".into(),
        _ => return,
    };
    eprintln!("kite: {what}");
}

struct Daemon {
    store: Store,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    session: Option<Active>,
    pairing: Pairing,
    /// The daemon is shutting down: nothing new starts.
    closing: bool,
}

#[derive(Default)]
enum Pairing {
    #[default]
    None,
    /// Dialling the phone; at most one pairing at a time.
    Starting,
    Waiting(Box<PairAttempt<TcpStream>>, Instant),
}

/// The tunnel, or what is left of the last one.
struct Active {
    shared: Arc<Shared>,
    transport: String,
    mode: Mode,
    kill_switch: bool,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Active {
    /// The tunnel has ended. A thread that died without saying so counts too.
    fn idle(&self) -> bool {
        self.shared.snapshot().state == State::Idle
            || self.thread.as_ref().is_some_and(JoinHandle::is_finished)
    }
}

impl Daemon {
    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn handle(&self, req: Request, admin: bool) -> Response {
        if !admin
            && matches!(
                req,
                Request::PairStart { .. } | Request::Pair { .. } | Request::RemovePeer { .. }
            )
        {
            return Response::err(
                "changing the paired phones needs root or the daemon's user; try sudo",
            );
        }
        match req {
            Request::Status {} => self.status(),
            Request::Stats {} => self.stats(),
            Request::Devices {} => match kite_link::adb::devices().await {
                Ok(list) => Response {
                    devices: Some(
                        list.into_iter()
                            .map(|(serial, state)| Device { serial, state })
                            .collect(),
                    ),
                    ..Response::ok()
                },
                Err(e) => Response::err(e.to_string()),
            },
            Request::Connect {
                transport,
                mode,
                kill_switch,
                listen,
            } => self.connect(transport, mode, kill_switch, listen),
            Request::Disconnect {} => self.disconnect().await,
            Request::PairStart { transport } => self.pair_start(transport.into()).await,
            Request::Pair { code } => self.pair(&code).await,
            Request::Peers {} => match self.store.phones() {
                Ok(phones) => peers(phones.iter().map(|k| hex(k)).collect()),
                Err(e) => Response::err(e.to_string()),
            },
            Request::RemovePeer { key } => match self.store.remove_phone(&key) {
                Ok(phone) => peers(vec![hex(&phone)]),
                Err(e) => Response::err(e.to_string()),
            },
        }
    }

    fn status(&self) -> Response {
        let inner = self.inner();
        let pairing = matches!(&inner.pairing, Pairing::Waiting(_, d) if Instant::now() < *d);
        let status = match &inner.session {
            None => Status {
                state: State::Idle,
                transport: None,
                mode: None,
                kill_switch: false,
                error: None,
                pairing,
            },
            Some(a) => {
                let s = a.shared.snapshot();
                Status {
                    state: s.state,
                    transport: Some(a.transport.clone()),
                    mode: Some(a.mode),
                    kill_switch: a.kill_switch,
                    error: s.error,
                    pairing,
                }
            }
        };
        Response {
            status: Some(status),
            ..Response::ok()
        }
    }

    fn stats(&self) -> Response {
        let stats = match &self.inner().session {
            None => Stats {
                streams: 0,
                max_streams: crate::proxy::MAX_STREAMS,
                reconnects: 0,
                uptime_secs: 0,
            },
            Some(a) => {
                let s = a.shared.snapshot();
                Stats {
                    streams: a.shared.active.load(Ordering::Relaxed),
                    max_streams: crate::proxy::MAX_STREAMS,
                    reconnects: s.reconnects,
                    uptime_secs: s.uptime.as_secs(),
                }
            }
        };
        Response {
            stats: Some(stats),
            ..Response::ok()
        }
    }

    /// Starts the tunnel on its own thread and returns; the outcome shows in `status`.
    fn connect(
        &self,
        transport: kite_ipc::Transport,
        mode: Mode,
        kill_switch: bool,
        listen: Option<SocketAddr>,
    ) -> Response {
        let mut inner = self.inner();
        if inner.closing {
            return Response::err("the daemon is shutting down");
        }
        if let Some(old) = &mut inner.session {
            if !old.idle() {
                return Response::err("already connected; disconnect first");
            }
            if let Some(thread) = old.thread.take() {
                let _ = thread.join();
            }
        }
        let params = Params {
            transport: transport.into(),
            tun: mode == Mode::Tun,
            listen: listen.unwrap_or(SocketAddr::from(([127, 0, 0, 1], 1080))),
            kill_switch,
        };
        let shown = params.transport.to_string();
        let tun = params.tun;
        let shared = Arc::new(Shared::default());
        let (stop, stopped) = oneshot::channel();
        let (store, theirs) = (self.store.clone(), shared.clone());
        let thread = std::thread::Builder::new()
            .name("kite-session".into())
            .spawn(move || {
                let result = crate::block_on(session(params, store, theirs.clone(), stopped), tun);
                theirs.finish(&result);
            });
        match thread {
            Ok(thread) => {
                inner.session = Some(Active {
                    shared,
                    transport: shown,
                    mode,
                    kill_switch,
                    stop: Some(stop),
                    thread: Some(thread),
                });
                Response::ok()
            }
            Err(e) => Response::err(format!("cannot start the tunnel: {e}")),
        }
    }

    /// Takes the tunnel down, and with it the routes it set up, and waits for that. Whoever asks
    /// while another disconnect is still running waits for it too.
    async fn disconnect(&self) -> Response {
        let stop = self.inner().session.as_mut().and_then(|a| a.stop.take());
        if let Some(stop) = stop {
            let _ = stop.send(());
        }
        // A tunnel that ended with the kill switch on left its block behind; this lifts it.
        let leftover = match &mut self.inner().session {
            Some(a) if a.kill_switch && a.idle() => std::mem::take(&mut a.kill_switch),
            _ => false,
        };
        if leftover {
            if let Err(e) = crate::tun::restore().await {
                return Response::err(e.to_string());
            }
        }
        let down = async {
            while !self.inner().session.as_ref().is_none_or(Active::idle) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        };
        match timeout(STOP_TIMEOUT, down).await {
            Ok(()) => Response::ok(),
            Err(_) => Response::err("the tunnel did not stop in time"),
        }
    }

    async fn pair_start(&self, transport: Transport) -> Response {
        {
            let mut inner = self.inner();
            if inner.closing {
                return Response::err("the daemon is shutting down");
            }
            match &inner.pairing {
                Pairing::Starting => return Response::err("a pairing is already in progress"),
                Pairing::Waiting(_, deadline) if Instant::now() < *deadline => {
                    return Response::err("a pairing is already waiting for its code");
                }
                _ => inner.pairing = Pairing::Starting,
            }
        }
        // Inside the request timeout, so the marker is always reset.
        let attempt = timeout(PAIR_START_TIMEOUT, async {
            let key = self.store.key()?;
            pair::start(crate::dial(&transport, false).await?, &key).await
        })
        .await
        .unwrap_or_else(|_| {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the phone did not answer",
            ))
        });
        let mut inner = self.inner();
        match attempt {
            Ok(attempt) => {
                inner.pairing =
                    Pairing::Waiting(Box::new(attempt), Instant::now() + pair::CODE_LIFETIME);
                Response::ok()
            }
            Err(e) => {
                inner.pairing = Pairing::None;
                Response::err(e.to_string())
            }
        }
    }

    /// One try per `pair_start`: the attempt is used up whatever the code.
    async fn pair(&self, code: &str) -> Response {
        let attempt = {
            let mut inner = self.inner();
            if !matches!(inner.pairing, Pairing::Waiting(..)) {
                return Response::err("no pairing is waiting for a code; start one first");
            }
            match std::mem::take(&mut inner.pairing) {
                Pairing::Waiting(attempt, deadline) if Instant::now() < deadline => attempt,
                _ => return Response::err("the pairing expired; start a new one"),
            }
        };
        match attempt.finish(code).await {
            Ok(phone) => match self.store.add_phone(phone) {
                Ok(()) => peers(vec![hex(&phone)]),
                Err(e) => Response::err(e.to_string()),
            },
            Err(e) => Response::err(e.to_string()),
        }
    }
}

fn peers(keys: Vec<String>) -> Response {
    Response {
        peers: Some(keys),
        ..Response::ok()
    }
}

impl From<kite_ipc::Transport> for Transport {
    fn from(t: kite_ipc::Transport) -> Self {
        match t {
            kite_ipc::Transport::Tcp { addr } => Self::Tcp(addr),
            kite_ipc::Transport::Usb { serial } => Self::Usb(serial),
        }
    }
}

/// The tunnel's thread: connects, then serves until told to stop.
async fn session(
    p: Params,
    store: Store,
    shared: Arc<Shared>,
    stopped: oneshot::Receiver<()>,
) -> io::Result<()> {
    // A dropped sender stops it as well.
    let stopped = async {
        let _ = stopped.await;
    };
    tokio::pin!(stopped);
    let started = tokio::select! {
        r = session::start(&p, &store) => r?,
        () = &mut stopped => return Ok(()),
    };
    started.serve(&shared, stopped).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};

    const STATUS: &str =
        "Name:\tkite\nUid:\t1000\t1001\t1000\t1001\nGid:\t200\t201\t200\t201\nGroups:\t10 20 30\n";

    #[test]
    fn parses_ids_from_proc_status() {
        let ids = Ids::parse(STATUS).unwrap();
        assert_eq!((ids.uid, ids.gid), (1001, 201));
        assert_eq!(ids.groups, [10, 20, 30]);
        assert!(Ids::parse("Uid:\t1\n").is_none());
        assert!(Ids::parse(&STATUS.replace("30", "x")).is_none());
        let root = Ids::parse("Uid:\t0\t0\nGid:\t0\t0\nGroups:\t\n").unwrap();
        assert!(root.groups.is_empty());
    }

    #[test]
    fn only_root_the_daemon_and_its_group_may_talk_to_it() {
        let me = Ids {
            uid: 900,
            gid: 901,
            groups: vec![],
        };
        let who = |uid, gid, groups: &[u32]| Ids {
            uid,
            gid,
            groups: groups.to_vec(),
        };
        assert!(who(0, 0, &[]).may_talk_to(&me));
        assert!(who(900, 5, &[]).may_talk_to(&me));
        assert!(who(1000, 901, &[]).may_talk_to(&me));
        assert!(who(1000, 1000, &[4, 901]).may_talk_to(&me));
        assert!(!who(1000, 1000, &[4, 27]).may_talk_to(&me));
        // Changing the paired phones is for root and the daemon's user only.
        assert!(who(0, 0, &[]).may_admin(&me));
        assert!(who(900, 5, &[]).may_admin(&me));
        assert!(!who(1000, 901, &[901]).may_admin(&me));
    }

    struct Running {
        socket: PathBuf,
        stop: oneshot::Sender<()>,
        task: tokio::task::JoinHandle<io::Result<()>>,
        dir: PathBuf,
    }

    async fn start(name: &str) -> Running {
        let dir = std::env::temp_dir().join(format!("kite-daemon-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("kite.sock");
        let opts = Options {
            socket: socket.clone(),
            state: dir.join("state"),
        };
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(run(opts, async {
            let _ = stopped.await;
        }));
        for _ in 0..100 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Running {
            socket,
            stop,
            task,
            dir,
        }
    }

    impl Running {
        async fn end(self) {
            self.stop.send(()).unwrap();
            self.task.await.unwrap().unwrap();
            assert!(!self.socket.exists(), "socket left behind");
            std::fs::remove_dir_all(self.dir).unwrap();
        }
    }

    async fn ask(socket: &PathBuf, line: &str) -> Response {
        let mut s = UnixStream::connect(socket).await.unwrap();
        s.write_all(format!("{line}\n").as_bytes()).await.unwrap();
        let mut reply = String::new();
        BufReader::new(s).read_line(&mut reply).await.unwrap();
        Response::parse(reply.trim_end().as_bytes()).unwrap()
    }

    #[tokio::test]
    async fn answers_commands_and_refuses_bad_ones() {
        let d = start("cmds").await;
        let mode = std::fs::metadata(&d.socket).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o660);

        let r = ask(&d.socket, r#"{"cmd":"status"}"#).await;
        assert_eq!(r.status.unwrap().state, State::Idle);
        let r = ask(&d.socket, r#"{"cmd":"peers"}"#).await;
        assert_eq!(r.peers, Some(vec![]));
        for bad in [
            r#"{"cmd":"status","x":1}"#,
            r#"{"cmd":"shell"}"#,
            "not json",
            r#"{"cmd":"remove_peer","key":"zz"}"#,
            r#"{"cmd":"pair","code":"123456"}"#,
            r#"{"cmd":"remove_peer","key":"ab"}"#,
        ] {
            let r = ask(&d.socket, bad).await;
            assert!(!r.ok && r.error.is_some(), "{bad}");
        }
        // Not paired yet: the tunnel starts and ends at once, and says why.
        let r = ask(
            &d.socket,
            r#"{"cmd":"connect","transport":{"type":"tcp","addr":"127.0.0.1:9"},"mode":"proxy"}"#,
        )
        .await;
        assert!(r.ok);
        let mut error = None;
        for _ in 0..100 {
            let s = ask(&d.socket, r#"{"cmd":"status"}"#).await.status.unwrap();
            if s.state == State::Idle {
                error = s.error;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(error.unwrap().contains("not paired"));
        let r = ask(&d.socket, r#"{"cmd":"stats"}"#).await;
        assert_eq!(r.stats.unwrap().streams, 0);
        assert!(ask(&d.socket, r#"{"cmd":"disconnect"}"#).await.ok);

        // A code with no pairing started is refused, and a failed start leaves nothing behind.
        let r = ask(&d.socket, r#"{"cmd":"pair","code":"123456"}"#).await;
        assert!(r.error.unwrap().contains("no pairing"));
        let start = r#"{"cmd":"pair_start","transport":{"type":"tcp","addr":"127.0.0.1:9"}}"#;
        for _ in 0..2 {
            let r = ask(&d.socket, start).await;
            assert!(!r.ok && !r.error.unwrap().contains("already"));
        }
        d.end().await;
    }

    #[tokio::test]
    async fn drops_oversized_clients() {
        let d = start("limits").await;
        let mut s = UnixStream::connect(&d.socket).await.unwrap();
        s.write_all(&vec![b'a'; MAX_REQUEST + 1]).await.unwrap();
        let mut buf = Vec::new();
        // The daemon closes the connection without an answer.
        assert_eq!(s.read_to_end(&mut buf).await.unwrap(), 0);
        assert!(ask(&d.socket, r#"{"cmd":"status"}"#).await.ok);
        d.end().await;
    }

    #[tokio::test]
    async fn refuses_a_second_daemon_on_the_same_socket() {
        let d = start("twice").await;
        let opts = Options {
            socket: d.socket.clone(),
            state: d.dir.join("state"),
        };
        let err = run(opts, std::future::pending()).await.unwrap_err();
        assert!(err.to_string().contains("already running"), "{err}");
        d.end().await;
    }
}
