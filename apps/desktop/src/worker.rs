//! Keeps what the window and the tray show in step with the daemon: polls it, runs the user's
//! commands, and publishes a [`View`] whenever something changed. No UI types in here.

use crate::daemon::{call, CallError};
use kite_ipc::{Device, Mode, Request, State, Status, Transport};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedReceiver;

/// Time between polls while the window is open, and while only the tray is.
const POLL_OPEN: Duration = Duration::from_secs(1);
const POLL_TRAY: Duration = Duration::from_secs(3);
/// Phones and paired phones change rarely; they are asked for this often while the window is open.
const SLOW: Duration = Duration::from_secs(3);
/// The daemon answers a poll at once; a command may take long (pairing dials the phone).
const POLL_WAIT: Duration = Duration::from_secs(5);
const COMMAND_WAIT: Duration = Duration::from_secs(35);

const NO_FINGERPRINT: &str = "The daemon did not say which computer this is, so the phone's \
     fingerprint cannot be checked. Update Kite and try again.";

/// What the user asked for.
#[derive(PartialEq, Eq)]
pub enum Cmd {
    Connect {
        transport: Transport,
        mode: Mode,
        kill_switch: bool,
    },
    Disconnect,
    PairStart {
        transport: Transport,
    },
    Pair {
        code: String,
    },
    PairCancel,
    /// The window opened: ask for everything now.
    Refresh,
}

// The pairing code must not reach a log through a stray `{:?}`.
impl std::fmt::Debug for Cmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pair { .. } => f.write_str("Pair { code: .. }"),
            Self::Connect { mode, .. } => write!(f, "Connect {{ mode: {mode:?}, .. }}"),
            Self::Disconnect => f.write_str("Disconnect"),
            Self::PairStart { .. } => f.write_str("PairStart { .. }"),
            Self::PairCancel => f.write_str("PairCancel"),
            Self::Refresh => f.write_str("Refresh"),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Pair {
    #[default]
    Idle,
    /// The phone is showing a code. The fingerprint is this computer's, for the user to compare
    /// with the one on the phone.
    Waiting { fingerprint: String },
    /// Paired; the start of the phone's key.
    Done(String),
}

/// Everything the UI shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct View {
    /// The daemon cannot be used; says what to do. Nothing else is shown then.
    pub unreachable: Option<String>,
    pub status: Option<Status>,
    pub devices: Vec<Device>,
    /// Why the phones could not be listed (no adb server, say).
    pub devices_error: Option<String>,
    /// How many phones are paired with this computer.
    pub paired: usize,
    /// Bytes per second to and from the phone.
    pub up: u64,
    pub down: u64,
    pub pair: Pair,
    /// The daemon refused the last command; its words.
    pub message: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Disconnected,
    Connecting,
    Connected,
    Error,
}

impl View {
    pub fn state(&self) -> Option<State> {
        self.status.as_ref().map(|s| s.state)
    }

    /// The last connection attempt or link ended with an error and nothing is running.
    pub fn failure(&self) -> Option<&str> {
        let s = self.status.as_ref()?;
        s.error.as_deref().filter(|_| s.state == State::Idle)
    }

    pub fn icon(&self) -> Icon {
        if self.unreachable.is_some() || self.failure().is_some() {
            return Icon::Error;
        }
        match self.state() {
            Some(State::Connected) => Icon::Connected,
            Some(State::Connecting) => Icon::Connecting,
            // The spec: the icon switches to Disconnected when the link is lost, even though the
            // kill switch keeps retrying (the window says so).
            _ => Icon::Disconnected,
        }
    }
}

/// Turns two readings of a byte counter into a speed.
#[derive(Default)]
pub struct Meter(Option<(Instant, u64, u64)>);

impl Meter {
    pub fn rate(&mut self, now: Instant, up: u64, down: u64) -> (u64, u64) {
        let rate = match self.0 {
            // A counter that went back is a new session.
            Some((t, u, d)) if now > t && up >= u && down >= d => {
                let secs = now.duration_since(t).as_secs_f64();
                (
                    ((up - u) as f64 / secs) as u64,
                    ((down - d) as f64 / secs) as u64,
                )
            }
            _ => (0, 0),
        };
        self.0 = Some((now, up, down));
        rate
    }

    pub fn reset(&mut self) {
        self.0 = None;
    }
}

