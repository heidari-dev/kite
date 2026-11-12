//! Pairing: which computers the phone serves, and how a new one is added.
//!
//! After the Noise handshake the phone sends one status byte. A known computer gets `OK` and the
//! tunnel starts. An unknown one gets `PAIR` if pairing is open, else `DENIED`.
//!
//! Pairing then runs inside the encrypted session:
//!
//! 1. phone -> computer: `PAIR`, commit = BLAKE2s(tag, n_phone)
//! 2. computer -> phone: n_computer
//! 3. phone -> computer: n_phone; the computer checks it against the commitment
//! 4. both derive a 6-digit code from the handshake hash and both nonces; the phone shows it
//! 5. the user types it on the computer, which compares it with its own value and sends it
//! 6. phone -> computer: `OK` and the computer's key is stored, or `DENIED`
//!
//! A man in the middle runs two handshakes with different hashes, so the two codes differ. The
//! commitment fixes each side's nonce before it sees the other's, so no side can grind its
//! inputs until the codes match: an attacker gets one guess in a million per attempt, and the
//! phone locks pairing after [`MAX_FAILURES`] failed attempts.

use crate::noise::{self, Session, StaticKey, KEY_LEN};
use snow::params::HashChoice;
use snow::resolvers::{CryptoResolver, DefaultResolver};
use std::io;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;
use subtle::ConstantTimeEq;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::Instant;

/// The computer is known: the tunnel starts.
const OK: u8 = 0;
/// The computer is unknown and may pair.
const PAIR: u8 = 1;
/// The computer is unknown and may not pair now.
const DENIED: u8 = 2;

/// How long a code is valid, and how long pairing stays open after [`Pairing::open`].
pub const CODE_LIFETIME: Duration = Duration::from_secs(120);
/// Failed attempts in a row before pairing locks.
pub const MAX_FAILURES: u32 = 5;
/// How long pairing stays locked.
pub const LOCKOUT: Duration = Duration::from_secs(60);
/// Longest wait for each pairing message other than the typed code.
const STEP_TIMEOUT: Duration = Duration::from_secs(10);

/// The phone's list of paired computers and the state of a pairing in progress. Share one
/// instance between the app and every tunnel.
///
/// The app loads the stored keys with [`Pairing::new`], shows [`Pairing::pairing_code`] while it
/// is `Some`, and saves [`Pairing::peers`] after a pairing completes or a peer is removed.
pub struct Pairing(Mutex<State>);

#[derive(Default)]
struct State {
    peers: Vec<[u8; KEY_LEN]>,
    open_until: Option<Instant>,
    /// At most one pairing runs at a time.
    pending: Option<Pending>,
    failures: u32,
    locked_until: Option<Instant>,
}

struct Pending {
    code: Option<u32>,
    expires: Instant,
}

impl Pairing {
    /// Starts with the computers paired earlier. With none, the first computer that connects
    /// may pair; after that only while pairing is open, see [`Pairing::open`].
    pub fn new(peers: Vec<[u8; KEY_LEN]>) -> Self {
        Self(Mutex::new(State {
            peers,
            ..State::default()
        }))
    }

    /// Lets one more computer pair within [`CODE_LIFETIME`].
    pub fn open(&self) {
        self.lock().open_until = Some(Instant::now() + CODE_LIFETIME);
    }

    /// The 6-digit code to show while a computer is pairing, or None.
    pub fn pairing_code(&self) -> Option<String> {
        let state = self.lock();
        let pending = state.pending.as_ref()?;
        let code = pending.code?;
        (pending.expires > Instant::now()).then(|| format!("{code:06}"))
    }

    /// Public keys of the paired computers.
    pub fn peers(&self) -> Vec<[u8; KEY_LEN]> {
        self.lock().peers.clone()
    }

