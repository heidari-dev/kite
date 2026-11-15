//! iPhones over USB through usbmuxd, the system service that owns the USB link to Apple devices
//! (the one Apple ships on macOS, `usbmuxd` on Linux, Apple Mobile Device Support on Windows).
//! This side speaks its plist protocol: list the devices, then ask it to connect to a TCP port on
//! one of them. After the reply the connection is a plain byte stream to the Kite app, which listens
//! on that port on the iPhone's loopback address while it shares.
//!
//! Nothing usbmuxd says is trusted: a frame's length is checked before anything is allocated,
//! every exchange has a timeout, and the plist reader is bounded in depth and size. Noise runs on
//! top of the stream, so a program that pretends to be usbmuxd, or the app, learns nothing.
//!
//! Only the shapes of reply Kite needs are read. usbmuxd answers a plist request in XML; a binary
//! plist is refused with a message.

use std::future::Future;
use std::io;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Time allowed for each request and its reply.
const STEP: Duration = Duration::from_secs(5);
/// Largest reply accepted. A list of devices is a few hundred bytes each.
const MAX_REPLY: usize = 64 * 1024;
/// More devices than this are ignored.
const MAX_DEVICES: usize = 16;
/// Deepest nesting and most elements the plist reader follows.
const MAX_DEPTH: u8 = 8;
const MAX_NODES: u32 = 4096;
const HEADER: usize = 16;
/// Header fields: protocol version 1 (plist) and message type 8 (plist).
const VERSION: u32 = 1;
const PLIST: u32 = 8;

/// Where usbmuxd listens.
#[cfg(unix)]
const SOCKET: &str = "/var/run/usbmuxd";
#[cfg(windows)]
const SOCKET: std::net::SocketAddr =
    std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 27015);

#[cfg(unix)]
type Mux = tokio::net::UnixStream;
#[cfg(windows)]
type Mux = TcpStream;

async fn dial() -> io::Result<Mux> {
    timeout(STEP, Mux::connect(SOCKET))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "usbmuxd did not answer"))?
}

/// An iPhone usbmuxd lists as connected by USB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// usbmuxd's number for it while it is attached.
    pub id: u32,
    /// Its UDID, which `--serial` takes.
    pub serial: String,
}

/// The iPhones connected by USB. NotFound or ConnectionRefused when usbmuxd is not there.
pub async fn devices() -> io::Result<Vec<Device>> {
    devices_with(&dial).await
}

/// Opens a stream to the Kite app on the iPhone with UDID `serial`, or on the only one, at TCP
/// port `port` of the iPhone.
pub async fn open(serial: Option<&str>, port: u16) -> io::Result<TcpStream> {
    let mux = connect_with(&dial, serial, port).await?;
    let (client, server) = crate::bridge::loopback_pair().await?;
    tokio::spawn(async move {
        // The tunnel ends with either direction, so a phone that ignores a half-close cannot
        // keep this task and its sockets open.
        let (mut from_phone, mut to_phone) = tokio::io::split(mux);
        let (mut from_kite, mut to_kite) = server.into_split();
        tokio::select! {
            _ = tokio::io::copy(&mut from_phone, &mut to_kite) => {}
            _ = tokio::io::copy(&mut from_kite, &mut to_phone) => {}
        }
    });
    Ok(client)
}

/// Picks the iPhone to use: the one with this UDID, or the only one.
pub fn select<'a>(devices: &'a [Device], serial: Option<&str>) -> io::Result<&'a Device> {
    match (serial, devices) {
        (Some(want), _) => devices.iter().find(|d| d.serial == want).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("no iPhone with serial {want} is connected by USB"),
            )
        }),
        (None, [one]) => Ok(one),
        (None, []) => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no iPhone found; connect it by USB and tap Trust on it",
        )),
        (None, many) => {
            let names: Vec<_> = many.iter().map(|d| d.serial.as_str()).collect();
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "several iPhones are connected ({}); choose one with --serial",
                    names.join(", ")
                ),
            ))
        }
    }
}

async fn devices_with<S, F, Fut>(connect: &F) -> io::Result<Vec<Device>>
where
    F: Fn() -> Fut,
    Fut: Future<Output = io::Result<S>>,
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut mux = connect().await?;
    send(&mut mux, 1, &message("ListDevices", "")).await?;
    parse_devices(&receive(&mut mux, 1).await?)
}

