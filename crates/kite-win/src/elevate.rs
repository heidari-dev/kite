//! Starting a program as an administrator: the system's own prompt (UAC) asks the user.

use std::io;
use std::path::Path;
use std::ptr::{null, null_mut};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// `SE_ERR_ACCESSDENIED`: what ShellExecute answers when the user declines the prompt.
const DECLINED: usize = 5;

/// Starts `exe` with the command line `args` as an administrator. The system shows its consent
/// prompt; this returns when the program has been started, not when it ends. A prompt the user
/// declined is a `PermissionDenied` error. Nothing from `args` is interpreted by a shell, but
/// the program splits it on spaces and quotes, so callers pass only text they have checked.
pub fn run_as_admin(exe: &Path, args: &str) -> io::Result<()> {
    let (verb, file, args) = (crate::wide("runas"), crate::wide(exe), crate::wide(args));
    // SAFETY: all three strings are zero-terminated and outlive the call; no window, no
    // directory.
    let result = unsafe {
        ShellExecuteW(
            null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            args.as_ptr(),
            null(),
            SW_SHOWNORMAL,
        )
    } as usize;
    match result {
        // Above 32 the call worked; the value is an instance handle.
        33.. => Ok(()),
        DECLINED => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the Windows prompt was declined",
        )),
        code => Err(io::Error::other(format!(
            "Windows could not start the program (code {code})"
        ))),
    }
}
