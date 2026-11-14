//! Finding phones on USB (usbfs on Linux, WinUSB on Windows, through `nusb`), claiming the ADB
//! interface and handing the stream to the session code.
//!
//! The tunnel code wants a TCP stream, so the USB side is bridged to one on the loopback address:
//! a socket pair made here, one end given back and the other served by a task. Only the end that
//! this function connected can be the one the task serves.

use super::key::AdbKey;
use super::session;
use nusb::io::{EndpointRead, EndpointWrite};
use nusb::transfer::{Bulk, Direction, In, Out};
use nusb::DeviceInfo;
use std::collections::HashSet;
use std::io;
use std::net::Ipv4Addr;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Mutex, PoisonError};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};

/// Class, subclass and protocol of the interface adb talks to.
const ADB: (u8, u8, u8) = (0xff, 0x42, 0x01);
/// Bytes per USB transfer; a multiple of every max packet size.
const TRANSFER: usize = 16 * 1024;

struct Phone {
    serial: String,
    info: DeviceInfo,
    interface: u8,
}

/// The phones with an ADB interface that have a serial of a kind adb uses.
async fn phones() -> io::Result<Vec<Phone>> {
    let list = nusb::list_devices().await?;
    Ok(list
        .filter_map(|info| {
            let interface = info
                .interfaces()
                .find(|i| (i.class(), i.subclass(), i.protocol()) == ADB)?
                .interface_number();
            let serial = info.serial_number()?.to_string();
            let ok = serial.len() <= 64
                && !serial.is_empty()
                && serial
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._:-".contains(c));
            ok.then_some(Phone {
                serial,
                info,
                interface,
            })
        })
        .collect())
}

/// The phones on USB as `(serial, state)`. The state is only known once the phone has been asked
/// to allow this computer, so it is always `device` here.
pub async fn devices() -> io::Result<Vec<(String, String)>> {
    Ok(phones()
        .await?
        .into_iter()
        .map(|p| (p.serial, "device".to_string()))
        .collect())
}

/// Serials in use: one session at a time for each phone.
static BUSY: Mutex<Option<HashSet<String>>> = Mutex::new(None);

struct Claim(String);

impl Claim {
    fn new(serial: &str) -> io::Result<Self> {
        let mut busy = BUSY.lock().unwrap_or_else(PoisonError::into_inner);
        if !busy.get_or_insert_default().insert(serial.to_string()) {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("a connection to phone {serial} is open already"),
            ));
        }
        Ok(Self(serial.to_string()))
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        if let Some(busy) = BUSY.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
            busy.remove(&self.0);
        }
    }
}

/// What went wrong with the USB device, in words that say what to do.
fn usb_error(e: nusb::Error) -> io::Error {
    use nusb::ErrorKind::{Busy, Disconnected, PermissionDenied};
    let (kind, advice) = match e.kind() {
        PermissionDenied => (
            io::ErrorKind::PermissionDenied,
            if cfg!(windows) {
                "Windows does not let Kite use the phone's ADB interface; see docs/windows.md".to_string()
            } else {
                "no permission to use the phone over USB; install the udev rule (the kite package does), \
                 replug the phone, or run adb start-server"
                    .to_string()
            },
        ),
        Busy => (
            io::ErrorKind::AddrInUse,
            "another program holds the phone's ADB interface (adb? close it, or run adb start-server)".into(),
        ),
        Disconnected => (io::ErrorKind::NotConnected, "the phone was unplugged".into()),
        _ if cfg!(windows) => (
            io::ErrorKind::Other,
            format!("{e}; the phone's ADB interface needs the WinUSB driver, see docs/windows.md"),
        ),
        _ => (io::ErrorKind::Other, e.to_string()),
    };
    io::Error::new(kind, format!("USB: {advice}"))
}

