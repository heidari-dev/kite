//! The protocol between the Kite daemon and its clients (the `kite` command, the desktop app).
//!
//! One JSON object per line, in both directions. The client sends a [`Request`] and reads one
//! [`Response`]. The daemon treats every request line as untrusted: it is cut off at
//! [`MAX_REQUEST`] bytes, parsed strictly (unknown commands and unknown fields are refused) and
//! its values are checked by [`Request::parse`] before anything acts on them.

use serde::{Deserialize, Serialize};
use std::io;
use std::net::SocketAddr;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

/// Where the daemon listens unless told otherwise.
pub const DEFAULT_SOCKET: &str = "/run/kite/kite.sock";
/// Longest request line, newline included.
pub const MAX_REQUEST: usize = 4096;
/// Longest response line a client accepts.
pub const MAX_RESPONSE: usize = 64 * 1024;

/// How to reach the phone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Transport {
    /// The relay at `addr` (`HOST:PORT`) over the network.
    Tcp { addr: String },
    /// A phone connected by USB, through the adb server. `serial` picks one of several.
    Usb { serial: Option<String> },
    /// The phone's relay on the local network or its hotspot: `addr` is `HOST` or `HOST:PORT`
    /// (port 47810 if left out), or None to look for the phone with mDNS.
    Wifi { addr: Option<String> },
}

/// What to share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// A local SOCKS5 proxy.
    Proxy,
    /// All traffic of the computer.
    Tun,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// The state of the connection.
    Status {},
    /// Phones connected by USB.
    Devices {},
    /// Opens the tunnel and returns at once; follow it with `Status`.
    Connect {
        transport: Transport,
        mode: Mode,
        /// Keep all traffic blocked while the link is down and reconnect. `Tun` only.
        #[serde(default)]
        kill_switch: bool,
        /// Proxy mode: the loopback address to serve. Default `127.0.0.1:1080`.
        #[serde(default)]
        listen: Option<SocketAddr>,
    },
    Disconnect {},
    /// Counters of the tunnel.
    Stats {},
    /// Starts pairing: the phone shows a code to send with `Pair`. Valid for two minutes.
    PairStart {
        transport: Transport,
    },
    /// Finishes the pairing started by `PairStart` with the 6-digit code from the phone.
    Pair {
        code: String,
    },
    /// The public keys of the paired phones.
    Peers {},
    /// Forgets the phone whose key starts with `key`.
    RemovePeer {
        key: String,
    },
}

impl Request {
    /// The request as one line, newline included.
    pub fn line(&self) -> Vec<u8> {
        let mut line = serde_json::to_vec(self).unwrap_or_default();
        line.push(b'\n');
        line
    }

    /// Parses and checks one request line (without its newline).
    pub fn parse(line: &[u8]) -> Result<Self, String> {
        if line.len() > MAX_REQUEST {
            return Err("request too long".into());
        }
        let req: Self = serde_json::from_slice(line).map_err(|e| format!("bad request: {e}"))?;
        req.check()?;
        Ok(req)
    }

    fn check(&self) -> Result<(), String> {
        let bad = |what: &str| Err(format!("bad {what}"));
        match self {
            Self::Connect {
                transport,
                mode,
                kill_switch,
                listen,
            } => {
                check_transport(transport)?;
                if *kill_switch && *mode != Mode::Tun {
                    return Err("kill_switch needs mode tun".into());
                }
                // A name would be looked up again at every redial, and with the link down that
                // lookup goes into the block.
                if let (true, Transport::Tcp { addr } | Transport::Wifi { addr: Some(addr) }) =
                    (*kill_switch, transport)
                {
                    if !is_ip(addr) {
                        return Err("kill_switch needs an IP address as the relay".into());
                    }
                }
                if let Some(listen) = listen {
                    if *mode != Mode::Proxy || !listen.ip().is_loopback() {
                        return bad("listen");
                    }
                }
            }
            Self::PairStart { transport } => check_transport(transport)?,
            Self::Pair { code } if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) => {
                return bad("code");
            }
            Self::RemovePeer { key }
                if key.is_empty()
                    || key.len() > 64
                    || !key.bytes().all(|b| b.is_ascii_hexdigit()) =>
            {
                return bad("key");
            }
            _ => {}
        }
        Ok(())
    }
}

/// `IP` or `IP:PORT`.
fn is_ip(addr: &str) -> bool {
    addr.parse::<SocketAddr>().is_ok() || addr.parse::<std::net::IpAddr>().is_ok()
}

