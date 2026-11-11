//! Noise XX over one byte stream: the handshake and the encrypted framing that yamux runs on.
//!
//! Every message on the wire, handshake or transport, is a 2-byte big-endian length followed by
//! that many bytes. Lengths are checked before anything is read into a buffer, and buffers are
//! allocated once per session at their fixed maximum size.

use snow::params::DHChoice;
use snow::resolvers::{CryptoResolver, DefaultResolver};
use std::io;
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use zeroize::{Zeroize, Zeroizing};

const PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";

/// Length of a public or private key.
pub const KEY_LEN: usize = 32;

/// Longest a peer gets to complete the handshake.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// ChaChaPoly authentication tag.
const TAG: usize = 16;
/// Largest transport frame body. Smaller than Noise's 65535 to keep the three buffers of a
/// session at about 48 KiB.
pub const MAX_FRAME: usize = 16 * 1024 + TAG;
/// Most plaintext bytes in one frame.
const MAX_PLAIN: usize = MAX_FRAME - TAG;
/// XX messages with empty payloads are 32, 96 and 64 bytes.
const MAX_HANDSHAKE: usize = 128;

/// A static X25519 private key. Wiped from memory when dropped.
// shortcut: snow 0.10 copies keys into its own state and does not wipe them; revisit if snow
// gains zeroize support.
pub struct StaticKey(Zeroizing<[u8; KEY_LEN]>);

impl StaticKey {
    /// A new random key.
    pub fn generate() -> io::Result<Self> {
        let pair = snow::Builder::new(params())
            .generate_keypair()
            .map_err(noise_err)?;
        let mut private = Zeroizing::new(pair.private);
        Self::from_bytes(&mut private)
    }

    /// Takes a key from `bytes` and wipes `bytes`, whether or not it is a valid key.
    pub fn from_bytes(bytes: &mut [u8]) -> io::Result<Self> {
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        let ok = bytes.len() == KEY_LEN;
        if ok {
            key.copy_from_slice(bytes);
        }
        bytes.zeroize();
        if !ok {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad key length"));
        }
        Ok(Self(key))
    }

    /// The private key, for storing it.
    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    /// The matching public key, the one peers know this side by.
    pub fn public(&self) -> io::Result<[u8; KEY_LEN]> {
        let mut dh = DefaultResolver
            .resolve_dh(&DHChoice::Curve25519)
            .ok_or_else(|| noise_err("no X25519"))?;
        dh.set(&self.0[..]);
        dh.pubkey().try_into().map_err(|_| noise_err("key length"))
    }
}

impl std::fmt::Debug for StaticKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StaticKey(..)")
    }
}

/// The other side of a finished handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    /// The peer's static public key.
    pub key: [u8; KEY_LEN],
    /// Noise handshake hash. Both ends hold the same value only if nobody sits in between.
    pub hash: [u8; 32],
}

/// Runs the handshake as initiator (the computer). `verify` sees the responder's static key
/// before this side sends its own; returning false aborts without revealing it.
pub async fn connect<T, F>(io: T, key: &StaticKey, verify: F) -> io::Result<(Session<T>, Peer)>
where
    T: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce(&[u8; KEY_LEN]) -> bool,
{
    timed(handshake(io, key, Some(verify))).await
}

/// Runs the handshake as responder (the phone). The caller decides what to do with the
/// initiator's key in the returned [`Peer`].
pub async fn accept<T>(io: T, key: &StaticKey) -> io::Result<(Session<T>, Peer)>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    timed(handshake(io, key, None::<fn(&[u8; KEY_LEN]) -> bool>)).await
}

async fn timed<R>(f: impl std::future::Future<Output = io::Result<R>>) -> io::Result<R> {
    tokio::time::timeout(HANDSHAKE_TIMEOUT, f)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "handshake timed out"))?
}

