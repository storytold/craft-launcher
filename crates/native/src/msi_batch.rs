//! Windows: one UAC prompt for every Craft App that installs together (#67).
//!
//! The Craft Apps' MSIs install per machine, so each needs administrator rights. Run one by one
//! from the launcher, each would ask for permission and show its own Windows Installer window.
//! Instead, the MSI jobs that are ready together become one batch (`crate::queue`: a batch
//! waits for the MSI jobs still downloading), and the launcher starts a copy of itself with
//! administrator rights once for the whole batch:
//!
//! ```text
//! artcraft-launcher.exe --install-msis <results file> (<msi> <sha256> <folder or ->)…
//! ```
//!
//! That helper installs each MSI with `msiexec /i <msi> /qn /norestart`, writes each exit code
//! to the results file, and exits. The user still chooses each install (or **Update all**) and
//! still answers the prompt; the cards say *Waiting for permission…* while it is up. Declining
//! it cancels the batch.
//!
//! Anyone can start the launcher with `--install-msis` and have Windows ask for permission in
//! ArtCraft Launcher's name, so the helper does only what the launcher itself may do (CWE-441):
//!
//! - It runs only when its own exe carries a valid Authenticode signature from
//!   [`crate::signing::PUBLISHER`], and installs only MSIs signed by that same publisher, checked on the
//!   staged copy `msiexec` reads.
//! - Each MSI must be a Craft App's own Windows release by file name (`policy::craft_msi`), may
//!   not replace a newer version installed for the whole machine, and installs where that
//!   install already is, or into a new folder named after the app outside the Windows folder.
//!   The registry it reads for that is the machine's (HKLM), never the user's.
//! - The launcher sends only MSIs that pass the same checks ([`batchable`], asked before the job
//!   joins the queue); any other MSI, such as a per-user install in a folder of its own, installs
//!   the way it did before the batch, with Windows Installer's own prompt.
//! - Its imported DLLs load from System32 only (`/DEPENDENTLOADFLAG`, in `build.rs`), not from
//!   the user-writable folder the exe runs from.
//!
//! Everything the helper does runs as administrator, so it trusts nothing that can change after
//! the prompt:
//!
//! - Paths and checksums come only from its command line, fixed when it starts.
//! - Each MSI path must already be its own canonical form (no links on the way). Every MSI is
//!   opened first, with sharing that refuses writes, renames and deletes, which also keeps the
//!   folders above it from being renamed or replaced while the batch runs.
//! - Each MSI is copied through that handle into a new folder of the batch's own, under a random
//!   name in the Windows temporary folder, whose access list is first replaced so only
//!   Administrators and SYSTEM can open it, and the SHA-256 is computed from the bytes copied.
//!   `msiexec` installs that copy, held open against changes, so what installs is exactly what
//!   was checked, whatever the download's path leads to afterwards. The folder goes however the
//!   helper ends.
//! - The results file and each MSI's log beside the download are created new (never an existing
//!   file or link). `msiexec` writes its log in the staging folder; the helper copies it into the
//!   new log through its own handle.
//! - `msiexec` and PowerShell are found through the machine's registry, not the user's
//!   environment.
//! - It takes only absolute `.msi` paths and lowercase hex checksums, at most [`MOST`] of them,
//!   and exits when the batch is done.
//!
//! Apps that are running are left out of the batch: the launcher checks just before it asks for
//! permission, and the helper again just before each `msiexec`, however long the prompt was up.
//! If Windows still reports a restart is needed (3010), the app's card says so rather than that
//! the update worked. While the prompt is up the launcher lists every app and version it covers.
//! Signatures are checked from local data only, never online. When the prompt can't
//! be shown at all (PowerShell blocked by policy, say), each MSI installs the way it did before,
//! asking for permission itself; once the helper has started, nothing in the batch is installed
//! any other way.

use std::ffi::OsString;
#[cfg(any(windows, test))]
use std::path::{Path, PathBuf};

#[cfg(any(windows, test))]
use artcraft_launcher_core::protocol::{CANCELLED, JobPhase};

#[cfg(any(windows, test))]
use crate::http::Stop;
#[cfg(any(windows, test))]
use crate::queue::{Queue, Step, Ticket};

/// The helper's first argument.
pub const FLAG: &str = "--install-msis";

/// The most MSIs one helper takes (the Craft App Suite has fewer apps than this).
pub const MOST: usize = 32;

/// The folder argument of an MSI that installs where it always does.
const DEFAULT_FOLDER: &str = "-";

/// The batch's stages (see [`crate::queue::Step::Running`]).
#[cfg(any(windows, test))]
const ASKING: u32 = 0;
#[cfg(any(windows, test))]
const INSTALLING: u32 = 1;

/// Codes the helper writes for an MSI it didn't hand to the Windows Installer, or whose
/// installer didn't say how it went. The Windows Installer's own are 0 and up.
pub mod code {
    /// The file's SHA-256 isn't the one it was downloaded against.
    pub const CHECKSUM: i32 = -1;
    /// A link on the way to the file, or an install folder that can't be passed on.
    pub const UNSAFE_PATH: i32 = -2;
    /// The file couldn't be opened or `msiexec` couldn't be started.
    pub const NOT_STARTED: i32 = -4;
    /// `msiexec` ended without an exit code.
    pub const INTERRUPTED: i32 = -5;
    /// The MSI isn't signed by ArtCraft Launcher's own publisher.
    pub const UNSIGNED: i32 = -6;
    /// The file isn't a Craft App's Windows MSI, or would replace a newer version of the app.
    pub const NOT_ALLOWED: i32 = -7;
    /// The install folder isn't one the app may get with administrator rights (see
    /// [`super::policy::folder_allowed`]).
    pub const FOLDER: i32 = -8;
    /// The app was running when its turn came: it was left out.
    pub const RUNNING: i32 = -9;
}

/// What the elevated helper allows, apart from the files' signatures: a Craft App's own Windows
/// MSI, no older than the version installed for the whole machine, into the folder where that
/// install already is or a new folder named after the app outside the Windows folder. Pure, so
/// the rules are tested on every platform.
pub mod policy {
    use std::path::{Component, Path};

    use artcraft_launcher_core::PackageKind;
    use artcraft_launcher_core::catalog::{CraftApp, SUITE};
    use artcraft_launcher_core::platform::parse_asset_name;
    use artcraft_launcher_core::version::{self, Version};