/// Asks usbmuxd for a stream to `port` on the iPhone; the stream it returns is the connection.
async fn connect_with<S, F, Fut>(connect: &F, serial: Option<&str>, port: u16) -> io::Result<S>
where
    F: Fn() -> Fut,
    Fut: Future<Output = io::Result<S>>,
    S: AsyncRead + AsyncWrite + Unpin,
{
    let devices = devices_with(connect).await?;
    let device = select(&devices, serial)?;
    let mut mux = connect().await?;
    // usbmuxd wants the port in network byte order, as libusbmuxd's htons(port) sends it.
    let extra = format!(
        "<key>DeviceID</key><integer>{}</integer><key>PortNumber</key><integer>{}</integer>",
        device.id,
        port.to_be()
    );
    send(&mut mux, 2, &message("Connect", &extra)).await?;
    match parse_result(&receive(&mut mux, 2).await?)? {
        0 => Ok(mux),
        3 => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the Kite app is not listening on the iPhone (open it and start sharing)",
        )),
        2 => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the iPhone went away; unlock it, tap Trust and try again",
        )),
        n => Err(io::Error::other(format!(
            "usbmuxd refused the connection ({n})"
        ))),
    }
}

fn message(kind: &str, extra: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\"><dict><key>MessageType</key><string>{kind}</string>\
         <key>ClientVersionString</key><string>kite</string><key>ProgName</key><string>kite</string>\
         <key>kLibUSBMuxVersion</key><integer>3</integer>{extra}</dict></plist>"
    )
}

async fn send<S: AsyncWrite + Unpin>(s: &mut S, tag: u32, plist: &str) -> io::Result<()> {
    let mut frame = Vec::with_capacity(HEADER + plist.len());
    for field in [(HEADER + plist.len()) as u32, VERSION, PLIST, tag] {
        frame.extend_from_slice(&field.to_le_bytes());
    }
    frame.extend_from_slice(plist.as_bytes());
    step(async {
        s.write_all(&frame).await?;
        s.flush().await
    })
    .await
}

/// One reply frame's payload. The length is checked before the buffer is made.
async fn receive<S: AsyncRead + Unpin>(s: &mut S, tag: u32) -> io::Result<Vec<u8>> {
    let mut header = [0u8; HEADER];
    step(async { s.read_exact(&mut header).await.map(drop) }).await?;
    let field =
        |i: usize| u32::from_le_bytes([header[i], header[i + 1], header[i + 2], header[i + 3]]);
    let len = field(0) as usize;
    if field(4) != VERSION || field(8) != PLIST || field(12) != tag {
        return Err(bad("unexpected reply header"));
    }
    if !(HEADER..=HEADER + MAX_REPLY).contains(&len) {
        return Err(bad("reply length out of range"));
    }
    let mut payload = vec![0u8; len - HEADER];
    step(async { s.read_exact(&mut payload).await.map(drop) }).await?;
    Ok(payload)
}

async fn step<T>(op: impl Future<Output = io::Result<T>>) -> io::Result<T> {
    timeout(STEP, op)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "usbmuxd stopped answering"))?
}

fn bad(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("usbmuxd: {why}"))
}

/// The iPhones in a `ListDevices` reply that are attached by USB and have a serial of a kind
/// `--serial` accepts. Others are skipped, not errors.
pub fn parse_devices(reply: &[u8]) -> io::Result<Vec<Device>> {
    let plist = parse(reply)?;
    let Some(Value::Array(list)) = plist.get("DeviceList") else {
        return Err(bad("no device list in the reply"));
    };
    Ok(list
        .iter()
        .filter_map(|entry| {
            let props = entry.get("Properties")?;
            if props.get("ConnectionType")?.as_str()? != "USB" {
                return None;
            }
            let serial = props.get("SerialNumber")?.as_str()?;
            let id = u32::try_from(entry.get("DeviceID")?.as_int()?).ok()?;
            let ok = !serial.is_empty()
                && serial.len() <= 64
                && serial
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._:-".contains(c));
            ok.then(|| Device {
                id,
                serial: serial.into(),
            })
        })
        .take(MAX_DEVICES)
        .collect())
}

