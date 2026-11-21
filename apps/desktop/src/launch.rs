//! A second launch of Kite asks the running copy to show its window and exits. The running copy
//! listens on a loopback port and writes the port next to the single-instance lock; a connection
//! to it is the whole message, so there is nothing to parse.

use crate::settings::Settings;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

const PORT_FILE: &str = "kite-desktop.port";

/// The user's runtime directory, or else their own config directory, never one that other users
/// share. The lock and the port file live here.
pub fn dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|d| d.is_absolute())
        .or_else(|| Settings::path()?.parent().map(PathBuf::from))
}

/// Starts listening and records the port in `dir`. Without it a second launch only says that Kite
/// is already running.
pub fn announce(dir: &Path) -> Option<TcpListener> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).ok()?;
    let port = listener.local_addr().ok()?.port();
    std::fs::write(dir.join(PORT_FILE), port.to_string()).ok()?;
    Some(listener)
}

/// Connects to the running copy named in `dir`. False when there is none to reach.
pub fn wake(dir: &Path) -> bool {
    let port = std::fs::read_to_string(dir.join(PORT_FILE))
        .ok()
        .and_then(|t| t.trim().parse::<u16>().ok());
    port.is_some_and(|port| {
        let at = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        TcpStream::connect_timeout(&at, Duration::from_secs(1)).is_ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kite-launch-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_second_launch_reaches_the_running_copy() {
        let dir = scratch("reach");
        let listener = announce(&dir).unwrap();
        assert!(wake(&dir));
        listener.accept().unwrap();
    }

    #[test]
    fn nothing_to_wake_without_a_listener() {
        let dir = scratch("none");
        assert!(!wake(&dir), "no port file");
        std::fs::write(dir.join(PORT_FILE), "not a port").unwrap();
        assert!(!wake(&dir), "garbage in the port file");
        // The copy that wrote the port is gone: the port refuses.
        drop(announce(&dir).unwrap());
        assert!(!wake(&dir));
    }
}