    /// The Craft App and version that a release MSI's file name names
    /// (`<stem>-<version>-windows-<arch>.msi`); `None` for any other file.
    pub fn craft_msi(file_name: &str) -> Option<(&'static CraftApp, Version)> {
        SUITE.iter().find_map(|app| {
            let stems: Vec<&str> = app.stems().collect();
            let parsed = parse_asset_name(file_name, &stems)?;
            if parsed.kind != PackageKind::Msi || !parsed.token.starts_with("windows-") {
                return None;
            }
            Some((app, version::parse(&parsed.version)?))
        })
    }

    /// Whether installing `new` keeps or raises the version installed for the whole machine.
    pub fn not_a_downgrade(new: &Version, installed: Option<&Version>) -> bool {
        installed.is_none_or(|old| !version::is_newer(old, new))
    }

    /// Whether `app`'s MSI may get `folder` as its `INSTALLFOLDER`. With no folder it installs
    /// where it always does. With one, it must be where the machine-wide install already is
    /// (`installed`) or, for a first install, a new folder named after the app, made only of
    /// plain parts, and outside the Windows folder (`windows`).
    pub fn folder_allowed(app: &CraftApp, folder: Option<&Path>, installed: Option<&Path>, windows: &Path) -> bool {
        let Some(folder) = folder else { return true };
        if let Some(installed) = installed {
            return key(folder) == key(installed);
        }
        let named = folder.file_name().and_then(|n| n.to_str()).is_some_and(|n| app.names().any(|a| a.eq_ignore_ascii_case(n)));
        let plain = folder.components().all(|c| matches!(c, Component::Prefix(_) | Component::RootDir | Component::Normal(_)));
        let windows = key(windows);
        let (path, inside) = (key(folder), format!("{windows}/"));
        named && plain && folder.is_absolute() && path != windows && !path.starts_with(&inside)
    }

    /// A path compared the way Windows compares them: case and separator blind, no trailing
    /// separator.
    fn key(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "/").trim_end_matches('/').to_lowercase()
    }
}

/// The command the batch's jobs share in the installer's queue.
pub fn command() -> Vec<OsString> {
    vec![OsString::from("msiexec"), OsString::from("/i")]
}

/// One MSI of a batch, as the helper gets it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// Absolute and canonical.
    pub msi: std::path::PathBuf,
    /// Lowercase hex SHA-256.
    pub sha256: String,
    /// `INSTALLFOLDER`, when the user chose one.
    pub folder: Option<std::path::PathBuf>,
}

/// What the helper is asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// Where it writes each MSI's result; must not exist yet.
    pub results: std::path::PathBuf,
    pub items: Vec<Item>,
}

/// The helper's arguments for `request` (without the program).
pub fn helper_args(request: &Request) -> Vec<OsString> {
    let mut args = vec![OsString::from(FLAG), request.results.clone().into_os_string()];
    for item in &request.items {
        args.push(item.msi.clone().into_os_string());
        args.push(OsString::from(&item.sha256));
        args.push(item.folder.clone().map_or_else(|| OsString::from(DEFAULT_FOLDER), std::path::PathBuf::into_os_string));
    }
    args
}

/// Reads [`helper_args`] back, refusing anything the launcher wouldn't have asked for.
pub fn parse_args(args: &[OsString]) -> Result<Request, String> {
    let [flag, results, rest @ ..] = args else { return Err("the arguments are incomplete".into()) };
    if flag != FLAG {
        return Err("not an MSI batch".into());
    }
    let results = std::path::PathBuf::from(results);
    if !results.is_absolute() || results.file_name().is_none() {
        return Err("the results file must be an absolute path".into());
    }
    if rest.is_empty() || !rest.len().is_multiple_of(3) || rest.len() / 3 > MOST {
        return Err(format!("expected 1 to {MOST} MSIs, each with its checksum and folder"));
    }
    let (items, []) = rest.as_chunks::<3>() else { return Err("an MSI is missing its checksum or folder".into()) };
    let items = items
        .iter()
        .map(|[msi, sha256, folder]| {
            let msi = std::path::PathBuf::from(msi);
            let is_msi = msi.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("msi"));
            if !msi.is_absolute() || !is_msi {
                return Err(format!("{} isn't an absolute path to an .msi file", msi.display()));
            }
            let sha256 = sha256.to_str().filter(|s| is_sha256(s)).ok_or("a checksum isn't lowercase hex SHA-256")?.to_string();
            let folder = if folder == DEFAULT_FOLDER {
                None
            } else {
                let folder = std::path::PathBuf::from(folder);
                if !folder.is_absolute() {
                    return Err(format!("{} isn't an absolute folder", folder.display()));
                }
                Some(folder)
            };
            Ok(Item { msi, sha256, folder })
        })
        .collect::<Result<Vec<_>, _>>()?;
    // Everything in one folder, the launcher's downloads: the results file is written beside them.
    if items.iter().any(|item| item.msi.parent() != results.parent()) {
        return Err("every MSI must be in the results file's folder".into());
    }
    Ok(Request { results, items })
}

/// Whether `s` is a lowercase hex SHA-256.
fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `args` as one Windows command line (the C runtime's quoting rules), for starting the helper
/// through the shell. `None` when an argument isn't Unicode or holds a NUL.
pub fn command_line(args: &[OsString]) -> Option<String> {
    let mut line = String::new();
    for arg in args {
        let arg = arg.to_str().filter(|a| !a.contains('\0'))?;
        if !line.is_empty() {
            line.push(' ');
        }
        if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{b}', '"']) {
            line.push_str(arg);
            continue;
        }
        line.push('"');
        let mut backslashes = 0usize;
        for c in arg.chars() {
            match c {
                '\\' => backslashes += 1,
                '"' => {
                    // Each backslash before a quote is doubled, and the quote escaped.
                    line.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                    line.push('"');
                    backslashes = 0;
                }
                c => {
                    line.extend(std::iter::repeat_n('\\', backslashes));
                    line.push(c);
                    backslashes = 0;
                }
            }
        }
        // Backslashes before the closing quote are doubled too.
        line.extend(std::iter::repeat_n('\\', backslashes * 2));
        line.push('"');
    }
    Some(line)
}

/// One line of the results file.
pub fn result_line(index: usize, code: i32) -> String {
    format!("{index} {code}")
}

/// Each MSI's code from the results file, for the `count` MSIs of the batch. `None` for one the
/// file doesn't mention (the helper stopped before it).
pub fn parse_results(text: &str, count: usize) -> Vec<Option<i32>> {
    let mut codes = vec![None; count];
    for line in text.lines() {
        let Some((index, code)) = line.trim().split_once(' ') else { continue };
        if let (Ok(index), Ok(code)) = (index.parse::<usize>(), code.parse::<i32>())
            && let Some(slot) = codes.get_mut(index)
        {
            *slot = Some(code);
        }
    }
    codes
}

