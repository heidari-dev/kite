//! One request to the Kite daemon (protocol in `kite-ipc`), with errors that say what to do.
//!
//! Every answer is untrusted: one line of at most `MAX_RESPONSE` bytes, parsed strictly, and the
//! wait for it has a timeout.

#[cfg(unix)]
use kite_ipc::{read_line, MAX_RESPONSE};
use kite_ipc::{Request, Response};
#[cfg(unix)]
use std::io;
use std::path::Path;
use std::time::Duration;

pub const NOT_RUNNING: &str =
    "The Kite daemon is not running. Start it with: sudo systemctl start kite";
pub const NO_PERMISSION: &str = "You are not allowed to use the Kite daemon. Add yourself to the \
     kite group with: sudo usermod -aG kite $USER, then log out and in again.";

/// Why a request did not give an answer.
#[derive(Debug, PartialEq, Eq)]
pub enum CallError {
    /// The daemon cannot be used at all; the text says what to do.
    Unreachable(String),
    /// The daemon answered and refused.
    Refused(String),
}

/// Sends one request and waits at most `wait` for the answer.
#[cfg(unix)]
pub async fn call(socket: &Path, req: &Request, wait: Duration) -> Result<Response, CallError> {
    use tokio::io::{AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;
    let exchange = async {
        let mut s = UnixStream::connect(socket).await?;
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

/// shortcut: the daemon only exists on Linux today; a named pipe client comes with the Windows
/// phase.
#[cfg(not(unix))]
pub async fn call(_: &Path, _: &Request, _: Duration) -> Result<Response, CallError> {
    Err(CallError::Unreachable(
        "The Kite daemon is not available on this system yet.".into(),
    ))
}

#[cfg(unix)]
fn unreachable(e: io::Error) -> CallError {
    CallError::Unreachable(match e.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => NOT_RUNNING.into(),
        io::ErrorKind::PermissionDenied => NO_PERMISSION.into(),
        _ => format!("Cannot talk to the Kite daemon: {e}"),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use kite_ipc::{State, Status};
    use std::path::PathBuf;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    const WAIT: Duration = Duration::from_secs(2);

    /// A socket in a fresh directory that answers each connection with the next canned reply
    /// and hands the request line it got to the test.
    fn fake(
        name: &str,
        replies: Vec<Vec<u8>>,
    ) -> (PathBuf, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let dir = std::env::temp_dir().join(format!("kite-desktop-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("kite.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (seen, requests) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            for reply in replies {
                let Ok((s, _)) = listener.accept().await else {
                    return;
                };
                let (r, mut w) = s.into_split();
                let mut line = String::new();
                BufReader::new(r).read_line(&mut line).await.unwrap();
                let _ = seen.send(line);
                let _ = w.write_all(&reply).await;
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
        let path = std::env::temp_dir().join("kite-desktop-nothing-here.sock");
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

    #[tokio::test(start_paused = true)]
    async fn a_silent_daemon_times_out() {
        let dir = std::env::temp_dir().join(format!("kite-desktop-silent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("kite.sock");
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let _held = listener.accept().await;
            std::future::pending::<()>().await;
        });
        let e = call(&path, &Request::Status {}, Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(matches!(e, CallError::Unreachable(m) if m.contains("in time")));
    }
}
