#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
//! One connection of this computer to the phone, shared by `kite connect --foreground` and the
//! daemon: dial and pair-check the relay, then serve the SOCKS5 proxy or TUN mode through it.
//!
//! With the kill switch a lost link is not the end: the proxy listener (and in TUN mode the device,
//! its rules and tun2proxy) stay up, local connections are refused at once, and the relay is dialled
//! again with backoff until it answers or the user disconnects. Nothing reaches the network
//! meanwhile, because everything that enters the tunnel ends at a proxy with no tunnel behind it.

use crate::proxy::Bytes;
use crate::store::Store;
use crate::{dial, proxy, Transport, Tunnel};
use kite_ipc::State;
use kite_relay::noise::{StaticKey, KEY_LEN};
use kite_relay::pair;
use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;

/// First wait before dialling again after a lost link, doubled up to [`MAX_BACKOFF`].
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(15);

pub struct Params {
    pub transport: Transport,
    pub tun: bool,
    /// Proxy mode: where to serve the SOCKS5 proxy.
    pub listen: SocketAddr,
    pub kill_switch: bool,
}

/// What the rest of the program may read about a running session.
pub struct Shared {
    /// Open streams.
    pub active: Arc<AtomicUsize>,
    /// What they carried since the session began.
    pub bytes: Arc<Bytes>,
    live: Mutex<Live>,
}

struct Live {
    state: State,
    error: Option<String>,
    reconnects: u32,
    up_since: Option<Instant>,
}

/// A copy of what [`Shared`] holds.
pub struct Snapshot {
    pub state: State,
    pub error: Option<String>,
    pub reconnects: u32,
    pub uptime: Duration,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            active: Arc::new(AtomicUsize::new(0)),
            bytes: Arc::default(),
            live: Mutex::new(Live {
                state: State::Connecting,
                error: None,
                reconnects: 0,
                up_since: None,
            }),
        }
    }
}

impl Shared {
    fn live(&self) -> std::sync::MutexGuard<'_, Live> {
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn set(&self, state: State, error: Option<String>) {
        let mut live = self.live();
        live.up_since = (state == State::Connected).then(Instant::now);
        live.state = state;
        live.error = error;
        if state != State::Connected {
            self.active.store(0, Ordering::Relaxed);
        }
    }

    /// The session ended with `result`.
    pub fn finish(&self, result: &io::Result<()>) {
        self.set(State::Idle, result.as_ref().err().map(ToString::to_string));
    }

    pub fn snapshot(&self) -> Snapshot {
        let live = self.live();
        Snapshot {
            state: live.state,
            error: live.error.clone(),
            reconnects: live.reconnects,
            uptime: live.up_since.map_or(Duration::ZERO, |t| t.elapsed()),
        }
    }
}

/// How to reach and recognise the phone.
struct Link {
    transport: Transport,
    tun: bool,
    key: StaticKey,
    phones: Vec<[u8; KEY_LEN]>,
}

impl Link {
    /// A tunnel to the relay, once the phone proved to be a paired one.
    async fn establish(&self) -> io::Result<Tunnel> {
        pair::connect(
            dial(&self.transport, self.tun).await?,
            &self.key,
            &self.phones,
        )
        .await
    }
}

/// A session that reached its relay once.
pub struct Started {
    link: Link,
    first: Tunnel,
    listener: TcpListener,
    kill_switch: bool,
}

/// Binds the proxy and dials the relay. This is where a missing pairing or an absent phone shows
/// up, before anything of the system is touched.
pub async fn start(p: &Params, store: &Store) -> io::Result<Started> {
    let phones = store.phones()?;
    if phones.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "not paired with a phone yet: run kite pair first",
        ));
    }
    // TUN mode feeds tun2proxy through a proxy on a free port.
    let listen = if p.tun {
        SocketAddr::from(([127, 0, 0, 1], 0))
    } else {
        p.listen
    };
    let listener = TcpListener::bind(listen).await?;
    let link = Link {
        transport: p.transport.clone(),
        tun: p.tun,
        key: store.key()?,
        phones,
    };
    let first = link.establish().await?;
    Ok(Started {
        link,
        first,
        listener,
        kill_switch: p.kill_switch,
    })
}

impl Started {
    /// What is being shared, for the user.
    pub fn mode(&self) -> io::Result<String> {
        Ok(if self.link.tun {
            "all traffic through the tunnel".to_string()
        } else {
            format!("SOCKS5 proxy on {}", self.listener.local_addr()?)
        })
    }

    /// Serves until `shutdown` completes (`Ok`) or the link is lost without the kill switch.
    pub async fn serve(
        self,
        shared: &Shared,
        shutdown: impl Future<Output = ()>,
    ) -> io::Result<()> {
        let Self {
            link,
            first,
            listener,
            kill_switch,
        } = self;
        #[cfg(target_os = "linux")]
        if link.tun {
            let addr = listener.local_addr()?;
            return crate::tun::run(
                addr,
                kill_switch,
                |token| {
                    keep(
                        &link,
                        first,
                        &listener,
                        shared,
                        kill_switch,
                        token.cancelled_owned(),
                    )
                },
                shutdown,
            )
            .await;
        }
        keep(&link, first, &listener, shared, kill_switch, shutdown).await
    }
}

/// Serves `listener` through `first`, and with `reconnect` through a new tunnel each time the
/// link is lost, until `shutdown` completes.
async fn keep(
    link: &Link,
    first: Tunnel,
    listener: &TcpListener,
    shared: &Shared,
    reconnect: bool,
    shutdown: impl Future<Output = ()>,
) -> io::Result<()> {
    tokio::pin!(shutdown);
    let mut tunnel = first;
    loop {
        shared.set(State::Connected, None);
        // Ok means shutdown; the tunnel itself ends in an error.
        let Err(lost) = proxy::run(
            tunnel,
            listener,
            shared.active.clone(),
            shared.bytes.clone(),
            shutdown.as_mut(),
        )
        .await
        else {
            return Ok(());
        };
        if !reconnect {
            return Err(lost);
        }
        shared.set(State::Reconnecting, Some(lost.to_string()));
        let mut delay = MIN_BACKOFF;
        tunnel = loop {
            tokio::select! {
                () = &mut shutdown => return Ok(()),
                never = refuse(listener) => match never {},
                () = tokio::time::sleep(delay) => {}
            }
            tokio::select! {
                () = &mut shutdown => return Ok(()),
                never = refuse(listener) => match never {},
                r = link.establish() => match r {
                    Ok(t) => break t,
                    Err(e) => shared.set(State::Reconnecting, Some(e.to_string())),
                },
            }
            delay = (delay * 2).min(MAX_BACKOFF);
        };
        shared.live().reconnects += 1;
    }
}

/// Closes local connections as they come, so programs fail at once while nothing carries them.
async fn refuse(listener: &TcpListener) -> Infallible {
    loop {
        match listener.accept().await {
            Ok((local, _)) => drop(local),
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}
