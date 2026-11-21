//! Windows: the adb server that ships with Kite. Some phones come with a USB driver that WinUSB
//! cannot replace (Samsung's, for one), so [`crate::usb`] cannot reach them, but adb can. When no
//! adb server runs and the USB layer says the driver is the problem, the caller starts Google's
//! `adb.exe` from the folder of the running `kite.exe` and goes through it.
//!
//! That folder is under Program Files, which only administrators can change; the service runs
//! this as SYSTEM, so nothing is ever started from `PATH`, the current directory or a user folder.
//! The installer checks the signature of the files, see `scripts/fetch-adb.ps1`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::time::{sleep, timeout};

/// Starts allowed in `WINDOW`, and the least time between two: a server that dies at once is
/// not started again in a loop.
const MAX_STARTS: u32 = 3;
const WINDOW: Duration = Duration::from_secs(10 * 60);
const GAP: Duration = Duration::from_secs(30);
/// Time `adb start-server` and then the first answer of the server may take.
const READY: Duration = Duration::from_secs(10);
const STOP: Duration = Duration::from_secs(5);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

struct Tries {
    count: u32,
    since: Option<Instant>,
    last: Option<Instant>,
}

impl Tries {
    const fn new() -> Self {
        Self {
            count: 0,
            since: None,
            last: None,
        }
    }

    /// Counts a start if one is allowed now.
    fn take(&mut self, now: Instant) -> bool {
        if self.since.is_none_or(|s| now.duration_since(s) > WINDOW) {
            (self.count, self.since) = (0, Some(now));
        }
        if self.count >= MAX_STARTS || self.last.is_some_and(|t| now.duration_since(t) < GAP) {
            return false;
        }
        self.count += 1;
        self.last = Some(now);
        true
    }
}

static TRIES: Mutex<Tries> = Mutex::new(Tries::new());
/// Whether this process started the server that runs now, and so stops it.
static STARTED: AtomicBool = AtomicBool::new(false);

/// Starts the bundled adb server and waits until it answers. False when it cannot be started
/// (too many tries, no adb next to `kite.exe`, or no answer in time).
pub async fn start() -> bool {
    let allowed = TRIES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take(Instant::now());
    let Some(adb) = allowed.then(adb_path).flatten() else {
        return false;
    };
    // Set before the wait: a server that comes up after a timeout is still ours to stop.
    STARTED.store(true, Ordering::Relaxed);
    if !run(&adb, "start-server", READY).await {
        return false;
    }
    let up = timeout(READY, async {
        while TcpStream::connect(crate::adb::SERVER).await.is_err() {
            sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .is_ok();
    if up {
        eprintln!("kite: started the adb server that comes with Kite");
    }
    up
}

/// Stops the server that [`start`] started, so that `adb.exe` is free to be replaced.
pub async fn stop() {
    if STARTED.swap(false, Ordering::Relaxed) {
        if let Some(adb) = adb_path() {
            run(&adb, "kill-server", STOP).await;
        }
    }
}

/// Runs `adb.exe arg` without a window; true if it exits with success in time.
async fn run(adb: &Path, arg: &str, wait: Duration) -> bool {
    let child = Command::new(adb)
        .arg(arg)
        // A port other than 5037 would be one nobody here asks.
        .env_remove("ANDROID_ADB_SERVER_PORT")
        .env_remove("ADB_SERVER_SOCKET")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .kill_on_drop(true)
        .spawn();
    match child {
        Ok(mut child) => matches!(timeout(wait, child.wait()).await, Ok(Ok(s)) if s.success()),
        Err(_) => false,
    }
}

/// `adb.exe` next to the running `kite.exe`, if that folder is under Program Files.
fn adb_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.to_str()?;
    let trusted = ["ProgramFiles", "ProgramW6432"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .any(|root| under(dir, &root));
    let adb = Path::new(dir).join("adb.exe");
    (trusted && adb.is_file()).then_some(adb)
}

/// Whether `dir` is `root` or inside it, ignoring case, and has no `..` in it.
fn under(dir: &str, root: &str) -> bool {
    let (dir, root) = (
        dir.to_lowercase(),
        root.trim_end_matches('\\').to_lowercase(),
    );
    !root.is_empty()
        && !dir.split('\\').any(|c| c == "..")
        && dir
            .strip_prefix(&root)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('\\'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_program_files_is_trusted() {
        let pf = r"C:\Program Files";
        assert!(under(r"C:\Program Files\Kite", pf));
        assert!(under(r"c:\program files\kite", r"C:\Program Files\"));
        assert!(!under(r"C:\Program Files (x86)\Kite", pf));
        assert!(!under(r"C:\Program FilesX\Kite", pf));
        assert!(!under(r"C:\Users\me\Kite", pf));
        assert!(!under(r"C:\Program Files\..\Users\me", pf));
        assert!(!under(r"C:\Program Files\Kite", ""));
        assert!(!under(r"\\?\C:\Program Files\Kite", pf));
    }

    #[test]
    fn starts_are_capped() {
        let t0 = Instant::now();
        let mut tries = Tries::new();
        assert!(tries.take(t0));
        // Not again at once, nor within the gap.
        assert!(!tries.take(t0 + Duration::from_secs(1)));
        assert!(!tries.take(t0 + GAP - Duration::from_secs(1)));
        assert!(tries.take(t0 + GAP));
        assert!(tries.take(t0 + GAP * 2));
        // Three in the window is the most.
        assert!(!tries.take(t0 + GAP * 3));
        // A new window starts after it has passed.
        assert!(tries.take(t0 + WINDOW + Duration::from_secs(1)));
    }
}
