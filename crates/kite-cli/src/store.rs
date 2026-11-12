//! This computer's Noise key and the phones it has paired with, in the user's config directory:
//! `kite/key` (32 bytes) and `kite/phones` (one hex public key per line). Both are owner-only,
//! and a file that other users can read or write is refused.

use kite_relay::noise::{StaticKey, KEY_LEN};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// Far more than any real list of phones; the file is read whole.
const MAX_PHONES_FILE: u64 = 64 * 1024;

#[derive(Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// The store in the user's config directory, created if missing.
    pub fn open() -> io::Result<Self> {
        Self::at(config_dir()?.join("kite"))
    }

    /// The store in `dir`, created owner-only if missing.
    pub fn at(dir: PathBuf) -> io::Result<Self> {
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(fs::DirBuilder::new().recursive(true), 0o700)
            .create(&dir)?;
        #[cfg(not(unix))]
        fs::create_dir_all(&dir)?;
        // Another user who can write the directory could swap the files.
        #[cfg(unix)]
        if std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&dir)?.permissions()) & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} is open to other users; run chmod 700 on it",
                    dir.display()
                ),
            ));
        }
        Ok(Self { dir })
    }

    /// This computer's key, created on first use.
    pub fn key(&self) -> io::Result<StaticKey> {
        let path = self.dir.join("key");
        let mut file = match open_private(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let key = StaticKey::generate()?;
                // A hard link never replaces an existing file: if another kite created the key
                // first, use that one.
                let tmp = write_temp(&path, key.as_bytes())?;
                let linked = fs::hard_link(&tmp, &path);
                let _ = fs::remove_file(&tmp);
                return match linked {
                    Ok(()) => Ok(key),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => self.key(),
                    Err(e) => Err(e),
                };
            }
            Err(e) => return Err(e),
        };
        if file.metadata()?.len() != KEY_LEN as u64 {
            return Err(bad_file(&path));
        }
        let mut bytes = [0u8; KEY_LEN];
        file.read_exact(&mut bytes)?;
        StaticKey::from_bytes(&mut bytes)
    }

    /// Public keys of the paired phones.
    pub fn phones(&self) -> io::Result<Vec<[u8; KEY_LEN]>> {
        let path = self.dir.join("phones");
        let mut file = match open_private(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        if file.metadata()?.len() > MAX_PHONES_FILE {
            return Err(bad_file(&path));
        }
        let mut text = String::new();
        file.read_to_string(&mut text)
            .map_err(|_| bad_file(&path))?;
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| decode(l).ok_or_else(|| bad_file(&path)))
            .collect()
    }

    pub fn add_phone(&self, key: [u8; KEY_LEN]) -> io::Result<()> {
        let mut phones = self.phones()?;
        if !phones.contains(&key) {
            phones.push(key);
        }
        self.save(&phones)
    }

    /// Removes the one phone whose hex key starts with `prefix`.
    pub fn remove_phone(&self, prefix: &str) -> io::Result<[u8; KEY_LEN]> {
        let prefix = prefix.to_ascii_lowercase();
        let mut phones = self.phones()?;
        let found: Vec<_> = phones
            .iter()
            .filter(|k| hex(&k[..]).starts_with(&prefix))
            .copied()
            .collect();
        match found[..] {
            [key] if !prefix.is_empty() => {
                phones.retain(|k| *k != key);
                self.save(&phones)?;
                Ok(key)
            }
            [] => Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no paired phone has that key",
            )),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "that matches several phones; give more of the key",
            )),
        }
    }

    fn save(&self, phones: &[[u8; KEY_LEN]]) -> io::Result<()> {
        let text: String = phones.iter().map(|k| hex(&k[..]) + "\n").collect();
        write_private(&self.dir.join("phones"), text.as_bytes())
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn decode(s: &str) -> Option<[u8; KEY_LEN]> {
    // from_str_radix alone would take a leading '+'.
    if s.len() != 2 * KEY_LEN || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut key = [0u8; KEY_LEN];
    for (i, b) in key.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(key)
}

fn config_dir() -> io::Result<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    base.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no config directory: set HOME or XDG_CONFIG_HOME",
        )
    })
}

