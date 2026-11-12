//! USB through a running adb server. The server owns the USB device and the RSA authorization;
//! this side only speaks its host protocol on loopback: pick a device, switch the connection to
//! its transport, then open the phone's abstract socket `kite`. After the second OKAY the
//! connection is a plain byte stream to the relay.
//!
//! Everything the server or the device says is untrusted: replies are length-capped and every
//! exchange has a timeout.
//!
//! shortcut: talking to the device over USB without an adb server (the ADB wire protocol with RSA
//! authorization and libusb) is not implemented; adb_client 3.2 cannot open a raw stream either.
//! Upgrade when users need it without installing adb.

use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Where adb servers listen by default.
pub const SERVER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 5037);
/// The abstract socket the phone app listens on.
const SOCKET: &str = "localabstract:kite";
/// Time allowed for each request and its reply.
const STEP: Duration = Duration::from_secs(5);
/// Longest device list or failure message accepted.
const MAX_REPLY: usize = 16 * 1024;

/// Opens the relay stream on the phone with serial `serial`, or on the only connected phone.
pub async fn open(serial: Option<&str>) -> io::Result<TcpStream> {
    open_with(|| TcpStream::connect(SERVER), serial).await
}

/// The phones connected by USB as `(serial, adb state)`.
pub async fn devices() -> io::Result<Vec<(String, String)>> {
    devices_with(|| TcpStream::connect(SERVER)).await
}

/// [`open`] with a custom way to reach the server, for tests.
pub async fn open_with<S, F, Fut>(connect: F, serial: Option<&str>) -> io::Result<S>
where
    F: Fn() -> Fut,
    Fut: Future<Output = io::Result<S>>,
    S: AsyncRead + AsyncWrite + Unpin,
{
    let devices = devices_with(&connect).await?;
    let target = select(&devices, serial)?;

    let mut stream = dial(&connect).await?;
    exchange(&mut stream, &format!("host:transport:{target}"), false)
        .await
        .map_err(|e| io::Error::new(e.kind(), format!("adb refused the phone: {e}")))?;
    exchange(&mut stream, SOCKET, false).await.map_err(|e| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("the Kite app is not listening on the phone (open it and start sharing): {e}"),
        )
    })?;
    Ok(stream)
}

async fn devices_with<S, F, Fut>(connect: F) -> io::Result<Vec<(String, String)>>
where
    F: Fn() -> Fut,
    Fut: Future<Output = io::Result<S>>,
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut list = dial(&connect).await?;
    Ok(parse_devices(
        &exchange(&mut list, "host:devices-l", true).await?,
    ))
}

async fn dial<S, F, Fut>(connect: F) -> io::Result<S>
where
    F: Fn() -> Fut,
    Fut: Future<Output = io::Result<S>>,
{
    match timeout(STEP, connect()).await {
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "adb server did not answer",
        )),
        Ok(Err(e)) if e.kind() == io::ErrorKind::ConnectionRefused => Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "no adb server is running; start it with `adb start-server`",
        )),
        Ok(r) => r,
    }
}

/// Sends one request and checks the reply. With `data` the reply carries a length-prefixed body,
/// which is returned.
async fn exchange<S>(s: &mut S, request: &str, data: bool) -> io::Result<String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    timeout(STEP, async {
        s.write_all(format!("{:04x}{request}", request.len()).as_bytes())
            .await?;
        let mut status = [0u8; 4];
        s.read_exact(&mut status).await?;
        match &status {
            b"OKAY" if data => body(s).await,
            b"OKAY" => Ok(String::new()),
            b"FAIL" => {
                let msg = body(s).await?;
                Err(io::Error::other(
                    msg.chars()
                        .filter(|c| !c.is_control() && !is_bidi(*c))
                        .take(200)
                        .collect::<String>(),
                ))
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected reply from the adb server",
            )),
        }
    })
    .await
    .unwrap_or_else(|_| {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "adb server did not answer",
        ))
    })
}

