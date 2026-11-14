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
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, ERROR_INSUFFICIENT_BUFFER, HANDLE};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    GetSecurityDescriptorDacl, GetTokenInformation, LookupAccountNameW, SidTypeAlias, SidTypeUser,
    TokenUser, ACL, DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    READ_CONTROL, WRITE_DAC,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

pub mod elevate;
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
        let got = unsafe {
            GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), need, &mut need)
        };
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
    account_sid(name, SidTypeAlias)
}

/// The SID of the user `name` (as the system writes it: with or without its domain), or None when there is no such user.
pub fn user_sid(name: &str) -> io::Result<Option<String>> {
    account_sid(name, SidTypeUser)
}

fn account_sid(name: &str, wanted: i32) -> io::Result<Option<String>> {
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
    if ok == 0 || kind != wanted {
        return Ok(None);
    }
    // SAFETY: LookupAccountNameW wrote a valid SID into `sid`.
    unsafe { sid_string(sid.as_mut_ptr().cast()) }.map(Some)
}

/// A user SID in the form SDDL takes: `S-1-` and digits and dashes, nothing else.
pub fn is_sid_text(s: &str) -> bool {
    s.len() <= 184
        && s.strip_prefix("S-1-").is_some_and(|rest| {
            !rest.is_empty()
                && rest.bytes().all(|b| b.is_ascii_digit() || b == b'-')
                && !rest.starts_with('-')
                && !rest.ends_with('-')
                && !rest.contains("--")
        })
}

/// At most this many users in the file.
const MAX_USERS: usize = 64;

/// The users in `users` in the data directory: one SID per line. A missing file is nobody; a line
/// that is not a valid SID is skipped (a byte order mark is ignored), so the file can put nothing
/// but SIDs into the pipe's access list. Any other trouble reading it is an error.
fn read_users() -> io::Result<Vec<String>> {
    let mut file = match std::fs::File::open(data_dir().join("users")) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    // The directory is closed to others once `private_dir` has run, but it is made with the
    // permissions of ProgramData first: a file somebody else made in that moment is theirs to
    // change later, so it does not count.
    if !trusted_owner(file.as_raw_handle(), &current_user_sid()?)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the list of allowed users belongs to another user; remove it",
        ));
    }
    let mut text = String::new();
    std::io::Read::read_to_string(&mut std::io::Read::take(&mut file, 64 * 1024), &mut text)?;
    Ok(text
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        // A SID of the right shape that Windows does not accept would stop the pipe being made.
        .filter(|l| is_sid_text(l) && Descriptor::parse(&format!("D:(A;;GA;;;{l})")).is_ok())
        .take(MAX_USERS)
        .map(String::from)
        .collect())
}

/// The users the installer or `kite allow` let use the daemon, besides the group `kite`. A file
/// that cannot be read means nobody, said on stderr.
pub fn allowed_users() -> Vec<String> {
    read_users().unwrap_or_else(|e| {
        eprintln!("kite: cannot read the list of allowed users: {e}");
        Vec::new()
    })
}

/// Lets the user with `sid` use the daemon (from the next start of the service). Needs the
/// rights to write the data directory, which are the administrators'.
pub fn allow_user(sid: &str) -> io::Result<()> {
    if !is_sid_text(sid) || Descriptor::parse(&format!("D:(A;;GA;;;{sid})")).is_err() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a SID"));
    }
    let dir = data_dir();
    private_dir(&dir)?;
    // An unreadable list is an error here: writing it again would lose the others.
    let mut users = read_users()?;
    if !users.iter().any(|u| u == sid) {
        if users.len() >= MAX_USERS {
            return Err(io::Error::other(format!(
                "{MAX_USERS} users are allowed already"
            )));
        }
        users.push(sid.to_string());
    }
    let text: String = users.iter().map(|u| format!("{u}\n")).collect();
    let tmp = dir.join(format!("users.tmp{}", std::process::id()));
    let written = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        std::io::Write::write_all(&mut f, text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, dir.join("users"))
    })();
    written.inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
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

/// Whether the file or directory behind `handle` is owned by SYSTEM, the Administrators or `me`.
/// Its owner can change who may use it, so anyone else's is not to be trusted.
fn trusted_owner(handle: HANDLE, me: &str) -> io::Result<bool> {
    let mut owner: PSID = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: the caller's handle is open for this call; the owner SID points into `sd`, freed
    // below.
    check(unsafe {
        GetSecurityInfo(
            handle,
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
    Ok(owner == me || owner == SYSTEM_SID || owner == ADMINISTRATORS_SID)
}

/// Creates `path` if it is missing and makes it usable by SYSTEM, Administrators and the current
/// user only, for what is created in it from now on. Refuses a directory that somebody else owns
/// (its owner could change the permissions back) and one that is a junction or a symbolic link.
/// The directory is opened once, without following links, and the checks and the new permissions
/// go through that one handle, so nothing can be swapped in between.
pub fn private_dir(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)?;
    let me = current_user_sid()?;
    let dir = std::fs::OpenOptions::new()
        .access_mode(READ_CONTROL | WRITE_DAC)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    if dir.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is a link; remove it", path.display()),
        ));
    }
    let handle = dir.as_raw_handle();

    if !trusted_owner(handle, &me)? {
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
    // SAFETY: `handle` is open (see above) and `dacl` stays valid while `acl` lives.
    check(unsafe {
        SetSecurityInfo(
            handle,
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
        let out = std::process::Command::new("icacls")
            .arg(&dir)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        // Users, Everyone and Authenticated Users must not appear in the list.
        for other in [
            "BUILTIN\\Users",
            "Everyone",
            "NT AUTHORITY\\Authenticated Users",
        ] {
            assert!(!text.contains(other), "{other} in {text}");
        }
        assert!(text.contains("NT AUTHORITY\\SYSTEM"), "{text}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_sids_are_sids() {
        for good in ["S-1-5-18", "S-1-5-21-1-2-3-1001", "S-1-5-32-544"] {
            assert!(is_sid_text(good), "{good}");
        }
        for bad in [
            "",
            "S-1-",
            "S-1-5-",
            "S-1--5",
            "x",
            "S-1-5-18)(A;;GA;;;WD",
            "S-1-5-18 ",
            "s-1-5-18",
            "S-2-5-18",
            &format!("S-1-{}", "1".repeat(200)),
        ] {
            assert!(!is_sid_text(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_sid_of_the_right_shape_that_windows_refuses_is_not_a_user() {
        // Too many sub-authorities, and a number that does not fit.
        for bad in [
            "S-1-5-1-2-3-4-5-6-7-8-9-10-11-12-13-14-15-16",
            "S-1-99999999999999999999",
        ] {
            assert!(is_sid_text(bad));
            assert!(allow_user(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn finds_the_user_but_not_a_group_or_a_stranger() {
        let user = std::env::var("USERNAME").unwrap();
        assert_eq!(user_sid(&user).unwrap(), Some(current_user_sid().unwrap()));
        assert_eq!(user_sid("Administrators").unwrap(), None);
        assert_eq!(user_sid("kite-no-such-user").unwrap(), None);
    }

    #[test]
    fn a_junction_is_refused() {
        let base = std::env::temp_dir().join(format!("kite-win-junction-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let target = base.join("target");
        std::fs::create_dir_all(&target).unwrap();
        let link = base.join("link");
        let made = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .unwrap();
        assert!(made.status.success(), "{made:?}");
        let e = private_dir(&link).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");
        std::fs::remove_dir_all(&base).unwrap();
    }
}
