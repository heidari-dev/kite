//! The daemon's Unix socket: bound for the owner and the group only, and every caller's
//! credentials checked again: root, the daemon's own user, or a member of the daemon's group.
//! On Linux they come from SO_PEERCRED and `/proc/PID/status`. On macOS the daemon is root and
//! the group is `admin`: the caller's user (getpeereid) is looked up for its groups.

use std::io;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};
use tokio::time::timeout;

pub type Stream = UnixStream;
/// Asked whether the caller may change the paired phones.
pub type Admin = Box<dyn FnMut() -> bool + Send>;

/// Group `admin`: the users who may become root on macOS.
#[cfg(target_os = "macos")]
const ADMIN_GID: u32 = 80;
/// Longest wait for the directory service to name a caller's groups.
#[cfg(target_os = "macos")]
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(2);

pub struct Server {
    listener: UnixListener,
    me: Ids,
    /// Removes the socket file again when the daemon ends.
    _socket: crate::status::SocketFile,
}

impl Server {
    pub async fn bind(path: PathBuf) -> io::Result<Self> {
        let (listener, _socket) = bind(path).await?;
        let me = Ids::daemon(&_socket.0)?;
        Ok(Self {
            listener,
            me,
            _socket,
        })
    }

    /// The next client, or None when the system says it may not talk to the daemon. A caller whose
    /// credentials cannot be read or whose group cannot be established is refused (a /proc mounted
    /// with hidepid hides other users' processes, and so their groups).
    pub async fn accept(&mut self) -> io::Result<Option<(Stream, Admin)>> {
        let (stream, _) = self.listener.accept().await?;
        let Ok(caller) = stream.peer_cred() else {
            return Ok(None);
        };
        let peer = Ids::of_caller(&caller).await;
        let Some(peer) = peer.filter(|p| p.uid == caller.uid() && p.may_talk_to(&self.me)) else {
            log_refusal();
            return Ok(None);
        };
        let admin = peer.may_admin(&self.me);
        Ok(Some((stream, Box::new(move || admin))))
    }
}

/// Says that a caller was refused, at most once a minute: anyone can make the daemon refuse.
fn log_refusal() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let last = LAST.load(Ordering::Relaxed);
    if now.saturating_sub(last) >= 60
        && LAST
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        eprintln!("kite: refused a connection from outside root and the daemon's group");
    }
}

/// Binds the socket owner and group only, replacing one left by a daemon that did not exit
/// cleanly. A socket that answers belongs to a running daemon. The guard removes the file again.
async fn bind(path: PathBuf) -> io::Result<(UnixListener, crate::status::SocketFile)> {
    #[cfg(target_os = "macos")]
    private_dir(&path)?;
    let listener = match UnixListener::bind(&path) {
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
            let alive = timeout(Duration::from_secs(1), UnixStream::connect(&path)).await;
            if !matches!(alive, Ok(Err(_))) {
                return Err(io::Error::other("another kite daemon is already running"));
            }
            // Only ever a stale socket, whatever `--socket` points at.
            if !std::fs::symlink_metadata(&path)?.file_type().is_socket() {
                return Err(io::Error::other("the socket path is not a socket"));
            }
            std::fs::remove_file(&path)?;
            UnixListener::bind(&path)?
        }
        bound => bound?,
    };
    let guard = crate::status::SocketFile(path);
    // Until this, the umask decides; a socket needs write permission to be connected to, and the
    // usual umask gives that to the owner only.
    std::fs::set_permissions(&guard.0, std::fs::Permissions::from_mode(0o660))?;
    #[cfg(target_os = "macos")]
    if std::os::unix::fs::MetadataExt::uid(&std::fs::metadata(&guard.0)?) == 0 {
        std::os::unix::fs::chown(&guard.0, None, Some(ADMIN_GID))?;
    }
    Ok((listener, guard))
}

/// macOS has no systemd to make /var/run/kite, and /var/run is empty after a boot. A root daemon
/// makes the directory itself for the administrators (0750, group admin) if it is missing, and
/// never changes a directory that was already there.
#[cfg(target_os = "macos")]
fn private_dir(socket: &std::path::Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    let Some(dir) = socket.parent().filter(|d| !d.as_os_str().is_empty()) else {
        return Ok(());
    };
    // The test of an own-made directory is that this call is the one that made it.
    match std::fs::DirBuilder::new().mode(0o750).create(dir) {
        Ok(()) => {
            // The umask may have taken bits off.
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o750))?;
            if std::fs::metadata(dir)?.uid() == 0 {
                std::os::unix::fs::chown(dir, None, Some(ADMIN_GID))?;
            }
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        r => r,
    }
}

/// User and groups of a process.
#[derive(Clone, Debug, PartialEq)]
struct Ids {
    uid: u32,
    gid: u32,
    groups: Vec<u32>,
}

impl Ids {
    /// The daemon's own: Linux reads its process, on macOS it is the owner of the socket it made
    /// and the group `admin`.
    #[cfg(target_os = "linux")]
    fn daemon(_: &std::path::Path) -> io::Result<Self> {
        Self::of_pid("self")
    }

    #[cfg(target_os = "macos")]
    fn daemon(socket: &std::path::Path) -> io::Result<Self> {
        let meta = std::fs::metadata(socket)?;
        let uid = std::os::unix::fs::MetadataExt::uid(&meta);
        Ok(Self {
            uid,
            // A daemon that is not root (a test, a run by hand) has the group of its socket.
            gid: if uid == 0 {
                ADMIN_GID
            } else {
                std::os::unix::fs::MetadataExt::gid(&meta)
            },
            groups: Vec::new(),
        })
    }

    #[cfg(target_os = "linux")]
    async fn of_caller(caller: &tokio::net::unix::UCred) -> Option<Self> {
        Self::of_pid(caller.pid()?).ok()
    }