fn check_transport(t: &Transport) -> Result<(), String> {
    match t {
        Transport::Tcp { addr } | Transport::Wifi { addr: Some(addr) }
            if addr.is_empty() || addr.len() > 255 || addr.chars().any(char::is_control) =>
        {
            Err("bad addr".into())
        }
        // The serial goes back into an adb request, so it keeps to what adb itself uses.
        Transport::Usb { serial: Some(s) }
            if s.is_empty()
                || s.len() > 64
                || !s
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._:-".contains(c)) =>
        {
            Err("bad serial".into())
        }
        _ => Ok(()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Idle,
    Connecting,
    Connected,
    /// The link is down; with the kill switch everything stays blocked until it is back.
    Reconnecting,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub state: State,
    /// The relay address, or `usb`.
    pub transport: Option<String>,
    pub mode: Option<Mode>,
    pub kill_switch: bool,
    /// Why the last connection attempt or the last link ended.
    pub error: Option<String>,
    /// A `PairStart` is waiting for its code.
    pub pairing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stats {
    pub streams: usize,
    pub max_streams: usize,
    /// Links lost and re-established since `Connect`.
    pub reconnects: u32,
    /// Seconds since the tunnel last came up; 0 when it is not up.
    pub uptime_secs: u64,
    /// Bytes sent to the phone since `Connect`. Two readings give a speed.
    #[serde(default)]
    pub bytes_up: u64,
    /// Bytes received from the phone since `Connect`.
    #[serde(default)]
    pub bytes_down: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub serial: String,
    /// The adb state: `device`, `unauthorized`, `offline`.
    pub state: String,
}

/// The answer to one request. Only the field that belongs to the command is set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<Stats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub devices: Option<Vec<Device>>,
    /// Hex public keys: `Peers`, and the phone just paired by `Pair`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peers: Option<Vec<String>>,
    /// This computer's key as the phone shows it, from `PairStart`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

impl Response {
    pub fn ok() -> Self {
        Self {
            ok: true,
            ..Self::default()
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            error: Some(msg.into()),
            ..Self::default()
        }
    }

    /// Parses a response line (without its newline).
    pub fn parse(line: &[u8]) -> io::Result<Self> {
        serde_json::from_slice(line)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("bad response: {e}")))
    }

    /// The response as one line, newline included.
    pub fn line(&self) -> Vec<u8> {
        let mut line = serde_json::to_vec(self).unwrap_or_else(|_| br#"{"ok":false}"#.to_vec());
        line.push(b'\n');
        line
    }
}

/// Reads one line of at most `max` bytes, newline included, without the newline. `None` at the
/// end of the stream. A longer line is an error, and the rest of the stream is not to be trusted.
pub async fn read_line<R: AsyncBufRead + Unpin>(
    r: &mut R,
    max: usize,
) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let buf = r.fill_buf().await?;
        if buf.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err(io::ErrorKind::UnexpectedEof.into())
            };
        }
        let (take, found) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (buf.len(), false),
        };
        if line.len() + take > max {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
        }
        line.extend_from_slice(&buf[..take]);
        r.consume(take);
        if found {
            line.pop();
            return Ok(Some(line));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Request, String> {
        Request::parse(s.as_bytes())
    }

    #[test]
    fn parses_every_command() {
        assert_eq!(parse(r#"{"cmd":"status"}"#), Ok(Request::Status {}));
        assert_eq!(
            parse(r#"{"cmd":"remove_peer","key":"ab12"}"#),
            Ok(Request::RemovePeer { key: "ab12".into() })
        );
        let c = parse(
            r#"{"cmd":"connect","transport":{"type":"usb","serial":null},"mode":"tun","kill_switch":true}"#,
        );
        assert_eq!(
            c,
            Ok(Request::Connect {
                transport: Transport::Usb { serial: None },
                mode: Mode::Tun,
                kill_switch: true,
                listen: None,
            })
        );
        assert!(parse(r#"{"cmd":"pair","code":"012345"}"#).is_ok());
        assert!(
            parse(r#"{"cmd":"pair_start","transport":{"type":"tcp","addr":"10.0.0.2:7000"}}"#)
                .is_ok()
        );
    }

    #[test]
    fn refuses_what_it_does_not_know() {
        for bad in [
            "",
            "{}",
            "[]",
            r#"{"cmd":"reboot"}"#,
            r#"{"cmd":"status","extra":1}"#,
            r#"{"cmd":"status"}{"cmd":"status"}"#,
            r#"{"cmd":"connect","mode":"tun"}"#,
            r#"{"cmd":"connect","transport":{"type":"usb","serial":null,"x":1},"mode":"tun"}"#,
            r#"{"cmd":"connect","transport":{"type":"tcp","addr":"a:1"},"mode":"tun","path":"/etc"}"#,
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn checks_values() {
        let connect = |t: &str, rest: &str| {
            parse(&format!(
                r#"{{"cmd":"connect","transport":{t},"mode":"proxy"{rest}}}"#
            ))
        };
        let tcp = r#"{"type":"tcp","addr":"a:1"}"#;
        assert!(connect(tcp, "").is_ok());
        assert!(connect(tcp, r#","listen":"127.0.0.1:9050""#).is_ok());
        assert!(connect(tcp, r#","listen":"0.0.0.0:1080""#).is_err());
        assert!(connect(tcp, r#","kill_switch":true"#).is_err());
        let tun = |addr: &str| {
            parse(&format!(
                r#"{{"cmd":"connect","transport":{{"type":"tcp","addr":"{addr}"}},"mode":"tun","kill_switch":true}}"#
            ))
        };
        assert!(tun("10.0.0.2:7000").is_ok());
        let wifi = |addr: &str, rest: &str| {
            parse(&format!(
                r#"{{"cmd":"connect","transport":{{"type":"wifi","addr":{addr}}},"mode":"tun"{rest}}}"#
            ))
        };
        let kill = r#","kill_switch":true"#;
        assert!(wifi("null", kill).is_ok());
        assert!(wifi(r#""192.168.43.1""#, kill).is_ok());
        assert!(wifi(r#""192.168.43.1:9""#, kill).is_ok());
        assert!(wifi(r#""phone.local""#, kill).is_err());
        assert!(wifi(r#""phone.local""#, "").is_ok());
        assert!(wifi(r#""""#, "").is_err());
        assert!(wifi(r#""a\nb""#, "").is_err());
        assert!(parse(r#"{"cmd":"pair_start","transport":{"type":"wifi","addr":null}}"#).is_ok());
        assert!(tun("relay.local:7000").is_err());
        assert!(connect(r#"{"type":"tcp","addr":""}"#, "").is_err());
        assert!(connect(r#"{"type":"tcp","addr":"a\nb"}"#, "").is_err());
        assert!(connect(r#"{"type":"usb","serial":"a b"}"#, "").is_err());
        assert!(connect(r#"{"type":"usb","serial":"R5CT.1-2:3"}"#, "").is_ok());
        for code in ["12345", "1234567", "12345a", "+12345", " 12345"] {
            assert!(
                parse(&format!(r#"{{"cmd":"pair","code":"{code}"}}"#)).is_err(),
                "{code}"
            );
        }
        for key in ["", "xy", &"a".repeat(65)] {
            assert!(
                parse(&format!(r#"{{"cmd":"remove_peer","key":"{key}"}}"#)).is_err(),
                "{key}"
            );
        }
        assert!(Request::parse(&vec![b' '; MAX_REQUEST + 1]).is_err());
    }

    #[tokio::test]
    async fn reads_bounded_lines() {
        let mut data: &[u8] = b"one\ntwo\nthree";
        assert_eq!(read_line(&mut data, 8).await.unwrap().unwrap(), b"one");
        assert_eq!(read_line(&mut data, 8).await.unwrap().unwrap(), b"two");
        assert!(
            read_line(&mut data, 8).await.is_err(),
            "no newline before the end"
        );
        let mut long: &[u8] = b"0123456789\n";
        assert!(read_line(&mut long, 8).await.is_err());
        let mut empty: &[u8] = b"";
        assert_eq!(read_line(&mut empty, 8).await.unwrap(), None);
    }

    #[test]
    fn stats_without_byte_counters_still_parse() {
        let old = br#"{"ok":true,"stats":{"streams":1,"max_streams":224,"reconnects":0,"uptime_secs":5}}"#;
        let stats = Response::parse(old).unwrap().stats.unwrap();
        assert_eq!((stats.bytes_up, stats.bytes_down), (0, 0));
    }

    #[test]
    fn responses_round_trip() {
        let r = Response {
            ok: true,
            status: Some(Status {
                state: State::Reconnecting,
                transport: Some("usb".into()),
                mode: Some(Mode::Tun),
                kill_switch: true,
                error: None,
                pairing: false,
            }),
            ..Response::default()
        };
        let line = r.line();
        assert_eq!(line.last(), Some(&b'\n'));
        assert_eq!(Response::parse(&line[..line.len() - 1]).unwrap(), r);
        let req = Request::Connect {
            transport: Transport::Tcp { addr: "a:1".into() },
            mode: Mode::Proxy,
            kill_switch: false,
            listen: None,
        };
        assert_eq!(Request::parse(&req.line()[..req.line().len() - 1]), Ok(req));
        assert!(!String::from_utf8(Response::ok().line())
            .unwrap()
            .contains("status"));
    }
}
