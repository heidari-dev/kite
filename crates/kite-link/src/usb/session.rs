//! The host side of an ADB connection over any byte stream that carries packets: the handshake
//! (CNXN, and AUTH when the phone wants to know this computer's key), opening the phone's abstract
//! socket `kite`, and then moving bytes between that stream and a local one.
//!
//! The device is untrusted: every packet is length-checked by [`wire`], every wait has a timeout
//! and a packet that is not the one expected ends the session.

use super::key::AdbKey;
use super::wire::{self, Message, AUTH, CLSE, CNXN, MAX_DATA, OKAY, OPEN, VERSION, WRTE};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, Notify};
use tokio::time::{sleep, timeout};

/// Time allowed for a packet to go out or an answer to come back.
const STEP: Duration = Duration::from_secs(10);
/// The phone waits this long for the user to answer "Allow USB debugging".
const PROMPT: Duration = Duration::from_secs(60);
/// How long a packet of ours may wait for its OKAY.
const ACK: Duration = Duration::from_secs(30);
const SOCKET: &str = "localabstract:kite";
/// Our id for the one stream.
const LOCAL_ID: u32 = 1;
const AUTH_TOKEN: u32 = 1;
const AUTH_SIGNATURE: u32 = 2;
const AUTH_PUBLIC_KEY: u32 = 3;
/// Packets of an earlier session that are skipped while a new one starts.
const MAX_STALE: usize = 8;
/// A token is 20 bytes; this leaves room and nothing more.
const MAX_TOKEN: usize = 256;

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// Sends one packet: the header and the data as two writes, each flushed, because the phone's USB
/// side reads them as two transfers.
async fn send<W: AsyncWrite + Unpin>(
    w: &mut W,
    command: u32,
    a0: u32,
    a1: u32,
    data: &[u8],
) -> io::Result<()> {
    let bytes = wire::encode(command, a0, a1, data)?;
    let step = async {
        w.write_all(&bytes[..wire::HEADER_LEN]).await?;
        w.flush().await?;
        if !data.is_empty() {
            w.write_all(&bytes[wire::HEADER_LEN..]).await?;
            w.flush().await?;
        }
        Ok(())
    };
    timeout(STEP, step)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the phone did not take a packet"))?
}

async fn recv<R: AsyncRead + Unpin>(r: &mut R, wait: Duration) -> io::Result<Message> {
    timeout(wait, wire::read_message(r))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the phone did not answer"))?
}

/// What the phone agreed to.
pub struct Connected {
    /// Most data per packet, as far as the phone takes it.
    pub max: usize,
}

/// CNXN, and the AUTH exchange when the phone asks for it: first a signature of its token with
/// our key (it knows the key if the user allowed it before), then, if it asks again, the key
/// itself, which makes it show its prompt.
pub async fn connect<T: AsyncRead + AsyncWrite + Unpin>(
    io: &mut T,
    key: &AdbKey,
) -> io::Result<Connected> {
    send(io, CNXN, VERSION, MAX_DATA as u32, b"host::\0").await?;
    let (mut signed, mut offered) = (false, false);
    let mut wait = STEP;
    let mut stale = 0;
    // The phone asks at most twice; anything more is a refusal or a confused device.
    let mut rounds = 0;
    while rounds < 4 {
        let m = recv(io, wait).await?;
        match m.command {
            // What an earlier session left in the pipe, before the phone answers this one.
            OKAY | WRTE | CLSE if stale < MAX_STALE => {
                stale += 1;
                continue;
            }
            CNXN => {
                if m.arg0 < VERSION {
                    return Err(bad("the phone speaks an ADB version that is too old"));
                }
                let max = usize::try_from(m.arg1).unwrap_or(0).min(MAX_DATA);
                if max < 1024 {
                    return Err(bad("the phone allows packets that are too small"));
                }
                return Ok(Connected { max });
            }
            AUTH if m.arg0 == AUTH_TOKEN && !m.data.is_empty() && m.data.len() <= MAX_TOKEN => {
                if !signed {
                    let signature = key.sign(&m.data)?;
                    send(io, AUTH, AUTH_SIGNATURE, 0, &signature).await?;
                    signed = true;
                } else if !offered {
                    send(io, AUTH, AUTH_PUBLIC_KEY, 0, &key.public_payload()).await?;
                    offered = true;
                    wait = PROMPT;
                } else {
                    break;
                }
            }
            _ => return Err(bad("an unexpected packet while connecting to the phone")),
        }
        rounds += 1;
    }
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "the phone did not allow this computer; unlock it, accept \"Allow USB debugging\" and try again",
    ))
}

