//! Some phones (Samsung's, for one) come with a Windows driver that WinUSB cannot use, so Kite
//! cannot reach their ADB interface itself, but adb can. When Android's platform-tools are
//! installed and no adb server runs, this starts one as the signed-in user before Kite needs the
//! phone; the daemon then goes through it. Elsewhere it does nothing.

#[cfg(windows)]
mod imp {
    use std::net::{SocketAddr, TcpStream};
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    const SERVER: SocketAddr =
        SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 5037);
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    /// A server that will not start is not tried again on every poll.
    const RETRY: Duration = Duration::from_secs(60);
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);

    fn running() -> bool {
        TcpStream::connect_timeout(&SERVER, Duration::from_millis(200)).is_ok()
    }

    /// Starts adb's server if it is installed and not running. True when one was started.
    pub fn start_server() -> bool {
        if running() {
            return false;
        }
        // An elevated Kite must not run a program that the user, or anything running as them,
        // can replace.
        if kite_win::is_elevated().unwrap_or(true) {
            return false;
        }
        {
            let mut last = LAST.lock().unwrap_or_else(|p| p.into_inner());
            if last.is_some_and(|t| t.elapsed() < RETRY) {
                return false;
            }
            *last = Some(Instant::now());
        }
        let Some(adb) = find() else { return false };
        Command::new(adb)
            .arg("start-server")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .is_ok()
    }

    /// As [`start_server`], then waits up to 10 s for the server to listen.
    pub async fn ensure_server() {
        if start_server() {
            for _ in 0..40 {
                tokio::time::sleep(Duration::from_millis(250)).await;
                if running() {
                    return;
                }
            }
        }
    }

    /// adb.exe on PATH, or where Android Studio and a plain platform-tools download put it.
    fn find() -> Option<PathBuf> {
        let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
            dirs.push(local.join(r"Android\Sdk\platform-tools"));
            dirs.push(local.join(r"Android\platform-tools"));
        }
        // A relative entry would be looked up in the current directory; a network share can
        // stall this check for seconds.
        dirs.into_iter()
            .filter(|d| d.is_absolute() && !d.as_os_str().to_string_lossy().starts_with(r"\\"))
            .map(|d| d.join("adb.exe"))
            .find(|p| p.is_file())
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn start_server() -> bool {
        false
    }

    pub async fn ensure_server() {}
}

pub use imp::{ensure_server, start_server};