    /// Unpairs a computer. A tunnel it has up keeps running until it closes.
    pub fn remove(&self, key: &[u8; KEY_LEN]) -> bool {
        let mut state = self.lock();
        let before = state.peers.len();
        state.peers.retain(|k| k != key);
        state.peers.len() != before
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // Every update is a plain assignment, so a poisoned State is still a valid one.
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn knows(&self, key: &[u8; KEY_LEN]) -> bool {
        self.lock().peers.contains(key)
    }

    /// Takes the single pairing slot, if pairing is allowed now.
    fn begin(&self) -> Option<Slot<'_>> {
        let now = Instant::now();
        let mut state = self.lock();
        if state.locked_until.is_some_and(|t| t > now)
            || state.pending.as_ref().is_some_and(|p| p.expires > now)
            || (!state.peers.is_empty() && !state.open_until.is_some_and(|t| t > now))
        {
            return None;
        }
        let expires = now + CODE_LIFETIME;
        state.pending = Some(Pending {
            code: None,
            expires,
        });
        Some(Slot {
            pairing: self,
            expires,
            paired: false,
        })
    }
}

impl std::fmt::Debug for Pairing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pairing(..)")
    }
}

/// A pairing in progress. Dropping it without success counts as a failed attempt, so a
/// computer that hangs up or times out costs a try like a wrong code does.
struct Slot<'a> {
    pairing: &'a Pairing,
    expires: Instant,
    paired: bool,
}

impl Slot<'_> {
    fn show(&self, code: u32) {
        if let Some(p) = self.pairing.lock().pending.as_mut() {
            p.code = Some(code);
        }
    }

    /// Checks the code the computer sent and stores its key if it matches.
    fn finish(&mut self, key: [u8; KEY_LEN], typed: [u8; 4], code: u32) -> bool {
        if !bool::from(typed.ct_eq(&code.to_be_bytes())) {
            return false;
        }
        let mut state = self.pairing.lock();
        if !state.peers.contains(&key) {
            state.peers.push(key);
        }
        state.open_until = None;
        state.failures = 0;
        state.pending = None;
        self.paired = true;
        true
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        if self.paired {
            return;
        }
        let mut state = self.pairing.lock();
        state.pending = None;
        state.failures += 1;
        if state.failures >= MAX_FAILURES {
            state.failures = 0;
            state.locked_until = Some(Instant::now() + LOCKOUT);
        }
    }
}

/// Phone side: runs the handshake, pairs an unknown computer if allowed, and returns the session
/// to pass to [`crate::serve`]. An unknown computer that may not pair gets an error.
pub async fn accept<T>(io: T, key: &StaticKey, pairing: &Pairing) -> io::Result<Session<T>>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let (mut s, peer) = noise::accept(io, key).await?;
    if pairing.knows(&peer.key) {
        send(&mut s, &[OK]).await?;
        return Ok(s);
    }
    let Some(mut slot) = pairing.begin() else {
        let _ = send(&mut s, &[DENIED]).await;
        return Err(denied("unknown computer and pairing is not open"));
    };
    let mine = random()?;
    let mut msg = [PAIR; 1 + 32];
    msg[1..].copy_from_slice(&commit(&mine)?);
    send(&mut s, &msg).await?;
    let mut theirs = [0u8; 32];
    step(STEP_TIMEOUT, s.read_exact(&mut theirs)).await?;
    send(&mut s, &mine).await?;
    let code = code(&peer.hash, &theirs, &mine)?;
    slot.show(code);
    let mut typed = [0u8; 4];
    let left = slot.expires.saturating_duration_since(Instant::now());
    step(left, s.read_exact(&mut typed)).await?;
    if !slot.finish(peer.key, typed, code) {
        let _ = send(&mut s, &[DENIED]).await;
        return Err(denied("wrong pairing code"));
    }
    send(&mut s, &[OK]).await?;
    Ok(s)
}

/// Computer side: runs the handshake with a phone whose key is in `phones` and returns the
/// session for yamux. Any other phone is refused before this computer's key is sent.
pub async fn connect<T>(io: T, key: &StaticKey, phones: &[[u8; KEY_LEN]]) -> io::Result<Session<T>>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let (mut s, _) = noise::connect(io, key, |k| phones.contains(k))
        .await
        .map_err(|e| match e.kind() {
            io::ErrorKind::PermissionDenied => denied("the phone is not paired with this computer"),
            _ => e,
        })?;
    match status(&mut s).await? {
        OK => Ok(s),
        PAIR => Err(denied(
            "the phone does not know this computer; run kite pair",
        )),
        _ => Err(denied("the phone refused this computer")),
    }
}

