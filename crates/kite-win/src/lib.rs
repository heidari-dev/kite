#![cfg(windows)]
//! Windows glue for Kite. This and the TUN glue are the only places outside the mobile bindings
//! that use `unsafe`; every block says why it is sound. The rest of the workspace calls the safe
//! functions here and keeps `#![forbid(unsafe_code)]`.
//!
//! - [`private_dir`]: a directory only the user, SYSTEM and Administrators can use, for keys.
//! - [`pipe`]: the daemon's named pipe, with a DACL, and a check for an administrator caller.
//! - [`route`]: the IP Helper calls TUN mode needs.

use std::ffi::c_void;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, ERROR_INSUFFICIENT_BUFFER, HANDLE};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    GetNamedSecurityInfoW, SetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    GetSecurityDescriptorDacl, GetTokenInformation, LookupAccountNameW, SidTypeAlias, TokenUser,
    ACL, DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

pub mod pipe;
pub mod route;
pub mod service;

pub const SYSTEM_SID: &str = "S-1-5-18";
pub const ADMINISTRATORS_SID: &str = "S-1-5-32-544";

/// `%ProgramData%\Kite`: what the daemon keeps (its key, the paired phones, the TUN lock and
/// state, its log). Call [`private_dir`] on it before use.
pub fn data_dir() -> std::path::PathBuf {
    std::env::var_os("ProgramData")
        .map_or_else(|| r"C:\ProgramData".into(), std::path::PathBuf::from)
        .join("Kite")
}

/// A UTF-16 string with the terminating zero the system calls expect.
fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}

fn check(code: u32) -> io::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code as i32))
    }
}

/// Memory the system allocated for us, freed with `LocalFree`.
struct Local(*mut c_void);

impl Drop for Local {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from a call documented to need `LocalFree`, and is freed
            // once, here.
            unsafe { LocalFree(self.0) };
        }
    }
}