/// `verify` is Some exactly for the initiator.
async fn handshake<T, F>(
    mut io: T,
    key: &StaticKey,
    mut verify: Option<F>,
) -> io::Result<(Session<T>, Peer)>
where
    T: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce(&[u8; KEY_LEN]) -> bool,
{
    let builder = snow::Builder::new(params())
        .local_private_key(&key.0[..])
        .map_err(noise_err)?;
    let initiator = verify.is_some();
    let mut hs = if initiator {
        builder.build_initiator()
    } else {
        builder.build_responder()
    }
    .map_err(noise_err)?;
    let mut msg = [0u8; MAX_HANDSHAKE];
    let mut payload = [0u8; MAX_HANDSHAKE];
    // -> e; <- e, ee, s, es; -> s, se
    for step in 0..3 {
        if (step % 2 == 0) == initiator {
            let len = hs.write_message(&[], &mut msg).map_err(noise_err)?;
            write_frame(&mut io, &msg[..len]).await?;
        } else {
            let len = read_frame(&mut io, &mut msg).await?;
            // Our payloads are empty; a peer's goes into the bounded buffer and is dropped.
            hs.read_message(&msg[..len], &mut payload)
                .map_err(noise_err)?;
            if let Some(verify) = verify.take() {
                if !verify(&remote_static(&hs)?) {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "unknown peer key",
                    ));
                }
            }
        }
    }
    let peer = Peer {
        key: remote_static(&hs)?,
        hash: hs
            .get_handshake_hash()
            .try_into()
            .map_err(|_| noise_err("hash length"))?,
    };
    let noise = hs.into_transport_mode().map_err(noise_err)?;
    Ok((Session::new(io, noise), peer))
}

fn remote_static(hs: &snow::HandshakeState) -> io::Result<[u8; KEY_LEN]> {
    hs.get_remote_static()
        .and_then(|k| k.try_into().ok())
        .ok_or_else(|| noise_err("no remote static key"))
}

fn params() -> snow::params::NoiseParams {
    // A constant the tests exercise, so this cannot fail at run time.
    PATTERN.parse().expect("valid Noise pattern")
}

fn noise_err(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("noise: {e}"))
}

async fn write_frame<T: AsyncWrite + Unpin>(io: &mut T, body: &[u8]) -> io::Result<()> {
    let len = u16::try_from(body.len()).map_err(|_| noise_err("frame too large"))?;
    io.write_all(&len.to_be_bytes()).await?;
    io.write_all(body).await?;
    io.flush().await
}

/// Reads one frame into `buf` and returns its length. A frame longer than `buf` is refused
/// before its body is read.
async fn read_frame<T: AsyncRead + Unpin>(io: &mut T, buf: &mut [u8]) -> io::Result<usize> {
    let mut len = [0u8; 2];
    io.read_exact(&mut len).await?;
    let len = usize::from(u16::from_be_bytes(len));
    let body = buf
        .get_mut(..len)
        .ok_or_else(|| noise_err("frame too large"))?;
    io.read_exact(body).await?;
    Ok(len)
}

/// An encrypted byte stream after the handshake. Reads and writes plaintext; each write becomes
/// at most one frame on the underlying stream.
pub struct Session<T> {
    io: T,
    noise: snow::TransportState,
    /// Frame being read: 2-byte length, then the body.
    frame: Box<[u8]>,
    filled: usize,
    /// Decrypted bytes not yet handed to the reader.
    plain: Box<[u8]>,
    plain_pos: usize,
    plain_end: usize,
    /// Encrypted frame not yet written.
    out: Box<[u8]>,
    out_pos: usize,
    out_end: usize,
    /// Set after a bad or truncated frame: nothing more is read from a corrupt stream.
    broken: bool,
}

impl<T> Session<T> {
    fn new(io: T, noise: snow::TransportState) -> Self {
        Self {
            io,
            noise,
            frame: vec![0; 2 + MAX_FRAME].into_boxed_slice(),
            filled: 0,
            plain: vec![0; MAX_PLAIN].into_boxed_slice(),
            plain_pos: 0,
            plain_end: 0,
            out: vec![0; 2 + MAX_FRAME].into_boxed_slice(),
            out_pos: 0,
            out_end: 0,
            broken: false,
        }
    }