/// What the helper's `code` for an MSI means. `log` is where its Windows Installer log is.
#[cfg(any(windows, test))]
pub fn explain(code: Option<i32>, log: &Path) -> Result<(), String> {
    match code {
        Some(code::CHECKSUM) => Err("the downloaded installer changed after it was checked, so it wasn't installed. Try again".into()),
        Some(code::UNSAFE_PATH) => {
            Err("the download folder's path goes through a link, so the installer wasn't run with administrator rights".into())
        }
        Some(code::NOT_STARTED) => Err("couldn't start the Windows Installer".into()),
        Some(code::UNSIGNED) => Err("the installer isn't signed by ArtCraft Launcher's publisher, so it wasn't installed".into()),
        Some(code::NOT_ALLOWED) => Err("the installer isn't a Craft App release this launcher may install here, so it wasn't installed".into()),
        Some(code::FOLDER) => Err("the chosen install folder can't be used for an install with administrator rights, so it wasn't installed".into()),
        Some(code::RUNNING) => Err("the app was opened while waiting for permission, so it wasn't updated. Quit it, then try again".into()),
        Some(code::INTERRUPTED) | None => Err("the Windows Installer was interrupted".into()),
        // Files were in use: the new version is only in place once Windows restarts.
        Some(3010 | 1641) => Err("installed, but Windows needs to restart to finish. Restart, then check for updates again".into()),
        Some(c @ (1602 | 0)) => crate::install::msi_outcome(Some(c)),
        Some(c) => crate::install::msi_outcome(Some(c)).map_err(|e| format!("{e}. Its log is at {}", log.display())),
    }
}

/// One outcome per job of a batch, in order.
#[cfg(any(windows, test))]
type Outcomes = Vec<Result<(), String>>;

/// Installs a batch of jobs, reporting its stages (see [`install`]).
#[cfg(any(windows, test))]
pub type Install<'a> = dyn Fn(&[Job], &dyn Fn(u32)) -> Outcomes + Sync + 'a;

/// An MSI job waiting in the installer's queue.
#[cfg(any(windows, test))]
pub struct Job {
    /// The app's name, for messages.
    pub name: String,
    /// What the helper is told to install.
    pub item: Item,
    /// The installed copy's program, to check again that it isn't running.
    pub running: Option<PathBuf>,
    /// Whether it may go to the elevated helper (see [`batchable`]); otherwise it installs the way
    /// it did before batches, with Windows Installer's own prompt.
    pub batchable: bool,
}

#[cfg(any(windows, test))]
impl Job {
    /// A job for a downloaded `msi` checked against `sha256`.
    pub fn new(name: String, msi: &Path, sha256: &str, folder: Option<&Path>, running: Option<PathBuf>) -> Result<Self, String> {
        let msi = crate::install::msi_path(msi)?;
        if let Some(folder) = folder {
            // Refused here, before anyone is asked for permission.
            crate::install::msi_folder_argument(folder)?;
        }
        let sha256 = sha256.to_ascii_lowercase();
        if !is_sha256(&sha256) {
            return Err(format!("{name}'s checksum isn't a SHA-256"));
        }
        Ok(Self { name, item: Item { msi, sha256, folder: folder.map(Path::to_path_buf) }, running, batchable: false })
    }
}

/// Puts `job` in `queue` and sees its batch through: `install` runs the batch for the job that
/// starts it. `phase` hears *Waiting* while other installs run or other MSIs are still
/// downloading, *Elevating* while the permission prompt is up, then *Installing*.
#[cfg(any(windows, test))]
pub fn join(
    queue: &Queue<Job>,
    ticket: Option<Ticket<'_, Job>>,
    job: Job,
    stop: &Stop,
    phase: &mut dyn FnMut(JobPhase),
    install: &Install<'_>,
) -> Result<(), String> {
    let at = |stage: u32| if stage == ASKING { JobPhase::Elevating } else { JobPhase::Installing };
    // Told about its turn while it waits, and about its stages while it makes the call.
    let phase = std::cell::RefCell::new(phase);
    let tell = |p: JobPhase| {
        if let Ok(mut phase) = phase.try_borrow_mut() {
            (*phase)(p);
        }
    };
    let cancelled = || stop.check().is_err_and(|e| e == CANCELLED);
    let mut step = |s: Step| {
        tell(match s {
            Step::Waiting => JobPhase::Waiting,
            Step::Running(stage) => at(stage),
        });
    };
    queue.batch(ticket, command(), job, &cancelled, &mut step, &|_, jobs, advance| {
        let report = |stage: u32| {
            advance(stage);
            tell(at(stage));
        };
        // The others heard the call start; the job making it hears it here.
        tell(at(ASKING));
        install(jobs, &report)
    })
}

/// Whether `item` is a Craft App release the helper may install, where it's asked to (see
/// [`policy`]): the machine-wide install's program (to check it isn't running) when it is, the
/// code to report otherwise. Reads only what an administrator controls: the machine-wide
/// Windows Installer registrations and the Windows folder from the registry. The launcher asks
/// before the prompt, the helper again after it.
#[cfg(windows)]
pub(crate) fn vet(item: &Item) -> Result<Option<PathBuf>, i32> {
    let name = item.msi.file_name().and_then(|n| n.to_str()).ok_or(code::NOT_ALLOWED)?;
    let (app, version) = policy::craft_msi(name).ok_or(code::NOT_ALLOWED)?;
    let installed = crate::detect::windows::machine_msi_installs(app).into_iter().next();
    if !policy::not_a_downgrade(&version, installed.as_ref().and_then(|m| m.version.as_ref())) {
        return Err(code::NOT_ALLOWED);
    }
    let exe = installed.and_then(|m| m.exe);
    let windows = crate::install::windows::system_root();
    if !policy::folder_allowed(app, item.folder.as_deref(), exe.as_deref().and_then(Path::parent), &windows) {
        return Err(code::FOLDER);
    }
    Ok(exe)
}

/// Whether `item` may go to the elevated helper: this launcher is signed by
/// [`crate::signing::PUBLISHER`], the MSI by the same signer, and [`vet`] allows it. Asked on the
/// job's own thread before it joins the installer's queue, so the signature checks never hold
/// up another install. Anything else installs the way it did before batches, with Windows
/// Installer's own prompt, which a per-user install with its own folder, say, still needs.
#[cfg(windows)]
pub fn batchable(item: &Item) -> bool {
    let Some(ours) = crate::signing::own_publisher() else { return false };
    crate::signing::publisher_of(&item.msi).as_ref() == Some(&ours) && vet(item).is_ok()
}