/// The `Number` of a `Result` reply: 0 is success.
pub fn parse_result(reply: &[u8]) -> io::Result<i64> {
    let plist = parse(reply)?;
    if plist.get("MessageType").and_then(Value::as_str) != Some("Result") {
        return Err(bad("not a result"));
    }
    plist
        .get("Number")
        .and_then(Value::as_int)
        .ok_or_else(|| bad("no result number"))
}

/// The parts of a property list that usbmuxd's replies use. Dates, data and reals are read past
/// and kept as `Other`.
#[derive(Debug, PartialEq)]
enum Value {
    Int(i64),
    Str(String),
    Dict(Vec<(String, Value)>),
    Array(Vec<Value>),
    Other,
}

impl Value {
    fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(n) => Some(*n),
            _ => None,
        }
    }
}

/// An XML property list. Entities are not decoded: the strings Kite reads have none.
fn parse(data: &[u8]) -> io::Result<Value> {
    if data.starts_with(b"bplist") {
        return Err(bad("a binary plist, which Kite does not read"));
    }
    let text = std::str::from_utf8(data).map_err(|_| bad("the reply is not text"))?;
    let mut p = Reader {
        text,
        at: 0,
        nodes: 0,
    };
    let open = p.tag()?;
    if (open.name, open.kind) != ("plist", Kind::Open) {
        return Err(bad("not a plist"));
    }
    let value = p.value(0)?;
    let close = p.tag()?;
    if (close.name, close.kind) != ("plist", Kind::Close) {
        return Err(bad("plist not closed"));
    }
    Ok(value)
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Open,
    Close,
    Empty,
}

struct Tag<'a> {
    name: &'a str,
    kind: Kind,
}

struct Reader<'a> {
    text: &'a str,
    at: usize,
    nodes: u32,
}

impl<'a> Reader<'a> {
    /// Skips white space, the XML declaration, the doctype and comments.
    fn skip(&mut self) {
        loop {
            let rest = self.text[self.at..].trim_start();
            self.at = self.text.len() - rest.len();
            let end = if rest.starts_with("<?") {
                "?>"
            } else if rest.starts_with("<!--") {
                "-->"
            } else if rest.starts_with("<!") {
                ">"
            } else {
                return;
            };
            self.at = rest
                .find(end)
                .map_or(self.text.len(), |i| self.at + i + end.len());
        }
    }