/// "0 B/s", "950 B/s", "1.2 MB/s", "35 MB/s".
pub fn format_rate(bytes_per_sec: u64) -> String {
    let mut value = bytes_per_sec as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < 3 {
        value /= 1000.0;
        unit += 1;
    }
    let unit = ["B/s", "kB/s", "MB/s", "GB/s"][unit];
    if unit == "B/s" || value >= 9.95 {
        format!("{value:.0} {unit}")
    } else {
        format!("{value:.1} {unit}")
    }
}

struct Worker {
    socket: PathBuf,
    view: View,
    meter: Meter,
    slow_at: Option<Instant>,
    /// When the last `PairStart` was answered.
    pair_at: Option<Instant>,
}

/// A pairing the daemon does not report yet is given this long before it counts as over.
const PAIR_GRACE: Duration = Duration::from_secs(5);

/// Runs until `cmds` closes. `visible` says whether the window is open; `publish` gets a view each
/// time it changes.
pub async fn run(
    socket: PathBuf,
    mut cmds: UnboundedReceiver<Cmd>,
    visible: Arc<AtomicBool>,
    publish: impl Fn(View),
) {
    let mut w = Worker {
        socket,
        view: View::default(),
        meter: Meter::default(),
        slow_at: None,
        pair_at: None,
    };
    let mut shown = None;
    loop {
        w.poll(visible.load(Ordering::Relaxed)).await;
        if shown.as_ref() != Some(&w.view) {
            publish(w.view.clone());
            shown = Some(w.view.clone());
        }
        let wait = if visible.load(Ordering::Relaxed) {
            POLL_OPEN
        } else {
            POLL_TRAY
        };
        tokio::select! {
            cmd = cmds.recv() => match cmd {
                Some(cmd) => w.command(cmd).await,
                None => return,
            },
            () = tokio::time::sleep(wait) => {}
        }
    }
}

impl Worker {
    async fn ask(&mut self, req: Request, wait: Duration) -> Option<kite_ipc::Response> {
        match call(&self.socket, &req, wait).await {
            Ok(r) => Some(r),
            Err(CallError::Unreachable(m)) => {
                self.view.unreachable = Some(m);
                None
            }
            Err(CallError::Refused(m)) => {
                self.view.message = Some(m);
                None
            }
        }
    }

    async fn command(&mut self, cmd: Cmd) {
        self.view.message = None;
        match cmd {
            Cmd::Connect {
                transport,
                mode,
                kill_switch,
            } => {
                self.meter.reset();
                let req = Request::Connect {
                    transport,
                    mode,
                    kill_switch: kill_switch && mode == Mode::Tun,
                    listen: None,
                };
                self.ask(req, COMMAND_WAIT).await;
            }
            Cmd::Disconnect => {
                self.ask(Request::Disconnect {}, COMMAND_WAIT).await;
            }
            Cmd::PairStart { transport } => {
                let req = Request::PairStart { transport };
                if let Some(r) = self.ask(req, COMMAND_WAIT).await {
                    // The fingerprint is the user's only check that the phone talks to this
                    // computer, so a daemon that does not give one ends the pairing.
                    self.pair_at = Some(Instant::now());
                    self.view.pair = match r.fingerprint {
                        Some(fingerprint) => Pair::Waiting { fingerprint },
                        None => {
                            self.view.message = Some(NO_FINGERPRINT.into());
                            Pair::Idle
                        }
                    };
                }
            }
            Cmd::Pair { code } => {
                // One try per start, whatever the code: the dialog is over either way.
                let r = self.ask(Request::Pair { code }, COMMAND_WAIT).await;
                self.view.pair = match r.and_then(|r| r.peers?.into_iter().next()) {
                    Some(key) => Pair::Done(key.chars().take(8).collect()),
                    None => Pair::Idle,
                };
                self.slow_at = None;
            }
            Cmd::PairCancel => self.view.pair = Pair::Idle,
            Cmd::Refresh => self.slow_at = None,
        }
    }