/// Installs a batch (see the module docs): leaves out apps that are running, asks once, runs the
/// helper and reads back each MSI's result. `report` hears [`ASKING`], then [`INSTALLING`] once
/// the helper has started.
///
/// Only [`batchable`] jobs, in one folder, go to the helper. Any other MSI installs the way it did
/// before batches: `msiexec` asks for permission itself, in Windows Installer's name, so the
/// launcher's name never vouches for it.
#[cfg(windows)]
pub fn install(jobs: &[Job], report: &dyn Fn(u32)) -> Outcomes {
    let mut outcomes: Vec<Option<Result<(), String>>> = jobs
        .iter()
        .map(|job| {
            job.running
                .as_deref()
                .filter(|exe| crate::process::is_running(exe))
                .map(|_| Err(format!("{} is running. Quit it, then try again.", job.name)))
        })
        .collect();
    let pending: Vec<usize> = outcomes.iter().enumerate().filter(|(_, o)| o.is_none()).map(|(at, _)| at).collect();
    let folder = pending.iter().filter_map(|at| jobs.get(*at)).find(|job| job.batchable).and_then(|job| job.item.msi.parent()).map(Path::to_path_buf);
    let (batch, alone): (Vec<usize>, Vec<usize>) =
        pending.iter().partition(|at| jobs.get(**at).is_some_and(|job| job.batchable && job.item.msi.parent() == folder.as_deref()));
    if !batch.is_empty()
        && let Some(folder) = &folder
    {
        install_batch(jobs, &batch, folder, report, &mut outcomes);
    }
    if !alone.is_empty() {
        report(INSTALLING);
    }
    for at in &alone {
        if let (Some(slot), Some(job)) = (outcomes.get_mut(*at), jobs.get(*at))
            && slot.is_none()
        {
            *slot = Some(crate::install::windows::run_msi_alone(&job.item.msi, job.item.folder.as_deref()));
        }
    }
    outcomes.into_iter().map(|o| o.unwrap_or_else(|| Err("the Windows Installer was interrupted".into()))).collect()
}

/// Runs the elevated helper for the jobs at `batch`, all in `folder`, and fills in their
/// outcomes (see [`install`]).
#[cfg(windows)]
fn install_batch(jobs: &[Job], batch: &[usize], folder: &Path, report: &dyn Fn(u32), outcomes: &mut [Option<Result<(), String>>]) {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let results = folder.join(format!("msi-batch-{}-{nanos}.txt", std::process::id()));
    let _ = std::fs::remove_file(&results);
    let request = Request { results: results.clone(), items: batch.iter().filter_map(|at| jobs.get(*at)).map(|job| job.item.clone()).collect() };
    report(ASKING);
    log::info!("installing {} MSI(s) behind one permission prompt", request.items.len());
    let mut set = |at: usize, outcome: Result<(), String>| {
        if let Some(slot) = outcomes.get_mut(at) {
            *slot = Some(outcome);
        }
    };
    match elevated::run(&request, &|| report(INSTALLING)) {
        elevated::Ran::Declined => batch.iter().for_each(|at| set(*at, Err(CANCELLED.into()))),
        elevated::Ran::Unavailable(why) => {
            log::warn!("couldn't ask once for the whole batch ({why}); each install asks for itself");
            report(INSTALLING);
            for at in batch {
                if let Some(job) = jobs.get(*at) {
                    set(*at, crate::install::windows::run_msi_alone(&job.item.msi, job.item.folder.as_deref()));
                }
            }
        }
        elevated::Ran::Exited(exit) => {
            let text = std::fs::read_to_string(&results).unwrap_or_default();
            let codes = parse_results(&text, batch.len());
            for (code, at) in codes.into_iter().zip(batch) {
                let Some(job) = jobs.get(*at) else { continue };
                let log = crate::install::msi_log(&job.item.msi);
                set(
                    *at,
                    match (code, exit) {
                        (Some(code), _) => explain(Some(code), &log),
                        (None, Some(0)) => explain(None, &log),
                        (None, Some(exit)) if exit == i32::from(elevated::EXIT_UNSIGNED) => {
                            Err("this copy of ArtCraft Launcher isn't signed, so it didn't install anything".into())
                        }
                        (None, Some(exit)) => Err(format!("the installer helper stopped with error {exit}")),
                        (None, None) => Err("lost track of the installer helper. Check whether the app was installed, then try again".into()),
                    },
                );
            }
        }
    }
    let _ = std::fs::remove_file(&results);
}

/// Starting the helper with administrator rights, and the helper itself.
#[cfg(windows)]
mod elevated {
    use std::fs::{File, OpenOptions};
    use std::io::{Read, Write};
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use super::{Item, Request, code, command_line, helper_args, result_line, vet};
    use crate::signing;

    /// `FILE_SHARE_READ`: others may read the file while it's open, but not change it.
    const FILE_SHARE_READ: u32 = 0x1;
    /// `ERROR_CANCELLED`: the user declined the UAC prompt.
    const DECLINED: i32 = 1223;
    /// What the starter script exits with when it couldn't show the prompt at all. The helper
    /// never exits with it.
    const NO_PROMPT: i32 = 1;
    /// The helper ran the batch; the results file has each MSI's code.
    pub const EXIT_DONE: u8 = 0;
    /// The helper's arguments were refused; nothing ran.
    pub const EXIT_ARGUMENTS: u8 = 2;
    /// The results file couldn't be created or written.
    pub const EXIT_RESULTS: u8 = 3;
    /// The helper itself isn't signed, so it has no publisher to hold the MSIs to; nothing ran.
    pub const EXIT_UNSIGNED: u8 = 4;

    /// The environment variable [`SCRIPT`] reads the program to start from.
    const EXE_VAR: &str = "ARTCRAFT_LAUNCHER_ELEVATE_EXE";
    /// The environment variable [`SCRIPT`] reads the program's command line from.
    const ARGS_VAR: &str = "ARTCRAFT_LAUNCHER_ELEVATE_ARGS";

