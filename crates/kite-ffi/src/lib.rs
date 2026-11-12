//! Bindings of the phone relay for the mobile apps (uniffi; Kotlin today).
//!
//! The relay listens on the abstract Unix socket that `kite connect --usb` opens through adb,
//! runs on its own thread with a single-threaded tokio runtime, and opens every outbound socket in
//! the app's process, so the phone's VPN applies to them. No file descriptor crosses the FFI.

#![deny(unsafe_code)] // uniffi's generated scaffolding needs `allow`, which `forbid` would refuse
// The relay itself only runs where abstract Unix sockets exist.
#![cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]

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
pub enum RelayError {
    Io { message: String },
}

impl std::fmt::Display for RelayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RelayError::Io { message } => f.write_str(message),
        }
    }
}

impl From<io::Error> for RelayError {
    fn from(e: io::Error) -> Self {
        RelayError::Io {
            message: e.to_string(),
        }
    }
}

#[uniffi::export]
impl Relay {
    /// Listens on the abstract Unix socket `socket_name` and relays every computer that connects.
    /// Fails if the name is taken, for example by another running relay.
    #[uniffi::constructor]
    pub fn start(socket_name: String) -> Result<Arc<Self>, RelayError> {
        if socket_name.is_empty() || socket_name.len() > 100 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "bad socket name").into());
        }
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            // Name lookups run on blocking threads; the default allows 512 of them.
            .max_blocking_threads(8)
            .build()?;
        let listener = {
            let _guard = rt.enter();
            listen(&socket_name)?
        };
        let counters = Arc::new(Counters::default());
        let (stop, stopped) = oneshot::channel();
        let shared = counters.clone();
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
        }))
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

#[cfg(any(target_os = "linux", target_os = "android"))]
fn listen(name: &str) -> io::Result<tokio::net::UnixListener> {
    #[cfg(target_os = "android")]
    use std::os::android::net::SocketAddrExt;
    #[cfg(target_os = "linux")]
    use std::os::linux::net::SocketAddrExt;
    let addr = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes())?;
    let listener = std::os::unix::net::UnixListener::bind_addr(&addr)?;
    listener.set_nonblocking(true)?;
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
enum Unsupported {}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
async fn accept(listener: Unsupported, _: Arc<Counters>, _: oneshot::Receiver<()>) {
    match listener {}
}

#[cfg(any(target_os = "linux", target_os = "android"))]
async fn accept(
    listener: tokio::net::UnixListener,
    counters: Arc<Counters>,
    mut stopped: oneshot::Receiver<()>,
) {
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
                        tunnels.spawn(async move {
                            let _ = kite_relay::serve(stream, kite_relay::Config::default()).await;
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
    fn rejects_bad_socket_names() {
        assert!(Relay::start(String::new()).is_err());
        assert!(Relay::start("x".repeat(101)).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn runs_counts_and_stops() {
        use std::io::Write;
        use std::os::linux::net::SocketAddrExt;
        use std::os::unix::net::{SocketAddr, UnixStream};

        let name = format!("kite-test-{}", std::process::id());
        let relay = Relay::start(name.clone()).unwrap();
        assert!(Relay::start(name.clone()).is_err(), "name is taken");

        let addr = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let mut computer = UnixStream::connect_addr(&addr).unwrap();
        // Not yamux, so the relay ends the tunnel after reading it; the bytes still count.
        computer.write_all(b"not a yamux frame").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while relay.stats().bytes_in == 0 {
            assert!(std::time::Instant::now() < deadline, "no bytes counted");
            std::thread::sleep(Duration::from_millis(10));
        }

        relay.stop();
        relay.stop(); // twice is fine
                      // The name is free again once the relay stopped.
        drop(Relay::start(name).unwrap());
    }
}