    /// Reads the daemon. With the window closed only the state is asked for.
    async fn poll(&mut self, open: bool) {
        self.view.unreachable = None;
        let Some(r) = self.ask(Request::Status {}, POLL_WAIT).await else {
            if self.view.unreachable.is_some() {
                self.view = View {
                    unreachable: self.view.unreachable.take(),
                    ..View::default()
                };
                self.meter.reset();
            }
            return;
        };
        self.view.status = r.status;
        // The daemon drops a pairing after two minutes; the code field must go with it.
        let waiting = self.view.status.as_ref().is_some_and(|s| s.pairing);
        let started = self.pair_at.is_some_and(|t| t.elapsed() > PAIR_GRACE);
        if matches!(self.view.pair, Pair::Waiting { .. }) && !waiting && started {
            self.view.pair = Pair::Idle;
            self.view.message = Some("The pairing expired. Start it again.".into());
        }
        if !open {
            // A reading from before the window closed would give a wrong speed on reopening.
            self.meter.reset();
            return;
        }
        let connected = self.view.state() == Some(State::Connected);
        let stats = if connected {
            self.ask(Request::Stats {}, POLL_WAIT)
                .await
                .and_then(|r| r.stats)
        } else {
            None
        };
        (self.view.up, self.view.down) = match stats {
            Some(s) => self.meter.rate(Instant::now(), s.bytes_up, s.bytes_down),
            None => {
                self.meter.reset();
                (0, 0)
            }
        };
        // A daemon that stopped answering is not asked twice more.
        if self.view.unreachable.is_some() || self.slow_at.is_some_and(|t| t.elapsed() < SLOW) {
            return;
        }
        self.slow_at = Some(Instant::now());
        match call(&self.socket, &Request::Devices {}, POLL_WAIT).await {
            Ok(r) => {
                self.view.devices = r.devices.unwrap_or_default();
                self.view.devices_error = None;
            }
            Err(CallError::Refused(m)) => {
                self.view.devices.clear();
                self.view.devices_error = Some(m);
            }
            Err(CallError::Unreachable(m)) => {
                self.view.unreachable = Some(m);
                return;
            }
        }
        if let Some(r) = self.ask(Request::Peers {}, POLL_WAIT).await {
            self.view.paired = r.peers.map_or(0, |p| p.len());
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use kite_ipc::{read_line, Response, Stats, MAX_REQUEST};
    use std::sync::Mutex;
    use tokio::io::{AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    /// A daemon that answers every request with `answer` and remembers what it was asked.
    fn daemon(
        name: &str,
        answer: impl Fn(&Request) -> Response + Send + 'static,
    ) -> (PathBuf, Arc<Mutex<Vec<Request>>>) {
        let dir = std::env::temp_dir().join(format!("kite-worker-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("kite.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let log = asked.clone();
        tokio::spawn(async move {
            while let Ok((s, _)) = listener.accept().await {
                let (r, mut w) = s.into_split();
                let line = read_line(&mut BufReader::new(r), MAX_REQUEST)
                    .await
                    .unwrap()
                    .unwrap();
                let req = Request::parse(&line).expect("the app sent a request the daemon refuses");
                let reply = answer(&req);
                log.lock().unwrap().push(req);
                let _ = w.write_all(&reply.line()).await;
            }
        });
        (path, asked)
    }

    fn usb() -> Transport {
        Transport::Usb {
            serial: Some("R5CT".into()),
        }
    }

    fn wifi(addr: Option<&str>) -> Transport {
        Transport::Wifi {
            addr: addr.map(Into::into),
        }
    }

    fn worker(socket: PathBuf) -> Worker {
        Worker {
            socket,
            view: View::default(),
            meter: Meter::default(),
            slow_at: None,
            pair_at: None,
        }
    }

    fn status(state: State, error: Option<&str>) -> Status {
        Status {
            state,
            transport: Some("usb".into()),
            mode: Some(Mode::Tun),
            kill_switch: false,
            error: error.map(Into::into),
            pairing: false,
        }
    }

    fn idle_daemon(name: &str) -> (PathBuf, Arc<Mutex<Vec<Request>>>) {
        daemon(name, |req| match req {
            Request::Status {} => Response {
                status: Some(status(State::Idle, None)),
                ..Response::ok()
            },
            Request::PairStart { .. } => Response {
                fingerprint: Some("3F2A 91C0 77D4 0B1E".into()),
                ..Response::ok()
            },
            Request::Pair { code } if code == "123456" => Response {
                peers: Some(vec!["ab".repeat(32)]),
                ..Response::ok()
            },
            Request::Pair { .. } => Response::err("the code does not match"),
            _ => Response::ok(),
        })
    }

    #[tokio::test]
    async fn an_absent_daemon_is_the_whole_view() {
        let mut w = worker(std::env::temp_dir().join("kite-worker-nothing.sock"));
        w.view.paired = 3;
        w.poll(true).await;
        assert_eq!(
            w.view.unreachable.as_deref(),
            Some(crate::daemon::NOT_RUNNING)
        );
        assert_eq!(w.view.paired, 0);
        assert_eq!(w.view.icon(), Icon::Error);
    }

    #[tokio::test]
    async fn polls_state_phones_and_paired_phones() {
        let (path, asked) = daemon("poll", |req| match req {
            Request::Status {} => Response {
                status: Some(status(State::Connected, None)),
                ..Response::ok()
            },
            Request::Stats {} => Response {
                stats: Some(Stats {
                    streams: 1,
                    max_streams: 224,
                    reconnects: 0,
                    uptime_secs: 3,
                    bytes_up: 10,
                    bytes_down: 20,
                }),
                ..Response::ok()
            },
            Request::Devices {} => Response {
                devices: Some(vec![Device {
                    serial: "R5CT".into(),
                    state: "device".into(),
                }]),
                ..Response::ok()
            },
            Request::Peers {} => Response {
                peers: Some(vec!["ab".into(), "cd".into()]),
                ..Response::ok()
            },
            _ => Response::err("unexpected"),
        });
        let mut w = worker(path);
        w.poll(true).await;
        assert_eq!(w.view.icon(), Icon::Connected);
        assert_eq!(w.view.devices[0].serial, "R5CT");
        assert_eq!(w.view.paired, 2);
        assert_eq!((w.view.up, w.view.down), (0, 0), "one reading is no speed");
        // The window closed: only the state is asked for.
        let before = asked.lock().unwrap().len();
        w.poll(false).await;
        assert_eq!(asked.lock().unwrap().len(), before + 1);
    }

    #[tokio::test]
    async fn phones_that_cannot_be_listed_say_why() {
        let (path, _) = daemon("nophones", |req| match req {
            Request::Devices {} => Response::err("the adb server is not running"),
            _ => Response::ok(),
        });
        let mut w = worker(path);
        w.poll(true).await;
        assert!(w.view.unreachable.is_none());
        assert_eq!(
            w.view.devices_error.as_deref(),
            Some("the adb server is not running")
        );
    }

    #[tokio::test]
    async fn connect_names_the_phone_and_drops_the_kill_switch_outside_full_mode() {
        let (path, asked) = idle_daemon("connect");
        let mut w = worker(path);
        for (mode, kill_switch) in [(Mode::Tun, true), (Mode::Proxy, true)] {
            w.command(Cmd::Connect {
                transport: usb(),
                mode,
                kill_switch,
            })
            .await;
        }
        let asked = asked.lock().unwrap();
        let connect = |mode, kill_switch| Request::Connect {
            transport: usb(),
            mode,
            kill_switch,
            listen: None,
        };
        assert_eq!(asked[0], connect(Mode::Tun, true));
        assert_eq!(asked[1], connect(Mode::Proxy, false));
    }

    #[tokio::test]
    async fn wifi_connects_and_pairs_with_no_phone_on_usb() {
        let (path, asked) = daemon("wifi", |req| match req {
            Request::Devices {} => Response::err("the adb server is not running"),
            Request::PairStart { .. } => Response {
                fingerprint: Some("3F2A 91C0 77D4 0B1E".into()),
                ..Response::ok()
            },
            _ => Response::ok(),
        });
        let mut w = worker(path);
        w.poll(true).await;
        assert!(w.view.devices.is_empty());
        for addr in [Some("192.168.1.20:47810"), None] {
            w.command(Cmd::Connect {
                transport: wifi(addr),
                mode: Mode::Tun,
                kill_switch: false,
            })
            .await;
            assert_eq!(w.view.message, None);
        }
        w.command(Cmd::PairStart {
            transport: wifi(None),
        })
        .await;
        assert!(matches!(w.view.pair, Pair::Waiting { .. }));
        let asked = asked.lock().unwrap();
        let sent = |i: usize| match &asked[i] {
            Request::Connect { transport, .. } | Request::PairStart { transport } => {
                transport.clone()
            }
            other => panic!("{other:?}"),
        };
        let n = asked.len();
        assert_eq!(sent(n - 3), wifi(Some("192.168.1.20:47810")));
        assert_eq!(sent(n - 2), wifi(None));
        assert_eq!(sent(n - 1), wifi(None));
    }

    #[tokio::test]
    async fn a_refused_command_keeps_the_daemons_words() {
        let (path, _) = daemon("refuse", |_| {
            Response::err("already connected; disconnect first")
        });
        let mut w = worker(path);
        w.command(Cmd::Disconnect).await;
        assert_eq!(
            w.view.message.as_deref(),
            Some("already connected; disconnect first")
        );
        w.command(Cmd::Refresh).await;
        assert_eq!(w.view.message, None, "the next command clears it");
    }

    #[tokio::test]
    async fn pairing_shows_the_fingerprint_then_ends_after_one_try() {
        let (path, _) = idle_daemon("pair");
        let mut w = worker(path);
        let start = || Cmd::PairStart { transport: usb() };
        w.command(start()).await;
        assert_eq!(
            w.view.pair,
            Pair::Waiting {
                fingerprint: "3F2A 91C0 77D4 0B1E".into()
            }
        );
        w.command(Cmd::Pair {
            code: "123456".into(),
        })
        .await;
        assert_eq!(w.view.pair, Pair::Done("abababab".into()));

        w.command(start()).await;
        w.command(Cmd::Pair {
            code: "000000".into(),
        })
        .await;
        assert_eq!(w.view.pair, Pair::Idle);
        assert_eq!(w.view.message.as_deref(), Some("the code does not match"));
    }

    #[tokio::test]
    async fn pairing_without_a_fingerprint_does_not_go_on() {
        let (path, _) = daemon("nofp", |_| Response::ok());
        let mut w = worker(path);
        w.command(Cmd::PairStart { transport: usb() }).await;
        assert_eq!(w.view.pair, Pair::Idle);
        assert_eq!(w.view.message.as_deref(), Some(NO_FINGERPRINT));
    }

    #[tokio::test]
    async fn an_expired_pairing_closes_the_code_field() {
        let (path, _) = idle_daemon("expire");
        let mut w = worker(path);
        w.command(Cmd::PairStart { transport: usb() }).await;
        // Just started: the daemon may not report it yet.
        w.poll(false).await;
        assert!(matches!(w.view.pair, Pair::Waiting { .. }));
        // The idle daemon reports no pairing waiting.
        w.pair_at = Some(Instant::now() - Duration::from_secs(60));
        w.poll(false).await;
        assert_eq!(w.view.pair, Pair::Idle);
        assert!(w.view.message.unwrap().contains("expired"));
    }

    #[test]
    fn the_pairing_code_stays_out_of_debug_output() {
        let cmd = Cmd::Pair {
            code: "123456".into(),
        };
        assert!(!format!("{cmd:?}").contains("123456"));
    }

    #[test]
    fn icons_follow_the_state_and_failures() {
        let view = |state, error: Option<&str>| View {
            status: Some(status(state, error)),
            ..View::default()
        };
        assert_eq!(View::default().icon(), Icon::Disconnected);
        assert_eq!(view(State::Idle, None).icon(), Icon::Disconnected);
        assert_eq!(view(State::Connecting, None).icon(), Icon::Connecting);
        assert_eq!(
            view(State::Reconnecting, Some("lost")).icon(),
            Icon::Disconnected
        );
        assert_eq!(view(State::Connected, None).icon(), Icon::Connected);
        assert_eq!(view(State::Idle, Some("not paired")).icon(), Icon::Error);
        assert_eq!(
            view(State::Idle, Some("not paired")).failure(),
            Some("not paired")
        );
        assert_eq!(view(State::Reconnecting, Some("lost")).failure(), None);
    }

    #[test]
    fn the_meter_turns_counters_into_speeds() {
        let mut m = Meter::default();
        let t = Instant::now();
        assert_eq!(m.rate(t, 100, 100), (0, 0));
        assert_eq!(m.rate(t + Duration::from_secs(2), 2100, 4100), (1000, 2000));
        // A counter that went back is a new session.
        assert_eq!(m.rate(t + Duration::from_secs(3), 5, 5), (0, 0));
        m.reset();
        assert_eq!(m.rate(t + Duration::from_secs(4), 9000, 9000), (0, 0));
    }

    #[test]
    fn rates_are_short_to_read() {
        for (n, text) in [
            (0, "0 B/s"),
            (950, "950 B/s"),
            (1_200, "1.2 kB/s"),
            (35_000_000, "35 MB/s"),
            (1_240_000, "1.2 MB/s"),
            (9_996_000_000, "10 GB/s"),
            (u64::MAX, "18446744074 GB/s"),
        ] {
            assert_eq!(format_rate(n), text);
        }
    }
}