fn bad_file(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{} is damaged; remove it and pair again", path.display()),
    )
}

/// Opens a file for reading, unless other users can read or write it.
fn open_private(path: &Path) -> io::Result<fs::File> {
    let file = fs::File::open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if file.metadata()?.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} is open to other users; run chmod 600 on it",
                    path.display()
                ),
            ));
        }
    }
    // shortcut: on Windows the files rely on the per-user profile's ACL; check the ACL when the
    // Windows client gets a release.
    Ok(file)
}

/// Replaces `path` with `data` through an owner-only temporary file, so a crash leaves either
/// the old file or the new one.
// shortcut: two kite processes changing the phone list at once can lose one change; add a lock
// file if pairing ever runs from more than one process.
fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = write_temp(path, data)?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// Writes `data` to a new owner-only file next to `path`, named for this process.
fn write_temp(path: &Path, data: &[u8]) -> io::Result<PathBuf> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut file = opts.open(&tmp)?;
    file.write_all(data)?;
    file.sync_all()?;
    Ok(tmp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (Store, PathBuf) {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("kite-store-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        (Store::at(root.join("kite")).unwrap(), root)
    }

    #[test]
    fn key_is_created_once_and_reloaded() {
        let (store, root) = temp_store();
        let first = store.key().unwrap().public().unwrap();
        assert_eq!(store.key().unwrap().public().unwrap(), first);
        fs::write(store.dir.join("key"), b"short").unwrap();
        assert!(store.key().is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn phones_are_added_listed_and_removed() {
        let (store, root) = temp_store();
        assert!(store.phones().unwrap().is_empty());
        let (a, b) = ([0xab; KEY_LEN], [0xac; KEY_LEN]);
        store.add_phone(a).unwrap();
        store.add_phone(b).unwrap();
        store.add_phone(a).unwrap();
        assert_eq!(store.phones().unwrap(), vec![a, b]);
        let kind = |r: io::Result<[u8; KEY_LEN]>| r.unwrap_err().kind();
        assert_eq!(kind(store.remove_phone("a")), io::ErrorKind::InvalidInput);
        assert_eq!(kind(store.remove_phone("")), io::ErrorKind::InvalidInput);
        assert_eq!(kind(store.remove_phone("ff")), io::ErrorKind::NotFound);
        assert_eq!(store.remove_phone("ABAB").unwrap(), a);
        assert_eq!(store.phones().unwrap(), vec![b]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn damaged_phone_list_is_refused() {
        let (store, root) = temp_store();
        store.add_phone([1; KEY_LEN]).unwrap();
        let path = store.dir.join("phones");
        let plus = format!("+{}\n", &hex(&[1; KEY_LEN])[1..]);
        for bad in ["zz\n", plus.as_str()] {
            fs::write(&path, bad).unwrap();
            assert_eq!(
                store.phones().unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn files_are_owner_only_and_open_ones_are_refused() {
        use std::os::unix::fs::PermissionsExt;
        let (store, root) = temp_store();
        store.key().unwrap();
        store.add_phone([1; KEY_LEN]).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&store.dir), 0o700);
        for name in ["key", "phones"] {
            let path = store.dir.join(name);
            assert_eq!(mode(&path), 0o600, "{name}");
            for open in [0o640, 0o604, 0o620] {
                fs::set_permissions(&path, fs::Permissions::from_mode(open)).unwrap();
                let e = if name == "key" {
                    store.key().map(drop).unwrap_err()
                } else {
                    store.phones().map(drop).unwrap_err()
                };
                assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{name} {open:o}");
            }
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        fs::set_permissions(&store.dir, fs::Permissions::from_mode(0o770)).unwrap();
        let e = Store::at(store.dir.clone()).map(drop).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        fs::remove_dir_all(root).unwrap();
    }
}
