//! Running as the Windows service `ArtCraftLauncherUpdater` (LocalSystem), as its MSI installs it:
//! `artcraft-launcher-updater.exe service`.
//!
//! The service control manager calls [`ffi_service_main`] on a thread of its own once
//! [`run`] has connected to it. The service takes no start parameters, so the raw argument list
//! is never read (and nothing here needs `unsafe`).

use std::sync::mpsc;
use std::time::Duration;

use artcraft_launcher_native::paths::Paths;
use artcraft_launcher_native::service::SERVICE_NAME;
use windows_service::service::{ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult, ServiceStatusHandle};
use windows_service::service_dispatcher;

use crate::server::Server;
use artcraft_launcher_native::policy::Policy;

/// `ERROR_FAILED_SERVICE_CONTROLLER_CONNECT`: started from a console, not by Windows.
const NOT_A_SERVICE: i32 = 1063;
/// `ERROR_SERVICE_SPECIFIC_ERROR`'s companion code for a service that couldn't start its work.
const COULDNT_START: u32 = 1;

/// Hands this thread to the service control manager until the service stops.
pub fn run() -> Result<(), String> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main).map_err(|e| match e {
        windows_service::Error::Winapi(io) if io.raw_os_error() == Some(NOT_A_SERVICE) => {
            "`artcraft-launcher-updater service` is how Windows starts the update service. To try it in this console, run `artcraft-launcher-updater run`.".to_string()
        }
        windows_service::Error::Winapi(io) => format!("couldn't start the service ({io})"),
        other => format!("couldn't start the service ({other})"),
    })
}

extern "system" fn ffi_service_main(_argc: u32, _argv: *mut *mut u16) {
    if let Err(e) = service_main() {
        log::error!("the update service stopped: {e}");
    }
}

fn service_main() -> Result<(), String> {
    let (stop_tx, stop_rx) = mpsc::channel();
    let handler = move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = stop_tx.send(());
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let handle = service_control_handler::register(SERVICE_NAME, handler).map_err(|e| describe(&e))?;
    report(&handle, ServiceState::StartPending, 1, Duration::from_secs(30));
    let started = Paths::for_service().and_then(|paths| Server::new(paths, Policy::load));
    let server = match started {
        Ok(server) => server,
        Err(e) => {
            log::error!("the update service couldn't start: {e}");
            let _ = handle.set_service_status(status(ServiceState::Stopped, 0, Duration::ZERO, ServiceExitCode::ServiceSpecific(COULDNT_START)));
            return Err(e);
        }
    };
    server.start();
    let _ = handle.set_service_status(ServiceStatus {
        controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        ..status(ServiceState::Running, 0, Duration::ZERO, ServiceExitCode::Win32(0))
    });
    log::info!("update service running");
    // Until Windows asks it to stop (a closed channel means the same).
    let _ = stop_rx.recv();
    let mut checkpoint = 1;
    report(&handle, ServiceState::StopPending, checkpoint, Duration::from_secs(30));
    server.stop(&mut || {
        checkpoint += 1;
        report(&handle, ServiceState::StopPending, checkpoint, Duration::from_secs(30));
    });
    report(&handle, ServiceState::Stopped, 0, Duration::ZERO);
    Ok(())
}

fn status(state: ServiceState, checkpoint: u32, wait_hint: Duration, exit_code: ServiceExitCode) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code,
        checkpoint,
        wait_hint,
        process_id: None,
    }
}

/// Tells Windows where the service stands; a failure is only logged (there's no one else to tell).
fn report(handle: &ServiceStatusHandle, state: ServiceState, checkpoint: u32, wait_hint: Duration) {
    if let Err(e) = handle.set_service_status(status(state, checkpoint, wait_hint, ServiceExitCode::Win32(0))) {
        log::warn!("couldn't report the service's state: {}", describe(&e));
    }
}

fn describe(e: &windows_service::Error) -> String {
    match e {
        windows_service::Error::Winapi(io) => io.to_string(),
        other => other.to_string(),
    }
}
