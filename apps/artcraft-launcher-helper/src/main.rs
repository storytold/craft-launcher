//! ArtCraft Launcher's installer helper: the only program of the launcher that runs elevated.
//!
//! The launcher starts it behind one permission prompt for all-users Windows Installer work
//! (`artcraft_launcher_native::install::elevated`). It is a separate program, not the launcher
//! started again, so that what runs elevated loads no DLL from its own folder, which the person's
//! programs can write (the launcher installs per user):
//! - its own imports come only from System32 (`/DEPENDENTLOADFLAG:0x800`, `build.rs`), and it
//!   imports no graphics or windowing code, whose system DLLs would look in this folder for their
//!   own imports before `main` could stop them;
//! - the first thing `main` does is limit every later load to System32
//!   ([`load_from_system32_only`]), which covers DLLs that system DLLs load on demand, such as the
//!   `msasn1.dll` crypt32 loads during the signature check. If that fails, it does nothing else.
//!
//! It opens no window. Its log goes into the run folder, which only administrators can change.
//!
//! Its `unsafe` is limited to Windows calls with no safe wrapper, each reviewed where it is:
//! [`load_from_system32_only`] here, and in [`package_facts`] the signing root and UpgradeCode
//! that `install::identity` uses to admit only our packages, for the app they replace.

#![cfg_attr(windows, windows_subsystem = "windows")]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(windows)]
mod package_facts;
#[cfg(windows)]
mod run_log;

use std::process::ExitCode;

fn main() -> ExitCode {
    #[cfg(windows)]
    {
        if !load_from_system32_only() {
            return ExitCode::FAILURE;
        }
        let log = run_log::install();
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        let done = artcraft_launcher_native::install::elevated::finish(&args, &package_facts::WindowsFacts, |dir| {
            if let Some(log) = &log {
                log.attach(dir);
            }
        });
        if matches!(done, Some(Ok(()))) { ExitCode::SUCCESS } else { ExitCode::FAILURE }
    }
    // Only Windows has all-users installs to manage.
    #[cfg(not(windows))]
    ExitCode::FAILURE
}

/// Makes every later DLL load in this process search System32 only, never this program's folder,
/// the current folder or `PATH`. Whether Windows accepted it.
///
/// `unsafe` because winsafe doesn't wrap `SetDefaultDllDirectories`, and without it a DLL planted
/// beside this program would be loaded into an elevated process.
#[cfg(windows)]
#[allow(unsafe_code)]
fn load_from_system32_only() -> bool {
    use windows_sys::Win32::System::LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories};
    // SAFETY: the call takes one flag by value and touches no memory of ours. It has no
    // precondition, and failure is reported through its return value, which is checked.
    unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) != 0 }
}

#[cfg(test)]
mod tests {
    /// The lines of `header`'s table in `toml`.
    fn table(toml: &str, header: &str) -> Vec<String> {
        toml.lines()
            .map(str::trim)
            .skip_while(|line| *line != header)
            .skip(1)
            .take_while(|line| !line.is_empty() && !line.starts_with('['))
            .map(String::from)
            .collect()
    }

    /// This crate can't use `[lints] workspace = true` (see its Cargo.toml), so it keeps a copy.
    #[test]
    fn the_lints_are_the_workspaces_with_unsafe_code_denied_not_forbidden() {
        let ours = include_str!("../Cargo.toml");
        let workspace = include_str!("../../../Cargo.toml");
        let clippy = table(ours, "[lints.clippy]");
        assert!(!clippy.is_empty());
        assert_eq!(clippy, table(workspace, "[workspace.lints.clippy]"));
        assert_eq!(table(ours, "[lints.rust]"), ["unsafe_code = \"deny\""]);
        assert_eq!(table(workspace, "[workspace.lints.rust]"), ["unsafe_code = \"forbid\""]);
    }
}
