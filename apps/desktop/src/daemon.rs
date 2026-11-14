//! One request to the Kite daemon (protocol in `kite-ipc`), over its Unix socket or, on Windows,
//! its named pipe, with errors that say what to do.
//!
//! Every answer is untrusted: one line of at most `MAX_RESPONSE` bytes, parsed strictly, and the
//! wait for it has a timeout.

use kite_ipc::{read_line, Request, Response, MAX_RESPONSE};
use std::io;
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};

#[cfg(unix)]
pub const NOT_RUNNING: &str =
    "The Kite daemon is not running. Start it with: sudo systemctl start kite";
#[cfg(unix)]
pub const NO_PERMISSION: &str = "You are not allowed to use the Kite daemon. Add yourself to the      kite group with: sudo usermod -aG kite $USER, then log out and in again.";
#[cfg(windows)]
pub const NOT_RUNNING: &str =
    "The Kite service is not running. Start it from Services (Kite), or reinstall Kite.";
#[cfg(windows)]
pub const NO_PERMISSION: &str = "You are not allowed to use the Kite service. Ask an administrator      to add you to the Windows group kite, then sign out and in again.";

/// Why a request did not give an answer.
#[derive(Debug, PartialEq, Eq)]
pub enum CallError {
    /// The daemon cannot be used at all; the text says what to do.
    Unreachable(String),
    /// The daemon answered and refused.
    Refused(String),
}

/// Opens the socket.
#[cfg(unix)]
async fn open(socket: &Path) -> io::Result<impl AsyncRead + AsyncWrite + Unpin> {
    tokio::net::UnixStream::connect(socket).await
}

/// Opens the pipe; while every instance is serving someone, tries again for a few seconds.
#[cfg(windows)]
async fn open(socket: &Path) -> io::Result<impl AsyncRead + AsyncWrite + Unpin> {
    use tokio::net::windows::named_pipe::ClientOptions;
    /// `ERROR_PIPE_BUSY`.
    const PIPE_BUSY: i32 = 231;
    let mut tries = 0;
    loop {
        match ClientOptions::new().open(socket) {
            Err(e) if e.raw_os_error() == Some(PIPE_BUSY) && tries < 50 => {
                tries += 1;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            r => return r,
        }
    }
}

/// Sends one request and waits at most `wait` for the answer.
pub async fn call(socket: &Path, req: &Request, wait: Duration) -> Result<Response, CallError> {
    use tokio::io::{AsyncWriteExt, BufReader};
    let exchange = async {
        let mut s = open(socket).await?;
        s.write_all(&req.line()).await?;
        let line = read_line(&mut BufReader::new(s), MAX_RESPONSE)
            .await?
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::UnexpectedEof, "closed without an answer")
            })?;
        Response::parse(&line)
    };
    let response = tokio::time::timeout(wait, exchange)
        .await
        .map_err(|_| CallError::Unreachable("The Kite daemon did not answer in time.".into()))?
        .map_err(unreachable)?;
    let response = clean(response);
    if response.ok {
        Ok(response)
    } else {
        Err(CallError::Refused(
            response
                .error
                .unwrap_or_else(|| "The daemon refused.".into()),
        ))
    }
}

/// Longest text shown from an answer, and most phones and keys kept.
const MAX_TEXT: usize = 256;
const MAX_LIST: usize = 32;

/// Makes text from an answer fit for a window: short, with no control or direction characters
/// that could make it read as something else.
fn text(s: String) -> String {
    let odd = |c: &char| {
        c.is_control()
            || ('\u{200e}'..='\u{200f}').contains(c)
            || ('\u{202a}'..='\u{202e}').contains(c)
            || ('\u{2066}'..='\u{2069}').contains(c)
    };
    if s.chars().count() <= MAX_TEXT && !s.chars().any(|c| odd(&c)) {
        return s;
    }
    s.chars().filter(|c| !odd(c)).take(MAX_TEXT).collect()
}

fn clean(mut r: Response) -> Response {
    r.error = r.error.map(text);
    r.fingerprint = r.fingerprint.map(text);
    if let Some(s) = &mut r.status {
        s.transport = s.transport.take().map(text);
        s.error = s.error.take().map(text);
    }
    if let Some(devices) = &mut r.devices {
        devices.truncate(MAX_LIST);
        for d in devices {
            d.serial = text(std::mem::take(&mut d.serial));
            d.state = text(std::mem::take(&mut d.state));
        }
    }
    if let Some(peers) = &mut r.peers {
        peers.truncate(MAX_LIST);
    }
    r
}

