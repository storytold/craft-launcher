//! ArtCraft Launcher Updater: the ArtCraft Launcher Update Service, and a command line that checks
//! for and installs updates to the Craft App Suite.
//!
//! On Windows every Craft App installs for all users, into Program Files, so each update needs
//! administrator rights. The update service is the opt-in way round that: an administrator
//! installs it once (`artcraft-launcher-updater-<version>-windows-<arch>.msi`); it runs as
//! LocalSystem, installs updates in the background on a schedule, and takes update requests from
//! the launcher and from this command line, so they need no UAC prompt. It only ever updates apps
//! an administrator already installed, only to the newest release, and only with a package it has
//! checked (see `artcraft_launcher_native::service`).
//!
//! - `artcraft-launcher-updater service`: the service, as Windows starts it.
//! - `artcraft-launcher-updater check | update | status`: the command line.
//! - `artcraft-launcher-updater run [--once]`: the service's work in a console, for testing.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod cli;
mod server;
#[cfg(windows)]
mod winsvc;

use std::process::ExitCode;

use artcraft_launcher_core::CraftApp;
use artcraft_launcher_native::logging;
use artcraft_launcher_native::paths::Paths;

use crate::cli::Command;
use artcraft_launcher_native::policy::Policy;

/// The log file's name (`logs/artcraft-launcher-updater.log`).
const LOG_NAME: &str = "artcraft-launcher-updater";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match Command::parse(&args) {
        Ok(command) => command,
        Err(e) => {
            eprintln!("artcraft-launcher-updater: {e}\n\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    match command {
        Command::Help => {
            print!("{}", cli::USAGE);
            ExitCode::SUCCESS
        }
        Command::Version => {
            println!("artcraft-launcher-updater {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Command::Service => service(),
        Command::Run { once } => run(once),
        command => {
            // The command line's log goes beside the launcher's, in the user's data folder.
            start_logging(Paths::new(), command.verbose());
            cli::run(command)
        }
    }
}

/// Sends log records to `<paths>/logs/artcraft-launcher-updater.log` (and standard error when
/// `stderr`).
fn start_logging(paths: Result<Paths, String>, stderr: bool) {
    let Some(logs) = logging::install(LOG_NAME, stderr) else { return };
    logging::install_panic_hook(LOG_NAME);
    match paths.and_then(|p| logs.attach_dir(&p.logs())) {
        Ok(path) => log::info!("ArtCraft Launcher Updater {}, log file {}", env!("CARGO_PKG_VERSION"), path.display()),
        Err(e) => {
            logs.no_file();
            log::warn!("no log file: {e}");
        }
    }
}

#[cfg(windows)]
fn service() -> ExitCode {
    start_logging(Paths::for_service(), false);
    match winsvc::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log::error!("{e}");
            eprintln!("artcraft-launcher-updater: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn service() -> ExitCode {
    eprintln!("artcraft-launcher-updater: the update service runs on Windows only; here, use `artcraft-launcher-updater update`");
    ExitCode::FAILURE
}

/// The service's work in this console: its schedule and its requests until stopped (Ctrl+C), or
/// with `once` a single check and its installs.
fn run(once: bool) -> ExitCode {
    let paths = Paths::for_service();
    start_logging(paths.clone(), true);
    if artcraft_launcher_native::service::running() {
        eprintln!("Note: the {} is already running; this run takes no requests.", artcraft_launcher_native::service::DISPLAY_NAME);
    }
    let server = match paths.and_then(|p| server::Server::new(p, Policy::load)) {
        Ok(server) => server,
        Err(e) => {
            eprintln!("artcraft-launcher-updater: {e}");
            return ExitCode::FAILURE;
        }
    };
    if once {
        let (outcomes, _) = server.scheduled_run();
        for (id, outcome) in &outcomes {
            println!("{}", outcome.describe(CraftApp::by_id(id).map_or(*id, |a| a.name)));
        }
        if outcomes.is_empty() {
            println!("Nothing to install.");
        }
        server.stop(&mut || {});
        return if outcomes.iter().all(|(_, o)| o.is_success()) { ExitCode::SUCCESS } else { ExitCode::FAILURE };
    }
    server.start();
    println!("The update service's work is running in this console. Ctrl+C stops it.");
    loop {
        std::thread::park();
    }
}