/// Computer side of a pairing, waiting for the user to type the code the phone shows.
pub struct PairAttempt<T> {
    session: Session<T>,
    phone: [u8; KEY_LEN],
    code: u32,
}

impl<T> std::fmt::Debug for PairAttempt<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PairAttempt(..)")
    }
}

/// Computer side: starts pairing with whatever phone answers. Its key is trusted only once
/// [`PairAttempt::finish`] has matched the code.
pub async fn start<T>(io: T, key: &StaticKey) -> io::Result<PairAttempt<T>>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let (mut s, peer) = noise::connect(io, key, |_| true).await?;
    match status(&mut s).await? {
        PAIR => {}
        OK => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "the phone already knows this computer; remove it on the phone to pair again",
            ))
        }
        _ => {
            return Err(denied(
                "the phone refused pairing: open pairing in the app, or wait if it is locked",
            ))
        }
    }
    let mut commitment = [0u8; 32];
    step(STEP_TIMEOUT, s.read_exact(&mut commitment)).await?;
    let mine = random()?;
    send(&mut s, &mine).await?;
    let mut theirs = [0u8; 32];
    step(STEP_TIMEOUT, s.read_exact(&mut theirs)).await?;
    if !bool::from(commit(&theirs)?.ct_eq(&commitment)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the phone broke the pairing protocol",
        ));
    }
    let code = code(&peer.hash, &mine, &theirs)?;
    Ok(PairAttempt {
        session: s,
        phone: peer.key,
        code,
    })
}

impl<T: AsyncRead + AsyncWrite + Unpin> PairAttempt<T> {
    /// Checks `typed` against this side's code and, if it matches, has the phone store this
    /// computer. Returns the phone's key to store on this side.
    pub async fn finish(mut self, typed: &str) -> io::Result<[u8; KEY_LEN]> {
        let typed = parse_code(typed)?;
        if !bool::from(typed.ct_eq(&self.code.to_be_bytes())) {
            // Dropping the session counts as a failed attempt on the phone.
            return Err(denied(
                "the code does not match; if it was typed right, something between this \
                 computer and the phone is intercepting the link",
            ));
        }
        send(&mut self.session, &typed).await?;
        match status(&mut self.session).await? {
            OK => Ok(self.phone),
            _ => Err(denied("the phone refused the code")),
        }
    }
}

fn parse_code(typed: &str) -> io::Result<[u8; 4]> {
    let typed = typed.trim();
    match typed.parse::<u32>() {
        Ok(n) if typed.len() == 6 && typed.bytes().all(|b| b.is_ascii_digit()) => {
            Ok(n.to_be_bytes())
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the code is 6 digits",
        )),
    }
}

async fn status<T: AsyncRead + Unpin>(s: &mut Session<T>) -> io::Result<u8> {
    let mut b = [0u8; 1];
    step(STEP_TIMEOUT, s.read_exact(&mut b)).await?;
    Ok(b[0])
}

async fn send<T: AsyncWrite + Unpin>(s: &mut Session<T>, msg: &[u8]) -> io::Result<()> {
    s.write_all(msg).await?;
    s.flush().await
}

async fn step<R>(
    limit: Duration,
    f: impl std::future::Future<Output = io::Result<R>>,
) -> io::Result<R> {
    tokio::time::timeout(limit, f)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "pairing timed out"))?
}

fn denied(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, msg)
}

fn blake2s(parts: &[&[u8]]) -> io::Result<[u8; 32]> {
    let mut h = DefaultResolver
        .resolve_hash(&HashChoice::Blake2s)
        .ok_or_else(|| io::Error::other("no BLAKE2s"))?;
    for p in parts {
        h.input(p);
    }
    let mut out = [0u8; 32];
    h.result(&mut out);
    Ok(out)
}

