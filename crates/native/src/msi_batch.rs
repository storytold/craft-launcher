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
//! Everything the helper does runs as administrator, so it trusts nothing that can change after
//! the prompt:
//!
//! - Paths and checksums come only from its command line, fixed when it starts.
//! - Each MSI path must already be its own canonical form (no links on the way). Every MSI is
//!   opened first, with sharing that refuses writes, renames and deletes, which also keeps the
//!   folders above it from being renamed or replaced while the batch runs.
//! - Each MSI is copied through that handle into a new folder of the batch's own, under a random
//!   name in the Windows temporary folder (the user's account can neither list it nor change
//!   what is created in it), and the SHA-256 is computed from the bytes copied. `msiexec` installs
//!   that copy, held open against changes, so what installs is exactly what was checked,
//!   whatever the download's path leads to afterwards. The copy goes when the batch is done.
//! - The results file and each MSI's log beside the download are created new (never an existing
//!   file or link). `msiexec` writes its log in the staging folder; the helper copies it into the
//!   new log through its own handle.
//! - `msiexec` and PowerShell are found through the machine's registry, not the user's
//!   environment.
//! - It takes only absolute `.msi` paths and lowercase hex checksums, at most [`MOST`] of them,
//!   and exits when the batch is done.
//!
//! Apps that are running are left out of the batch: the launcher checks again just before it
//! asks for permission, so `/qn` never leaves files waiting for a reboot. When the prompt can't
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
        Some(code::INTERRUPTED) | None => Err("the Windows Installer was interrupted".into()),
        Some(c @ (1602 | 0 | 3010 | 1641)) => crate::install::msi_outcome(Some(c)),
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
        Ok(Self { name, item: Item { msi, sha256, folder: folder.map(Path::to_path_buf) }, running })
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

/// Installs a batch (see the module docs): leaves out apps that are running, asks once, runs the
/// helper and reads back each MSI's result. `report` hears [`ASKING`], then [`INSTALLING`] once
/// the helper has started.
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
    let Some(folder) = pending.first().and_then(|at| jobs.get(*at)).and_then(|job| job.item.msi.parent()) else {
        return outcomes.into_iter().map(|o| o.unwrap_or_else(|| Err(CANCELLED.into()))).collect();
    };
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let results = folder.join(format!("msi-batch-{}-{nanos}.txt", std::process::id()));
    let _ = std::fs::remove_file(&results);
    let request = Request { results: results.clone(), items: pending.iter().filter_map(|at| jobs.get(*at)).map(|job| job.item.clone()).collect() };
    report(ASKING);
    log::info!("installing {} MSI(s) behind one permission prompt", request.items.len());
    match elevated::run(&request, &|| report(INSTALLING)) {
        elevated::Ran::Declined => {
            for at in &pending {
                if let Some(slot) = outcomes.get_mut(*at) {
                    *slot = Some(Err(CANCELLED.into()));
                }
            }
        }
        elevated::Ran::Unavailable(why) => {
            log::warn!("couldn't ask once for the whole batch ({why}); each install asks for itself");
            report(INSTALLING);
            for at in &pending {
                if let (Some(slot), Some(job)) = (outcomes.get_mut(*at), jobs.get(*at)) {
                    *slot = Some(crate::install::windows::run_msi_alone(&job.item.msi, job.item.folder.as_deref()));
                }
            }
        }
        elevated::Ran::Exited(exit) => {
            let text = std::fs::read_to_string(&results).unwrap_or_default();
            let codes = parse_results(&text, pending.len());
            for (code, at) in codes.into_iter().zip(&pending) {
                let (Some(slot), Some(job)) = (outcomes.get_mut(*at), jobs.get(*at)) else { continue };
                let outcome = match (code, exit) {
                    (Some(code), _) => explain(Some(code), &crate::install::msi_log(&job.item.msi)),
                    (None, Some(0)) => explain(None, &crate::install::msi_log(&job.item.msi)),
                    (None, Some(exit)) => Err(format!("the installer helper stopped with error {exit}")),
                    (None, None) => Err("lost track of the installer helper. Check whether the app was installed, then try again".into()),
                };
                *slot = Some(outcome);
            }
        }
    }
    let _ = std::fs::remove_file(&results);
    outcomes.into_iter().map(|o| o.unwrap_or_else(|| Err("the Windows Installer was interrupted".into()))).collect()
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

    use super::{Item, Request, code, command_line, helper_args, result_line};

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
        let staging = staging_folder();
        let msiexec = crate::install::windows::msiexec();
        for (index, (item, source)) in request.items.iter().zip(sources).enumerate() {
            let code = match (source, &staging) {
                (Ok(source), Some(staging)) => install_one(index, item, source, staging, &msiexec),
                (Ok(_), None) => code::NOT_STARTED,
                (Err(code), _) => code,
            };
            if writeln!(results, "{}", result_line(index, code)).and_then(|()| results.flush()).is_err() {
                return EXIT_RESULTS;
            }
        }
        if let Some(staging) = staging {
            let _ = std::fs::remove_dir_all(staging);
        }
        EXIT_DONE
    }

    /// Opens a downloaded MSI so no one can write, rename or delete it while it's open.
    fn open_source(item: &Item) -> Result<File, i32> {
        if !plain(&item.msi) {
            return Err(code::UNSAFE_PATH);
        }
        OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&item.msi).map_err(|_| code::NOT_STARTED)
    }

    /// A new folder of this batch's own in the Windows temporary folder, under a random name: the
    /// user's account can't list that folder, nor change the files created in it.
    fn staging_folder() -> Option<PathBuf> {
        let mut name = [0u8; 16];
        getrandom::fill(&mut name).ok()?;
        let temp = dunce::canonicalize(crate::install::windows::system_root().join("Temp")).ok()?;
        let folder = temp.join(format!("artcraft-launcher-msi-{}", crate::http::hex(&name)));
        std::fs::create_dir(&folder).ok()?;
        Some(folder)
    }

    /// Copies the checked MSI into the staging folder and installs that copy without Windows
    /// Installer windows; its code for the results. The copy's SHA-256 is computed from the bytes
    /// written, so what msiexec installs is exactly what was checked, wherever the download's
    /// path leads afterwards.
    fn install_one(index: usize, item: &Item, mut source: File, staging: &Path, msiexec: &Path) -> i32 {
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
        drop(source);
        // Held while msiexec reads it: no one may change it in the meantime.
        let Ok(held) = OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&staged) else { return code::NOT_STARTED };
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
    fn stage(source: &mut File, staged: &Path) -> Option<String> {
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
                Item { msi: abs("cache/downloads/film craft.MSI"), sha256: sha('0'), folder: Some(abs("Apps/FilmCraft")) },
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
        let item = Item { msi: abs("d/a.msi"), sha256: sha('b'), folder: None };
        let too_many = Request { results: abs("d/r.txt"), items: vec![item; MOST + 1] };
        assert!(parse_args(&helper_args(&too_many)).is_err());
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
        assert_eq!(explain(Some(3010), &log), Ok(()));
        assert_eq!(explain(Some(1602), &log), Err(CANCELLED.to_string()));
        assert!(explain(Some(1603), &log).is_err_and(|e| e.contains("1603") && e.contains(&log.display().to_string())));
        assert!(explain(Some(code::CHECKSUM), &log).is_err_and(|e| e.contains("changed")));
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
}