    fn fail(&mut self, kind: io::ErrorKind, msg: &'static str) -> io::Error {
        self.broken = true;
        io::Error::new(kind, msg)
    }
}

impl<T: AsyncWrite + Unpin> Session<T> {
    /// Writes out the pending frame.
    fn poll_drain(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        while self.out_pos < self.out_end {
            let pending = &self.out[self.out_pos..self.out_end];
            let n = ready!(Pin::new(&mut self.io).poll_write(cx, pending))?;
            if n == 0 {
                return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
            }
            self.out_pos += n;
        }
        Poll::Ready(Ok(()))
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for Session<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            if this.plain_pos < this.plain_end {
                let n = buf.remaining().min(this.plain_end - this.plain_pos);
                buf.put_slice(&this.plain[this.plain_pos..this.plain_pos + n]);
                this.plain_pos += n;
                return Poll::Ready(Ok(()));
            }
            if this.broken {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "stream is broken",
                )));
            }
            let want = if this.filled < 2 {
                2
            } else {
                let len = usize::from(u16::from_be_bytes([this.frame[0], this.frame[1]]));
                if len > MAX_FRAME {
                    let e = this.fail(io::ErrorKind::InvalidData, "frame too large");
                    return Poll::Ready(Err(e));
                }
                // Never sent; refusing them means every frame carries data.
                if len <= TAG {
                    let e = this.fail(io::ErrorKind::InvalidData, "frame too short");
                    return Poll::Ready(Err(e));
                }
                2 + len
            };
            if this.filled == want {
                this.filled = 0;
                match this
                    .noise
                    .read_message(&this.frame[2..want], &mut this.plain)
                {
                    Ok(n) => {
                        this.plain_pos = 0;
                        this.plain_end = n;
                    }
                    Err(_) => {
                        let e = this.fail(io::ErrorKind::InvalidData, "frame failed to decrypt");
                        return Poll::Ready(Err(e));
                    }
                }
                continue;
            }
            let mut rb = ReadBuf::new(&mut this.frame[this.filled..want]);
            ready!(Pin::new(&mut this.io).poll_read(cx, &mut rb))?;
            let got = rb.filled().len();
            if got == 0 {
                // Noise has no close message; yamux notices a close that is not its own.
                if this.filled == 0 {
                    return Poll::Ready(Ok(()));
                }
                let e = this.fail(io::ErrorKind::UnexpectedEof, "truncated frame");
                return Poll::Ready(Err(e));
            }
            this.filled += got;
        }
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Session<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        ready!(this.poll_drain(cx))?;
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let n = buf.len().min(MAX_PLAIN);
        let len = this
            .noise
            .write_message(&buf[..n], &mut this.out[2..])
            .map_err(noise_err)?;
        let len16 = u16::try_from(len).map_err(|_| noise_err("frame too large"))?;
        this.out[..2].copy_from_slice(&len16.to_be_bytes());
        this.out_pos = 0;
        this.out_end = 2 + len;
        // The bytes are taken; if the stream is busy the frame goes out on the next call.
        if let Poll::Ready(Err(e)) = this.poll_drain(cx) {
            return Poll::Ready(Err(e));
        }
        Poll::Ready(Ok(n))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        ready!(this.poll_drain(cx))?;
        Pin::new(&mut this.io).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        ready!(this.poll_drain(cx))?;
        Pin::new(&mut this.io).poll_shutdown(cx)
    }
}

impl<T> std::fmt::Debug for Session<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Session(..)")
    }
}

#[cfg(any(test, fuzzing))]
type Duplex = tokio::io::DuplexStream;

