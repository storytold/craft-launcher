//! Going back to an older version of an app installed with the Windows Installer.
//!
//! The app packages refuse to go over a newer copy (their upgrade rule stops with "A newer
//! version of … is already installed"), so the older version only goes in once the newer one is
//! gone. The person asked for this version by name; going back is never offered on its own.
//!
//! Before anything changes, both packages are downloaded and checked against their published
//! checksums: the chosen one, and the installed version's own, to put back (or, for a copy whose
//! version has no release of its own, the pre-release with its numbers that the person agreed to).
//! Then:
//! 1. The installed copy is removed. If that fails, nothing has changed.
//! 2. The chosen version is installed in the same folder and context. If that fails, the
//!    installed version goes back in from its package.
//!
//! An all-users copy needs Windows' permission, and the three steps run under ONE prompt, in one
//! elevated session ([`script`]). Saying no changes nothing, and once it is yes, putting the old
//! version back needs no second answer. Separate prompts proved fragile: they look identical, so
//! a refused install was followed by a refused put-back, read as a retry, leaving no app at all.
//! The session is given only `msiexec` command lines for the two held, checked packages and the
//! installed copy's product codes, as [`msi_batch`](super::msi_batch) does for Update all.
//!
//! If that session cannot start, an all-users downgrade stops before anything is removed: falling
//! back to one prompt per step would bring the trap back.
//!
//! A per-user copy needs no permission, so its steps run one after another ([`run`]), all in one
//! turn of the installer queue: once the copy is removed, nothing queued can stop the put-back.

#![cfg_attr(not(windows), allow(dead_code))]

use artcraft_launcher_core::protocol::CANCELLED;

use super::msi_batch::{self, DECLINED, LAUNCH_FAILED};

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

/// A downgrade that didn't finish: what to tell the person, and what happened to the previous
/// package. Which registration the put-back is worked out by the caller, only after a completed
/// removal, from the registrations Windows Installer lists afterwards ([`put_back_registration`]).
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Failed {
    pub error: String,
    pub put_back: PutBack,
}

/// Whether a failed downgrade put the previous package back, and whether the removal step got
/// through every registration it covers before that: only the second kind of put-back can be
/// recorded, because a removal that stopped partway leaves registrations behind that cannot be
/// told apart from the restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PutBack {
    /// The previous package was not put back (nothing needed it, or putting it back failed too),
    /// so no registration was created that could be recorded.
    Not,
    /// The removal step removed every registration it covers, and the checked previous package
    /// then went back in. Windows Installer gives a package the same product code every time, so
    /// the restore can carry a code the machine already listed before the downgrade. What the step
    /// covers depends on the route: an all-users downgrade removes the machine-wide records and
    /// deliberately leaves the account's own per-user record out, so that one can sit beside the
    /// restore.
    AfterRemoval,
    /// A removal failed after another registration was already gone, so the app was only partly
    /// removed. What is still registered cannot be told apart from the restore — a registration
    /// that appeared since the snapshot can carry the previous numbers just as the restore does —
    /// so nothing is recorded and the person is told the app is still installed.
    AfterPartialRemoval,
}

impl Failed {
    fn without_put_back(error: String) -> Self {
        Self { error, put_back: PutBack::Not }
    }

    /// The previous package is back after the removal step removed every registration it covers.
    fn completed_removal_put_back(error: String) -> Self {
        Self { error, put_back: PutBack::AfterRemoval }
    }

    /// The previous package is back after a removal that stopped once part of the app was gone: the
    /// app is partly gone too, and nothing is recorded for it.
    fn partial_removal_put_back(error: String) -> Self {
        Self { error, put_back: PutBack::AfterPartialRemoval }
    }
}

