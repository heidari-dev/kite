//! Pairing on Linux. The daemon lets only root and its own user change the paired phones, so the
//! window cannot pair for a normal user. It shows the command to run in a terminal instead, and
//! copies it when asked.

use crate::elevate::pair_args;
use kite_ipc::Transport;

/// The `kite pair` command for `transport`, or None when the phone's address or serial holds a
/// character that must not reach a command line.
pub fn command(transport: &Transport) -> Option<String> {
    let args = pair_args(transport)?;
    let args = args.strip_suffix(" --pause")?;
    // `[` and `%` are allowed in an address; a shell would take `[fd00::1]` for a pattern.
    let args = args
        .split(' ')
        .map(|a| {
            if a.contains(['[', '%']) {
                format!("'{a}'")
            } else {
                a.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    Some(format!("sudo kite {args}"))
}

/// Puts `text` on the clipboard with whichever of the usual tools is installed. True if one took
/// it; there is no clipboard API in the toolkit and a library for one is not worth its weight.
#[cfg(target_os = "linux")]
pub fn copy(text: &str) -> bool {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let mut tools: Vec<(&str, &[&str])> = vec![
        ("/usr/bin/xclip", &["-selection", "clipboard"]),
        ("/usr/bin/xsel", &["--clipboard", "--input"]),
    ];
    if wayland {
        tools.insert(0, ("/usr/bin/wl-copy", &[]));
    } else {
        tools.push(("/usr/bin/wl-copy", &[]));
    }
    for (exe, args) in tools {
        let Ok(mut child) = Command::new(exe)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        let wrote = child
            .stdin
            .take()
            .is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok());
        // The tools usually hand the text to a copy that stays and exit at once; one that exits with
        // an error (no display of its kind) is not taken. One still running is serving the text.
        let mut failed = !wrote;
        for _ in 0..10 {
            match child.try_wait() {
                Ok(Some(status)) => {
                    failed |= !status.success();
                    break;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(30)),
                Err(_) => {
                    failed = true;
                    break;
                }
            }
        }
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        if !failed {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_is_the_one_a_terminal_runs() {
        let usb = Transport::Usb { serial: None };
        assert_eq!(command(&usb).unwrap(), "sudo kite pair --usb");
        let serial = Transport::Usb {
            serial: Some("R5CT1234".into()),
        };
        assert_eq!(
            command(&serial).unwrap(),
            "sudo kite pair --usb --serial R5CT1234"
        );
        let wifi = Transport::Wifi {
            addr: Some("192.168.43.1:47810".into()),
        };
        assert_eq!(
            command(&wifi).unwrap(),
            "sudo kite pair --wifi 192.168.43.1:47810"
        );
    }

    #[test]
    fn an_ipv6_address_is_quoted_for_the_shell() {
        let wifi = Transport::Wifi {
            addr: Some("[fd00::1]".into()),
        };
        assert_eq!(command(&wifi).unwrap(), "sudo kite pair --wifi '[fd00::1]'");
    }

    #[test]
    fn no_command_for_text_that_could_change_it() {
        let bad = Transport::Wifi {
            addr: Some("a b; rm -rf ~".into()),
        };
        assert_eq!(command(&bad), None);
    }
}
