//! Bindings of the phone relay for the mobile apps (uniffi; Kotlin today).
//!
//! The relay listens on the abstract Unix socket that `kite connect --usb` opens through adb,
//! runs on its own thread with a single-threaded tokio runtime, and opens every outbound socket in
//! the app's process, so the phone's VPN applies to them. No file descriptor crosses the FFI.
//! Every computer runs the Noise handshake and must be paired (see `kite_relay::pair`).

// uniffi's generated scaffolding needs `allow`, which `forbid` would refuse.
#![deny(unsafe_code)]
// The relay itself only runs where abstract Unix sockets exist.
#![cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]

use kite_relay::noise::{StaticKey, KEY_LEN};
use kite_relay::pair::Pairing;
use kite_relay::wifi::{self, Admission, Ticket};
use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::task::JoinHandle as Task;

uniffi::setup_scaffolding!();

/// At most this many computers at once; more connections are closed right away.
const MAX_TUNNELS: usize = 4;

/// A running relay. Dropping it stops it as well.
#[derive(uniffi::Object)]
pub struct Relay {
    stop: Mutex<Option<oneshot::Sender<()>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    counters: Arc<Counters>,
    pairing: Arc<Pairing>,
    shared: Shared,
    handle: tokio::runtime::Handle,
    /// The WiFi listeners. Dropping the runtime ends them as well.
    wifi: Mutex<HashMap<SocketAddr, Task<()>>>,
}

/// A new private key for the relay. The app stores it and passes it to [`Relay::start`].
#[uniffi::export]
pub fn generate_key() -> Result<Vec<u8>, RelayError> {
    Ok(StaticKey::generate()?.as_bytes().to_vec())
}

/// A short name for a computer's key to show the user, the same one `kite pair` prints on the
/// computer. None if `key` is not a key.
#[uniffi::export]
pub fn fingerprint(key: Vec<u8>) -> Option<String> {
    <[u8; KEY_LEN]>::try_from(key)
        .ok()
        .and_then(|k| kite_relay::pair::fingerprint(&k))
}

/// Live counters of the tunnels' bytes, including framing.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct Stats {
    /// Bytes received from computers.
    pub bytes_in: u64,
    /// Bytes sent to computers.
    pub bytes_out: u64,
    /// Computers connected now.
    pub tunnels: u32,
}

#[derive(uniffi::Error, Debug)]
/// `reason`, not `message`: Kotlin exceptions already have a `message`.
pub enum RelayError {
    Io { reason: String },
}

impl std::fmt::Display for RelayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RelayError::Io { reason } => f.write_str(reason),
        }
    }
}

impl From<io::Error> for RelayError {
    fn from(e: io::Error) -> Self {
        RelayError::Io {
            reason: e.to_string(),
        }
    }
}