/// Two sessions joined over an in-memory stream, for tests and fuzzing.
#[cfg(any(test, fuzzing))]
pub(crate) async fn pair() -> io::Result<(Session<Duplex>, Session<Duplex>)> {
    let (a, b) = tokio::io::duplex(4 * MAX_FRAME);
    let (ka, kb) = (StaticKey::generate()?, StaticKey::generate()?);
    let (client, server) = tokio::join!(connect(a, &ka, |_| true), accept(b, &kb));
    Ok((client?.0, server?.0))
}

#[cfg(any(test, fuzzing))]
impl<T> Session<T> {
    /// The stream under the encryption, to inject raw bytes.
    pub(crate) fn raw(&mut self) -> &mut T {
        &mut self.io
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn both_ends_agree_and_data_round_trips() {
        let (a, b) = tokio::io::duplex(4096);
        let (ka, kb) = (
            StaticKey::generate().unwrap(),
            StaticKey::generate().unwrap(),
        );
        let pa = ka.public().unwrap();
        let pb = kb.public().unwrap();
        let (client, server) = tokio::join!(connect(a, &ka, |k| *k == pb), accept(b, &kb));
        let (mut client, cpeer) = client.unwrap();
        let (mut server, speer) = server.unwrap();
        assert_eq!(cpeer.key, pb);
        assert_eq!(speer.key, pa);
        assert_eq!(cpeer.hash, speer.hash);

        let data: Vec<u8> = (0..100_000u32).map(|i| i as u8).collect();
        let sent = data.clone();
        let writer = tokio::spawn(async move {
            client.write_all(&sent).await.unwrap();
            client.shutdown().await.unwrap();
        });
        let mut back = Vec::new();
        server.read_to_end(&mut back).await.unwrap();
        writer.await.unwrap();
        assert!(back == data, "bytes differ");
    }

    #[test]
    fn key_bytes_are_wiped_and_checked() {
        let key = StaticKey::generate().unwrap();
        let mut bytes = *key.as_bytes();
        let again = StaticKey::from_bytes(&mut bytes).unwrap();
        assert_eq!(bytes, [0; KEY_LEN]);
        assert_eq!(again.public().unwrap(), key.public().unwrap());
        let mut short = [7u8; 31];
        assert!(StaticKey::from_bytes(&mut short).is_err());
        assert_eq!(short, [0; 31]);
        assert_eq!(format!("{key:?}"), "StaticKey(..)");
    }

    #[tokio::test]
    async fn refuses_wrong_static_key() {
        let (a, b) = tokio::io::duplex(4096);
        let (ka, kb) = (
            StaticKey::generate().unwrap(),
            StaticKey::generate().unwrap(),
        );
        let expected = StaticKey::generate().unwrap().public().unwrap();
        // The initiator drops its stream on failure, which ends the responder's wait.
        let client = async { connect(a, &ka, |k| *k == expected).await.map(|_| ()) };
        let (client, server) = tokio::join!(client, accept(b, &kb));
        assert_eq!(client.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        // So the responder never learned the initiator's key.
        assert!(server.is_err());
    }

    #[tokio::test]
    async fn truncated_frame_is_an_error() {
        let (mut client, mut server) = pair().await.unwrap();
        client.raw().write_all(&[0, 100, 1, 2, 3]).await.unwrap();
        client.raw().shutdown().await.unwrap();
        let mut buf = [0u8; 64];
        let e = server.read(&mut buf).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
        assert!(server.read(&mut buf).await.is_err());
    }

    #[tokio::test]
    async fn close_at_frame_boundary_is_eof() {
        let (mut client, mut server) = pair().await.unwrap();
        client.write_all(b"hi").await.unwrap();
        client.shutdown().await.unwrap();
        let mut back = Vec::new();
        server.read_to_end(&mut back).await.unwrap();
        assert_eq!(back, b"hi");
    }

    #[tokio::test]
    async fn bad_lengths_are_refused_before_the_body() {
        for len in [MAX_FRAME + 1, usize::from(u16::MAX), TAG, 0] {
            let (mut client, mut server) = pair().await.unwrap();
            let len = u16::try_from(len).unwrap().to_be_bytes();
            // No body follows: the reader must refuse on the length alone, not wait for it.
            client.raw().write_all(&len).await.unwrap();
            let mut buf = [0u8; 64];
            let e = server.read(&mut buf).await.unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        }
    }

    #[tokio::test]
    async fn oversized_handshake_frame_is_refused() {
        let kb = StaticKey::generate().unwrap();
        let mut input = ((MAX_HANDSHAKE + 1) as u16).to_be_bytes().to_vec();
        input.resize(2 + MAX_HANDSHAKE + 1, 0);
        let io = tokio::io::join(&input[..], tokio::io::sink());
        let e = accept(io, &kb).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }

    /// Sends `msg` and returns the frame it became on the wire.
    async fn sent_frame(client: &mut Session<Duplex>, msg: &[u8]) -> Vec<u8> {
        client.write_all(msg).await.unwrap();
        client.flush().await.unwrap();
        client.out[..client.out_end].to_vec()
    }

    #[tokio::test]
    async fn replayed_and_tampered_frames_fail() {
        let mut buf = [0u8; 64];
        let (mut client, mut server) = pair().await.unwrap();
        let frame = sent_frame(&mut client, b"first").await;
        let n = server.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"first");
        // The same frame again: its nonce is used up.
        client.raw().write_all(&frame).await.unwrap();
        let e = server.read(&mut buf).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);

        // A frame encrypted for this session but altered on the wire.
        let (mut client, mut server) = pair().await.unwrap();
        let mut frame = [0u8; 64];
        let len = client
            .noise
            .write_message(b"second", &mut frame[2..])
            .unwrap();
        frame[..2].copy_from_slice(&(len as u16).to_be_bytes());
        frame[2 + len - 1] ^= 1;
        client.raw().write_all(&frame[..2 + len]).await.unwrap();
        let e = server.read(&mut buf).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }

