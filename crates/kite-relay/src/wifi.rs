//! The phone's WiFi listener: which addresses it may bind, and how many strangers it lets hold a
//! handshake open at once.
//!
//! Anyone on the WiFi or hotspot can open a TCP connection, so a connection holds one [`Ticket`]
//! until the handshake and pairing have finished. Tickets are capped in total, per source address
//! and per source address and time, so one device cannot fill the phone's tasks, and a flood
//! from the link costs it a refused connect, not memory. The Noise handshake and pairing that follow
//! are the same as on every other transport (see [`crate::pair::accept`]).

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::{IpAddr, TcpListener as StdListener};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use tokio::time::Instant;

/// The port the phone listens on, so `kite connect --wifi 192.168.43.1` needs no port.
pub const PORT: u16 = 47810;
/// Connections in a handshake or pairing at once, from all addresses together.
pub const MAX_HANDSHAKES: usize = 8;
/// ...and from one address.
pub const MAX_PER_ADDRESS: u8 = 2;
/// New connections one address may open per [`WINDOW`].
pub const MAX_PER_WINDOW: u32 = 10;
pub const WINDOW: Duration = Duration::from_secs(10);
/// Addresses remembered for the window. A full table refuses new addresses until entries expire.
const MAX_TRACKED: usize = 256;

/// Whether the phone may listen on `ip`. Private IPv4 only: that is what a WiFi network, a hotspot,
/// WiFi Direct and USB tethering hand out, and it keeps the listener off a public address. Some
/// carriers hand out private addresses too, so this is only the second check: the app picks
/// the interfaces and takes only WiFi, hotspot and ethernet ones.
// shortcut: IPv6 networks are not served; add it when a user needs it.
pub fn shareable(ip: IpAddr) -> bool {
    matches!(ip, IpAddr::V4(v4) if v4.is_private())
}

/// A listener on `ip:port`, ready for [`TcpListener::from_std`]. Never a wildcard address.
pub fn bind(ip: IpAddr, port: u16) -> io::Result<StdListener> {
    if !shareable(ip) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a private IPv4 address",
        ));
    }
    let listener = StdListener::bind((ip, port))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

/// The caps above. Clones share one count.
#[derive(Clone, Default)]
pub struct Admission(Arc<Mutex<State>>);

#[derive(Default)]
struct State {
    active: usize,
    seen: HashMap<IpAddr, Seen>,
}

struct Seen {
    active: u8,
    window_start: Instant,
    opened: u32,
}

/// A connection's place among the unauthenticated ones; give it back by dropping it.
pub struct Ticket {
    admission: Admission,
    ip: IpAddr,
}