fn select(phones: Vec<Phone>, serial: Option<&str>) -> io::Result<Phone> {
    let mut phones = phones;
    match (serial, phones.len()) {
        (Some(want), _) => phones
            .into_iter()
            .find(|p| p.serial == want)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no phone with serial {want} is connected by USB"),
                )
            }),
        (None, 0) => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no phone found; connect it by USB and turn on USB debugging",
        )),
        (None, 1) => Ok(phones.remove(0)),
        (None, _) => {
            let names: Vec<_> = phones.iter().map(|p| p.serial.as_str()).collect();
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "several phones are connected ({}); choose one with --serial",
                    names.join(", ")
                ),
            ))
        }
    }
}

/// The two bulk endpoints as one stream. A flush ends the message with a short or zero-length
/// packet, which is how the phone tells where a packet stops.
struct Usb {
    read: EndpointRead<Bulk>,
    write: EndpointWrite<Bulk>,
    ending: bool,
}

impl AsyncRead for Usb {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.read).poll_read(cx, buf)
    }
}

impl AsyncWrite for Usb {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.write).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if !self.ending {
            self.write.submit_end();
            self.ending = true;
        }
        let done = Pin::new(&mut self.write).poll_flush(cx);
        if done.is_ready() {
            self.ending = false;
        }
        done
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_flush(cx)
    }
}

/// Connects to the phone with `serial` (or the only one), over USB, and opens the relay's socket on
/// it. `key_path` is the ADB key this computer shows the phone.
pub async fn open(serial: Option<&str>, key_path: &Path) -> io::Result<TcpStream> {
    let phone = select(phones().await?, serial)?;
    let claim = Claim::new(&phone.serial)?;
    let path = key_path.to_path_buf();
    // The first time this makes a key, which takes a while.
    let key = tokio::task::spawn_blocking(move || AdbKey::load_or_create(&path))
        .await
        .map_err(io::Error::other)??;

    let device = phone.info.open().await.map_err(usb_error)?;
    let interface = device
        .claim_interface(phone.interface)
        .await
        .map_err(usb_error)?;
    let (mut ep_in, mut ep_out) = (None, None);
    if let Some(desc) = interface.descriptor() {
        for ep in desc.endpoints() {
            if ep.transfer_type() == nusb::descriptors::TransferType::Bulk {
                match ep.direction() {
                    Direction::In => ep_in = ep_in.or(Some(ep.address())),
                    Direction::Out => ep_out = ep_out.or(Some(ep.address())),
                }
            }
        }
    }
    let (Some(ep_in), Some(ep_out)) = (ep_in, ep_out) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the phone's ADB interface has no bulk endpoints",
        ));
    };
    let mut usb = Usb {
        read: interface.endpoint::<Bulk, In>(ep_in)?.reader(TRANSFER),
        write: interface.endpoint::<Bulk, Out>(ep_out)?.writer(TRANSFER),
        ending: false,
    };
    let connected = session::connect(&mut usb, &key).await?;
    let remote = session::open_stream(&mut usb).await?;

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let (client, accepted) = tokio::try_join!(
        TcpStream::connect(listener.local_addr()?),
        listener.accept()
    )?;
    let (server, peer) = accepted;
    if peer != client.local_addr()? {
        return Err(io::Error::other(
            "another program connected to the USB bridge",
        ));
    }
    drop(listener);
    tokio::spawn(async move {
        // The claim, the interface and the device stay open for as long as the stream does.
        let _held = (claim, device, interface);
        let _ = session::serve(usb, server, remote, connected.max).await;
    });
    Ok(client)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_session_per_serial() {
        let first = Claim::new("R5CT1234").unwrap();
        let again = Claim::new("R5CT1234").map(drop).unwrap_err();
        assert_eq!(again.kind(), io::ErrorKind::AddrInUse);
        // Another phone is another session.
        let other = Claim::new("R5CT9999").unwrap();
        drop(first);
        Claim::new("R5CT1234").unwrap();
        drop(other);
    }
}