    /// Records what one side writes.
    struct Tap<T> {
        inner: T,
        log: Arc<Mutex<Vec<u8>>>,
    }

    impl<T: AsyncRead + Unpin> AsyncRead for Tap<T> {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_read(cx, buf)
        }
    }

    impl<T: AsyncWrite + Unpin> AsyncWrite for Tap<T> {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            let n = ready!(Pin::new(&mut self.inner).poll_write(cx, buf))?;
            self.log.lock().unwrap().extend_from_slice(&buf[..n]);
            Poll::Ready(Ok(n))
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_flush(cx)
        }

        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    #[tokio::test]
    async fn replayed_handshake_messages_fail() {
        let (ka, kb) = (
            StaticKey::generate().unwrap(),
            StaticKey::generate().unwrap(),
        );
        let (a, b) = tokio::io::duplex(4096);
        let from_client = Arc::new(Mutex::new(Vec::new()));
        let from_server = Arc::new(Mutex::new(Vec::new()));
        let a = Tap {
            inner: a,
            log: from_client.clone(),
        };
        let b = Tap {
            inner: b,
            log: from_server.clone(),
        };
        let (client, server) = tokio::join!(connect(a, &ka, |_| true), accept(b, &kb));
        client.unwrap();
        server.unwrap();
        let from_client = from_client.lock().unwrap().clone();
        let from_server = from_server.lock().unwrap().clone();

        // The initiator's messages replayed to the same responder key: message 3 was made for
        // the old responder ephemeral and does not decrypt.
        let io = tokio::io::join(&from_client[..], tokio::io::sink());
        assert!(accept(io, &kb).await.is_err());

        // The responder's message 2 replayed to a new initiator run fails the same way.
        let io = tokio::io::join(&from_server[..], tokio::io::sink());
        assert!(connect(io, &ka, |_| true).await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn silent_peer_times_out() {
        let (_a, b) = tokio::io::duplex(4096);
        let kb = StaticKey::generate().unwrap();
        let e = accept(b, &kb).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
    }
}