#[uniffi::export]
impl Relay {
    /// Listens on the abstract Unix socket `socket_name` and relays every paired computer that
    /// connects. `key` is the relay's private key from [`generate_key`] (wiped here); `peers` are
    /// the public keys of the computers paired earlier. Fails if the name is taken, for example
    /// by another running relay.
    #[uniffi::constructor]
    pub fn start(
        socket_name: String,
        mut key: Vec<u8>,
        peers: Vec<Vec<u8>>,
    ) -> Result<Arc<Self>, RelayError> {
        let key = StaticKey::from_bytes(&mut key)?;
        if socket_name.is_empty() || socket_name.len() > 100 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "bad socket name").into());
        }
        let peers = peers
            .into_iter()
            .map(|p| p.try_into())
            .collect::<Result<Vec<[u8; KEY_LEN]>, _>>()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bad peer key"))?;
        let pairing = Arc::new(Pairing::new(peers));
        // Bound before the runtime exists: dropping a runtime panics if the caller is async.
        let listener = listen(&socket_name)?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            // Name lookups run on blocking threads; the default allows 512 of them.
            .max_blocking_threads(8)
            .build()?;
        let registered = {
            let _guard = rt.enter();
            register(listener)
        };
        let listener = match registered {
            Ok(listener) => listener,
            Err(e) => {
                rt.shutdown_background();
                return Err(e.into());
            }
        };
        let counters = Arc::new(Counters::default());
        let (stop, stopped) = oneshot::channel();
        let shared = Shared {
            key: Arc::new(key),
            pairing: pairing.clone(),
            counters: counters.clone(),
            admission: Admission::default(),
        };
        let handle = rt.handle().clone();
        let thread = {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("kite-relay".into())
                .spawn(move || {
                    rt.block_on(accept(listener, shared, stopped));
                    // Lookups still running on blocking threads must not hold up the app.
                    rt.shutdown_timeout(Duration::from_secs(2));
                })?
        };
        Ok(Arc::new(Relay {
            stop: Mutex::new(Some(stop)),
            thread: Mutex::new(Some(thread)),
            counters,
            pairing,
            shared,
            handle,
            wifi: Mutex::new(HashMap::new()),
        }))
    }

    /// Makes the relay listen for computers on exactly these addresses, the phone's own on WiFi,
    /// hotspot or ethernet networks, and stops listening on any it listened on before. An empty
    /// list stops all, and so does the end of the network the app saw: call it again when the
    /// networks change. Private IPv4 addresses only; a cellular or VPN interface's address is the
    /// app's to leave out. Returns the `address:port` pairs now listening. Computers that pair or
    /// connect this way go through the same handshake and pairing as over USB.
    pub fn share_wifi(&self, addresses: Vec<String>) -> Result<Vec<String>, RelayError> {
        self.share_wifi_on(addresses, wifi::PORT)
    }

    /// Lets one more computer pair for two minutes. Call it only when the user asks to pair.
    pub fn open_pairing(&self) {
        self.pairing.open();
    }

    /// The computer asking to pair, as a fingerprint of its key, or None. Ask the user whether
    /// to allow it and pass the answer to [`Relay::answer_pairing`]; the code appears after a yes.
    pub fn pairing_request(&self) -> Option<String> {
        self.pairing
            .request()
            .and_then(|key| kite_relay::pair::fingerprint(&key))
    }

    /// The user's answer to [`Relay::pairing_request`], with the fingerprint that was shown. It
    /// does nothing if another computer is waiting by now.
    pub fn answer_pairing(&self, fingerprint: String, allow: bool) {
        if let Some(key) = self
            .pairing
            .request()
            .filter(|k| kite_relay::pair::fingerprint(k).is_some_and(|f| f == fingerprint))
        {
            self.pairing.answer(&key, allow);
        }
    }

    /// The 6-digit code to show while a computer is pairing, or None.
    pub fn pairing_code(&self) -> Option<String> {
        self.pairing.pairing_code()
    }

    /// Public keys of the paired computers; the app stores them for the next start.
    pub fn peers(&self) -> Vec<Vec<u8>> {
        self.pairing.peers().iter().map(|k| k.to_vec()).collect()
    }

    /// Unpairs a computer. A tunnel it has up keeps running until it closes.
    pub fn remove_peer(&self, key: Vec<u8>) -> bool {
        key.try_into().is_ok_and(|k| self.pairing.remove(&k))
    }

    /// Stops the relay and waits up to a few seconds for it. Call it off the UI thread.
    pub fn stop(&self) {
        drop(
            self.stop
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
        if let Some(thread) = self
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let _ = thread.join();
        }
    }

    pub fn stats(&self) -> Stats {
        Stats {
            bytes_in: self.counters.bytes_in.load(Ordering::Relaxed),
            bytes_out: self.counters.bytes_out.load(Ordering::Relaxed),
            tunnels: self.counters.tunnels.load(Ordering::Relaxed),
        }
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Default)]
struct Counters {
    bytes_in: AtomicU64,
    bytes_out: AtomicU64,
    tunnels: AtomicU32,
}

impl Counters {
    /// A place among the [`MAX_TUNNELS`] computers, for every transport together.
    fn admit(self: &Arc<Self>) -> Option<TunnelSlot> {
        let slot = TunnelSlot(self.clone());
        // Past the limit the slot's drop gives the count back.
        (self.tunnels.fetch_add(1, Ordering::Relaxed) < MAX_TUNNELS as u32).then_some(slot)
    }
}

/// Counts a computer as connected until dropped.
struct TunnelSlot(Arc<Counters>);

impl Drop for TunnelSlot {
    fn drop(&mut self) {
        self.0.tunnels.fetch_sub(1, Ordering::Relaxed);
    }
}

/// What every tunnel needs.
#[derive(Clone)]
struct Shared {
    key: Arc<StaticKey>,
    pairing: Arc<Pairing>,
    counters: Arc<Counters>,
    admission: Admission,
}

