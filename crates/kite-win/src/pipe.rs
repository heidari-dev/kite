//! The daemon's named pipe: who may open it, and whether the caller on the other end is an
//! administrator.
//!
//! The pipe's DACL grants SYSTEM and Administrators everything, the user a console daemon runs as
//! (never SYSTEM, which is the service) everything, and the members of the local group `kite`
//! read and write, without the right to create another instance. `FILE_CREATE_PIPE_INSTANCE` is
//! the same bit as `FILE_APPEND_DATA`, and a member who could open a second instance could answer
//! the other members' requests. Remote clients are refused by the pipe itself.

use crate::{Descriptor, SYSTEM_SID};
use std::io;
use std::os::windows::io::{AsHandle, AsRawHandle, OwnedHandle};
use std::ptr::null_mut;
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, PipeMode, ServerOptions,
};
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Security::{
    CheckTokenMembership, CreateWellKnownSid, RevertToSelf, WinBuiltinAdministratorsSid,
    SECURITY_ATTRIBUTES,
};
use windows_sys::Win32::System::Pipes::ImpersonateNamedPipeClient;

/// The local group whose members may use the daemon.
pub const GROUP: &str = "kite";
/// Read and write data and attributes, no `FILE_APPEND_DATA` (see the module comment).
const MEMBER_RIGHTS: &str = "0x12019b";
/// One listening instance plus the clients being served; the daemon serves at most 8.
const MAX_INSTANCES: usize = 16;
/// Request and answer lines are a few hundred bytes.
const BUFFER: u32 = 4096;

/// A pipe name with the access rules worked out for it.
pub struct Pipe {
    name: String,
    sddl: String,
}

impl Pipe {
    /// Works out the DACL for `name` (`\\.\pipe\...`). The group `kite` is looked up now.
    pub fn new(name: impl Into<String>) -> io::Result<Self> {
        // Named with this computer: a bare name would be looked up in the domain as well, where
        // someone else may have made a group of that name.
        let group = match std::env::var("COMPUTERNAME") {
            Ok(computer) => crate::local_group_sid(&format!("{computer}\\{GROUP}"))?,
            Err(_) => None,
        };
        let mut sddl = access(&crate::current_user_sid()?, group.as_deref());
        for sid in crate::allowed_users() {
            sddl += &format!("(A;;{MEMBER_RIGHTS};;;{sid})");
        }
        Descriptor::parse(&sddl)?;
        Ok(Self {
            name: name.into(),
            sddl,
        })
    }

    /// Creates one instance of the pipe. `first` must be true for the first one: it then fails
    /// when anybody else already made a pipe of this name, so nobody can stand in for the daemon.
    pub fn instance(&self, first: bool) -> io::Result<NamedPipeServer> {
        let sd = Descriptor::parse(&self.sddl)?;
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.as_ptr(),
            bInheritHandle: 0,
        };
        let mut options = ServerOptions::new();
        options
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .pipe_mode(PipeMode::Byte)
            .max_instances(MAX_INSTANCES)
            .in_buffer_size(BUFFER)
            .out_buffer_size(BUFFER);
        // SAFETY: `attributes` is a valid SECURITY_ATTRIBUTES whose descriptor (`sd`) outlives the
        // call; the system copies both while creating the pipe.
        unsafe {
            options.create_with_security_attributes_raw(
                &self.name,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
            )
        }
    }
}

/// `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`: the server may learn who the client is but
/// not act as it. Without it a program that made the pipe before the service did could act as an
/// administrator who runs `kite`.
const IDENTIFICATION: u32 = 0x0010_0000 | 0x0001_0000;
/// `ERROR_PIPE_BUSY`: every instance serves someone; one is free again in a moment.
const PIPE_BUSY: i32 = 231;

