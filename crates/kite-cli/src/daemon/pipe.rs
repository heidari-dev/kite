//! The daemon's named pipe on Windows. Its DACL (see `kite-win`) lets in SYSTEM, the
//! administrators, the user a console daemon runs as, and the members of the local group `kite`;
//! remote clients are refused by the pipe itself. Whether a caller may change the paired phones
//! is asked of the system when a request needs it: only an elevated administrator or SYSTEM may.

use kite_win::pipe::{caller, Pipe};
use std::io;
use std::path::PathBuf;
use tokio::net::windows::named_pipe::NamedPipeServer;

pub type Stream = NamedPipeServer;
/// Asked whether the caller may change the paired phones.
pub type Admin = Box<dyn FnMut() -> bool + Send>;

const PREFIX: &str = r"\\.\pipe\";
/// `ERROR_ACCESS_DENIED`: what creating the first instance of a pipe that exists already gives.
const ACCESS_DENIED: i32 = 5;

pub struct Server {
    pipe: Pipe,
    /// The instance clients connect to next.
    next: NamedPipeServer,
}

impl Server {
    pub async fn bind(path: PathBuf) -> io::Result<Self> {
        let name = path
            .to_str()
            .filter(|n| n.starts_with(PREFIX) && n.len() > PREFIX.len())
            .ok_or_else(|| {
                io::Error::other(r"the socket must be a pipe name like \\.\pipe\kite")
            })?;
        let pipe = Pipe::new(name)?;
        // As the first instance it fails when anyone made a pipe of this name before: no one can
        // stand in for the daemon.
        let next = pipe.instance(true).map_err(|e| {
            if e.raw_os_error() == Some(ACCESS_DENIED) {
                io::Error::other(
                    "another kite daemon is already running, or the pipe name is taken",
                )
            } else {
                e
            }
        })?;
        Ok(Self { pipe, next })
    }

    /// The next client. The system has already decided whether it may open the pipe at all.
    pub async fn accept(&mut self) -> io::Result<Option<(Stream, Admin)>> {
        self.next.connect().await?;
        // The instance for the client after this one exists before this one is handed out. If it
        // cannot be made, `next` stays the connected one and the next call hands it out.
        let fresh = self.pipe.instance(false)?;
        let ready = std::mem::replace(&mut self.next, fresh);
        let caller = caller(&ready)?;
        Ok(Some((
            ready,
            Box::new(move || caller.is_admin().unwrap_or(false)),
        )))
    }
}

#[cfg(test)]
pub fn test_path(_dir: &std::path::Path, name: &str) -> PathBuf {
    format!(r"\\.\pipe\kite-daemon-{name}-{}", std::process::id()).into()
}

/// Whether the tests run elevated, which `net session` needs.
#[cfg(test)]
pub fn test_is_admin() -> bool {
    std::process::Command::new("net")
        .arg("session")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[cfg(test)]
pub async fn connect(
    path: &std::path::Path,
) -> io::Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    kite_win::pipe::connect(path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn only_pipe_names_are_taken() {
        for bad in [
            "kite",
            r"C:\kite",
            r"\\.\pipe\",
            r"\\server\pipe\kite",
            "/run/kite.sock",
        ] {
            let e = Server::bind(bad.into()).await.map(drop).unwrap_err();
            assert!(e.to_string().contains("pipe name"), "{bad}: {e}");
        }
    }
}
