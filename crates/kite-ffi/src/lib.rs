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
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::oneshot;

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
}

/// A new private key for the relay. The app stores it and passes it to [`Relay::start`].
#[uniffi::export]
pub fn generate_key() -> Result<Vec<u8>, RelayError> {
    Ok(StaticKey::generate()?.as_bytes().to_vec())
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
        };
        let thread = std::thread::Builder::new()
            .name("kite-relay".into())
            .spawn(move || {
                rt.block_on(accept(listener, shared, stopped));
                // Lookups still running on blocking threads must not hold up the app.
                rt.shutdown_timeout(Duration::from_secs(2));
            })?;
        Ok(Arc::new(Relay {
            stop: Mutex::new(Some(stop)),
            thread: Mutex::new(Some(thread)),
            counters,
            pairing,
        }))
    }

    /// Lets one more computer pair for two minutes. Call it only when the user asks to pair.
    pub fn open_pairing(&self) {
        self.pairing.open();
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

/// What every tunnel needs.
struct Shared {
    key: Arc<StaticKey>,
    pairing: Arc<Pairing>,
    counters: Arc<Counters>,
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
                    if tunnels.len() < MAX_TUNNELS && peer.is_ok_and(|uid| allowed.contains(&uid)) {
                        let stream = Counted { inner: stream, counters: counters.clone() };
                        let (key, pairing) = (shared.key.clone(), shared.pairing.clone());
                        tunnels.spawn(async move {
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
        // Bounded by MAX_TUNNELS.
        counters
            .tunnels
            .store(tunnels.len() as u32, Ordering::Relaxed);
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

    fn key() -> Vec<u8> {
        generate_key().unwrap()
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
        let code = relay.pairing_code().expect("code shown while pairing");
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