/// Opens the daemon's pipe as a client, waiting up to five seconds while it is busy, and
/// without letting the server impersonate the caller.
pub async fn connect(path: &std::path::Path) -> io::Result<NamedPipeClient> {
    let mut tries = 0;
    loop {
        match ClientOptions::new()
            .security_qos_flags(IDENTIFICATION)
            .open(path)
        {
            Err(e) if e.raw_os_error() == Some(PIPE_BUSY) && tries < 50 => {
                tries += 1;
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            r => return r,
        }
    }
}

/// The SDDL for a pipe: `me` is the user this process runs as, `group` the SID of the local group.
fn access(me: &str, group: Option<&str>) -> String {
    let mut sddl = String::from("D:P(A;;GA;;;SY)(A;;GA;;;BA)");
    if me != SYSTEM_SID {
        sddl += &format!("(A;;GA;;;{me})");
    }
    if let Some(group) = group {
        sddl += &format!("(A;;{MEMBER_RIGHTS};;;{group})");
    }
    sddl
}

/// The client of a connected pipe instance, kept to be asked about later: its own handle to the
/// instance, so it stays valid whatever happens to the one the connection reads and writes.
pub struct Caller(OwnedHandle);

/// Remembers who is connected to `pipe`.
pub fn caller(pipe: &NamedPipeServer) -> io::Result<Caller> {
    Ok(Caller(pipe.as_handle().try_clone_to_owned()?))
}

impl Caller {
    /// Whether the process on the other end runs as an administrator (an elevated one: a filtered
    /// token of an administrator does not count) or as SYSTEM. The server must have read from the
    /// pipe already, or the system refuses to say who the client is.
    pub fn is_admin(&self) -> io::Result<bool> {
        let handle = self.0.as_raw_handle() as usize;
        // On a thread of its own: it impersonates the client, and a thread that ends takes the
        // impersonation with it, so it can never leak into the daemon's own work.
        std::thread::scope(|s| {
            s.spawn(move || {
                // SAFETY: `self` owns the handle, and the scope joins this thread before it ends.
                unsafe { administrator_on_this_thread(handle as HANDLE) }
            })
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("the caller check failed")))
        })
    }
}

/// # Safety
/// `pipe` must be the open server end of a named pipe.
unsafe fn administrator_on_this_thread(pipe: HANDLE) -> io::Result<bool> {
    // u64 elements keep the SID aligned; 72 bytes is more than any SID of this kind needs.
    let mut sid = [0u64; 9];
    let mut len = std::mem::size_of_val(&sid) as u32;
    // SAFETY: `sid` has `len` bytes.
    if unsafe {
        CreateWellKnownSid(
            WinBuiltinAdministratorsSid,
            null_mut(),
            sid.as_mut_ptr().cast(),
            &mut len,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `pipe` is open by the caller's promise.
    if unsafe { ImpersonateNamedPipeClient(pipe) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut member = 0;
    // SAFETY: a null token means the impersonation token of this thread; `sid` is a valid SID.
    let ok = unsafe { CheckTokenMembership(null_mut(), sid.as_mut_ptr().cast(), &mut member) };
    let error = io::Error::last_os_error();
    // SAFETY: no arguments; ends the impersonation that started above.
    unsafe { RevertToSelf() };
    if ok == 0 {
        Err(error)
    } else {
        Ok(member != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn name(tag: &str) -> String {
        format!(r"\\.\pipe\kite-win-test-{tag}-{}", std::process::id())
    }

    #[test]
    fn the_dacl_names_the_right_sids() {
        assert_eq!(
            access("S-1-5-21-1-2-3-1001", Some("S-1-5-21-1-2-3-1002")),
            "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;S-1-5-21-1-2-3-1001)(A;;0x12019b;;;S-1-5-21-1-2-3-1002)"
        );
        // The service is SYSTEM already, and without the group nobody else gets in.
        assert_eq!(access(SYSTEM_SID, None), "D:P(A;;GA;;;SY)(A;;GA;;;BA)");
        for sddl in [
            access("S-1-5-21-1-2-3-1001", Some("S-1-5-32-544")),
            access(SYSTEM_SID, None),
        ] {
            Descriptor::parse(&sddl).unwrap();
        }
        assert!(Descriptor::parse("D:P(A;;GA;;;not-a-sid)").is_err());
    }

    #[tokio::test]
    async fn serves_the_user_and_refuses_a_second_first_instance() {
        let pipe = Pipe::new(name("serve")).unwrap();
        let mut server = pipe.instance(true).unwrap();
        // Another pipe of this name, claiming to be the first, is refused.
        assert!(pipe.instance(true).is_err());

        let client = tokio::spawn({
            let n = pipe.name.clone();
            async move {
                let mut c = connect(std::path::Path::new(&n)).await.unwrap();
                c.write_all(b"hello").await.unwrap();
                let mut back = [0u8; 2];
                c.read_exact(&mut back).await.unwrap();
                back
            }
        });
        server.connect().await.unwrap();
        let mut got = [0u8; 5];
        server.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"hello");
        // The answer must match whether this test runs elevated.
        let elevated = std::process::Command::new("net")
            .arg("session")
            .output()
            .unwrap()
            .status
            .success();
        assert_eq!(caller(&server).unwrap().is_admin().unwrap(), elevated);
        server.write_all(b"ok").await.unwrap();
        assert_eq!(&client.await.unwrap(), b"ok");

        // A later instance for the next client is fine.
        pipe.instance(false).unwrap();
    }
}
