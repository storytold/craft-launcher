//! Going back to an older version of an app installed with the Windows Installer.
//!
//! The app packages refuse to go over a newer copy (their upgrade rule stops with "A newer
//! version of … is already installed"), so the older version only goes in once the newer one is
//! gone. The person asked for this version by name; going back is never offered on its own.
//!
//! Before anything changes, both packages are downloaded and checked against their published
//! checksums: the chosen one, and the installed version's own, to put back. Then:
//! 1. The installed copy is removed. If that fails, nothing has changed.
//! 2. The chosen version is installed in the same folder and context. If that fails, the
//!    installed version goes back in from its package.
//!
//! An all-users copy needs Windows' permission, and the three steps run under ONE prompt, in an
//! elevated run of the launcher's installer helper ([`elevated`](super::elevated)), which calls [`run`]
//! there. Saying no changes nothing, and once it is yes, putting the old version back needs no
//! second answer. Separate prompts proved fragile: they look identical, so a refused install was
//! followed by a refused put-back, read as a retry, leaving no app at all.
//!
//! A per-user copy needs no permission, so its steps run one after another ([`run`]), all in one
//! turn of the installer queue: once the copy is removed, nothing queued can stop the put-back.

#![cfg_attr(not(windows), allow(dead_code))]

use artcraft_launcher_core::protocol::CANCELLED;

/// The three Windows Installer steps of a downgrade, and the names the messages use.
pub(super) struct Steps<'a> {
    pub app: &'a str,
    /// The version going in, and the one installed now (put back on failure).
    pub chosen: &'a str,
    pub previous: &'a str,
    pub remove: &'a mut dyn FnMut() -> Result<(), Removal>,
    pub install_chosen: &'a mut dyn FnMut() -> Result<(), String>,
    pub reinstall_previous: &'a mut dyn FnMut() -> Result<(), String>,
}

/// A removal that stopped: its error, and whether an earlier registration was already removed (an
/// app can be registered more than once), in which case the app is partly gone.
#[derive(Debug)]
pub(super) struct Removal {
    pub error: String,
    pub partly: bool,
}

/// Removes the installed copy, installs the chosen version, and puts the previous one back if
/// that install does not finish, one step after another. A removal that stops after removing
/// something also puts the previous version back.
pub(super) fn run(steps: Steps<'_>) -> Result<(), String> {
    let Steps { app, chosen, previous, remove, install_chosen, reinstall_previous } = steps;
    if let Err(Removal { error, partly }) = remove() {
        if !partly {
            return Err(error);
        }
        let why = reason(&error);
        return match reinstall_previous() {
            Ok(()) => Err(partly_removed_put_back(app, previous, &why)),
            Err(again) => Err(partly_removed_not_put_back(app, previous, &why, &reason(&again))),
        };
    }
    let why = match install_chosen() {
        Ok(()) => return Ok(()),
        Err(e) => reason(&e),
    };
    match reinstall_previous() {
        Ok(()) => Err(put_back(app, chosen, previous, &why)),
        Err(again) => Err(not_put_back(app, chosen, previous, &why, &reason(&again))),
    }
}

fn put_back(app: &str, chosen: &str, previous: &str, why: &str) -> String {
    format!("{app} {chosen} couldn't be installed ({why}). {app} {previous} was put back.")
}

fn not_put_back(app: &str, chosen: &str, previous: &str, why: &str, again: &str) -> String {
    format!("{app} {chosen} couldn't be installed ({why}), and putting {previous} back failed too ({again}). Install {app} again from the launcher.")
}

fn partly_removed_put_back(app: &str, previous: &str, why: &str) -> String {
    format!("{app} {previous} couldn't be fully removed ({why}). It was put back, so {app} {previous} is still installed.")
}

fn partly_removed_not_put_back(app: &str, previous: &str, why: &str, again: &str) -> String {
    format!("{app} {previous} couldn't be fully removed ({why}), and putting it back failed too ({again}). Install {app} again from the launcher.")
}

/// A refused permission prompt reads as what happened, not as the cancel code.
fn reason(error: &str) -> String {
    if error == CANCELLED { "permission wasn't given".to_string() } else { error.to_string() }
}

#[cfg(test)]
#[path = "msi_downgrade_tests.rs"]
mod tests;
