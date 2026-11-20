//! Moving an app installed for everyone (an all-users Windows Installer copy) to this account.
//!
//! The copy for this account goes in first, started by this account: over-the-shoulder elevation
//! runs as the administrator, so an elevated helper would install it for the wrong account. Only
//! once it is in is the copy for everyone removed, and that removal is the one step that asks for
//! administrator permission. So:
//! 1. If the install for this account fails (say, a managed account whose policy blocks per-user
//!    installs), nothing else has changed: the copy for everyone stays.
//! 2. If the removal is refused or fails, both copies stay installed and either one starts. The
//!    next update stops on them, as for any app installed both ways, so the message says how to
//!    remove the shared one.
//!
//! Nothing is ever put back, so there is no second permission prompt (see
//! [`msi_downgrade`](super::msi_downgrade) on why separate prompts are avoided).
//!
//! The two copies must be different Windows Installer products: one product can't be registered
//! for this account while it is registered for everyone. A package with the installed copy's
//! product code ([`same_product`]) is not migrated.

#![cfg_attr(not(windows), allow(dead_code))]

use artcraft_launcher_core::protocol::CANCELLED;

/// The two Windows Installer steps of a migration, and the names the messages use.
pub(super) struct Steps<'a> {
    pub app: &'a str,
    /// The version going in for this account, and the one installed for everyone now.
    pub chosen: &'a str,
    pub previous: &'a str,
    pub install_for_user: &'a mut dyn FnMut() -> Result<(), String>,
    pub remove_shared: &'a mut dyn FnMut() -> Result<(), String>,
}

/// Installs the chosen version for this account, then removes the copy for everyone. A failed
/// install stops before anything is removed.
pub(super) fn run(steps: Steps<'_>) -> Result<(), String> {
    let Steps { app, chosen, previous, install_for_user, remove_shared } = steps;
    if let Err(e) = install_for_user() {
        return Err(not_installed(app, chosen, previous, &reason(&e)));
    }
    remove_shared().map_err(|e| not_removed(app, chosen, previous, &reason(&e)))
}

/// Whether the package is the installed product itself. An unknown installed product code counts
/// as the same, so nothing is migrated on a guess.
pub(super) fn same_product(installed: Option<&str>, package: &str) -> bool {
    installed.is_none_or(|code| code.eq_ignore_ascii_case(package))
}

fn not_installed(app: &str, chosen: &str, previous: &str, why: &str) -> String {
    format!("{app} {chosen} couldn't be installed for this account ({why}). {app} {previous} is still installed for everyone; nothing was removed.")
}

fn not_removed(app: &str, chosen: &str, previous: &str, why: &str) -> String {
    format!(
        "{app} {chosen} is installed for this account, but {app} {previous} for everyone couldn't be removed ({why}). Both copies are installed now. \
         Remove the one for everyone in Windows Settings > Apps (this needs an administrator) before the next update of {app}."
    )
}

/// A refused permission prompt reads as what happened, not as the cancel code.
fn reason(error: &str) -> String {
    if error == CANCELLED { "permission wasn't given".to_string() } else { error.to_string() }
}

#[cfg(test)]
#[path = "msi_migration_tests.rs"]
mod tests;