impl Admission {
    /// A ticket for a connection from `ip`, or None if a cap is reached and it must be closed.
    pub fn enter(&self, ip: IpAddr) -> Option<Ticket> {
        let ip = ip.to_canonical();
        let now = Instant::now();
        let mut guard = self.lock();
        let State { active, seen } = &mut *guard;
        if *active >= MAX_HANDSHAKES {
            return None;
        }
        if seen.len() >= MAX_TRACKED && !seen.contains_key(&ip) {
            seen.retain(|_, s| s.active > 0 || now.duration_since(s.window_start) < WINDOW);
            if seen.len() >= MAX_TRACKED {
                return None;
            }
        }
        let s = seen.entry(ip).or_insert(Seen {
            active: 0,
            window_start: now,
            opened: 0,
        });
        if now.duration_since(s.window_start) >= WINDOW {
            s.window_start = now;
            s.opened = 0;
        }
        if s.active >= MAX_PER_ADDRESS || s.opened >= MAX_PER_WINDOW {
            return None;
        }
        s.active += 1;
        s.opened += 1;
        *active += 1;
        Some(Ticket {
            admission: self.clone(),
            ip,
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // Plain counters, so a poisoned state is still a valid one.
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        let mut guard = self.admission.lock();
        guard.active = guard.active.saturating_sub(1);
        if let Some(s) = guard.seen.get_mut(&self.ip) {
            s.active = s.active.saturating_sub(1);
        }
    }
}

/// Accepts connections on `listener` until the task is dropped or aborted, and runs `serve` for
/// each one that gets a ticket; the others are closed at once. The sessions belong to this task,
/// so ending it ends them: they must not outlive the network they came in on.
pub async fn accept_loop<F, Fut>(listener: TcpListener, admission: Admission, serve: F)
where
    F: Fn(TcpStream, Ticket) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let mut sessions = JoinSet::new();
    loop {
        tokio::select! {
            Some(_) = sessions.join_next() => {}
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    if let Some(ticket) = admission.enter(peer.ip()) {
                        let _ = stream.set_nodelay(true);
                        sessions.spawn(serve(stream, ticket));
                    }
                }
                // Out of file descriptors, for one; do not spin.
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(n: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(192, 168, 43, n))
    }

    #[test]
    fn listens_on_private_ipv4_only() {
        for ok in ["192.168.43.1", "10.1.2.3", "172.16.0.9", "172.31.255.1"] {
            assert!(shareable(ok.parse().unwrap()), "{ok}");
        }
        for no in [
            "0.0.0.0",
            "127.0.0.1",
            "8.8.8.8",
            "100.64.0.1",
            "169.254.1.1",
            "172.32.0.1",
            "192.0.0.4",
            "::",
            "::1",
            "fd00::1",
            "::ffff:192.168.1.1",
        ] {
            assert!(!shareable(no.parse().unwrap()), "{no}");
        }
        assert!(bind("0.0.0.0".parse().unwrap(), 0).is_err());
        assert!(bind("127.0.0.1".parse().unwrap(), 0).is_err());
    }

    #[tokio::test]
    async fn caps_total_and_per_address_and_gives_tickets_back() {
        let a = Admission::default();
        let one = a.enter(ip(2)).unwrap();
        let two = a.enter(ip(2)).unwrap();
        assert!(a.enter(ip(2)).is_none(), "per address");
        assert!(a.enter(ip(3)).is_some(), "another address is not affected");
        drop(one);
        let _again = a.enter(ip(2)).unwrap();
        drop(two);

        // `_again` holds one ticket, so these fill the rest.
        let held: Vec<_> = (10..10 + MAX_HANDSHAKES as u8 - 1)
            .map(|n| a.enter(ip(n)).unwrap())
            .collect();
        assert!(a.enter(ip(200)).is_none(), "total");
        drop(held);
        assert!(a.enter(ip(200)).is_some());
    }

    #[tokio::test]
    async fn counts_the_mapped_form_as_the_same_address() {
        let a = Admission::default();
        let _x = a.enter("::ffff:192.168.43.5".parse().unwrap()).unwrap();
        let _y = a.enter(ip(5)).unwrap();
        assert!(a.enter(ip(5)).is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn limits_the_rate_per_address_and_forgets_after_the_window() {
        let a = Admission::default();
        for _ in 0..MAX_PER_WINDOW {
            drop(a.enter(ip(7)).unwrap());
        }
        assert!(a.enter(ip(7)).is_none(), "rate");
        assert!(a.enter(ip(8)).is_some(), "another address is not affected");
        tokio::time::advance(WINDOW).await;
        assert!(a.enter(ip(7)).is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_table_refuses_new_addresses_until_entries_expire() {
        let a = Admission::default();
        for n in 0..MAX_TRACKED as u32 {
            drop(
                a.enter(IpAddr::V4(Ipv4Addr::from(0x0a00_0000 + n)))
                    .unwrap(),
            );
        }
        assert!(a.enter(ip(1)).is_none());
        tokio::time::advance(WINDOW).await;
        assert!(a.enter(ip(1)).is_some());
    }

    #[tokio::test]
    async fn closes_connections_over_the_cap_and_ends_sessions_with_the_task() {
        use tokio::io::AsyncReadExt;
        let std = StdListener::bind("127.0.0.1:0").unwrap();
        std.set_nonblocking(true).unwrap();
        let addr = std.local_addr().unwrap();
        let admission = Admission::default();
        let (opened_tx, mut opened) = tokio::sync::mpsc::unbounded_channel();
        let server = tokio::spawn(accept_loop(
            TcpListener::from_std(std).unwrap(),
            admission.clone(),
            move |stream, ticket| {
                let opened = opened_tx.clone();
                async move {
                    opened.send(()).unwrap();
                    // Holds the ticket and the stream, like a handshake that stalls.
                    std::future::pending::<()>().await;
                    drop((stream, ticket));
                }
            },
        ));

        let mut clients = Vec::new();
        for _ in 0..MAX_PER_ADDRESS {
            clients.push(tokio::net::TcpStream::connect(addr).await.unwrap());
            opened.recv().await.unwrap();
        }
        // The cap is per address and every client here is 127.0.0.1: the third is closed.
        let mut third = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut b = [0u8; 1];
        let n = tokio::time::timeout(Duration::from_secs(5), third.read(&mut b))
            .await
            .unwrap();
        assert!(matches!(n, Ok(0) | Err(_)), "closed without a byte");
        assert!(opened.try_recv().is_err());

        // Ending the listener task ends its sessions and gives their tickets back.
        server.abort();
        let _ = server.await;
        let deadline = Instant::now() + Duration::from_secs(5);
        while admission.lock().active != 0 {
            assert!(Instant::now() < deadline, "tickets not returned");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        drop(clients);
    }
}