impl Shared {
    /// One computer that reached a WiFi listener. The ticket is held until the handshake and any
    /// pairing are over; only then does the computer count as a tunnel, so strangers cannot use up
    /// the places of paired computers.
    async fn wifi_tunnel(&self, stream: TcpStream, ticket: Ticket) {
        let stream = Counted {
            inner: stream,
            counters: self.counters.clone(),
        };
        // Unknown computers stop here, unless pairing is open.
        let session = kite_relay::pair::accept(stream, &self.key, &self.pairing).await;
        drop(ticket);
        let Ok(session) = session else {
            return;
        };
        let Some(_slot) = self.counters.admit() else {
            return;
        };
        let _ = kite_relay::serve(session, kite_relay::Config::default()).await;
    }
}

impl Relay {
    fn share_wifi_on(&self, addresses: Vec<String>, port: u16) -> Result<Vec<String>, RelayError> {
        let want = addresses
            .iter()
            .map(|a| a.parse::<IpAddr>().map(|ip| SocketAddr::new(ip, port)))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bad address"))?;
        let mut live = self.wifi.lock().unwrap_or_else(PoisonError::into_inner);
        live.retain(|addr, task| {
            let keep = want.contains(addr) && !task.is_finished();
            if !keep {
                // Its sessions end with it.
                task.abort();
            }
            keep
        });
        let mut failed = None;
        for addr in want {
            if live.contains_key(&addr) {
                continue;
            }
            match self.listen_wifi(addr) {
                Ok(task) => {
                    live.insert(addr, task);
                }
                Err(e) => failed = Some(e),
            }
        }
        match failed {
            Some(e) if live.is_empty() => Err(e.into()),
            _ => {
                let mut up: Vec<_> = live.keys().map(|a| a.to_string()).collect();
                up.sort();
                Ok(up)
            }
        }
    }

    fn listen_wifi(&self, addr: SocketAddr) -> io::Result<Task<()>> {
        // A listener stopped a moment ago closes its socket when the relay thread next runs, so
        // the address may be busy for a few milliseconds. Called from the app's worker thread.
        let mut tries = 0;
        let std = loop {
            match wifi::bind(addr.ip(), addr.port()) {
                Err(e) if e.kind() == io::ErrorKind::AddrInUse && tries < 50 => {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(20));
                }
                bound => break bound?,
            }
        };
        let _guard = self.handle.enter();
        let listener = tokio::net::TcpListener::from_std(std)?;
        let shared = self.shared.clone();
        let admission = shared.admission.clone();
        Ok(self.handle.spawn(wifi::accept_loop(
            listener,
            admission,
            move |stream, ticket| {
                let shared = shared.clone();
                async move { shared.wifi_tunnel(stream, ticket).await }
            },
        )))
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn listen(name: &str) -> io::Result<std::os::unix::net::UnixListener> {
    #[cfg(target_os = "android")]
    use std::os::android::net::SocketAddrExt;
    #[cfg(target_os = "linux")]
    use std::os::linux::net::SocketAddrExt;
    let addr = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes())?;
    let listener = std::os::unix::net::UnixListener::bind_addr(&addr)?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn register(listener: std::os::unix::net::UnixListener) -> io::Result<tokio::net::UnixListener> {
    tokio::net::UnixListener::from_std(listener)
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn listen(_: &str) -> io::Result<Unsupported> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the relay listens on Android and Linux only",
    ))
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn register(listener: Unsupported) -> io::Result<Unsupported> {
    match listener {}
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
enum Unsupported {}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
async fn accept(listener: Unsupported, _: Shared, _: oneshot::Receiver<()>) {
    match listener {}
}

#[cfg(any(target_os = "linux", target_os = "android"))]
async fn accept(
    listener: tokio::net::UnixListener,
    shared: Shared,
    mut stopped: oneshot::Receiver<()>,
) {
    let counters = &shared.counters;
    let allowed = allowed_uids();
    let mut tunnels = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            // A stop request, or the Relay dropped without one.
            _ = &mut stopped => return,
            Some(_) = tunnels.join_next() => {}
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    // Any app on the phone can reach an abstract socket. Only adbd (shell, or
                    // root on debug builds) and this app may use the relay; others would get
                    // network access they were not granted.
                    let peer = stream.peer_cred().map(|c| c.uid());
                    let slot = peer.is_ok_and(|uid| allowed.contains(&uid)).then(|| counters.admit()).flatten();
                    if let Some(slot) = slot {
                        let stream = Counted { inner: stream, counters: counters.clone() };
                        let (key, pairing) = (shared.key.clone(), shared.pairing.clone());
                        tunnels.spawn(async move {
                            let _slot = slot;
                            // Unpaired computers stop here, unless pairing is open.
                            let Ok(session) = kite_relay::pair::accept(stream, &key, &pairing).await else {
                                return;
                            };
                            let _ = kite_relay::serve(session, kite_relay::Config::default()).await;
                        });
                    }
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            },
        }
    }
}