/// Zero-width and bidirectional formatting characters, which could disguise an error line.
fn is_bidi(c: char) -> bool {
    matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Reads a 4-digit hex length and that many bytes.
async fn body<S: AsyncRead + Unpin>(s: &mut S) -> io::Result<String> {
    let mut len = [0u8; 4];
    s.read_exact(&mut len).await?;
    let len = std::str::from_utf8(&len)
        .ok()
        .and_then(|l| usize::from_str_radix(l, 16).ok())
        .filter(|&l| l <= MAX_REPLY)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "bad length from the adb server")
        })?;
    let mut buf = vec![0u8; len];
    s.read_exact(&mut buf).await?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// One line per device: `serial state key:value...`, padded with spaces. Only phones on USB (they
/// have a `usb:` field) count; adb can also list WiFi devices, which this transport must not use.
/// Lines that do not fit are ignored, and so are serials with characters outside what adb itself
/// uses, since the serial goes back into a request.
fn parse_devices(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let (serial, state) = (words.next()?, words.next()?);
            let ok = serial.len() <= 64
                && serial
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._:-".contains(c));
            (ok && words.any(|w| w.starts_with("usb:"))).then(|| (serial.into(), state.into()))
        })
        .collect()
}

/// Picks the device to use and checks that it is ready.
fn select(devices: &[(String, String)], serial: Option<&str>) -> io::Result<String> {
    let (serial, state) = match (serial, devices) {
        (Some(want), _) => devices.iter().find(|(s, _)| s == want).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("no phone with serial {want} is connected by USB"),
            )
        })?,
        (None, []) => {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no phone found; connect it by USB and turn on USB debugging",
            ))
        }
        (None, [one]) => one,
        (None, many) => {
            let names: Vec<_> = many.iter().map(|(s, _)| s.as_str()).collect();
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "several phones are connected ({}); choose one with --serial",
                    names.join(", ")
                ),
            ));
        }
    };
    match state.as_str() {
        "device" => Ok(serial.clone()),
        "unauthorized" => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "phone {serial} has not allowed this computer; accept the USB debugging prompt \
                 on the phone and try again"
            ),
        )),
        "offline" => Err(io::Error::other(format!(
            "phone {serial} is offline; unplug and replug it"
        ))),
        _ => Err(io::Error::other(format!("phone {serial} is not ready"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, DuplexStream};

    #[derive(Clone)]
    struct Fake {
        devices: String,
        app_up: bool,
    }

    /// Plays an adb server on one connection: answers requests until the connection is a stream.
    async fn serve(mut s: DuplexStream, fake: Fake) {
        loop {
            let mut len = [0u8; 4];
            if s.read_exact(&mut len).await.is_err() {
                return;
            }
            let len = usize::from_str_radix(std::str::from_utf8(&len).unwrap(), 16).unwrap();
            let mut req = vec![0u8; len];
            s.read_exact(&mut req).await.unwrap();
            let req = String::from_utf8(req).unwrap();
            let reply = match req.as_str() {
                "host:devices-l" => format!("OKAY{:04x}{}", fake.devices.len(), fake.devices),
                r if r.starts_with("host:transport:") => "OKAY".into(),
                "localabstract:kite" if fake.app_up => "OKAY".into(),
                "localabstract:kite" => "FAIL0006closed".into(),
                _ => "FAIL0004what".into(),
            };
            s.write_all(reply.as_bytes()).await.unwrap();
            if req == "localabstract:kite" && fake.app_up {
                // From here on it is the phone's socket: echo.
                let mut buf = [0u8; 64];
                while let Ok(n) = s.read(&mut buf).await {
                    if n == 0 || s.write_all(&buf[..n]).await.is_err() {
                        return;
                    }
                }
                return;
            }
        }
    }

    async fn run(devices: &str, app_up: bool, serial: Option<&str>) -> io::Result<DuplexStream> {
        let fake = Fake {
            devices: devices.into(),
            app_up,
        };
        open_with(
            move || {
                let (client, server) = duplex(4096);
                tokio::spawn(serve(server, fake.clone()));
                async move { Ok(client) }
            },
            serial,
        )
        .await
    }

    fn err(r: io::Result<DuplexStream>) -> io::Error {
        r.expect_err("expected an error")
    }

    #[tokio::test]
    async fn opens_the_stream_on_the_only_phone() {
        let mut s = run("R5CT1234 device usb:1-2 product:p model:m\n", true, None)
            .await
            .unwrap();
        s.write_all(b"hello").await.unwrap();
        let mut back = [0u8; 5];
        s.read_exact(&mut back).await.unwrap();
        assert_eq!(&back, b"hello");
    }

    #[tokio::test]
    async fn picks_the_serial_among_several() {
        assert!(
            run("A1 device usb:1-1\nB2 device usb:1-2\n", true, Some("B2"))
                .await
                .is_ok()
        );
        let e = err(run("A1 device usb:1-1\nB2 device usb:1-2\n", true, None).await);
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        assert!(e.to_string().contains("--serial") && e.to_string().contains("A1"));
        let e = err(run("A1 device usb:1-1\n", true, Some("C3")).await);
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn explains_phones_that_are_not_ready() {
        let e = err(run("A1 unauthorized usb:1-1\n", true, None).await);
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        assert!(e.to_string().contains("prompt on the phone"));
        assert!(err(run("A1 offline usb:1-1\n", true, None).await)
            .to_string()
            .contains("offline"));
        let e = err(run("", true, None).await);
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
        assert!(e.to_string().contains("USB debugging"));
    }

    #[tokio::test]
    async fn says_when_the_app_is_not_listening() {
        let e = err(run("A1 device usb:1-1\n", false, None).await);
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
        assert!(e.to_string().contains("Kite app"));
    }

    #[tokio::test]
    async fn says_when_no_server_runs() {
        let r: io::Result<DuplexStream> = open_with(
            || async { Err(io::Error::from(io::ErrorKind::ConnectionRefused)) },
            None,
        )
        .await;
        assert!(r.err().unwrap().to_string().contains("adb start-server"));
    }

    #[test]
    fn ignores_malformed_device_lines() {
        let list = parse_devices(
            "ok-1.2 device usb:1-3 model:m\nx;y device usb:1-4\nlonely\n\n192.168.1.9:5555 device product:p\n",
        );
        assert_eq!(list, [("ok-1.2".to_string(), "device".to_string())]);
    }

    #[tokio::test]
    async fn ignores_phones_that_are_not_on_usb() {
        let wifi = "192.168.1.9:5555 device product:p\n";
        let e = err(run(wifi, true, None).await);
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
        let e = err(run(wifi, true, Some("192.168.1.9:5555")).await);
        assert!(e.to_string().contains("by USB"));
    }

    #[tokio::test]
    async fn cleans_failure_messages_from_the_server() {
        let (mut client, mut server) = duplex(1024);
        let msg = "bad\x1b[31m\u{202e}news";
        server
            .write_all(format!("FAIL{:04x}{msg}", msg.len()).as_bytes())
            .await
            .unwrap();
        let e = exchange(&mut client, "host:devices-l", true)
            .await
            .unwrap_err();
        assert_eq!(e.to_string(), "bad[31mnews");
    }

    #[tokio::test]
    async fn rejects_bad_replies() {
        // Oversized length, non-hex length, unknown status.
        for reply in [&b"OKAYffff"[..], b"OKAYzzzz", b"WHAT", b"FAILffff"] {
            let (mut client, mut server) = duplex(64);
            server.write_all(reply).await.unwrap();
            let e = exchange(&mut client, "host:devices-l", true)
                .await
                .unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{reply:?}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn gives_up_on_a_silent_server() {
        let (mut client, _server) = duplex(64);
        let e = exchange(&mut client, "host:devices-l", true)
            .await
            .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
    }
}