    fn tag(&mut self) -> io::Result<Tag<'a>> {
        self.skip();
        let rest = &self.text[self.at..];
        let inner = rest
            .strip_prefix('<')
            .and_then(|r| r.split_once('>'))
            .ok_or_else(|| bad("expected a tag"))?
            .0;
        self.at += inner.len() + 2;
        let (kind, inner) = if let Some(name) = inner.strip_prefix('/') {
            (Kind::Close, name)
        } else if let Some(name) = inner.strip_suffix('/') {
            (Kind::Empty, name)
        } else {
            (Kind::Open, inner)
        };
        // Attributes, such as the plist's version, are not read.
        let name = inner.split_ascii_whitespace().next().unwrap_or("");
        Ok(Tag { name, kind })
    }

    /// The text of an element up to its closing tag `name`.
    fn text(&mut self, name: &str) -> io::Result<&'a str> {
        let rest = &self.text[self.at..];
        let end = rest.find('<').ok_or_else(|| bad("text without an end"))?;
        self.at += end;
        let close = self.tag()?;
        if (close.name, close.kind) != (name, Kind::Close) {
            return Err(bad("mismatched tag"));
        }
        Ok(&rest[..end])
    }

    fn value(&mut self, depth: u8) -> io::Result<Value> {
        self.nodes += 1;
        if depth > MAX_DEPTH || self.nodes > MAX_NODES {
            return Err(bad("plist too large"));
        }
        let tag = self.tag()?;
        Ok(match (tag.name, tag.kind) {
            (_, Kind::Close) => return Err(bad("unexpected closing tag")),
            ("dict", Kind::Empty) => Value::Dict(Vec::new()),
            ("array", Kind::Empty) => Value::Array(Vec::new()),
            ("string", Kind::Empty) => Value::Str(String::new()),
            (_, Kind::Empty) => Value::Other, // <true/>, <false/>
            ("dict", Kind::Open) => {
                let mut items = Vec::new();
                loop {
                    let key = self.tag()?;
                    match (key.name, key.kind) {
                        ("dict", Kind::Close) => break Value::Dict(items),
                        ("key", Kind::Open) => {
                            let name = self.text("key")?.to_owned();
                            items.push((name, self.value(depth + 1)?));
                        }
                        _ => return Err(bad("bad dictionary")),
                    }
                }
            }
            ("array", Kind::Open) => {
                let mut items = Vec::new();
                loop {
                    let before = self.at;
                    let next = self.tag()?;
                    if (next.name, next.kind) == ("array", Kind::Close) {
                        break Value::Array(items);
                    }
                    self.at = before;
                    items.push(self.value(depth + 1)?);
                }
            }
            ("integer", Kind::Open) => Value::Int(
                self.text("integer")?
                    .trim()
                    .parse()
                    .map_err(|_| bad("bad integer"))?,
            ),
            ("string", Kind::Open) => Value::Str(self.text("string")?.to_owned()),
            ("real" | "date" | "data", Kind::Open) => {
                self.text(tag.name)?;
                Value::Other
            }
            _ => return Err(bad("unknown element")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, DuplexStream};

    const LIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>DeviceList</key>
	<array>
		<dict>
			<key>DeviceID</key><integer>7</integer>
			<key>MessageType</key><string>Attached</string>
			<key>Properties</key>
			<dict>
				<key>ConnectionType</key><string>USB</string>
				<key>DeviceID</key><integer>7</integer>
				<key>LocationID</key><integer>1048576</integer>
				<key>ProductID</key><integer>4776</integer>
				<key>SerialNumber</key><string>00008110-001A2B3C4D5E6F78</string>
			</dict>
		</dict>
		<dict>
			<key>DeviceID</key><integer>9</integer>
			<key>MessageType</key><string>Attached</string>
			<key>Properties</key>
			<dict>
				<key>ConnectionType</key><string>Network</string>
				<key>SerialNumber</key><string>00008030-AAAABBBBCCCCDDDD</string>
			</dict>
		</dict>
	</array>
</dict>
</plist>"#;

    fn result(n: i64) -> String {
        format!(
            "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>MessageType</key><string>Result</string><key>Number</key><integer>{n}</integer></dict></plist>"
        )
    }

    #[test]
    fn lists_only_usb_iphones() {
        assert_eq!(
            parse_devices(LIST.as_bytes()).unwrap(),
            vec![Device {
                id: 7,
                serial: "00008110-001A2B3C4D5E6F78".into()
            }]
        );
    }

    #[test]
    fn skips_serials_that_are_not_safe_and_ids_out_of_range() {
        let entry = |id: i64, serial: &str| {
            format!("<dict><key>DeviceID</key><integer>{id}</integer><key>Properties</key><dict><key>ConnectionType</key><string>USB</string><key>SerialNumber</key><string>{serial}</string></dict></dict>")
        };
        let reply = format!(
            "<plist><dict><key>DeviceList</key><array>{}{}{}{}</array></dict></plist>",
            entry(1, "a b"),
            entry(2, &"x".repeat(65)),
            entry(-1, "ok"),
            entry(4_294_967_296, "big"),
        );
        assert!(parse_devices(reply.as_bytes()).unwrap().is_empty());
    }

    #[test]
    fn reads_results() {
        assert_eq!(parse_result(result(3).as_bytes()).unwrap(), 3);
        assert!(parse_result(LIST.as_bytes()).is_err());
    }

    #[test]
    fn refuses_what_is_not_a_small_xml_plist() {
        let deep = format!(
            "<plist>{}{}</plist>",
            "<array>".repeat(20),
            "</array>".repeat(20)
        );
        let wide = format!("<plist><array>{}</array></plist>", "<true/>".repeat(5000));
        for bad in [
            &b""[..],
            b"bplist00\x00\x01",
            b"\xff\xfe<plist>",
            b"<plist><dict><key>a</key></dict></plist>",
            b"<plist><dict><string>a</string></dict></plist>",
            b"<plist><integer>99999999999999999999</integer></plist>",
            b"<plist><dict>",
            b"<plist><array><integer>1</string></array></plist>",
            b"</plist>",
            b"<<<>>>",
            deep.as_bytes(),
            wide.as_bytes(),
        ] {
            assert!(parse(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
    }

    /// Plays usbmuxd on one connection: answers one request, then, if it was a good Connect,
    /// echoes whatever the stream carries.
    async fn serve(mut s: DuplexStream, connect_result: i64) {
        let mut header = [0u8; HEADER];
        s.read_exact(&mut header).await.unwrap();
        let len = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
        let tag = u32::from_le_bytes(header[12..].try_into().unwrap());
        let mut payload = vec![0u8; len - HEADER];
        s.read_exact(&mut payload).await.unwrap();
        let payload = String::from_utf8(payload).unwrap();
        let listing = payload.contains("<string>ListDevices</string>");
        let reply = if listing {
            LIST.to_string()
        } else {
            assert!(payload.contains("<key>DeviceID</key><integer>7</integer>"));
            // 47810 sent as htons does: 0xBAC2 as 0xC2BA.
            assert!(payload.contains("<key>PortNumber</key><integer>49850</integer>"));
            result(connect_result)
        };
        let mut frame = Vec::new();
        for field in [(HEADER + reply.len()) as u32, VERSION, PLIST, tag] {
            frame.extend_from_slice(&field.to_le_bytes());
        }
        frame.extend_from_slice(reply.as_bytes());
        s.write_all(&frame).await.unwrap();
        if connect_result == 0 && !listing {
            let mut buf = [0u8; 64];
            while let Ok(n) = s.read(&mut buf).await {
                if n == 0 || s.write_all(&buf[..n]).await.is_err() {
                    return;
                }
            }
        }
    }

    async fn fake(connect_result: i64) -> io::Result<DuplexStream> {
        let (ours, theirs) = duplex(4096);
        tokio::spawn(serve(theirs, connect_result));
        Ok(ours)
    }

    #[tokio::test]
    async fn connects_to_the_phone_and_the_stream_is_the_phones() {
        let connect = || fake(0);
        let found = devices_with(&connect).await.unwrap();
        assert_eq!(found.len(), 1);
        let mut s = connect_with(&connect, None, 47810).await.unwrap();
        s.write_all(b"hello").await.unwrap();
        let mut back = [0u8; 5];
        s.read_exact(&mut back).await.unwrap();
        assert_eq!(&back, b"hello");
        // A serial that is not attached, or is only on the network, is not found.
        for serial in ["nope", "00008030-AAAABBBBCCCCDDDD"] {
            let e = connect_with(&connect, Some(serial), 47810)
                .await
                .err()
                .unwrap();
            assert_eq!(e.kind(), io::ErrorKind::NotFound);
        }
    }

    #[tokio::test]
    async fn says_when_the_app_is_not_listening() {
        let connect = || fake(3);
        let e = connect_with(&connect, None, 47810).await.err().unwrap();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
        assert!(e.to_string().contains("not listening"), "{e}");
    }

    #[tokio::test]
    async fn refuses_a_reply_that_lies_about_its_size() {
        for header in [
            [17u32 + MAX_REPLY as u32, VERSION, PLIST, 1], // too long
            [8, VERSION, PLIST, 1],                        // shorter than a header
            [HEADER as u32, 2, PLIST, 1],                  // another protocol version
            [HEADER as u32, VERSION, 6, 1],                // not a plist
            [HEADER as u32, VERSION, PLIST, 9],            // someone else's tag
        ] {
            let (mut ours, mut theirs) = duplex(64);
            for field in header {
                theirs.write_all(&field.to_le_bytes()).await.unwrap();
            }
            let e = receive(&mut ours, 1).await.unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{header:?}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn gives_up_on_a_usbmuxd_that_stops_answering() {
        let (mut ours, _silent) = duplex(64);
        let e = receive(&mut ours, 1).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn picks_by_serial_or_the_only_phone() {
        let a = Device {
            id: 1,
            serial: "A".into(),
        };
        let b = Device {
            id: 2,
            serial: "B".into(),
        };
        assert_eq!(select(std::slice::from_ref(&a), None).unwrap(), &a);
        assert_eq!(select(&[a.clone(), b.clone()], Some("B")).unwrap(), &b);
        assert_eq!(
            select(&[a, b], None).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            select(&[], None).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }
}