    /// The groups of the caller's user, from the directory service. That can block, so it runs on
    /// a thread of its own and under a timeout; a caller it cannot name is refused.
    #[cfg(target_os = "macos")]
    async fn of_caller(caller: &tokio::net::unix::UCred) -> Option<Self> {
        let (uid, gid) = (caller.uid(), caller.gid());
        // Root and the daemon's user need no lookup. For the others one lookup runs at a time and a
        // caller that finds it busy is refused, so a flood cannot stall the accept loop.
        if uid == 0 {
            return Some(Self {
                uid,
                gid,
                groups: Vec::new(),
            });
        }
        static LOOKUP: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
        let _slot = LOOKUP.try_acquire().ok()?;
        let mut groups = timeout(LOOKUP_TIMEOUT, groups_of(uid)).await.ok()??;
        groups.push(gid);
        Some(Self { uid, gid, groups })
    }

    #[cfg(target_os = "linux")]
    fn of_pid(pid: impl std::fmt::Display) -> io::Result<Self> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/status"))?;
        Self::parse(&text).ok_or_else(|| io::Error::other("unreadable /proc status"))
    }

    /// The effective user and group and the supplementary groups.
    #[cfg(target_os = "linux")]
    fn parse(status: &str) -> Option<Self> {
        let field = |name: &str| status.lines().find_map(|l| l.strip_prefix(name));
        let effective = |name: &str| field(name)?.split_whitespace().nth(1)?.parse().ok();
        Some(Self {
            uid: effective("Uid:")?,
            gid: effective("Gid:")?,
            groups: field("Groups:")?
                .split_whitespace()
                .map(|g| g.parse().ok())
                .collect::<Option<_>>()?,
        })
    }

    /// Root or the daemon's own user. Pairing decides which phones the tunnel, and in TUN mode all
    /// traffic of the computer, may go to, so the group does not get it. On macOS the group is
    /// `admin`, whose members can become root anyway, and the daemon is root: they get it.
    fn may_admin(&self, daemon: &Self) -> bool {
        self.uid == 0
            || self.uid == daemon.uid
            || (cfg!(target_os = "macos") && daemon.uid == 0 && self.may_talk_to(daemon))
    }

    /// Root, the daemon's own user, or a member of the daemon's group.
    fn may_talk_to(&self, daemon: &Self) -> bool {
        self.uid == 0
            || self.uid == daemon.uid
            || self.gid == daemon.gid
            || self.groups.contains(&daemon.gid)
    }
}

/// The groups of user `uid`, or None when there is no such user or its groups cannot be read.
#[cfg(target_os = "macos")]
async fn groups_of(uid: u32) -> Option<Vec<u32>> {
    // opendirectoryd is the only thing that knows the groups, and libc has no safe way to ask it
    // here; `id` does, and takes a numeric user.
    let out = tokio::process::Command::new("/usr/bin/id")
        .args(["-G", &uid.to_string()])
        .env_clear()
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout)
        .ok()?
        .split_whitespace()
        .map(|g| g.parse().ok())
        .collect()
}

#[cfg(test)]
pub fn test_path(dir: &Path, _name: &str) -> PathBuf {
    dir.join("kite.sock")
}

/// The tests run as the daemon's own user, who may pair.
#[cfg(test)]
pub fn test_is_admin() -> bool {
    true
}

#[cfg(test)]
pub async fn connect(path: &Path) -> io::Result<UnixStream> {
    UnixStream::connect(path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    const STATUS: &str =
        "Name:\tkite\nUid:\t1000\t1001\t1000\t1001\nGid:\t200\t201\t200\t201\nGroups:\t10 20 30\n";

    #[cfg(target_os = "linux")]
    #[test]
    fn parses_ids_from_proc_status() {
        let ids = Ids::parse(STATUS).unwrap();
        assert_eq!((ids.uid, ids.gid), (1001, 201));
        assert_eq!(ids.groups, [10, 20, 30]);
        assert!(Ids::parse("Uid:\t1\n").is_none());
        assert!(Ids::parse(&STATUS.replace("30", "x")).is_none());
        let root = Ids::parse("Uid:\t0\t0\nGid:\t0\t0\nGroups:\t\n").unwrap();
        assert!(root.groups.is_empty());
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn names_the_groups_of_a_user() {
        assert!(groups_of(0).await.unwrap().contains(&0));
        assert!(groups_of(u32::MAX - 1).await.is_none());
    }

    #[test]
    fn only_root_the_daemon_and_its_group_may_talk_to_it() {
        let me = Ids {
            uid: 900,
            gid: 901,
            groups: vec![],
        };
        let who = |uid, gid, groups: &[u32]| Ids {
            uid,
            gid,
            groups: groups.to_vec(),
        };
        assert!(who(0, 0, &[]).may_talk_to(&me));
        assert!(who(900, 5, &[]).may_talk_to(&me));
        assert!(who(1000, 901, &[]).may_talk_to(&me));
        assert!(who(1000, 1000, &[4, 901]).may_talk_to(&me));
        assert!(!who(1000, 1000, &[4, 27]).may_talk_to(&me));
        // Changing the paired phones is for root and the daemon's user only.
        assert!(who(0, 0, &[]).may_admin(&me));
        assert!(who(900, 5, &[]).may_admin(&me));
        // Members of the group may change them on macOS only, where the daemon is root and the
        // group is `admin`.
        let root = Ids {
            uid: 0,
            ..me.clone()
        };
        assert_eq!(
            who(1000, 901, &[901]).may_admin(&root),
            cfg!(target_os = "macos")
        );
        assert!(!who(1000, 901, &[901]).may_admin(&me));
    }
}
