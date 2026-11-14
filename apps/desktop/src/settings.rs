//! What the app remembers per user: the mode, the kill switch and how to reach the phone, in
//! `kite/desktop.json` in the user's config directory. The file is only a convenience: a missing or
//! damaged one gives the defaults, and nothing in it is used for anything but those choices.

use kite_ipc::Mode;
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

/// The file is read whole; a real one is a few dozen bytes.
const MAX_FILE: u64 = 4096;

/// Longest phone address, as the daemon takes it.
const MAX_ADDR: usize = 255;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Settings {
    pub mode: Mode,
    /// Block all traffic while the phone is away. Off until the user turns it on.
    pub kill_switch: bool,
    /// Reach the phone over WiFi instead of USB.
    pub wifi: bool,
    /// The phone's `HOST` or `HOST:PORT`; empty finds it with mDNS. Always passes [`clean_addr`].
    pub wifi_addr: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::Tun,
            kill_switch: false,
            wifi: false,
            wifi_addr: String::new(),
        }
    }
}

/// Whether there is a kill switch to turn on: not on Windows yet.
pub const KILL_SWITCH: bool = cfg!(not(windows));

/// The kill switch redials without a lookup, so the daemon takes only an IP address (with or
/// without a port) for it; a name is caught here, before the daemon has to refuse it.
pub fn for_kill_switch(addr: Option<String>) -> Result<Option<String>, &'static str> {
    match addr {
        Some(a) if a.parse::<SocketAddr>().is_err() && a.parse::<IpAddr>().is_err() => {
            Err("The kill switch needs the phone's IP address, not a name.")
        }
        other => Ok(other),
    }
}

/// What the user typed as the phone's address, ready to send: trimmed, `None` when empty. Refused
/// when it is too long or has a character that cannot be part of an address, which includes the
/// ones that make text read in another order.
pub fn clean_addr(text: &str) -> Result<Option<String>, &'static str> {
    let text = text.trim();
    if text.len() > MAX_ADDR {
        return Err("The address is too long.");
    }
    let hidden = |c: char| {
        c.is_control()
            || c.is_whitespace()
            || matches!(
                c,
                '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}'
            )
    };
    if text.chars().any(hidden) {
        return Err("The address has spaces or hidden characters. Remove them.");
    }
    Ok((!text.is_empty()).then(|| text.to_string()))
}

impl Settings {
    pub fn path() -> Option<PathBuf> {
        let base = if cfg!(windows) {
            std::env::var_os("APPDATA").map(PathBuf::from)
        } else {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .or_else(|| {
                    std::env::var_os("HOME")
                        .map(PathBuf::from)
                        .filter(|p| p.is_absolute())
                        .map(|h| h.join(".config"))
                })
        };
        base.map(|b| b.join("kite").join("desktop.json"))
    }

    pub fn load(path: &Path) -> Self {
        let read = || -> io::Result<Self> {
            let file = std::fs::File::open(path)?;
            if file.metadata()?.len() > MAX_FILE {
                return Err(io::ErrorKind::InvalidData.into());
            }
            // The length check does not hold for a pipe or a device, so the read is capped too.
            serde_json::from_reader(file.take(MAX_FILE)).map_err(io::Error::other)
        };
        let mut s = read().unwrap_or_default();
        // An address that would be refused on its way out means the file is not one of ours.
        match clean_addr(&s.wifi_addr) {
            Ok(addr) => {
                s.wifi_addr = addr.unwrap_or_default();
                // A file copied from another system may ask for what this one cannot do.
                s.kill_switch &= KILL_SWITCH;
                s
            }
            Err(_) => Self::default(),
        }
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
            kill_switch: KILL_SWITCH,
            wifi: true,
            wifi_addr: "192.168.1.20:47810".into(),
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
    fn a_kill_switch_this_system_lacks_is_dropped() {
        let d = dir("clamp");
        let path = d.join("kite").join("desktop.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"kill_switch":true}"#).unwrap();
        assert_eq!(Settings::load(&path).kill_switch, KILL_SWITCH);
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
            r#"{"wifi":"yes"}"#.into(),
            r#"{"wifi_addr":"10.0.\n0.2"}"#.into(),
            r#"{"wifi_addr":"a\u0007b"}"#.into(),
            r#"{"wifi_addr":"a\u202eb"}"#.into(),
            format!(r#"{{"wifi_addr":"{}"}}"#, "a".repeat(MAX_ADDR + 1)),
        ] {
            std::fs::write(&path, bad).unwrap();
            assert_eq!(Settings::load(&path), Settings::default());
        }
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_loaded_address_is_trimmed() {
        let d = dir("trim");
        std::fs::create_dir_all(&d).unwrap();
        let path = d.join("desktop.json");
        std::fs::write(&path, r#"{"wifi":true,"wifi_addr":"  phone.local "}"#).unwrap();
        let s = Settings::load(&path);
        assert!(s.wifi);
        assert_eq!(s.wifi_addr, "phone.local");
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn the_kill_switch_wants_an_ip() {
        for ok in [
            None,
            Some("10.0.0.2"),
            Some("10.0.0.2:5"),
            Some("[fe80::1]:5"),
        ] {
            let addr = ok.map(String::from);
            assert_eq!(for_kill_switch(addr.clone()), Ok(addr));
        }
        assert!(for_kill_switch(Some("phone.local".into())).is_err());
        assert!(for_kill_switch(Some("phone.local:47810".into())).is_err());
    }

    #[test]
    fn typed_addresses_are_cleaned_or_refused() {
        assert_eq!(clean_addr(""), Ok(None));
        assert_eq!(clean_addr("  \t "), Ok(None));
        assert_eq!(
            clean_addr(" 192.168.1.20 "),
            Ok(Some("192.168.1.20".into()))
        );
        assert_eq!(
            clean_addr("[fe80::1]:47810"),
            Ok(Some("[fe80::1]:47810".into()))
        );
        // The limit is the daemon's: 255 bytes, so 63 four-byte emoji pass and 64 do not.
        assert!(clean_addr(&"a".repeat(MAX_ADDR)).is_ok());
        assert!(clean_addr(&"a".repeat(MAX_ADDR + 1)).is_err());
        assert!(clean_addr(&"\u{1F4F1}".repeat(63)).is_ok());
        assert!(clean_addr(&"\u{1F4F1}".repeat(64)).is_err());
        for bad in [
            "10.0.0.2\n10.0.0.3",
            "10.0.0.\u{7}2",
            "10.0.0.2 :1",
            "\u{202E}2.0.0.01",
            "10.0.0.\u{200B}2",
            "10.0.0.\u{FEFF}2",
        ] {
            assert!(clean_addr(bad).is_err(), "{bad:?}");
        }
    }
}
