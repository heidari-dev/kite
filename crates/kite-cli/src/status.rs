//! `kite status`: the running tunnel answers on an owner-only socket in `$XDG_RUNTIME_DIR`.
//! The server only writes; it never reads a request, so there is nothing to parse.

use std::fs::Permissions;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::time::timeout;

use crate::proxy::MAX_STREAMS;

/// What `kite status` prints about a running tunnel.
pub struct Info {
    pub relay: String,
    pub mode: String,
    pub active: Arc<AtomicUsize>,
    pub started: Instant,
}

impl Info {
    fn text(&self) -> String {
        format!(
            "relay: {}\nmode: {}\nstreams: {} of {}\nuptime: {}s\n",
            self.relay,
            self.mode,
            self.active.load(Ordering::Relaxed),
            MAX_STREAMS,
            self.started.elapsed().as_secs(),
        )
    }
}

/// Removes the socket file when the tunnel ends.
pub struct SocketFile(pub PathBuf);

impl Drop for SocketFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn path() -> io::Result<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set"))?;
    // Other users must not be able to replace or pre-create the socket.
    if std::fs::metadata(&dir)?.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "XDG_RUNTIME_DIR is accessible to other users",
        ));
    }
    Ok(dir.join("kite.sock"))
}

/// Starts answering status requests in a background task.
pub fn serve(info: Info) -> io::Result<SocketFile> {
    let path = path()?;
    let listener = match UnixListener::bind(&path) {
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
            if std::os::unix::net::UnixStream::connect(&path).is_ok() {
                return Err(io::Error::other("another kite is already running"));
            }
            // Left behind by a tunnel that did not exit cleanly.
            std::fs::remove_file(&path)?;
            UnixListener::bind(&path)?
        }
        bound => bound?,
    };
    let file = SocketFile(path);
    std::fs::set_permissions(&file.0, Permissions::from_mode(0o600))?;
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((mut s, _)) => {
                    let _ =
                        timeout(Duration::from_secs(1), s.write_all(info.text().as_bytes())).await;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    });
    Ok(file)
}

/// Prints the status of the running tunnel.
pub async fn query() -> io::Result<()> {
    let mut s = match UnixStream::connect(path()?).await {
        Ok(s) => s,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Err(io::Error::new(e.kind(), "not connected"));
        }
        Err(e) => return Err(e),
    };
    let mut text = String::new();
    timeout(
        Duration::from_secs(2),
        (&mut s).take(4096).read_to_string(&mut text),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no answer from kite"))??;
    print!("{text}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn info() -> Info {
        Info {
            relay: "127.0.0.1:7000".into(),
            mode: "SOCKS5 proxy on 127.0.0.1:1080".into(),
            active: Arc::new(AtomicUsize::new(3)),
            started: Instant::now(),
        }
    }

    // The only test that sets XDG_RUNTIME_DIR.
    #[tokio::test]
    async fn serves_status_on_an_owner_only_socket() {
        let dir = std::env::temp_dir().join(format!("kite-status-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", &dir);
        assert!(serve(info()).is_err(), "accepted a shared directory");
        std::fs::set_permissions(&dir, Permissions::from_mode(0o700)).unwrap();
        let sock = dir.join("kite.sock");

        // A socket file left by a crashed run is replaced.
        drop(std::os::unix::net::UnixListener::bind(&sock).unwrap());
        let file = serve(info()).unwrap();
        assert_eq!(std::fs::metadata(&sock).unwrap().mode() & 0o777, 0o600);

        let mut text = String::new();
        let mut s = UnixStream::connect(&sock).await.unwrap();
        s.read_to_string(&mut text).await.unwrap();
        assert!(text.contains("streams: 3 of 224"), "{text}");
        assert!(
            text.contains("mode: SOCKS5 proxy on 127.0.0.1:1080"),
            "{text}"
        );

        let err = serve(info()).err().unwrap();
        assert!(err.to_string().contains("already running"), "{err}");

        drop(file);
        assert!(!sock.exists());
        let _ = std::fs::remove_dir(&dir);
    }
}