    /// Starts `$env:ARTCRAFT_LAUNCHER_ELEVATE_EXE` with `$env:ARTCRAFT_LAUNCHER_ELEVATE_ARGS` as
    /// administrator (the UAC prompt names that program) and exits with its exit code: 1223
    /// when the prompt was declined, 1 when it couldn't be shown. The paths travel in the
    /// environment, so nothing in them is ever read as PowerShell.
    const SCRIPT: &str = "$ErrorActionPreference = 'Stop'; \
        $i = New-Object System.Diagnostics.ProcessStartInfo; \
        $i.FileName = $env:ARTCRAFT_LAUNCHER_ELEVATE_EXE; \
        $i.Arguments = $env:ARTCRAFT_LAUNCHER_ELEVATE_ARGS; \
        $i.Verb = 'runas'; $i.UseShellExecute = $true; $i.WindowStyle = 'Hidden'; \
        try { $p = [System.Diagnostics.Process]::Start($i) } catch { \
            $e = $_.Exception; \
            while ($e) { if ($e -is [System.ComponentModel.Win32Exception] -and $e.NativeErrorCode -eq 1223) { exit 1223 }; $e = $e.InnerException }; \
            exit 1 }; \
        if (-not $p) { exit 1 }; \
        $p.WaitForExit(); exit $p.ExitCode";

    /// How the elevated helper's run went.
    pub enum Ran {
        /// The user declined the prompt; nothing ran.
        Declined,
        /// The prompt couldn't be shown; nothing ran, so each MSI may still ask for itself.
        Unavailable(String),
        /// The helper ran (or may have run) and exited with this code, `None` when the launcher
        /// lost track of it. The results file says the rest; no MSI of the batch is installed
        /// again any other way.
        Exited(Option<i32>),
    }

    /// Windows PowerShell, from the folder the machine's registry names.
    fn powershell() -> PathBuf {
        crate::install::windows::system_root().join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
    }