fn commit(nonce: &[u8; 32]) -> io::Result<[u8; 32]> {
    blake2s(&[b"kite pairing commit", nonce])
}

/// The 6-digit code. The modulo bias over 2^32 is under 0.03 %.
fn code(hash: &[u8; 32], computer: &[u8; 32], phone: &[u8; 32]) -> io::Result<u32> {
    let h = blake2s(&[b"kite pairing code", hash, computer, phone])?;
    Ok(u32::from_be_bytes([h[0], h[1], h[2], h[3]]) % 1_000_000)
}

fn random() -> io::Result<[u8; 32]> {
    let mut rng = DefaultResolver
        .resolve_rng()
        .ok_or_else(|| io::Error::other("no RNG"))?;
    let mut out = [0u8; 32];
    rng.try_fill_bytes(&mut out)
        .map_err(|e| io::Error::other(format!("RNG: {e}")))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::io::{duplex, DuplexStream};

    fn key() -> Arc<StaticKey> {
        Arc::new(StaticKey::generate().unwrap())
    }

    /// Starts the phone side of one connection in a task.
    fn phone(
        pairing: &Arc<Pairing>,
        key: &Arc<StaticKey>,
    ) -> (DuplexStream, tokio::task::JoinHandle<io::Result<()>>) {
        let (a, b) = duplex(4096);
        let (pairing, key) = (pairing.clone(), key.clone());
        let task = tokio::spawn(async move { accept(b, &key, &pairing).await.map(drop) });
        (a, task)
    }

    #[tokio::test]
    async fn pairs_then_connects_and_refuses_strangers() {
        let (pairing, pk, ck) = (Arc::new(Pairing::new(Vec::new())), key(), key());
        let (io, task) = phone(&pairing, &pk);
        let attempt = start(io, &ck).await.unwrap();
        let code = pairing.pairing_code().unwrap();
        let phone_key = attempt.finish(&code).await.unwrap();
        task.await.unwrap().unwrap();
        assert_eq!(phone_key, pk.public().unwrap());
        assert_eq!(pairing.peers(), vec![ck.public().unwrap()]);
        assert_eq!(pairing.pairing_code(), None);

        // The paired computer connects to the pinned phone.
        let (io, task) = phone(&pairing, &pk);
        connect(io, &ck, &[phone_key]).await.unwrap();
        task.await.unwrap().unwrap();

        // Another phone key is refused before the computer reveals its own.
        let (io, task) = phone(&pairing, &pk);
        let e = connect(io, &ck, &[key().public().unwrap()])
            .await
            .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        assert!(task.await.unwrap().is_err());

        // A stranger may neither pair nor connect once a computer is paired.
        let stranger = key();
        let (io, task) = phone(&pairing, &pk);
        let e = start(io, &stranger).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        assert!(task.await.unwrap().is_err());
        let (io, task) = phone(&pairing, &pk);
        assert!(connect(io, &stranger, &[phone_key]).await.is_err());
        assert!(task.await.unwrap().is_err());
        assert_eq!(pairing.peers().len(), 1);

        // Opening pairing in the app lets one more in.
        pairing.open();
        let (io, task) = phone(&pairing, &pk);
        let attempt = start(io, &stranger).await.unwrap();
        attempt
            .finish(&pairing.pairing_code().unwrap())
            .await
            .unwrap();
        task.await.unwrap().unwrap();
        assert_eq!(pairing.peers().len(), 2);
        assert!(pairing.remove(&stranger.public().unwrap()));
        assert_eq!(pairing.peers(), vec![ck.public().unwrap()]);
    }

    #[tokio::test]
    async fn man_in_the_middle_gets_a_different_code() {
        // The attacker answers the computer as a phone and pairs with the real phone itself.
        let (real, attacker) = (
            Arc::new(Pairing::new(Vec::new())),
            Arc::new(Pairing::new(Vec::new())),
        );
        let (pk, mk, ck) = (key(), key(), key());
        let (to_phone, phone_task) = phone(&real, &pk);
        let (to_attacker, attacker_task) = phone(&attacker, &mk);
        let _attacker_to_phone = start(to_phone, &mk).await.unwrap();
        let computer = start(to_attacker, &ck).await.unwrap();

        // The user types what the real phone shows; the two handshakes differ, so do the codes.
        let shown = real.pairing_code().unwrap();
        assert_ne!(Some(shown.clone()), attacker.pairing_code());
        let e = computer.finish(&shown).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        assert!(attacker_task.await.unwrap().is_err());
        assert!(attacker.peers().is_empty());
        drop(_attacker_to_phone);
        assert!(phone_task.await.unwrap().is_err());
        assert!(real.peers().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn code_expires_after_two_minutes() {
        let (pairing, pk, ck) = (Arc::new(Pairing::new(Vec::new())), key(), key());
        let (io, task) = phone(&pairing, &pk);
        let attempt = start(io, &ck).await.unwrap();
        let code = pairing.pairing_code().unwrap();
        tokio::time::sleep(CODE_LIFETIME + Duration::from_secs(1)).await;
        assert_eq!(pairing.pairing_code(), None);
        assert_eq!(
            task.await.unwrap().unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(attempt.finish(&code).await.is_err());
        assert!(pairing.peers().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn five_failures_lock_pairing_for_a_minute() {
        let (pairing, pk, ck) = (Arc::new(Pairing::new(Vec::new())), key(), key());
        for _ in 0..MAX_FAILURES {
            let (io, task) = phone(&pairing, &pk);
            let attempt = start(io, &ck).await.unwrap();
            let shown: u32 = pairing.pairing_code().unwrap().parse().unwrap();
            let wrong = format!("{:06}", (shown + 1) % 1_000_000);
            assert!(attempt.finish(&wrong).await.is_err());
            assert!(task.await.unwrap().is_err());
        }
        let (io, task) = phone(&pairing, &pk);
        let e = start(io, &ck).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        assert!(task.await.unwrap().is_err());

        tokio::time::sleep(LOCKOUT).await;
        let (io, task) = phone(&pairing, &pk);
        let attempt = start(io, &ck).await.unwrap();
        attempt
            .finish(&pairing.pairing_code().unwrap())
            .await
            .unwrap();
        task.await.unwrap().unwrap();
    }

    #[test]
    fn phone_checks_the_code_and_allows_one_pairing_at_a_time() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let _guard = rt.enter();
        let pairing = Pairing::new(Vec::new());
        let mut slot = pairing.begin().unwrap();
        assert!(
            pairing.begin().is_none(),
            "second pairing while one is pending"
        );
        slot.show(42);
        assert_eq!(pairing.pairing_code().as_deref(), Some("000042"));
        assert!(!slot.finish([1; KEY_LEN], 43u32.to_be_bytes(), 42));
        assert!(slot.finish([1; KEY_LEN], 42u32.to_be_bytes(), 42));
        assert_eq!(pairing.peers(), vec![[1; KEY_LEN]]);
    }

    #[tokio::test]
    async fn computer_refuses_a_phone_that_breaks_its_commitment() {
        let (a, b) = duplex(4096);
        let pk = key();
        tokio::spawn(async move {
            let (mut s, _) = noise::accept(b, &pk).await?;
            let mut msg = [PAIR; 33];
            msg[1..].copy_from_slice(&commit(&[1; 32])?);
            send(&mut s, &msg).await?;
            let mut theirs = [0u8; 32];
            s.read_exact(&mut theirs).await?;
            send(&mut s, &[2; 32]).await?;
            io::Result::Ok(s)
        });
        let e = start(a, &key()).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn code_must_be_six_digits() {
        assert_eq!(parse_code(" 012345\n").unwrap(), 12345u32.to_be_bytes());
        for bad in [
            "",
            "12345",
            "1234567",
            "+12345",
            "12 345",
            "abcdef",
            // A non-ASCII digit that some parsers accept.
            "\u{661}23456",
        ] {
            assert!(parse_code(bad).is_err(), "{bad}");
        }
    }
}