/// The text of a zero-terminated UTF-16 string the system gave us.
///
/// # Safety
/// `p` must point to a readable, zero-terminated UTF-16 string.
unsafe fn from_wide(p: *const u16) -> String {
    let mut len = 0;
    // SAFETY: the caller promises the string ends in a zero, so every read is inside it.
    while unsafe { *p.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` elements were just read.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) })
}

/// A SID as the string form SDDL uses (`S-1-5-18`).
///
/// # Safety
/// `sid` must point to a valid SID.
unsafe fn sid_string(sid: PSID) -> io::Result<String> {
    let mut text = null_mut();
    // SAFETY: `sid` is valid by the caller's promise; `text` receives a LocalAlloc'ed string.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let text = Local(text.cast());
    // SAFETY: on success `text` is a zero-terminated UTF-16 string.
    Ok(unsafe { from_wide(text.0.cast()) })
}

/// The SID of the user this process runs as.
pub fn current_user_sid() -> io::Result<String> {
    let mut token: HANDLE = null_mut();
    // SAFETY: the pseudo handle from GetCurrentProcess is always valid; `token` is an out value.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let result = (|| {
        let mut need = 0u32;
        // SAFETY: a null buffer of length 0 only asks for the size.
        let first = unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut need) };
        if first != 0
            || io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
        {
            return Err(io::Error::last_os_error());
        }
        // u64 elements keep the TOKEN_USER the system writes here aligned.
        let mut buf = vec![0u64; (need as usize).div_ceil(8)];
        // SAFETY: `buf` is at least `need` bytes.
        let got =
            unsafe { GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), need, &mut need) };
        if got == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the system wrote a TOKEN_USER at the start of the aligned buffer, and its SID
        // points inside the same buffer, which is alive until the end of this block.
        unsafe { sid_string((*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid) }
    })();
    // SAFETY: `token` was opened above and is closed once.
    unsafe { CloseHandle(token) };
    result
}

/// Whether this process runs as SYSTEM, as a service does.
pub fn is_system() -> io::Result<bool> {
    Ok(current_user_sid()? == SYSTEM_SID)
}

/// The SID of the local group `name`, or None when there is no such local group. A user or a
/// domain group of that name does not count: only an administrator can create a local group.
pub fn local_group_sid(name: &str) -> io::Result<Option<String>> {
    let name = wide(name);
    let (mut sid_len, mut domain_len, mut kind) = (0u32, 0u32, 0);
    // SAFETY: null buffers of length 0 only ask for the sizes.
    let first = unsafe {
        LookupAccountNameW(
            null(),
            name.as_ptr(),
            null_mut(),
            &mut sid_len,
            null_mut(),
            &mut domain_len,
            &mut kind,
        )
    };
    if first != 0 || sid_len == 0 {
        return Ok(None); // ERROR_NONE_MAPPED and friends: no such account
    }
    let mut sid = vec![0u64; (sid_len as usize).div_ceil(8)];
    let mut domain = vec![0u16; domain_len as usize];
    // SAFETY: the buffers have the sizes the first call asked for.
    let ok = unsafe {
        LookupAccountNameW(
            null(),
            name.as_ptr(),
            sid.as_mut_ptr().cast(),
            &mut sid_len,
            domain.as_mut_ptr(),
            &mut domain_len,
            &mut kind,
        )
    };
    if ok == 0 || kind != SidTypeAlias {
        return Ok(None);
    }
    // SAFETY: LookupAccountNameW wrote a valid SID into `sid`.
    unsafe { sid_string(sid.as_mut_ptr().cast()) }.map(Some)
}

/// A security descriptor parsed from SDDL text.
struct Descriptor(Local);

impl Descriptor {
    fn parse(sddl: &str) -> io::Result<Self> {
        let sddl = wide(sddl);
        let mut sd: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: `sddl` is zero-terminated; `sd` receives a LocalAlloc'ed descriptor.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(Local(sd.cast())))
    }

    fn as_ptr(&self) -> *mut c_void {
        self.0 .0
    }
}

/// Creates `path` if it is missing and makes it usable by SYSTEM, Administrators and the current
/// user only, for the directory and everything created in it. Refuses a directory that somebody
/// else owns: its owner could change the permissions back.
pub fn private_dir(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)?;
    let me = current_user_sid()?;
    let path_w = wide(path);

    let mut owner: PSID = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `path_w` is zero-terminated; the owner SID points into `sd`, freed below.
    check(unsafe {
        GetNamedSecurityInfoW(
            path_w.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut sd,
        )
    })?;
    let _sd = Local(sd.cast());
    // SAFETY: the call above succeeded, so `owner` is a valid SID.
    let owner = unsafe { sid_string(owner) }?;
    if owner != me && owner != SYSTEM_SID && owner != ADMINISTRATORS_SID {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} belongs to another user; remove it", path.display()),
        ));
    }

    let acl = Descriptor::parse(&format!(
        "D:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{me})"
    ))?;
    let (mut present, mut defaulted) = (0, 0);
    let mut dacl: *mut ACL = null_mut();
    // SAFETY: `acl` is a valid descriptor; `dacl` will point into it.
    let got =
        unsafe { GetSecurityDescriptorDacl(acl.as_ptr(), &mut present, &mut dacl, &mut defaulted) };
    if got == 0 || present == 0 {
        return Err(io::Error::other("no DACL in the descriptor"));
    }
    // SAFETY: `path_w` is zero-terminated and `dacl` stays valid while `acl` lives.
    check(unsafe {
        SetNamedSecurityInfoW(
            path_w.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            dacl,
            null(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_the_current_user() {
        let me = current_user_sid().unwrap();
        assert!(me.starts_with("S-1-5-"), "{me}");
        assert_eq!(is_system().unwrap(), me == SYSTEM_SID);
    }

    #[test]
    fn finds_local_groups_and_nothing_else() {
        assert_eq!(
            local_group_sid("Administrators").unwrap().as_deref(),
            Some(ADMINISTRATORS_SID)
        );
        assert_eq!(local_group_sid("kite-no-such-group").unwrap(), None);
        // A user is not a group.
        let user = std::env::var("USERNAME").unwrap();
        assert_eq!(local_group_sid(&user).unwrap(), None);
    }

    #[test]
    fn a_private_dir_is_closed_to_others() {
        let dir = std::env::temp_dir().join(format!("kite-win-private-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        private_dir(&dir).unwrap();
        // Twice: replacing the permissions is repeatable.
        private_dir(&dir).unwrap();
        std::fs::write(dir.join("key"), b"x").unwrap();
        let out = std::process::Command::new("icacls").arg(&dir).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        // Users, Everyone and Authenticated Users must not appear in the list.
        for other in ["BUILTIN\\Users", "Everyone", "NT AUTHORITY\\Authenticated Users"] {
            assert!(!text.contains(other), "{other} in {text}");
        }
        assert!(text.contains("NT AUTHORITY\\SYSTEM"), "{text}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
