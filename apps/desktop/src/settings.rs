//! What the app remembers per user: the mode and the kill switch, in `kite/desktop.json` in the
//! user's config directory. The file is only a convenience: a missing or damaged one gives the
//! defaults, and nothing in it is used for anything but those two choices.

use kite_ipc::Mode;
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The file is read whole; a real one is a few dozen bytes.
const MAX_FILE: u64 = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Settings {
    pub mode: Mode,
    /// Block all traffic while the phone is away. Off until the user turns it on.
    pub kill_switch: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::Tun,
            kill_switch: false,
        }
    }
}

impl Settings {
    pub fn path() -> Option<PathBuf> {
        let base = if cfg!(windows) {
            std::env::var_os("APPDATA").map(PathBuf::from)
        } else {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        };
        base.map(|b| b.join("kite").join("desktop.json"))
    }

    pub fn load(path: &Path) -> Self {
        let read = || -> io::Result<Self> {
            let file = std::fs::File::open(path)?;
            if file.metadata()?.len() > MAX_FILE {
                return Err(io::ErrorKind::InvalidData.into());
            }
            serde_json::from_reader(file).map_err(io::Error::other)
        };
        read().unwrap_or_default()
    }

    /// Replaces the file through a temporary one, so a crash leaves the old or the new.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let dir = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(std::fs::DirBuilder::new().recursive(true), 0o700)
            .create(dir)?;
        #[cfg(not(unix))]
        std::fs::create_dir_all(dir)?;
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut file = opts.open(&tmp)?;
        file.write_all(&serde_json::to_vec(self).map_err(io::Error::other)?)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kite-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn defaults_to_the_kill_switch_off() {
        let d = dir("default");
        let s = Settings::load(&d.join("kite").join("desktop.json"));
        assert_eq!(s, Settings::default());
        assert!(!s.kill_switch);
    }

    #[test]
    fn remembers_what_was_saved() {
        let d = dir("save");
        let path = d.join("kite").join("desktop.json");
        let s = Settings {
            mode: Mode::Proxy,
            kill_switch: true,
        };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o600);
            assert_eq!(mode(path.parent().unwrap()), 0o700);
        }
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_damaged_file_gives_the_defaults() {
        let d = dir("damaged");
        std::fs::create_dir_all(&d).unwrap();
        let path = d.join("desktop.json");
        for bad in [
            "not json".to_string(),
            r#"{"kill_switch":"yes"}"#.into(),
            r#"{"path":"/etc"}"#.into(),
            " ".repeat(MAX_FILE as usize + 1),
        ] {
            std::fs::write(&path, bad).unwrap();
            assert_eq!(Settings::load(&path), Settings::default());
        }
        std::fs::remove_dir_all(d).unwrap();
    }
}