    /// Asks once and runs the helper for `request`, waiting for it. `started` is called once
    /// the helper is running with administrator rights (it has created the results file).
    pub fn run(request: &Request, started: &dyn Fn()) -> Ran {
        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(e) => return Ran::Unavailable(format!("couldn't find this program ({e})")),
        };
        let Some(line) = command_line(&helper_args(request)) else { return Ran::Unavailable("a path has unusual characters".into()) };
        let mut command = crate::process::command(powershell());
        command
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", SCRIPT])
            .env(EXE_VAR, &exe)
            .env(ARGS_VAR, &line)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => return Ran::Unavailable(format!("couldn't start PowerShell ({e})")),
        };
        let mut told = false;
        // Once the helper has created the results file it has started installing: whatever
        // happens next, nothing falls back to installing the batch a second way.
        let mut helper_started = || {
            if !told && request.results.exists() {
                told = true;
                started();
            }
            told
        };
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    helper_started();
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(e) if helper_started() => {
                    log::warn!("lost track of the elevated installer helper ({e})");
                    return Ran::Exited(None);
                }
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ran::Unavailable(format!("lost track of PowerShell ({e})"));
                }
            }
        };
        match status.code() {
            _ if helper_started() => Ran::Exited(status.code()),
            Some(DECLINED) => Ran::Declined,
            Some(NO_PROMPT) | None => Ran::Unavailable(format!("PowerShell couldn't show the prompt ({status})")),
            Some(code) => Ran::Exited(Some(code)),
        }
    }

    /// Whether `path` is already its own canonical form: no links, junctions or `..` on the way.
    fn plain(path: &Path) -> bool {
        dunce::canonicalize(path).is_ok_and(|canonical| canonical == path)
    }

    /// The helper, running as administrator: installs every MSI of `request` and writes each
    /// one's code to the results file. Returns its exit code.
    pub fn helper(request: &Request) -> u8 {
        // Anyone can start this program with these arguments and have Windows ask for
        // permission in its name. So it installs nothing its own publisher didn't sign.
        let Some(publisher) = signing::own_publisher() else {
            log::error!("this copy of ArtCraft Launcher isn't signed, so it installs nothing with administrator rights");
            return EXIT_UNSIGNED;
        };
        // Every MSI is opened first, refusing writes, renames and deletes. While a file is open
        // the folders above it can't be renamed or replaced either, so the results file and the
        // logs created beside the MSIs below land where the paths say.
        let sources: Vec<Result<File, i32>> = request.items.iter().map(open_source).collect();
        if !request.results.parent().is_some_and(plain) {
            log::error!("the results file's folder isn't a plain path");
            return EXIT_RESULTS;
        }
        let mut results = match OpenOptions::new().write(true).create_new(true).open(&request.results) {
            Ok(file) => file,
            Err(e) => {
                log::error!("couldn't create the results file ({e})");
                return EXIT_RESULTS;
            }
        };
        // Removed however the helper ends.
        let staging = staging_folder();
        let msiexec = crate::install::windows::msiexec();
        for (index, (item, source)) in request.items.iter().zip(sources).enumerate() {
            let code = match (vet(item), source, &staging) {
                (Err(code), _, _) | (Ok(_), Err(code), _) => code,
                (Ok(_), Ok(_), None) => code::NOT_STARTED,
                // Looked at again now: the prompt may have been up for a while.
                (Ok(Some(exe)), Ok(_), Some(_)) if crate::process::is_running(&exe) => code::RUNNING,
                (Ok(_), Ok(source), Some(staging)) => install_one(index, item, source, &staging.0, &msiexec, &publisher),
            };
            if writeln!(results, "{}", result_line(index, code)).and_then(|()| results.flush()).is_err() {
                return EXIT_RESULTS;
            }
        }
        EXIT_DONE
    }

    /// Opens a downloaded MSI so no one can write, rename or delete it while it's open.
    pub fn open_source(item: &Item) -> Result<File, i32> {
        if !plain(&item.msi) {
            return Err(code::UNSAFE_PATH);
        }
        OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&item.msi).map_err(|_| code::NOT_STARTED)
    }

    /// The batch's staging folder, removed with everything in it when this goes, however the
    /// helper ends: what's in it only administrators could clean up.
    pub struct Staging(pub PathBuf);

    impl Drop for Staging {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A new folder of this batch's own in the Windows temporary folder (see [`staging_in`]).
    fn staging_folder() -> Option<Staging> {
        staging_in(&dunce::canonicalize(crate::install::windows::system_root().join("Temp")).ok()?)
    }

    /// A new folder in `parent`, under a random name, that only administrators and the system
    /// can open: nothing in it can be read, changed or added by the user's account. `None` (and
    /// nothing staged) when its access can't be set.
    pub fn staging_in(parent: &Path) -> Option<Staging> {
        let mut name = [0u8; 16];
        getrandom::fill(&mut name).ok()?;
        let staging = Staging(parent.join(format!("artcraft-launcher-msi-{}", crate::http::hex(&name))));
        std::fs::create_dir(&staging.0).ok()?;
        if lock_down(&staging.0) {
            Some(staging)
        } else {
            log::error!("couldn't restrict the staging folder to administrators");
            None
        }
    }

    /// Replaces `folder`'s access list with full control for Administrators and SYSTEM only, for
    /// it and everything created in it, nothing inherited (Windows' own `icacls`, from the folder
    /// the machine's registry names).
    pub fn lock_down(folder: &Path) -> bool {
        let icacls = crate::install::windows::system_root().join(r"System32\icacls.exe");
        crate::process::command(icacls)
            .arg(folder)
            .args(["/inheritance:r", "/grant:r", "*S-1-5-32-544:(OI)(CI)F", "*S-1-5-18:(OI)(CI)F", "/Q"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    /// Copies the checked MSI into the staging folder and installs that copy without Windows
    /// Installer windows; its code for the results. The copy's SHA-256 is computed from the bytes
    /// written, so what msiexec installs is exactly what was checked, wherever the download's
    /// path leads afterwards.
    fn install_one(index: usize, item: &Item, mut source: File, staging: &Path, msiexec: &Path, publisher: &verifysign::Name) -> i32 {
        let Some(name) = item.msi.file_name() else { return code::UNSAFE_PATH };
        let folder = staging.join(index.to_string());
        if std::fs::create_dir(&folder).is_err() {
            return code::NOT_STARTED;
        }
        let staged = folder.join(name);
        match stage(&mut source, &staged) {
            Some(sha) if sha == item.sha256 => {}
            Some(_) => return code::CHECKSUM,
            None => return code::NOT_STARTED,
        }
        // Held while msiexec reads it: no one may change it in the meantime.
        let Ok(held) = OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&staged) else { return code::NOT_STARTED };
        // Checked on the copy msiexec will read, held unchanged until it has: an authorized
        // Craft release carries this launcher's own publisher's valid signature.
        if signing::publisher_of(&staged).as_ref() != Some(publisher) {
            return code::UNSIGNED;
        }
        let staged_log = folder.join("install.log");
        // The log the user reads, beside the download: created new now (a file or link left there
        // goes first; removing a link never touches what it points to), and filled from the
        // staged log through this handle once msiexec is done.
        let user_log = crate::install::msi_log(&item.msi);
        let _ = std::fs::remove_file(&user_log);
        let mut kept_log = OpenOptions::new().write(true).create_new(true).share_mode(FILE_SHARE_READ).open(&user_log).ok();
        let mut command = crate::process::command(msiexec);
        command.arg("/i").arg(&staged).args(["/qn", "/norestart", "/l*v"]).arg(&staged_log);
        if let Some(folder) = &item.folder {
            match crate::install::msi_folder_argument(folder) {
                Ok(argument) => {
                    command.raw_arg(argument);
                }
                Err(_) => return code::UNSAFE_PATH,
            }
        }
        let code = match command.status() {
            Ok(status) => status.code().unwrap_or(code::INTERRUPTED),
            Err(_) => code::NOT_STARTED,
        };
        drop(held);
        if let (Some(out), Ok(mut log)) = (kept_log.as_mut(), File::open(&staged_log)) {
            let _ = std::io::copy(&mut log, out);
        }
        code
    }

    /// Copies `source` into a new file at `staged`, which no one else may open meanwhile, and
    /// returns the lowercase hex SHA-256 of the bytes written.
    pub fn stage(source: &mut File, staged: &Path) -> Option<String> {
        use sha2::{Digest, Sha256};
        let mut out = OpenOptions::new().write(true).create_new(true).share_mode(0).open(staged).ok()?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1 << 16];
        loop {
            let read = source.read(&mut buffer).ok()?;
            if read == 0 {
                break;
            }
            let chunk = buffer.get(..read)?;
            hasher.update(chunk);
            out.write_all(chunk).ok()?;
        }
        out.flush().ok()?;
        Some(crate::http::hex(&hasher.finalize()))
    }

    /// For [`super::helper`]: the arguments' own error, logged.
    pub fn refuse(why: &str) -> u8 {
        log::error!("the MSI batch's arguments were refused: {why}");
        EXIT_ARGUMENTS
    }
}

/// Runs the helper when this program was started as one (`args` without the program name):
/// `None` when they don't start with [`FLAG`], otherwise its exit code.
#[cfg(windows)]
pub fn helper(args: &[OsString]) -> Option<u8> {
    if args.first().is_none_or(|a| a != FLAG) {
        return None;
    }
    Some(match parse_args(args) {
        Ok(request) => elevated::helper(&request),
        Err(why) => elevated::refuse(&why),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::mpsc;

    use super::*;

    fn sha(c: char) -> String {
        std::iter::repeat_n(c, 64).collect()
    }

    fn abs(p: &str) -> PathBuf {
        if cfg!(windows) { PathBuf::from(format!("C:\\{}", p.replace('/', "\\"))) } else { PathBuf::from(format!("/{p}")) }
    }

    fn request() -> Request {
        Request {
            results: abs("cache/downloads/msi-batch-1-2.txt"),
            items: vec![
                Item { msi: abs("cache/downloads/photocraft-0.5.0-windows-x64.msi"), sha256: sha('a'), folder: None },
                Item { msi: abs("cache/downloads/filmcraft-1.2.0-windows-x64.MSI"), sha256: sha('0'), folder: Some(abs("Apps/FilmCraft")) },
            ],
        }
    }

    #[test]
    fn the_helpers_arguments_read_back_as_they_were_written() {
        let request = request();
        assert_eq!(parse_args(&helper_args(&request)), Ok(request));
    }

    #[test]
    fn the_helper_refuses_anything_the_launcher_would_not_ask_for() {
        let good = helper_args(&request());
        let with = |at: usize, value: &str| {
            let mut args = good.clone();
            args[at] = OsString::from(value);
            parse_args(&args)
        };
        assert!(with(0, "--finish-update").is_err());
        assert!(with(1, "results.txt").is_err(), "a relative results file");
        assert!(with(2, "photocraft.msi").is_err(), "a relative MSI");
        assert!(with(2, &abs("cache/downloads/photocraft.exe").display().to_string()).is_err(), "not an MSI");
        assert!(with(3, &sha('A')).is_err(), "upper-case checksum");
        assert!(with(3, &sha('g')).is_err(), "not hex");
        assert!(with(3, "abc").is_err(), "too short");
        assert!(with(7, "Apps/FilmCraft").is_err(), "a relative folder");
        assert!(parse_args(&good[..good.len() - 1]).is_err(), "an MSI without its folder");
        assert!(parse_args(&good[..2]).is_err(), "no MSIs");
        assert!(with(2, &abs("elsewhere/photocraft-0.5.0-windows-x64.msi").display().to_string()).is_err(), "not beside the results file");
        let item = Item { msi: abs("d/a.msi"), sha256: sha('b'), folder: None };
        let too_many = Request { results: abs("d/r.txt"), items: vec![item; MOST + 1] };
        assert!(parse_args(&helper_args(&too_many)).is_err());
    }

    #[test]
    fn only_a_craft_apps_own_windows_msi_is_allowed() {
        let named = |name: &str| policy::craft_msi(name).map(|(app, v)| (app.id, v.to_string()));
        assert_eq!(named("photocraft-0.5.0-windows-x64.msi"), Some(("photocraft", "0.5.0".to_string())));
        assert_eq!(named("filmcraft-1.2.0-windows-arm64.msi").map(|(id, _)| id), Some("filmcraft"));
        assert_eq!(named("photocraft-0.5.0-windows-x64-portable.zip"), None, "not an MSI");
        assert_eq!(named("photocraft-0.5.0-linux-x86_64.msi"), None, "not a Windows build");
        assert_eq!(named("evilcraft-1.0.0-windows-x64.msi"), None, "not a Craft App");
        assert_eq!(named("setup.msi"), None);
        assert_eq!(named("artcraft-launcher-0.7.0-windows-x64.msi"), None, "the launcher updates itself another way");
    }

    #[test]
    fn an_older_release_never_replaces_a_newer_install() {
        let v = |s: &str| artcraft_launcher_core::version::parse(s).unwrap();
        assert!(policy::not_a_downgrade(&v("0.5.0"), None));
        assert!(policy::not_a_downgrade(&v("0.5.0"), Some(&v("0.4.9"))));
        assert!(policy::not_a_downgrade(&v("0.5.0"), Some(&v("0.5.0"))), "a repair of the same version");
        assert!(!policy::not_a_downgrade(&v("0.4.0"), Some(&v("0.5.0"))));
    }

    #[test]
    fn an_install_folder_is_the_existing_one_or_a_new_one_named_after_the_app() {
        let app = artcraft_launcher_core::catalog::CraftApp::by_id("photocraft").unwrap();
        let windows = abs("Windows");
        let allowed = |folder: Option<&str>, installed: Option<&str>| {
            policy::folder_allowed(app, folder.map(abs).as_deref(), installed.map(abs).as_deref(), &windows)
        };
        assert!(allowed(None, None), "where it always installs");
        assert!(allowed(None, Some("Program Files/PhotoCraft")));
        assert!(allowed(Some("Apps/PhotoCraft"), None));
        assert!(allowed(Some("Apps/photocraft"), None), "case doesn't matter on Windows");
        assert!(allowed(Some("Program Files/PhotoCraft"), Some("program files/photocraft")));
        assert!(!allowed(Some("Apps/PhotoCraft"), Some("Program Files/PhotoCraft")), "not beside the existing install");
        assert!(!allowed(Some("Apps/FilmCraft"), None), "another app's name");
        assert!(!allowed(Some("Apps"), None), "not a folder of its own");
        assert!(!allowed(Some("Windows/System32/PhotoCraft"), None), "inside the Windows folder");
        assert!(!allowed(Some("Windows/PhotoCraft"), None));
        assert!(!allowed(Some("Apps/../Windows/PhotoCraft"), None), "a path that climbs out");
        assert!(!policy::folder_allowed(app, Some(Path::new("PhotoCraft")), None, &windows), "a relative folder");
    }

    #[test]
    fn the_command_line_quotes_like_the_c_runtime_reads_it() {
        let line = |args: &[&str]| command_line(&args.iter().map(OsString::from).collect::<Vec<_>>());
        assert_eq!(line(&["--install-msis", r"C:\Users\A B\x.msi", ""]).as_deref(), Some(r#"--install-msis "C:\Users\A B\x.msi" """#));
        assert_eq!(line(&[r"C:\a b\"]).as_deref(), Some(r#""C:\a b\\""#));
        assert_eq!(line(&[r#"say "hi""#]).as_deref(), Some(r#""say \"hi\"""#));
        assert_eq!(line(&[r#"a\"b c"#]).as_deref(), Some(r#""a\\\"b c""#));
        assert_eq!(line(&[r"C:\plain\path.msi"]).as_deref(), Some(r"C:\plain\path.msi"));
        assert_eq!(line(&["nul\0byte"]), None);
    }

    #[test]
    fn results_are_read_per_msi_and_gaps_stay_unknown() {
        let text = [result_line(0, 0), result_line(2, 1603), "garbage".into(), result_line(9, 0)].join("\n");
        assert_eq!(parse_results(&text, 3), [Some(0), None, Some(1603)]);
    }

    #[test]
    fn each_result_says_what_happened() {
        let log = abs("d/a.log");
        assert_eq!(explain(Some(0), &log), Ok(()));
        assert!(explain(Some(3010), &log).is_err_and(|e| e.contains("restart")), "not called a success while the old files stay");
        assert!(explain(Some(code::FOLDER), &log).is_err_and(|e| e.contains("install folder")));
        assert!(explain(Some(code::RUNNING), &log).is_err_and(|e| e.contains("Quit it")));
        assert_eq!(explain(Some(1602), &log), Err(CANCELLED.to_string()));
        assert!(explain(Some(1603), &log).is_err_and(|e| e.contains("1603") && e.contains(&log.display().to_string())));
        assert!(explain(Some(code::CHECKSUM), &log).is_err_and(|e| e.contains("changed")));
        assert!(explain(Some(code::UNSIGNED), &log).is_err_and(|e| e.contains("isn't signed")));
        assert!(explain(Some(code::NOT_ALLOWED), &log).is_err_and(|e| e.contains("wasn't installed")));
        assert!(explain(None, &log).is_err_and(|e| e.contains("interrupted")));
    }

    #[test]
    fn a_job_refuses_a_bad_checksum_or_folder_before_anyone_is_asked() {
        let dir = tempfile::tempdir().unwrap();
        let msi = dir.path().join("a.msi");
        std::fs::write(&msi, b"package").unwrap();
        let job = Job::new("PhotoCraft".into(), &msi, &sha('A'), None, None).unwrap();
        assert_eq!(job.item.sha256, sha('a'), "kept lower-case");
        assert!(Job::new("PhotoCraft".into(), &msi, "nope", None, None).is_err());
        assert!(Job::new("PhotoCraft".into(), &msi, &sha('a'), Some(Path::new("bad\nfolder")), None).is_err());
        assert!(Job::new("PhotoCraft".into(), &dir.path().join("missing.msi"), &sha('a'), None, None).is_err());
    }

    /// Jobs started together, on threads, through [`join`]: returns each one's outcome and the
    /// phases it heard, and every batch `install` was called with (the names).
    fn run_together(names: &[&str], install: &Install<'_>) -> Vec<(Result<(), String>, Vec<JobPhase>)> {
        let queue: Queue<Job> = Queue::new();
        let dir = tempfile::tempdir().unwrap();
        std::thread::scope(|scope| {
            let queue = &queue;
            // Every job says it's coming before any download is ready.
            let tickets: Vec<_> = names.iter().map(|_| queue.expect(command())).collect();
            let (ready, readies) = mpsc::channel::<()>();
            let handles: Vec<_> = names
                .iter()
                .zip(tickets)
                .enumerate()
                .map(|(n, (name, ticket))| {
                    let msi = dir.path().join(format!("{name}.msi"));
                    std::fs::write(&msi, name.as_bytes()).unwrap();
                    let ready = ready.clone();
                    scope.spawn(move || {
                        // The downloads finish one after another.
                        std::thread::sleep(std::time::Duration::from_millis(40 * n as u64));
                        let job = Job::new(name.to_string(), &msi, &sha('c'), None, None).unwrap();
                        let mut heard = Vec::new();
                        let result = join(queue, Some(ticket), job, &Stop::default(), &mut |p| heard.push(p), install);
                        ready.send(()).unwrap();
                        (result, heard)
                    })
                })
                .collect();
            drop(ready);
            let _ = readies.iter().count();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        })
    }

    #[test]
    fn update_all_installs_every_msi_in_one_batch_behind_one_prompt() {
        let batches = Mutex::new(Vec::new());
        let install = |jobs: &[Job], report: &dyn Fn(u32)| {
            batches.lock().unwrap().push(jobs.iter().map(|j| j.name.clone()).collect::<Vec<_>>());
            // The prompt is up, then answered.
            std::thread::sleep(std::time::Duration::from_millis(150));
            report(INSTALLING);
            std::thread::sleep(std::time::Duration::from_millis(150));
            jobs.iter().map(|j| if j.name == "FilmCraft" { Err("the Windows Installer stopped with error 1603".into()) } else { Ok(()) }).collect()
        };
        let results = run_together(&["PhotoCraft", "FilmCraft", "LightCraft"], &install);
        assert_eq!(batches.lock().unwrap().len(), 1, "one batch, so one prompt");
        assert_eq!(batches.lock().unwrap()[0].len(), 3);
        let outcomes: Vec<_> = results.iter().map(|(r, _)| r.clone()).collect();
        assert_eq!(outcomes, [Ok(()), Err("the Windows Installer stopped with error 1603".into()), Ok(())], "one failed app doesn't fail the others");
        for (_, heard) in &results {
            assert!(heard.contains(&JobPhase::Elevating), "{heard:?}");
            assert_eq!(heard.last(), Some(&JobPhase::Installing), "{heard:?}");
            let asked = heard.iter().position(|p| *p == JobPhase::Elevating);
            let installing = heard.iter().position(|p| *p == JobPhase::Installing);
            assert!(asked < installing, "{heard:?}");
        }
    }

    #[test]
    fn a_declined_prompt_cancels_the_whole_batch() {
        let install = |jobs: &[Job], _: &dyn Fn(u32)| jobs.iter().map(|_| Err(CANCELLED.to_string())).collect();
        let results = run_together(&["PhotoCraft", "FilmCraft"], &install);
        assert!(results.iter().all(|(r, _)| *r == Err(CANCELLED.to_string())), "{results:?}");
    }

    /// The elevated helper's file handling, on the Windows CI runner (which runs as an
    /// administrator, like the helper).
    #[cfg(windows)]
    mod elevated_files {
        use super::super::elevated::{lock_down, open_source, stage, staging_in};
        use super::*;

        #[test]
        fn a_download_held_for_the_batch_cant_be_changed_renamed_or_deleted() {
            let dir = tempfile::tempdir().unwrap();
            let msi = dunce::canonicalize(dir.path()).unwrap().join("photocraft-0.5.0-windows-x64.msi");
            std::fs::write(&msi, b"package").unwrap();
            let item = Item { msi: msi.clone(), sha256: sha('a'), folder: None };
            let held = open_source(&item).unwrap();
            assert!(std::fs::OpenOptions::new().write(true).open(&msi).is_err(), "written");
            assert!(std::fs::rename(&msi, msi.with_extension("old")).is_err(), "renamed");
            assert!(std::fs::remove_file(&msi).is_err(), "deleted");
            assert!(std::fs::read(&msi).is_ok(), "but still readable");
            drop(held);
            assert!(std::fs::remove_file(&msi).is_ok());
        }

        #[test]
        fn a_path_through_a_link_isnt_opened() {
            let dir = tempfile::tempdir().unwrap();
            let item = Item { msi: dir.path().join("sub").join("..").join("a.msi"), sha256: sha('a'), folder: None };
            assert_eq!(open_source(&item).err(), Some(code::UNSAFE_PATH));
        }

        #[test]
        fn the_staged_copy_is_exactly_the_bytes_hashed() {
            use sha2::{Digest, Sha256};
            let dir = tempfile::tempdir().unwrap();
            let msi = dunce::canonicalize(dir.path()).unwrap().join("photocraft-0.5.0-windows-x64.msi");
            let body: Vec<u8> = (0..200_000u32).map(|n| (n % 251) as u8).collect();
            std::fs::write(&msi, &body).unwrap();
            let mut source = open_source(&Item { msi, sha256: sha('a'), folder: None }).unwrap();
            let staged = dir.path().join("staged.msi");
            let sha = stage(&mut source, &staged).unwrap();
            assert_eq!(sha, crate::http::hex(&Sha256::digest(&body)));
            assert_eq!(std::fs::read(&staged).unwrap(), body);
            assert!(stage(&mut source, &staged).is_none(), "never over an existing file");
        }

        #[test]
        fn the_staging_folder_is_locked_down_and_goes_however_the_helper_ends() {
            let dir = tempfile::tempdir().unwrap();
            let staging = staging_in(&dunce::canonicalize(dir.path()).unwrap()).expect("icacls restricts the folder");
            let folder = staging.0.clone();
            assert!(folder.is_dir());
            std::fs::write(folder.join("left over.msi"), b"package").unwrap();
            // Not inherited: only what lock_down granted.
            let acl = std::process::Command::new(crate::install::windows::system_root().join(r"System32\icacls.exe")).arg(&folder).output().unwrap();
            let acl = String::from_utf8_lossy(&acl.stdout).to_string();
            assert!(!acl.contains("(I)"), "{acl}");
            assert!(lock_down(&folder));
            drop(staging);
            assert!(!folder.exists(), "removed with what was in it");
        }
    }
}
