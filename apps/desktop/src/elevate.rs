//! Pairing on Windows needs an administrator: the service refuses anyone else, because pairing
//! decides where all of the computer's traffic may go. So the Pair button opens an elevated
//! `kite pair` in a window of its own (Windows asks the user first), and the code is typed there.

use kite_ipc::Transport;

/// What the window says while that window is open.
pub const NOTE: &str =
    "Windows asks for permission to continue: kite pair runs as an administrator. \
    In the window that opens, allow this computer on the phone, then type the 6-digit code there.";

/// The command line of `kite pair` for `transport`, or None when it holds a character that must
/// not reach a command line. The phone's address and serial are the only text from outside.
pub fn pair_args(transport: &Transport) -> Option<String> {
    let plain = |s: &str| {
        !s.is_empty()
            && s.len() <= 255
            && !s.starts_with('-')
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "._:-[]%".contains(c))
    };
    let target = match transport {
        Transport::Usb { serial: None } => "--usb".to_string(),
        Transport::Usb { serial: Some(s) } if plain(s) => format!("--usb --serial {s}"),
        Transport::Wifi { addr: None } => "--wifi".to_string(),
        Transport::Wifi { addr: Some(a) } if plain(a) => format!("--wifi {a}"),
        _ => return None,
    };
    Some(format!("pair {target} --pause"))
}

/// Starts the elevated `kite pair`, or says in words why not.
#[cfg(windows)]
pub fn start(transport: &Transport) -> Result<(), String> {
    let args = pair_args(transport)
        .ok_or("The phone's address has characters that cannot be passed on.")?;
    let exe = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .with_file_name("kite.exe");
    if !exe.is_file() {
        return Err("kite.exe is not next to this program; reinstall Kite.".into());
    }
    kite_win::elevate::run_as_admin(&exe, &args).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            "The Windows prompt was declined, so pairing did not start.".to_string()
        } else {
            e.to_string()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_pair_command_for_each_link() {
        let usb = |serial: Option<&str>| Transport::Usb {
            serial: serial.map(Into::into),
        };
        let wifi = |addr: Option<&str>| Transport::Wifi {
            addr: addr.map(Into::into),
        };
        assert_eq!(pair_args(&usb(None)).unwrap(), "pair --usb --pause");
        assert_eq!(
            pair_args(&usb(Some("R5CT1234"))).unwrap(),
            "pair --usb --serial R5CT1234 --pause"
        );
        assert_eq!(pair_args(&wifi(None)).unwrap(), "pair --wifi --pause");
        assert_eq!(
            pair_args(&wifi(Some("192.168.43.1:47810"))).unwrap(),
            "pair --wifi 192.168.43.1:47810 --pause"
        );
        assert_eq!(
            pair_args(&wifi(Some("[fd00::1]"))).unwrap(),
            "pair --wifi [fd00::1] --pause"
        );
    }

    #[test]
    fn refuses_text_that_could_change_the_command() {
        for bad in [
            "a b",
            "a\"b",
            "a&b",
            "a|b",
            "--tun",
            "-x",
            "",
            "a%PATH%b\n",
            "é",
        ] {
            let serial = Transport::Usb {
                serial: Some(bad.into()),
            };
            let addr = Transport::Wifi {
                addr: Some(bad.into()),
            };
            assert_eq!(pair_args(&serial), None, "{bad:?}");
            assert_eq!(pair_args(&addr), None, "{bad:?}");
        }
        assert_eq!(
            pair_args(&Transport::Tcp {
                addr: "10.0.0.1:7000".into()
            }),
            None
        );
    }
}
