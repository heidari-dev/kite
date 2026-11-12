#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
//! One connection of this computer to the phone, shared by `kite connect --foreground` and the
//! daemon: dial and pair-check the relay, then serve the SOCKS5 proxy or TUN mode through it.

use crate::store::Store;
use crate::{dial, proxy, Transport, Tunnel};
use kite_ipc::State;
use kite_relay::noise::{StaticKey, KEY_LEN};
use kite_relay::pair;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;

pub struct Params {
    pub transport: Transport,
    pub tun: bool,
    /// Proxy mode: where to serve the SOCKS5 proxy.
    pub listen: SocketAddr,
}

/// What the rest of the program may read about a running session.
pub struct Shared {
    /// Open streams.
    pub active: Arc<AtomicUsize>,
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

    /// Serves until `shutdown` completes (`Ok`) or the link is lost.
    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
    pub async fn serve(
        self,
        shared: &Shared,
        shutdown: impl Future<Output = ()>,
    ) -> io::Result<()> {
        let Self {
            link,
            first,
            listener,
        } = self;
        #[cfg(target_os = "linux")]
        if link.tun {
            let addr = listener.local_addr()?;
            return crate::tun::run(
                addr,
                |token| keep(first, &listener, shared, token.cancelled_owned()),
                shutdown,
            )
            .await;
        }
        keep(first, &listener, shared, shutdown).await
    }
}

/// Serves `listener` through `tunnel` until `shutdown` completes (`Ok`) or the tunnel ends.
async fn keep(
    tunnel: Tunnel,
    listener: &TcpListener,
    shared: &Shared,
    shutdown: impl Future<Output = ()>,
) -> io::Result<()> {
    shared.set(State::Connected, None);
    proxy::run(tunnel, listener, shared.active.clone(), shutdown).await
}