/// Removes the installed copy, installs the chosen version, and puts the previous one back if
/// that install does not finish, one step after another. A removal that stops after removing
/// something also puts the previous version back.
pub(super) fn run(steps: Steps<'_>) -> Result<(), Failed> {
    let Steps { app, chosen, previous, remove, install_chosen, reinstall_previous } = steps;
    if let Err(Removal { error, partly }) = remove() {
        if !partly {
            return Err(Failed::without_put_back(error));
        }
        let why = reason(&error);
        return match reinstall_previous() {
            Ok(()) => Err(Failed::partial_removal_put_back(partly_removed_put_back(app, previous, &why))),
            Err(again) => Err(Failed::without_put_back(partly_removed_not_put_back(app, previous, &why, &reason(&again)))),
        };
    }
    let why = match install_chosen() {
        Ok(()) => return Ok(()),
        Err(e) => reason(&e),
    };
    match reinstall_previous() {
        Ok(()) => Err(Failed::completed_removal_put_back(put_back(app, chosen, previous, &why))),
        Err(again) => Err(Failed::without_put_back(not_put_back(app, chosen, previous, &why, &reason(&again)))),
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

/// An all-users downgrade whose one-prompt session could not start: nothing was removed.
pub(super) fn no_session(app: &str, chosen: &str) -> String {
    format!("couldn't ask Windows for permission to go back to {app} {chosen}, so nothing was changed.")
}

/// A refused permission prompt reads as what happened, not as the cancel code.
fn reason(error: &str) -> String {
    if error == CANCELLED { "permission wasn't given".to_string() } else { error.to_string() }
}

/// The step that stopped the elevated session, in its exit code: bit 29 (the "customer" bit, which
/// no Windows or Windows Installer code sets), the step in bits 24-27, and the `msiexec` codes in
/// bits 12-23 and 0-11. Windows Installer codes are below 4096.
const OURS: i32 = 0x2000_0000;
const REMOVAL_FAILED: i32 = 1;
const PUT_BACK: i32 = 2;
const NOT_PUT_BACK: i32 = 3;
const PARTLY_REMOVED_PUT_BACK: i32 = 4;
const PARTLY_REMOVED_NOT_PUT_BACK: i32 = 5;

/// The elevated session: removes each installed registration (`remove`, `msiexec` arguments),
/// installs `chosen`, and on failure installs `previous`, all through `program`. A removal that
/// fails after another succeeded also installs `previous`. It exits 0 when the chosen version went
/// in, or with one of the codes [`ended`] reads.
pub(super) fn script(program: &str, remove: &[String], chosen: &str, previous: &str) -> String {
    let list = remove.iter().map(|l| msi_batch::quoted(l)).collect::<Vec<_>>().join(",");
    format!(
        "$ErrorActionPreference = 'Stop'\n\
         $ProgressPreference = 'SilentlyContinue'\n\
         $program = {program}\n\
         function Run([string]$arguments) {{ (Start-Process -FilePath $program -ArgumentList $arguments -Wait -PassThru).ExitCode }}\n\
         function Fine([int]$code) {{ $code -eq 0 -or $code -eq 3010 -or $code -eq 1641 }}\n\
         function Code([int]$code) {{ if ($code -lt 0 -or $code -gt 4094) {{ 4095 }} else {{ $code }} }}\n\
         function Ended([int]$step, [int]$first, [int]$second) {{ {OURS} -bor ($step -shl 24) -bor ((Code $first) -shl 12) -bor (Code $second) }}\n\
         $removed = 0\n\
         foreach ($arguments in @({list})) {{\n\
         \x20 $code = Run $arguments\n\
         \x20 if (-not (Fine $code)) {{\n\
         \x20   if ($removed -eq 0) {{ exit (Ended {REMOVAL_FAILED} $code 0) }}\n\
         \x20   $back = Run {previous}\n\
         \x20   if (Fine $back) {{ exit (Ended {PARTLY_REMOVED_PUT_BACK} $code 0) }}\n\
         \x20   exit (Ended {PARTLY_REMOVED_NOT_PUT_BACK} $code $back)\n\
         \x20 }}\n\
         \x20 $removed++\n\
         }}\n\
         $code = Run {chosen}\n\
         if (Fine $code) {{ exit 0 }}\n\
         $back = Run {previous}\n\
         if (Fine $back) {{ exit (Ended {PUT_BACK} $code 0) }}\n\
         exit (Ended {NOT_PUT_BACK} $code $back)\n",
        program = msi_batch::quoted(program),
        chosen = msi_batch::quoted(chosen),
        previous = msi_batch::quoted(previous),
    )
}

/// What the elevated session's exit code says happened.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Ended {
    Done,
    /// The one prompt was refused: nothing ran.
    Declined,
    /// The session never started; nothing ran.
    LaunchFailed,
    RemovalFailed(i32),
    /// The chosen version failed with this code, and the previous one is back.
    PutBack(i32),
    NotPutBack {
        install: i32,
        back: i32,
    },
    /// A removal failed with this code after another registration was removed; the previous
    /// version is back.
    PartlyRemovedPutBack(i32),
    PartlyRemovedNotPutBack {
        removal: i32,
        back: i32,
    },
    Other(Option<i32>),
}

pub(super) fn ended(code: Option<i32>) -> Ended {
    let Some(code) = code else { return Ended::Other(None) };
    let step = (code >> 24) & 0xF;
    let (first, second) = ((code >> 12) & 0xFFF, code & 0xFFF);
    match code {
        0 => Ended::Done,
        DECLINED => Ended::Declined,
        LAUNCH_FAILED => Ended::LaunchFailed,
        _ if code & !0x0FFF_FFFF == OURS && step == REMOVAL_FAILED => Ended::RemovalFailed(first),
        _ if code & !0x0FFF_FFFF == OURS && step == PUT_BACK => Ended::PutBack(first),
        _ if code & !0x0FFF_FFFF == OURS && step == NOT_PUT_BACK => Ended::NotPutBack { install: first, back: second },
        _ if code & !0x0FFF_FFFF == OURS && step == PARTLY_REMOVED_PUT_BACK => Ended::PartlyRemovedPutBack(first),
        _ if code & !0x0FFF_FFFF == OURS && step == PARTLY_REMOVED_NOT_PUT_BACK => Ended::PartlyRemovedNotPutBack { removal: first, back: second },
        _ => Ended::Other(Some(code)),
    }
}

/// What a finished elevated downgrade means for the person. `LaunchFailed` is not passed here:
/// the caller reports [`no_session`] instead.
pub(super) fn report(ended: Ended, app: &str, chosen: &str, previous: &str) -> Result<(), Failed> {
    // Inside the session permission was already given: a cancel there is the progress window's.
    let msiexec = |code: i32| match super::explain(Some(code)).err().unwrap_or_default() {
        e if e == CANCELLED => "it was cancelled".to_string(),
        e => e,
    };
    match ended {
        Ended::Done => Ok(()),
        Ended::Declined => Err(Failed::without_put_back(CANCELLED.to_string())),
        Ended::RemovalFailed(code) => {
            Err(Failed::without_put_back(format!("{app} {previous} couldn't be removed ({}), so nothing was changed.", msiexec(code))))
        }
        Ended::PutBack(code) => Err(Failed::completed_removal_put_back(put_back(app, chosen, previous, &msiexec(code)))),
        Ended::NotPutBack { install, back } => Err(Failed::without_put_back(not_put_back(app, chosen, previous, &msiexec(install), &msiexec(back)))),
        Ended::PartlyRemovedPutBack(code) => Err(Failed::partial_removal_put_back(partly_removed_put_back(app, previous, &msiexec(code)))),
        Ended::PartlyRemovedNotPutBack { removal, back } => {
            Err(Failed::without_put_back(partly_removed_not_put_back(app, previous, &msiexec(removal), &msiexec(back))))
        }
        Ended::LaunchFailed | Ended::Other(_) => {
            let code = match ended {
                Ended::Other(Some(code)) => code.to_string(),
                _ => "none".to_string(),
            };
            Err(Failed::without_put_back(format!(
                "going back to {app} {chosen} stopped unexpectedly (code {code}). Check which version of {app} is installed."
            )))
        }
    }
}

/// The registration a put-back is, from the registrations Windows Installer lists now (`after`),
/// each with the version it reports, and what the downgrade said about the removal ([`PutBack`]).
/// Windows Installer keeps only the numbers, so they are compared without any pre-release part.
///
/// Only a put-back after a completed removal ([`PutBack::AfterRemoval`]) is recorded: every
/// registration the removal step covers is gone, so the one registration carrying `previous`'s
/// numbers is the package that just went back in — whether or not Windows Installer reused a
/// product code the machine already had, and even when a registration the step never covers (such
/// as an account's own per-user record) sits beside it with other numbers.
///
/// A partial removal ([`PutBack::AfterPartialRemoval`]) records nothing: the registrations it left
/// behind cannot be told apart from the restore, and a registration that appeared since the
/// snapshot can carry the previous numbers just as the restore does. Neither does [`PutBack::Not`].
///
/// `None` unless there is exactly one registration with `previous`'s numbers: nothing is ever
/// guessed from rows.
pub(super) fn put_back_registration(
    put_back: PutBack,
    after: &[(String, Option<super::version::Version>)],
    previous: Option<&super::version::Version>,
) -> Option<String> {
    let previous = previous?;
    if put_back != PutBack::AfterRemoval {
        return None;
    }
    let mut restored = after
        .iter()
        .filter(|(_, numbers)| numbers.as_ref().is_some_and(|n| (n.major, n.minor, n.patch) == (previous.major, previous.minor, previous.patch)));
    let (code, _) = restored.next()?;
    restored.next().is_none().then(|| code.clone())
}

/// The `msiexec` arguments that remove one registration. Product codes come from the registry, so
/// only the exact `{GUID}` shape is let into the elevated session.
pub(super) fn removal(product_code: &str) -> Result<String, String> {
    let inner = product_code.strip_prefix('{').and_then(|c| c.strip_suffix('}')).unwrap_or("");
    let shape = inner.len() == 36 && inner.char_indices().all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() });
    if !shape {
        return Err(format!("couldn't read the installed copy's registration ({product_code})"));
    }
    Ok(format!("/x {product_code} /passive /norestart"))
}

/// The removals the elevated session runs: one per product code that a machine-wide uninstall
/// record (HKLM) names as the app. Those records are writable only by administrators. A record in
/// the current account's own hive (HKCU) is writable without elevation, so anything running as
/// the user could make one carry the app's name and an unrelated all-users product's code, and
/// have that product removed under the app's permission prompt (CWE-863). An all-users downgrade
/// replaces an all-users copy, which a per-user record never names, so those records are left out.
pub(super) fn elevated_removals<'a>(app: &str, installs: impl IntoIterator<Item = (&'a str, super::MsiScope)>) -> Result<Vec<String>, String> {
    let removals = installs
        .into_iter()
        .filter(|(_, scope)| *scope == super::MsiScope::PerMachine)
        .map(|(code, _)| removal(code))
        .collect::<Result<Vec<_>, _>>()?;
    if removals.is_empty() {
        return Err(format!("{app} isn't registered with the Windows Installer for all users"));
    }
    Ok(removals)
}

#[cfg(test)]
#[path = "msi_downgrade_tests.rs"]
mod tests;
