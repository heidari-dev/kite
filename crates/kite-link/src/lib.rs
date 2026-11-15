//! Transports that yield a single byte stream between computer and phone.

#![forbid(unsafe_code)]

pub mod adb;
mod bridge;
pub mod mdns;
pub mod usb;
pub mod usbmux;

use std::io;
use std::path::Path;
use tokio::net::TcpStream;

/// What `kite devices` shows for an iPhone, where an Android phone has its adb state.
const IPHONE: &str = "iPhone";

/// Opens the relay stream on a phone connected by USB: an iPhone if one is attached (usbmuxd,
/// to TCP port `port` of the iPhone), else an Android phone (adb, with the ADB key in `adb_key`).
/// `serial` picks one of several, whichever kind it belongs to.
pub async fn open(serial: Option<&str>, port: u16, adb_key: &Path) -> io::Result<TcpStream> {
    // A usbmuxd that does not work must not stop an Android phone from being used.
    let phones = iphones().await;
    if wants_iphone(phones.as_deref().unwrap_or_default(), serial) {
        return usbmux::open(serial, port).await;
    }
    adb::open(serial, adb_key).await.map_err(|e| match phones {
        Err(usbmuxd) => io::Error::new(e.kind(), format!("{e} (usbmuxd: {usbmuxd})")),
        Ok(_) => e,
    })
}

/// The phones connected by USB as `(serial, state)`: the iPhones, then the Android phones.
pub async fn devices() -> io::Result<Vec<(String, String)>> {
    let android = adb::devices().await;
    match (iphones().await, android) {
        (Ok(phones), Ok(android)) => Ok(phones
            .into_iter()
            .map(|d| (d.serial, IPHONE.to_string()))
            .chain(android)
            .collect()),
        (Ok(phones), Err(_)) if !phones.is_empty() => Ok(phones
            .into_iter()
            .map(|d| (d.serial, IPHONE.to_string()))
            .collect()),
        // A usbmuxd that does not work must not hide the Android phones.
        (_, Ok(android)) => Ok(android),
        (_, Err(e)) => Err(e),
    }
}

/// The iPhones on USB; none when there is no usbmuxd on this computer.
async fn iphones() -> io::Result<Vec<usbmux::Device>> {
    match usbmux::devices().await {
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(Vec::new())
        }
        r => r,
    }
}

/// An iPhone is used when `serial` names one, or when no serial is given and one is attached.
fn wants_iphone(phones: &[usbmux::Device], serial: Option<&str>) -> bool {
    match serial {
        Some(want) => phones.iter().any(|d| d.serial == want),
        None => !phones.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iphones_come_first_unless_the_serial_is_an_android_one() {
        let phone = |serial: &str| usbmux::Device {
            id: 1,
            serial: serial.into(),
        };
        assert!(!wants_iphone(&[], None));
        assert!(wants_iphone(&[phone("A")], None));
        assert!(wants_iphone(&[phone("A")], Some("A")));
        assert!(!wants_iphone(&[phone("A")], Some("R5CT123")));
        assert!(!wants_iphone(&[], Some("A")));
    }
}