/// Opens the phone's abstract socket `kite`; the answer carries the phone's id for the stream.
pub async fn open_stream<T: AsyncRead + AsyncWrite + Unpin>(io: &mut T) -> io::Result<u32> {
    let mut name = SOCKET.as_bytes().to_vec();
    name.push(0);
    send(io, OPEN, LOCAL_ID, 0, &name).await?;
    // Packets of an earlier stream may still arrive first; they name another stream or none.
    for _ in 0..=MAX_STALE {
        let m = recv(io, STEP).await?;
        match m.command {
            OKAY if m.arg1 == LOCAL_ID && m.arg0 != 0 => return Ok(m.arg0),
            CLSE if m.arg1 == LOCAL_ID => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "the Kite app is not listening on the phone (open it and start sharing)",
                ))
            }
            OKAY | WRTE | CLSE => {}
            _ => break,
        }
    }
    Err(bad("an unexpected answer to opening the stream"))
}

/// What the USB writer is asked to send: command, two arguments, data.
type Packet = (u32, u32, u32, Vec<u8>);

/// Moves bytes between the phone's stream and `local` until either ends. Four loops run side by
/// side and none waits for another one's work:
///
/// - the USB reader only reads: the phone's data goes to the local writer through a channel (the
///   phone sends one packet and waits for our OKAY, so one is outstanding at a time), and an OKAY
///   for a packet of ours wakes the loop that sent it;
/// - the local writer writes that data, then has the OKAY sent;
/// - the local reader sends one WRTE at a time and waits for its OKAY;
/// - the USB writer sends what the others queue, one packet at a time.
///
/// So the phone's packets are always read and answered, whatever the local side does: a local
/// side that is slow only slows the phone down, which is what ADB's flow control is for, and two
/// directions at full speed cannot hold each other up. Every queue is bounded.
pub async fn serve<T, S>(io: T, local: S, remote: u32, max: usize) -> io::Result<()>
where
    T: AsyncRead + AsyncWrite + Unpin,
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (dev_r, dev_w) = tokio::io::split(io);
    let (local_r, local_w) = tokio::io::split(local);
    let (out_tx, out_rx) = mpsc::channel::<Packet>(8);
    let (data_tx, data_rx) = mpsc::channel::<Vec<u8>>(2);
    let acked = Notify::new();
    let in_flight = AtomicBool::new(false);
    tokio::select! {
        r = read_usb(dev_r, remote, data_tx, &acked, &in_flight) => r,
        r = write_local(data_rx, local_w, out_tx.clone(), remote) => r,
        r = read_local(local_r, out_tx, &acked, &in_flight, remote, max) => r,
        r = write_usb(dev_w, out_rx) => r,
    }
}

async fn read_usb<R: AsyncRead + Unpin>(
    mut dev: R,
    remote: u32,
    data: mpsc::Sender<Vec<u8>>,
    acked: &Notify,
    in_flight: &AtomicBool,
) -> io::Result<()> {
    loop {
        let m = wire::read_message(&mut dev).await?;
        match m.command {
            WRTE if m.arg0 == remote && m.arg1 == LOCAL_ID => {
                // Never full unless the phone sends before it was answered.
                data.try_send(m.data)
                    .map_err(|_| bad("the phone sent data without waiting for its OKAY"))?;
            }
            OKAY if m.arg0 == remote
                && m.arg1 == LOCAL_ID
                && in_flight.swap(false, Ordering::SeqCst) =>
            {
                acked.notify_one();
            }
            CLSE if m.arg0 == remote => return Ok(()),
            _ => return Err(bad("an unexpected packet from the phone")),
        }
    }
}

async fn write_local<W: AsyncWrite + Unpin>(
    mut data: mpsc::Receiver<Vec<u8>>,
    mut local: W,
    out: mpsc::Sender<Packet>,
    remote: u32,
) -> io::Result<()> {
    loop {
        let Some(bytes) = data.recv().await else {
            // The USB reader ended; its result is the one that counts.
            return std::future::pending().await;
        };
        timeout(STEP, local.write_all(&bytes)).await.map_err(|_| {
            io::Error::new(io::ErrorKind::TimedOut, "the tunnel did not take data")
        })??;
        out.send((OKAY, LOCAL_ID, remote, Vec::new()))
            .await
            .map_err(|_| bad("the USB writer stopped"))?;
    }
}