fn unreachable(e: io::Error) -> CallError {
    CallError::Unreachable(match e.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => NOT_RUNNING.into(),
        io::ErrorKind::PermissionDenied => NO_PERMISSION.into(),
        _ => format!("Cannot talk to the Kite daemon: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kite_ipc::{State, Status};
    use std::path::PathBuf;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    const WAIT: Duration = Duration::from_secs(2);

    /// Where a stand-in daemon listens, and a server for it that is not yet connected to.
    #[cfg(unix)]
    struct Place(tokio::net::UnixListener);
    #[cfg(windows)]
    struct Place(tokio::net::windows::named_pipe::NamedPipeServer, PathBuf);

    #[cfg(unix)]
    fn place(name: &str) -> (PathBuf, Place) {
        let dir = std::env::temp_dir().join(format!("kite-desktop-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("kite.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        (path, Place(listener))
    }

    #[cfg(windows)]
    fn place(name: &str) -> (PathBuf, Place) {
        use tokio::net::windows::named_pipe::ServerOptions;
        let name = format!(r"\\.\pipe\kite-desktop-{name}-{}", std::process::id());
        let path = PathBuf::from(name);
        let first = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&path)
            .unwrap();
        (path.clone(), Place(first, path))
    }

    impl Place {
        /// The next client, as a stream.
        #[cfg(unix)]
        async fn accept(&mut self) -> io::Result<tokio::net::UnixStream> {
            Ok(self.0.accept().await?.0)
        }

        /// The next client, as a stream; a fresh instance is ready for the one after.
        #[cfg(windows)]
        async fn accept(&mut self) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
            use tokio::net::windows::named_pipe::ServerOptions;
            self.0.connect().await?;
            let next = ServerOptions::new().create(&self.1)?;
            Ok(std::mem::replace(&mut self.0, next))
        }
    }

    /// A socket in a fresh directory that answers each connection with the next canned reply
    /// and hands the request line it got to the test.
    fn fake(
        name: &str,
        replies: Vec<Vec<u8>>,
    ) -> (PathBuf, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let (path, mut place) = place(name);
        let (seen, requests) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            for reply in replies {
                let Ok(s) = place.accept().await else {
                    return;
                };
                let (r, mut w) = tokio::io::split(s);
                let mut line = String::new();
                BufReader::new(r).read_line(&mut line).await.unwrap();
                let _ = seen.send(line);
                let _ = w.write_all(&reply).await;
                let _ = w.flush().await;
            }
        });
        (path, requests)
    }

    fn status_reply() -> Vec<u8> {
        Response {
            status: Some(Status {
                state: State::Connected,
                transport: Some("usb".into()),
                mode: None,
                kill_switch: false,
                error: None,
                pairing: false,
            }),
            ..Response::ok()
        }
        .line()
    }

    #[tokio::test]
    async fn sends_the_request_and_parses_the_answer() {
        let (path, mut seen) = fake("ok", vec![status_reply()]);
        let r = call(&path, &Request::Status {}, WAIT).await.unwrap();
        assert_eq!(r.status.unwrap().state, State::Connected);
        let line = seen.recv().await.unwrap();
        assert_eq!(
            Request::parse(line.trim_end().as_bytes()),
            Ok(Request::Status {})
        );
    }

    #[tokio::test]
    async fn a_refusal_keeps_the_daemons_words() {
        let (path, _) = fake("refused", vec![Response::err("already connected").line()]);
        let e = call(&path, &Request::Disconnect {}, WAIT)
            .await
            .unwrap_err();
        assert_eq!(e, CallError::Refused("already connected".into()));
    }

    #[tokio::test]
    async fn a_missing_socket_says_the_daemon_is_not_running() {
        #[cfg(unix)]
        let path = std::env::temp_dir().join("kite-desktop-nothing-here.sock");
        #[cfg(windows)]
        let path = PathBuf::from(r"\\.\pipe\kite-desktop-nothing-here");
        let e = call(&path, &Request::Status {}, WAIT).await.unwrap_err();
        assert_eq!(e, CallError::Unreachable(NOT_RUNNING.into()));
    }

    #[test]
    fn a_refused_connection_says_what_to_do() {
        let kind = |k| unreachable(io::Error::from(k));
        assert_eq!(
            kind(io::ErrorKind::PermissionDenied),
            CallError::Unreachable(NO_PERMISSION.into())
        );
        assert_eq!(
            kind(io::ErrorKind::ConnectionRefused),
            CallError::Unreachable(NOT_RUNNING.into())
        );
    }

    #[tokio::test]
    async fn garbage_oversized_and_empty_answers_are_errors() {
        let mut long = vec![b'a'; MAX_RESPONSE + 10];
        long.push(b'\n');
        let (path, _) = fake("bad", vec![b"not json\n".to_vec(), long, Vec::new()]);
        for _ in 0..3 {
            let e = call(&path, &Request::Status {}, WAIT).await.unwrap_err();
            assert!(matches!(e, CallError::Unreachable(_)), "{e:?}");
        }
    }

    #[test]
    fn text_from_the_daemon_is_short_and_plain() {
        assert_eq!(text("3F2A 91C0".into()), "3F2A 91C0");
        // A direction override and a newline would let a name pose as another.
        assert_eq!(text("ab\u{202e}cd\ne".into()), "abcde");
        assert_eq!(text("x".repeat(10_000)).len(), MAX_TEXT);
        let r = clean(Response {
            devices: Some(vec![
                kite_ipc::Device {
                    serial: "a".into(),
                    state: "device".into()
                };
                100
            ]),
            peers: Some(vec!["ab".into(); 100]),
            ..Response::ok()
        });
        assert_eq!(
            (r.devices.unwrap().len(), r.peers.unwrap().len()),
            (MAX_LIST, MAX_LIST)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_daemon_times_out() {
        let (path, mut place) = place("silent");
        tokio::spawn(async move {
            let _held = place.accept().await;
            std::future::pending::<()>().await;
        });
        let e = call(&path, &Request::Status {}, Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(matches!(e, CallError::Unreachable(m) if m.contains("in time")));
    }
}
