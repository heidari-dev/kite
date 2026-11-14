//! Running as a Windows service: the dispatcher, the status the service manager is told, and a
//! log file in place of the console a service does not have.

use std::ffi::OsString;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;
use tokio::sync::oneshot;
use windows_service::define_windows_service;
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{
    self, ServiceControlHandlerResult, ServiceStatusHandle,
};
use windows_service::service_dispatcher;
use windows_sys::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE};

/// The name the service is installed under.
pub const NAME: &str = "Kite";
/// How long the service manager waits for the daemon to come up or to stop.
const WAIT: Duration = Duration::from_secs(20);
/// A log over this size is moved aside when the service starts.
const MAX_LOG: u64 = 1024 * 1024;
/// `ERROR_FAILED_SERVICE_CONTROLLER_CONNECT`: started from a console, not by the service manager.
const NOT_A_SERVICE: i32 = 1063;

/// What the daemon gets to work with.
pub struct Control {
    /// Completes when the service manager asks the service to stop, or the system shuts down.
    pub stop: oneshot::Receiver<()>,
    pub up: Up,
}

/// Tells the service manager that the daemon is up.
pub struct Up(ServiceStatusHandle);

impl Up {
    pub fn running(&self) -> io::Result<()> {
        report(&self.0, ServiceState::Running, ServiceExitCode::Win32(0))
    }
}

type Body = Box<dyn FnOnce(Control) -> io::Result<()> + Send>;
static BODY: Mutex<Option<Body>> = Mutex::new(None);

define_windows_service!(ffi_service_main, service_main);

/// Runs `body` as the service `Kite` and returns when it has stopped. The log goes to `log`.
/// Fails at once when the process was not started by the service manager.
pub fn run(
    log: &std::path::Path,
    body: impl FnOnce(Control) -> io::Result<()> + Send + 'static,
) -> io::Result<()> {
    redirect_stderr(log)?;
    *BODY.lock().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(body));
    service_dispatcher::start(NAME, ffi_service_main).map_err(|e| match e {
        windows_service::Error::Winapi(e) if e.raw_os_error() == Some(NOT_A_SERVICE) => {
            io::Error::other(
                "`kite daemon --service` is for the service manager; run `kite daemon` in a \
                 console to try the daemon by hand",
            )
        }
        e => io::Error::other(e),
    })
}

fn service_main(_arguments: Vec<OsString>) {
    let Some(body) = BODY.lock().unwrap_or_else(PoisonError::into_inner).take() else {
        return;
    };
    let (tx, stop) = oneshot::channel();
    let tx = Mutex::new(Some(tx));
    let handle = Arc::new(OnceLock::<ServiceStatusHandle>::new());
    let seen = handle.clone();
    let registered = service_control_handler::register(NAME, move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            if let Some(h) = seen.get() {
                let _ = report(h, ServiceState::StopPending, ServiceExitCode::Win32(0));
            }
            if let Some(tx) = tx.lock().unwrap_or_else(PoisonError::into_inner).take() {
                let _ = tx.send(());
            }
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    });
    let Ok(status) = registered else {
        eprintln!("kite: cannot register with the service manager");
        return;
    };
    let _ = handle.set(status);
    let _ = report(
        &status,
        ServiceState::StartPending,
        ServiceExitCode::Win32(0),
    );
    let result = body(Control {
        stop,
        up: Up(status),
    });
    let code = match &result {
        Ok(()) => ServiceExitCode::Win32(0),
        Err(e) => {
            eprintln!("kite: the daemon stopped: {e}");
            ServiceExitCode::ServiceSpecific(1)
        }
    };
    let _ = report(&status, ServiceState::Stopped, code);
}

fn report(
    handle: &ServiceStatusHandle,
    state: ServiceState,
    exit_code: ServiceExitCode,
) -> io::Result<()> {
    let pending = matches!(
        state,
        ServiceState::StartPending | ServiceState::StopPending
    );
    handle
        .set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: if state == ServiceState::Running {
                ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
            } else {
                ServiceControlAccept::empty()
            },
            exit_code,
            checkpoint: 0,
            wait_hint: if pending { WAIT } else { Duration::ZERO },
            process_id: None,
        })
        .map_err(io::Error::other)
}

/// Sends what the process writes to stderr to `path`, appending; an old log over [`MAX_LOG`] is
/// moved to `path.old` first. The file lives in the daemon's private directory.
fn redirect_stderr(path: &std::path::Path) -> io::Result<()> {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_LOG) {
        let _ = std::fs::rename(path, path.with_extension("log.old"));
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    // SAFETY: the handle belongs to `file`, which is leaked below so that it stays open for the
    // life of the process; stderr then writes to it.
    if unsafe { SetStdHandle(STD_ERROR_HANDLE, file.as_raw_handle()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    std::mem::forget(file);
    Ok(())
}