async fn read_local<R: AsyncRead + Unpin>(
    mut local: R,
    out: mpsc::Sender<Packet>,
    acked: &Notify,
    in_flight: &AtomicBool,
    remote: u32,
    max: usize,
) -> io::Result<()> {
    // Not a multiple of 64, which most reads would not be either; a transfer of such a length gets
    // its zero-length packet from the USB side.
    let size = if max.is_multiple_of(64) { max - 1 } else { max };
    let mut buf = vec![0u8; size];
    loop {
        let n = local.read(&mut buf).await?;
        if n == 0 {
            let _ = out.send((CLSE, LOCAL_ID, remote, Vec::new())).await;
            // Let the writer take it out before the session ends.
            sleep(Duration::from_millis(100)).await;
            return Ok(());
        }
        in_flight.store(true, Ordering::SeqCst);
        out.send((WRTE, LOCAL_ID, remote, buf[..n].to_vec()))
            .await
            .map_err(|_| bad("the USB writer stopped"))?;
        timeout(ACK, acked.notified())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the phone stopped answering"))?;
    }
}

async fn write_usb<W: AsyncWrite + Unpin>(
    mut dev: W,
    mut out: mpsc::Receiver<Packet>,
) -> io::Result<()> {
    while let Some((command, a0, a1, data)) = out.recv().await {
        send(&mut dev, command, a0, a1, &data).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, DuplexStream};

    /// How the fake phone treats the host.
    #[derive(Clone, Copy, PartialEq)]
    enum Phone {
        /// Knows the key from the start.
        Knows,
        /// Asks for the key and accepts it.
        Asks,
        /// Never accepts.
        Refuses,
        /// Has no app listening.
        NoApp,
    }

    async fn phone(mut io: DuplexStream, kind: Phone) {
        async fn read(io: &mut DuplexStream) -> io::Result<Message> {
            wire::read_message(io).await
        }
        let m = read(&mut io).await.unwrap();
        assert_eq!(m.command, CNXN);
        let token = [9u8; 20];
        let hello = wire::encode(CNXN, VERSION, 4096, b"device::\0").unwrap();
        match kind {
            Phone::Knows => {
                io.write_all(&wire::encode(AUTH, AUTH_TOKEN, 0, &token).unwrap())
                    .await
                    .unwrap();
                let sig = read(&mut io).await.unwrap();
                assert_eq!(
                    (sig.command, sig.arg0, sig.data.len()),
                    (AUTH, AUTH_SIGNATURE, 256)
                );
                io.write_all(&hello).await.unwrap();
            }
            Phone::Asks | Phone::Refuses | Phone::NoApp => {
                for round in 0..3 {
                    io.write_all(&wire::encode(AUTH, AUTH_TOKEN, 0, &token).unwrap())
                        .await
                        .unwrap();
                    let a = read(&mut io).await.unwrap();
                    assert_eq!(a.command, AUTH);
                    // Signature first, then the key.
                    assert_eq!(
                        a.arg0,
                        if round == 0 {
                            AUTH_SIGNATURE
                        } else {
                            AUTH_PUBLIC_KEY
                        }
                    );
                    if round == 1 {
                        assert!(a.data.ends_with(b" kite@computer\0"));
                        if kind != Phone::Refuses {
                            io.write_all(&hello).await.unwrap();
                            break;
                        }
                    }
                }
                if kind == Phone::Refuses {
                    return;
                }
            }
        }
        let open = read(&mut io).await.unwrap();
        assert_eq!(
            (open.command, open.arg0, open.data.as_slice()),
            (OPEN, LOCAL_ID, &b"localabstract:kite\0"[..])
        );
        if kind == Phone::NoApp {
            io.write_all(&wire::encode(CLSE, 0, LOCAL_ID, &[]).unwrap())
                .await
                .unwrap();
            return;
        }
        io.write_all(&wire::encode(OKAY, 5, LOCAL_ID, &[]).unwrap())
            .await
            .unwrap();
        // An echo: every WRTE is acknowledged and sent back, split in two.
        loop {
            let Ok(m) = read(&mut io).await else { return };
            match m.command {
                WRTE => {
                    io.write_all(&wire::encode(OKAY, 5, LOCAL_ID, &[]).unwrap())
                        .await
                        .unwrap();
                    let (a, b) = m.data.split_at(m.data.len() / 2);
                    for part in [a, b] {
                        io.write_all(&wire::encode(WRTE, 5, LOCAL_ID, part).unwrap())
                            .await
                            .unwrap();
                    }
                }
                CLSE => return,
                OKAY => {}
                other => panic!("unexpected {other:x}"),
            }
        }
    }

    fn key() -> AdbKey {
        let dir = std::env::temp_dir().join(format!("kite-session-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        AdbKey::load_or_create(&dir.join("adbkey")).unwrap()
    }

    #[tokio::test]
    async fn connects_whether_or_not_the_phone_knows_the_key() {
        let key = key();
        for kind in [Phone::Knows, Phone::Asks] {
            let (mut host, device) = duplex(1 << 16);
            tokio::spawn(phone(device, kind));
            let c = connect(&mut host, &key).await.unwrap();
            assert_eq!(c.max, 4096);
            assert_eq!(open_stream(&mut host).await.unwrap(), 5);
        }
    }

    #[tokio::test]
    async fn a_phone_that_refuses_the_key_ends_the_connection_with_advice() {
        let key = key();
        let (mut host, device) = duplex(1 << 16);
        tokio::spawn(phone(device, Phone::Refuses));
        let e = connect(&mut host, &key).await.map(drop).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        assert!(e.to_string().contains("Allow USB debugging"));
    }

    #[tokio::test]
    async fn a_phone_without_the_app_says_so() {
        let key = key();
        let (mut host, device) = duplex(1 << 16);
        tokio::spawn(phone(device, Phone::NoApp));
        connect(&mut host, &key).await.unwrap();
        let e = open_stream(&mut host).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn carries_bytes_both_ways_one_packet_at_a_time() {
        let key = key();
        let (mut host, device) = duplex(1 << 16);
        tokio::spawn(phone(device, Phone::Knows));
        let c = connect(&mut host, &key).await.unwrap();
        let remote = open_stream(&mut host).await.unwrap();
        let (mine, theirs) = duplex(1 << 16);
        tokio::spawn(serve(host, theirs, remote, c.max));
        // Enough data for several packets; the echo sends it back in halves.
        let data: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
        let (mut r, mut w) = tokio::io::split(mine);
        let sent = data.clone();
        let writer = tokio::spawn(async move {
            w.write_all(&sent).await.unwrap();
            w
        });
        let mut back = vec![0u8; data.len()];
        r.read_exact(&mut back).await.unwrap();
        assert_eq!(back, data);
        drop(writer.await.unwrap());
    }

    #[tokio::test]
    async fn skips_what_an_earlier_session_left_in_the_pipe() {
        let key = key();
        let (mut host, mut device) = duplex(1 << 16);
        tokio::spawn(async move {
            let _ = wire::read_message(&mut device).await;
            for stale in [OKAY, WRTE, CLSE] {
                let p = wire::encode(stale, 9, 9, b"x").unwrap();
                device.write_all(&p).await.unwrap();
            }
            let hello = wire::encode(CNXN, VERSION, 4096, &[]).unwrap();
            device.write_all(&hello).await.unwrap();
            let _ = wire::read_message(&mut device).await;
            let stale = wire::encode(WRTE, 9, 9, b"x").unwrap();
            device.write_all(&stale).await.unwrap();
            let ok = wire::encode(OKAY, 5, LOCAL_ID, &[]).unwrap();
            device.write_all(&ok).await.unwrap();
        });
        connect(&mut host, &key).await.unwrap();
        assert_eq!(open_stream(&mut host).await.unwrap(), 5);
        // A phone that speaks an older version is refused with a reason.
        let (mut host, mut device) = duplex(1 << 16);
        tokio::spawn(async move {
            let _ = wire::read_message(&mut device).await;
            let old = wire::encode(CNXN, 0x0100_0000, 4096, &[]).unwrap();
            device.write_all(&old).await.unwrap();
        });
        let e = connect(&mut host, &key).await.map(drop).unwrap_err();
        assert!(e.to_string().contains("too old"));
    }

    #[tokio::test]
    async fn refuses_packets_that_do_not_belong() {
        let key = key();
        let (mut host, mut device) = duplex(1 << 16);
        tokio::spawn(async move {
            let _ = wire::read_message(&mut device).await;
            // An OPEN where a CNXN or AUTH is expected.
            device
                .write_all(&wire::encode(OPEN, 1, 1, &[]).unwrap())
                .await
                .unwrap();
        });
        let e = connect(&mut host, &key).await.map(drop).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        // A packet that is too long is refused before any of it is read.
        let (mut host, mut device) = duplex(1 << 16);
        tokio::spawn(async move {
            let _ = wire::read_message(&mut device).await;
            let mut h = wire::encode(CNXN, VERSION, 4096, &[]).unwrap();
            h[12..16].copy_from_slice(&(u32::MAX).to_le_bytes());
            device.write_all(&h).await.unwrap();
        });
        let e = connect(&mut host, &key).await.map(drop).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_phone_times_out_at_every_step() {
        let key = key();
        let (mut host, _device) = duplex(1 << 16);
        let e = connect(&mut host, &key).await.map(drop).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
    }

    /// The byte at `i` of the data each side sends.
    fn byte(i: usize, salt: usize) -> u8 {
        ((i * 31 + salt) % 251) as u8
    }

    /// A phone that follows ADB's flow control and streams `total` bytes to the host while it
    /// takes whatever the host sends, both at once; it counts what it received and checks it.
    async fn streaming_phone(io: DuplexStream, total: usize) -> usize {
        use std::sync::Arc;
        use tokio::sync::Mutex;
        let (mut r, w) = tokio::io::split(io);
        let w = Arc::new(Mutex::new(w));
        let m = wire::read_message(&mut r).await.unwrap();
        assert_eq!(m.command, CNXN);
        w.lock()
            .await
            .write_all(&wire::encode(CNXN, VERSION, 4096, b"device::\0").unwrap())
            .await
            .unwrap();
        let m = wire::read_message(&mut r).await.unwrap();
        assert_eq!(m.command, OPEN);
        w.lock()
            .await
            .write_all(&wire::encode(OKAY, 5, LOCAL_ID, &[]).unwrap())
            .await
            .unwrap();

        // The sending side: one WRTE, then wait for the OKAY for it.
        let (ack_tx, mut ack_rx) = mpsc::channel::<()>(1);
        let sender = tokio::spawn({
            let w = w.clone();
            async move {
                let mut sent = 0;
                while sent < total {
                    let n = 3000.min(total - sent);
                    let chunk: Vec<u8> = (sent..sent + n).map(|i| byte(i, 7)).collect();
                    let p = wire::encode(WRTE, 5, LOCAL_ID, &chunk).unwrap();
                    w.lock().await.write_all(&p).await.unwrap();
                    ack_rx.recv().await.unwrap();
                    sent += n;
                }
            }
        });
        // The receiving side: answer each WRTE at once, count, check.
        let mut got = 0;
        while got < total || !sender.is_finished() {
            let m = wire::read_message(&mut r).await.unwrap();
            match m.command {
                WRTE => {
                    for (k, b) in m.data.iter().enumerate() {
                        assert_eq!(*b, byte(got + k, 3), "byte {}", got + k);
                    }
                    got += m.data.len();
                    w.lock()
                        .await
                        .write_all(&wire::encode(OKAY, 5, LOCAL_ID, &[]).unwrap())
                        .await
                        .unwrap();
                }
                OKAY => ack_tx.send(()).await.unwrap(),
                CLSE => {
                    let p = wire::encode(CLSE, 5, LOCAL_ID, &[]).unwrap();
                    w.lock().await.write_all(&p).await.unwrap();
                    break;
                }
                other => panic!("unexpected {other:x}"),
            }
        }
        sender.await.unwrap();
        got
    }

    #[tokio::test]
    async fn both_directions_at_full_speed_do_not_hold_each_other_up() {
        const TOTAL: usize = 6 * 1024 * 1024;
        let key = key();
        // Small pipes, so every side is held back by the others all the time.
        let (mut host, device) = duplex(4096);
        let phone = tokio::spawn(streaming_phone(device, TOTAL));
        let c = connect(&mut host, &key).await.unwrap();
        let remote = open_stream(&mut host).await.unwrap();
        let (mine, theirs) = duplex(4096);
        let session = tokio::spawn(serve(host, theirs, remote, c.max));

        let (mut r, mut w) = tokio::io::split(mine);
        // One task that writes a chunk, then reads one: it cannot read while a write is blocked,
        // as a tunnel that handles its output before its input cannot. If the session did not read
        // from this side while the phone's data waits to be written to it, both would wait.
        let both = async {
            let (mut sent, mut got) = (0, 0);
            let mut buf = vec![0u8; 7000];
            while sent < TOTAL || got < TOTAL {
                if sent < TOTAL {
                    let n = 5000.min(TOTAL - sent);
                    let chunk: Vec<u8> = (sent..sent + n).map(|i| byte(i, 3)).collect();
                    w.write_all(&chunk).await.unwrap();
                    sent += n;
                }
                if got < TOTAL {
                    let n = r.read(&mut buf).await.unwrap();
                    assert!(n > 0, "the tunnel ended after {got} bytes");
                    for (k, b) in buf[..n].iter().enumerate() {
                        assert_eq!(*b, byte(got + k, 7), "byte {}", got + k);
                    }
                    got += n;
                }
            }
        };
        timeout(Duration::from_secs(120), both)
            .await
            .expect("the session stalled");
        // Closing the local end ends the session cleanly, and the phone saw every byte.
        drop((r, w));
        assert!(session.await.unwrap().is_ok());
        assert_eq!(phone.await.unwrap(), TOTAL);
    }
}
