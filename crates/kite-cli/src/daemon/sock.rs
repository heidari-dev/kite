//! The daemon's Unix socket: bound for the owner and the group only, and every caller's
//! credentials (SO_PEERCRED) checked again: root, the daemon's own user, or a member of the
//! daemon's group.

use std::io;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};
use tokio::time::timeout;

pub type Stream = UnixStream;
/// Asked whether the caller may change the paired phones.
pub type Admin = Box<dyn FnMut() -> bool + Send>;

pub struct Server {
    listener: UnixListener,
    me: Ids,
    /// Removes the socket file again when the daemon ends.
    _socket: crate::status::SocketFile,
}

impl Server {
    pub async fn bind(path: PathBuf) -> io::Result<Self> {
        let me = Ids::of_pid("self")?;
        let (listener, _socket) = bind(path).await?;
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
        let peer = caller.pid().and_then(|pid| Ids::of_pid(pid).ok());
        let Some(peer) = peer.filter(|p| p.uid == caller.uid() && p.may_talk_to(&self.me)) else {
            eprintln!("kite: refused a connection from outside root and the daemon's group");
            return Ok(None);
        };
        let admin = peer.may_admin(&self.me);
        Ok(Some((stream, Box::new(move || admin))))
    }
}

/// Binds the socket owner and group only, replacing one left by a daemon that did not exit
/// cleanly. A socket that answers belongs to a running daemon. The guard removes the file again.
async fn bind(path: PathBuf) -> io::Result<(UnixListener, crate::status::SocketFile)> {
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
    Ok((listener, guard))
}

/// User and groups of a process, from `/proc/PID/status`.
#[derive(Clone, Debug, PartialEq)]
struct Ids {
    uid: u32,
    gid: u32,
    groups: Vec<u32>,
}

impl Ids {
    fn of_pid(pid: impl std::fmt::Display) -> io::Result<Self> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/status"))?;
        Self::parse(&text).ok_or_else(|| io::Error::other("unreadable /proc status"))
    }

    /// The effective user and group and the supplementary groups.
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
    /// traffic of the computer, may go to, so the group does not get it.
    fn may_admin(&self, daemon: &Self) -> bool {
        self.uid == 0 || self.uid == daemon.uid
    }

    /// Root, the daemon's own user, or a member of the daemon's group.
    fn may_talk_to(&self, daemon: &Self) -> bool {
        self.uid == 0
            || self.uid == daemon.uid
            || self.gid == daemon.gid
            || self.groups.contains(&daemon.gid)
    }
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

    const STATUS: &str =
        "Name:\tkite\nUid:\t1000\t1001\t1000\t1001\nGid:\t200\t201\t200\t201\nGroups:\t10 20 30\n";

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
        assert!(!who(1000, 901, &[901]).may_admin(&me));
    }
}