/// Root, shell (adbd runs as shell on release builds) and this process's own user.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn allowed_uids() -> Vec<u32> {
    use std::os::unix::fs::MetadataExt;
    // /proc/self belongs to the process's user; this avoids an unsafe getuid() call.
    let own = std::fs::metadata("/proc/self").map(|m| m.uid());
    [0, 2000].into_iter().chain(own.ok()).collect()
}

/// Counts the bytes that pass through a tunnel.
struct Counted<S> {
    inner: S,
    counters: Arc<Counters>,
}

impl<S: AsyncRead + Unpin> AsyncRead for Counted<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.inner).poll_read(cx, buf);
        let read = (buf.filled().len() - before) as u64;
        self.counters.bytes_in.fetch_add(read, Ordering::Relaxed);
        polled
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Counted<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let polled = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = polled {
            self.counters
                .bytes_out
                .fetch_add(n as u64, Ordering::Relaxed);
        }
        polled
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use kite_relay::pair;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn counts_bytes_both_ways() {
        let (a, mut b) = tokio::io::duplex(64);
        let counters = Arc::new(Counters::default());
        let mut counted = Counted {
            inner: a,
            counters: counters.clone(),
        };
        counted.write_all(b"hello").await.unwrap();
        let mut got = [0u8; 5];
        b.read_exact(&mut got).await.unwrap();
        b.write_all(b"abc").await.unwrap();
        let mut back = [0u8; 3];
        counted.read_exact(&mut back).await.unwrap();
        assert_eq!(counters.bytes_out.load(Ordering::Relaxed), 5);
        assert_eq!(counters.bytes_in.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn tunnel_places_are_shared_and_given_back() {
        let counters = Arc::new(Counters::default());
        let held: Vec<_> = (0..MAX_TUNNELS)
            .map(|_| counters.admit().unwrap())
            .collect();
        assert!(counters.admit().is_none());
        assert_eq!(counters.tunnels.load(Ordering::Relaxed), MAX_TUNNELS as u32);
        drop(held);
        assert_eq!(counters.tunnels.load(Ordering::Relaxed), 0);
        assert!(counters.admit().is_some());
    }

    fn key() -> Vec<u8> {
        generate_key().unwrap()
    }

    /// A private IPv4 address of this machine, if it has one. The OS picks the source address of a
    /// UDP socket "connected" to a TEST-NET address; nothing is sent.
    #[cfg(target_os = "linux")]
    fn private_address() -> Option<IpAddr> {
        let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        sock.connect("192.0.2.1:9").ok()?;
        let ip = sock.local_addr().ok()?.ip();
        wifi::shareable(ip).then_some(ip)
    }

    #[cfg(target_os = "linux")]
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn serves_paired_computers_over_wifi_only_while_listening() {
        let Some(ip) = private_address() else {
            return; // no private address on this machine
        };
        let (computer, stranger) = (
            StaticKey::generate().unwrap(),
            StaticKey::generate().unwrap(),
        );
        let phone = key();
        let phone_public = StaticKey::from_bytes(&mut phone.clone())
            .unwrap()
            .public()
            .unwrap();
        let relay = Relay::start(
            name("wifi"),
            phone,
            vec![computer.public().unwrap().to_vec()],
        )
        .unwrap();
        let port = free_port();

        // Never a wildcard, loopback or public address.
        for bad in ["0.0.0.0", "127.0.0.1", "8.8.8.8", "::", "nonsense"] {
            assert!(
                relay.share_wifi_on(vec![bad.into()], port).is_err(),
                "{bad}"
            );
        }

        assert_eq!(
            relay.share_wifi_on(vec![ip.to_string()], port).unwrap(),
            vec![format!("{ip}:{port}")]
        );
        // The same list again changes nothing.
        assert_eq!(
            relay
                .share_wifi_on(vec![ip.to_string()], port)
                .unwrap()
                .len(),
            1
        );

        let dial = || async { tokio::net::TcpStream::connect((ip, port)).await.unwrap() };
        // A stranger gets no tunnel while pairing is closed; a paired computer does.
        assert!(pair::start(dial().await, &stranger).await.is_err());
        let session = pair::connect(dial().await, &computer, &[phone_public]).await;
        assert!(session.is_ok(), "paired computer refused");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while relay.stats().tunnels != 1 {
            assert!(std::time::Instant::now() < deadline, "tunnel not counted");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // The network went away: the listener and the tunnel it carried end.
        assert!(relay.share_wifi_on(vec![], port).unwrap().is_empty());
        loop {
            let up = tokio::net::TcpStream::connect((ip, port)).await.is_ok();
            if !up && relay.stats().tunnels == 0 {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "listener still up");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // The network came back right away: the address is free again.
        assert_eq!(
            relay
                .share_wifi_on(vec![ip.to_string()], port)
                .unwrap()
                .len(),
            1
        );
        relay.share_wifi_on(vec![], port).unwrap();
        relay.share_wifi_on(vec![ip.to_string()], port).unwrap();
        drop(session);
    }

    #[test]
    fn rejects_bad_arguments() {
        assert!(Relay::start(String::new(), key(), vec![]).is_err());
        assert!(Relay::start("x".repeat(101), key(), vec![]).is_err());
        assert!(Relay::start("kite-bad-key".into(), vec![1; 31], vec![]).is_err());
        assert!(Relay::start("kite-bad-peer".into(), key(), vec![vec![1; 33]]).is_err());
    }

    #[cfg(target_os = "linux")]
    fn name(test: &str) -> String {
        format!("kite-test-{test}-{}", std::process::id())
    }

    #[cfg(target_os = "linux")]
    async fn dial(name: &str) -> tokio::net::UnixStream {
        use std::os::linux::net::SocketAddrExt;
        let addr = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let s = std::os::unix::net::UnixStream::connect_addr(&addr).unwrap();
        s.set_nonblocking(true).unwrap();
        tokio::net::UnixStream::from_std(s).unwrap()
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn runs_counts_and_stops() {
        let name = name("run");
        let relay = Relay::start(name.clone(), key(), vec![]).unwrap();
        assert!(
            Relay::start(name.clone(), key(), vec![]).is_err(),
            "name is taken"
        );

        let mut computer = dial(&name).await;
        // Not a Noise handshake, so the relay ends the tunnel after reading it; the bytes count.
        computer.write_all(b"not a handshake").await.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while relay.stats().bytes_in == 0 {
            assert!(std::time::Instant::now() < deadline, "no bytes counted");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        relay.stop();
        relay.stop(); // twice is fine
                      // The name is free again once the relay stopped.
        drop(Relay::start(name, key(), vec![]).unwrap());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn pairs_a_computer_only_when_opened() {
        let name = name("pair");
        let phone = key();
        let relay = Relay::start(name.clone(), phone.clone(), vec![]).unwrap();
        let computer = StaticKey::generate().unwrap();

        // Closed: an unknown computer is refused.
        assert!(pair::start(dial(&name).await, &computer).await.is_err());

        relay.open_pairing();
        let attempt = pair::start(dial(&name).await, &computer).await.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while relay.pairing_request().is_none() {
            assert!(std::time::Instant::now() < deadline, "no request shown");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            relay.pairing_code(),
            None,
            "code hidden before the user allows"
        );
        let shown = relay.pairing_request().unwrap();
        relay.answer_pairing(shown, true);
        let code = loop {
            if let Some(code) = relay.pairing_code() {
                break code;
            }
            assert!(std::time::Instant::now() < deadline, "no code shown");
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        let pinned = attempt.finish(&code).await.unwrap();

        let mut phone = phone;
        assert_eq!(
            pinned,
            StaticKey::from_bytes(&mut phone).unwrap().public().unwrap()
        );
        let computer_public = computer.public().unwrap().to_vec();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while relay.peers() != vec![computer_public.clone()] {
            assert!(std::time::Instant::now() < deadline, "computer not stored");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(relay.remove_peer(computer_public.clone()));
        assert!(!relay.remove_peer(computer_public));
        assert!(!relay.remove_peer(vec![0; 3]));
    }
}
