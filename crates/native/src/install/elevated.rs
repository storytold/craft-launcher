//! All-users Windows Installer work under one permission prompt, run by the installer helper.
//!
//! Going back to an older version of an all-users app takes three `msiexec` steps (remove, install,
//! put back on failure), and asking for permission at each step proved fragile: a refused later
//! prompt can leave no app at all (see [`msi_downgrade`](super::msi_downgrade)). So the launcher
//! starts its signed installer helper ([`HELPER`], beside the launcher) elevated, once: Windows'
//! prompt names ArtCraft Launcher and its publisher. No script, interpreter or encoded command is
//! involved; every program that runs elevated is ours or `msiexec`, called directly. The helper is
//! a separate small program rather than the launcher started again, so that what runs elevated
//! loads no DLL from its folder, which the person's programs can write (see its `main.rs`).
//!
//! The whole job travels on the helper's command line ([`parameters`]), which nobody can
//! change once the prompt is up. Nothing it acts on is read from a file the person's account can
//! write. The helper ([`finish`]) then trusts only what it checks itself:
//! - each package is opened so nobody can change, move or delete it, hashed, and copied only when
//!   it has the checksum on the command line, into a new folder under `Program Files\ArtCraft`
//!   ([`runs_base`]), where ordinary accounts can read but neither add, change nor delete files;
//! - each copy must carry the same Authenticode publisher as the helper, chain to the pinned
//!   root, and be a package of the app it replaces ([`identity`](super::identity));
//! - the install folder must be the folder of one of the app's all-users registrations, read from
//!   the registry by the helper, so the caller cannot send an all-users install elsewhere.
//!
//! `msiexec` then installs from that folder, so nothing can be swapped between the check and the
//! install.
//!
//! The prompt gives back no handle to wait on (`winsafe`'s `ShellExecuteEx` keeps it), so the two
//! processes meet in that folder: the helper holds a lock file there without sharing while
//! it works and writes its result beside it before letting go. The launcher waits for the lock to
//! appear, then to be released, then reads the result.

#![cfg_attr(not(windows), allow(dead_code))]

use std::ffi::OsString;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

use artcraft_launcher_core::catalog::{AppKind, CraftApp};
use sha2::{Digest, Sha256};

/// The first argument of the helper, followed by the job ([`parameters`]).
pub const FLAG: &str = "--elevated-windows-installer";
/// The installer helper's file name, in the launcher's own folder.
pub const HELPER: &str = "artcraft-launcher-helper.exe";

/// The folder in `Program Files\ArtCraft` that holds each run's folder.
const RUNS: &str = "Launcher installer runs";
/// Each run's folder is this followed by the run id.
const RUN_PREFIX: &str = "artcraft-launcher-elevated-";
/// Held without sharing while the helper works.
const LOCK: &str = "running";
/// Written before the lock is let go: `ok`, or `failed` and the message on the next line.
const RESULT: &str = "result";
/// The packages' names in the run folder; `msiexec` writes its log beside each.
const CHOSEN: &str = "chosen.msi";
const PREVIOUS: &str = "previous.msi";
/// The helper's own log, so a run stopped part way (by antivirus, say) still says how far it got.
pub const LOG: &str = "artcraft-launcher.log";
/// Every name a run folder can hold, so an old one is cleared file by file, never recursively.
const RUN_FILES: [&str; 7] = [LOCK, RESULT, CHOSEN, PREVIOUS, "chosen.log", "previous.log", LOG];

/// One downloaded package: where it is, the checksum it was checked against, and its version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Package {
    pub file: PathBuf,
    pub sha256: String,
    pub version: String,
}

/// An all-users downgrade: remove `app`'s installed copy, install `chosen` in `folder`, and put
/// `previous` back if that does not finish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Job {
    pub app: String,
    pub chosen: Package,
    pub previous: Package,
    pub folder: Option<PathBuf>,
}

/// A fresh run id: 32 lower-case hex digits.
fn new_run() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| format!("couldn't prepare the installer helper ({e})"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Whether `run` has the shape [`new_run`] gives, so it can't name another folder.
fn valid_run(run: &str) -> bool {
    run.len() == 32 && run.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A catalogue app id: lower-case letters, digits and hyphens.
fn valid_app(app: &str) -> bool {
    !app.is_empty() && app.len() <= 64 && app.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'))
}

/// A version label such as `0.13.0` or `0.14.0-nightly.3`: nothing that needs quoting.
fn valid_version(version: &str) -> bool {
    !version.is_empty() && version.len() <= 64 && version.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
}

/// A SHA-256 as 64 hex digits.
fn valid_sha256(sha256: &str) -> bool {
    sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit())
}

fn run_dir(base: &Path, run: &str) -> PathBuf {
    base.join(format!("{RUN_PREFIX}{run}"))
}

/// `path` in quotes on the command line. A trailing backslash would escape the closing quote.
fn quoted_path(path: &Path) -> Result<String, String> {
    let text = super::windows_text(path)?;
    if text.ends_with('\\') {
        return Err(format!("couldn't prepare the installer helper for {text}"));
    }
    Ok(format!("\"{text}\""))
}

/// The helper's command line: [`FLAG`], the run id, the app, each package's version,
/// checksum and file, and the install folder (empty quotes for the package's own).
fn parameters(run: &str, job: &Job) -> Result<String, String> {
    let packages = [&job.chosen, &job.previous];
    if !valid_run(run) || !valid_app(&job.app) || !packages.iter().all(|p| valid_version(&p.version) && valid_sha256(p.sha256.trim())) {
        return Err(format!("couldn't prepare the installer helper for {}", job.app));
    }
    let folder = match &job.folder {
        Some(folder) => quoted_path(folder)?,
        None => "\"\"".to_string(),
    };
    let package = |p: &Package| -> Result<String, String> { Ok(format!("{} {} {}", p.version, p.sha256.trim(), quoted_path(&p.file)?)) };
    Ok(format!("{FLAG} {run} {} {} {} {folder}", job.app, package(&job.chosen)?, package(&job.previous)?))
}

/// Reads [`parameters`] back, checking each part again. `None` when this program was not started
/// as the helper.
fn parse_args(args: &[OsString]) -> Option<Result<(String, Job), String>> {
    if args.first()? != FLAG {
        return None;
    }
    let text: Option<Vec<&str>> = args.iter().map(|a| a.to_str()).collect();
    let package = |version: &str, sha256: &str, file: &str| {
        (valid_version(version) && valid_sha256(sha256) && !file.is_empty()).then(|| Package {
            file: PathBuf::from(file),
            sha256: sha256.to_string(),
            version: version.to_string(),
        })
    };
    let parsed = match text.as_deref() {
        Some([_, run, app, cv, cs, cf, pv, ps, pf, folder]) if valid_run(run) && valid_app(app) => {
            package(cv, cs, cf).zip(package(pv, ps, pf)).map(|(chosen, previous)| {
                let folder = (!folder.is_empty()).then(|| PathBuf::from(folder));
                (run.to_string(), Job { app: app.to_string(), chosen, previous, folder })
            })
        }
        _ => None,
    };
    Some(parsed.ok_or_else(|| "the installer helper's arguments are incomplete".to_string()))
}

/// Whether two Windows folders are the same folder, ignoring case and a trailing separator.
fn same_folder(a: &Path, b: &Path) -> bool {
    let text = |p: &Path| p.to_string_lossy().trim_end_matches(['\\', '/']).to_string();
    text(a).eq_ignore_ascii_case(&text(b))
}

/// The install folder the helper will use: one of the `registered` all-users folders, never a
/// folder the caller made up. No folder is refused: `msiexec` would use the package's default,
/// which moves a copy from a folder the person chose.
fn checked_folder<'a>(folder: Option<&'a Path>, registered: &[PathBuf], app: &str) -> Result<Option<&'a Path>, String> {
    match folder {
        None => Err(no_folder(app)),
        Some(folder) if registered.iter().any(|r| same_folder(r, folder)) => Ok(Some(folder)),
        Some(folder) => Err(format!("{} isn't where {app} is installed for all users, so nothing was changed", folder.display())),
    }
}

/// Why a downgrade with no known all-users folder is refused, before the prompt and in the helper.
pub(super) fn no_folder(app: &str) -> String {
    format!("Windows doesn't say where {app} is installed for all users, so nothing was changed")
}

/// Opens `file` so nobody can change, move or delete it while it is open: others may only read.
pub(super) fn open_unchangeable(file: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        /// `FILE_SHARE_READ`: no write or delete sharing.
        const SHARE_READ: u32 = 0x1;
        std::fs::File::options().read(true).share_mode(SHARE_READ).open(file)
    }
    #[cfg(not(windows))]
    std::fs::File::open(file)
}

/// Reads `source` to the end into `copy` (if any), and returns the SHA-256 of what was read.
pub(super) fn hash_through(source: &mut std::fs::File, mut copy: Option<&mut std::fs::File>) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let n = source.read(&mut buffer)?;
        let Some(chunk) = buffer.get(..n).filter(|c| !c.is_empty()) else { break };
        hasher.update(chunk);
        if let Some(copy) = copy.as_deref_mut() {
            copy.write_all(chunk)?;
        }
    }
    if let Some(copy) = copy {
        copy.flush()?;
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Copies `package` to a new file `name` in `dir`. The source is held unchangeable and hashed
/// first, so nothing is written unless it has the package's checksum: a link to some other file
/// copies nothing. The copy is hashed as it is written too, and is what gets installed.
fn stage(package: &Package, dir: &Path, name: &str) -> Result<PathBuf, String> {
    let shown = package.file.display();
    let changed = || format!("{shown} changed after it was checked, so it wasn't installed. Try again.");
    let matches = |digest: &str| digest.eq_ignore_ascii_case(package.sha256.trim());
    let mut source = open_unchangeable(&package.file).map_err(|e| format!("couldn't read {shown} ({e})"))?;
    let digest = hash_through(&mut source, None).map_err(|e| format!("couldn't read {shown} ({e})"))?;
    if !matches(&digest) {
        return Err(changed());
    }
    source.rewind().map_err(|e| format!("couldn't read {shown} ({e})"))?;
    let target = dir.join(name);
    let mut copy = std::fs::File::create_new(&target).map_err(|e| format!("couldn't copy {shown} to install it ({e})"))?;
    let copied = hash_through(&mut source, Some(&mut copy)).map_err(|e| format!("couldn't copy {shown} to install it ({e})"));
    drop(copy);
    match copied {
        Ok(digest) if matches(&digest) => Ok(target),
        other => {
            let _ = std::fs::remove_file(&target);
            Err(other.err().unwrap_or_else(changed))
        }
    }
}

/// Writes the result file. Created new, so nothing already there is followed or overwritten.
fn write_result(dir: &Path, result: &Result<(), String>) -> std::io::Result<()> {
    let text = match result {
        Ok(()) => "ok\n".to_string(),
        Err(e) => format!("failed\n{e}"),
    };
    let mut file = std::fs::File::create_new(dir.join(RESULT))?;
    file.write_all(text.as_bytes())?;
    file.flush()
}

/// What the helper reported. No result once the lock is let go means it stopped part way.
fn read_result(dir: &Path, app: &str) -> Result<(), String> {
    let stopped = || format!("the installer helper stopped unexpectedly. Check which version of {app} is installed.");
    let text = std::fs::read_to_string(dir.join(RESULT)).map_err(|_| stopped())?;
    match text.split_once('\n') {
        Some(("ok", _)) => Ok(()),
        Some(("failed", message)) if !message.trim().is_empty() => Err(message.trim().to_string()),
        _ => Err(stopped()),
    }
}

/// Clears run folders older than a day, left by earlier runs: only the names a run writes, one
/// file at a time, then the folder if that emptied it. Nothing is followed into or recursed.
fn clear_old_runs(base: &Path) {
    let Ok(entries) = std::fs::read_dir(base) else { return };
    let day = std::time::Duration::from_secs(24 * 60 * 60);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let ours = name.to_str().and_then(|n| n.strip_prefix(RUN_PREFIX)).is_some_and(valid_run);
        let old = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > day);
        if ours && old && entry.file_type().is_ok_and(|t| t.is_dir()) {
            let dir = entry.path();
            for file in RUN_FILES {
                let _ = std::fs::remove_file(dir.join(file));
            }
            let _ = std::fs::remove_dir(&dir);
        }
    }
}

/// Refuses `base` or its parent when either is a link or junction rather than a real folder, so
/// the helper never creates, cleans or installs from somewhere it leads.
fn real_folders(base: &Path) -> Result<(), String> {
    for folder in [Some(base), base.parent()].into_iter().flatten() {
        let real = std::fs::symlink_metadata(folder).is_ok_and(|m| m.file_type().is_dir());
        if !real {
            return Err(format!("{} isn't an ordinary folder, so nothing was changed", folder.display()));
        }
    }
    Ok(())
}

/// A signature's publisher: the leaf certificate's subject, and its issuer.
#[cfg(windows)]
#[derive(Debug, PartialEq, Eq)]
struct Signer {
    subject: verifysign::Name,
    issuer: verifysign::Name,
}

/// Whether `theirs` is the same publisher as `ours`: the same subject, issued by the same
/// organisation. Not the same certificate or issuing CA: the packages are signed through
/// Microsoft's Trusted Signing, whose certificates last days and whose issuing CA changes
/// (`Microsoft ID Verified CS AOC CA 03`, `CA 04`), so a pin on either would refuse real releases.
#[cfg(windows)]
fn same_publisher(ours: &Signer, theirs: &Signer) -> bool {
    ours.subject == theirs.subject && ours.issuer.organization.is_some() && ours.issuer.organization == theirs.issuer.organization
}

/// Whether `file` is signed by the same publisher as `reference` (this program). Windows checks
/// the signature and its chain to a trusted root first.
#[cfg(windows)]
pub(super) fn signed_like(reference: &Path, file: &Path) -> Result<(), String> {
    let signer = |path: &Path| verifysign::verify_file(path).map(|s| Signer { subject: s.subject_name(), issuer: s.issuer_name() });
    let ours = signer(reference).map_err(|e| format!("this copy of ArtCraft Launcher isn't signed ({e}), so it can't install for all users"))?;
    let theirs = signer(file).map_err(|e| format!("{} isn't signed ({e})", file.display()))?;
    if same_publisher(&ours, &theirs) { Ok(()) } else { Err(format!("{} isn't signed by ArtCraft Launcher's publisher", file.display())) }
}

/// `Program Files\ArtCraft\Launcher installer runs`, with Program Files from Windows rather than
/// the environment. Not ProgramData: ordinary accounts may add files to any folder there, even one
/// an administrator made, so they could plant a run's log or result before the helper writes it.
/// Program Files lets them only read, and its folders hold nothing a run doesn't write.
#[cfg(windows)]
fn runs_base() -> Result<PathBuf, String> {
    use winsafe::{SHGetKnownFolderPath, co};
    let path = SHGetKnownFolderPath(&co::KNOWNFOLDERID::ProgramFiles, co::KF::DEFAULT, None)
        .map(PathBuf::from)
        .map_err(|e| format!("couldn't find the Program Files folder ({e})"))?;
    if path.is_absolute() { Ok(path.join("ArtCraft").join(RUNS)) } else { Err("couldn't find the Program Files folder".into()) }
}

/// Why an all-users downgrade of `app` can't go through the helper, said before any prompt rather
/// than after it. The helper installs only packages signed by the launcher's publisher, and a
/// community app is signed by its own developer. Not relaxed for them: the checksum the helper
/// checks comes from the launcher running as the person, so it can't be what admits a package.
pub(super) fn refusal(app: &CraftApp) -> Option<String> {
    (app.kind != AppKind::Official).then(|| {
        format!(
            "ArtCraft Launcher can't install an older version of {} for all users, because {}'s packages aren't signed by ArtCraft Launcher's publisher. Nothing was changed. Use the version's installer from {}'s release page instead.",
            app.name, app.name, app.name
        )
    })
}

/// The installer helper beside `launcher`, the launcher's own program. Never anywhere else, so
/// the prompt always names the program the launcher's installer put there.
fn helper_beside(launcher: &Path) -> Result<PathBuf, String> {
    let helper = launcher.with_file_name(HELPER);
    if helper.is_file() {
        Ok(helper)
    } else {
        Err(format!("the installer helper ({HELPER}) is missing beside ArtCraft Launcher, so nothing was changed. Reinstall ArtCraft Launcher."))
    }
}

/// `helper` resolved, held so nobody can change, move or replace it, and checked to carry
/// `launcher`'s publisher. It sits in the person's own launcher folder, which their programs can
/// write, so without this the prompt would name whatever had been put there. Hold the returned
/// file until the helper is running: Windows still starts a program held this way.
///
/// The limit: a program that can rewrite the launcher itself defeats any check the launcher makes,
/// so the publisher the permission prompt shows stays the last gate until the helper lives in a
/// folder only administrators can write.
#[cfg(windows)]
fn checked_helper(launcher: &Path, helper: &Path) -> Result<(PathBuf, std::fs::File), String> {
    let refused = |e: String| format!("{e}. Nothing was changed.");
    let resolved =
        dunce::canonicalize(helper).map_err(|e| refused(format!("couldn't find the installer helper ({e}). Reinstall ArtCraft Launcher")))?;
    let held = open_unchangeable(&resolved).map_err(|e| refused(format!("couldn't read the installer helper ({e})")))?;
    signed_like(launcher, &resolved).map_err(refused)?;
    // The publisher alone admits any program we ever signed, such as an older launcher renamed
    // here, which loads planted DLLs when elevated: it must be this build's helper.
    super::helper_build::check(&resolved).map_err(refused)?;
    Ok((resolved, held))
}

/// Where a run's lock stands, as the launcher sees it.
#[derive(Debug, PartialEq, Eq)]
enum Lock {
    /// The helper hasn't started yet.
    Missing,
    Held,
    /// The helper has finished, or stopped.
    Released,
}

#[cfg(windows)]
fn lock_state(lock: &Path) -> Result<Lock, String> {
    /// `ERROR_SHARING_VIOLATION`: the helper holds the file without sharing.
    const SHARING_VIOLATION: i32 = 32;
    match std::fs::File::open(lock) {
        Ok(_) => Ok(Lock::Released),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Lock::Missing),
        Err(e) if e.raw_os_error() == Some(SHARING_VIOLATION) => Ok(Lock::Held),
        Err(e) => Err(format!("couldn't follow the installer helper ({e})")),
    }
}

/// Waits for the helper to take its lock (up to `start`) and then to let it go.
#[cfg(windows)]
fn wait(lock: &Path, start: std::time::Duration) -> Result<(), String> {
    let began = std::time::Instant::now();
    loop {
        match lock_state(lock)? {
            Lock::Released => return Ok(()),
            Lock::Missing if began.elapsed() > start => {
                return Err("permission was given, but the installer helper didn't start, so nothing was changed".into());
            }
            Lock::Missing | Lock::Held => std::thread::sleep(std::time::Duration::from_millis(250)),
        }
    }
}

/// Runs `job` for `app` (its name, for messages) in the installer helper, elevated behind one
/// permission prompt, and waits for it. A refused prompt is
/// [`CANCELLED`](artcraft_launcher_core::protocol::CANCELLED) and changes nothing.
#[cfg(windows)]
pub(super) fn run(job: &Job, app: &str) -> Result<(), String> {
    use artcraft_launcher_core::protocol::CANCELLED;
    use winsafe::{SHELLEXECUTEINFO, ShellExecuteEx, co};

    let run = new_run()?;
    let parameters = parameters(&run, job)?;
    let me = std::env::current_exe().map_err(|e| format!("couldn't find this program ({e})"))?;
    // Held from its check until the helper has finished, so the prompt names the checked file.
    let (helper, _held) = checked_helper(&me, &helper_beside(&me)?)?;
    log::info!("{app}: going back to {} under one permission prompt", job.chosen.version);
    let asked = ShellExecuteEx(&SHELLEXECUTEINFO {
        mask: co::SEE_MASK::NOASYNC | co::SEE_MASK::FLAG_NO_UI,
        verb: Some("runas"),
        file: super::windows_text(&helper)?,
        parameters: Some(&parameters),
        // Nothing is hidden: the helper opens no window of its own, and msiexec shows its progress.
        show: co::SW::SHOWNORMAL,
        ..Default::default()
    });
    match asked {
        Ok(()) => {}
        Err(co::ERROR::CANCELLED) => return Err(CANCELLED.to_string()),
        Err(e) => return Err(format!("couldn't ask Windows for permission ({e}), so nothing was changed")),
    }
    // Only an administrator can make or fill this folder, so the result is the helper's.
    let dir = run_dir(&runs_base()?, &run);
    wait(&dir.join(LOCK), std::time::Duration::from_secs(60))?;
    read_result(&dir, app)
}

/// The helper's work, when it was started with [`FLAG`]; `args` are its arguments without the
/// program name. `None` otherwise. `facts` reads each package's signing root and UpgradeCode
/// (the helper's own Windows calls). `log_to` is given the run folder as soon as it exists, for
/// the log ([`LOG`]).
#[cfg(windows)]
pub fn finish(args: &[OsString], facts: &dyn super::identity::PackageFacts, log_to: impl FnOnce(&Path)) -> Option<Result<(), String>> {
    use std::os::windows::fs::OpenOptionsExt;

    let (run, job) = match parse_args(args)? {
        Ok(parsed) => parsed,
        Err(e) => return Some(Err(e)),
    };
    let base = match runs_base() {
        Ok(base) => base,
        Err(e) => return Some(Err(e)),
    };
    // Made here, elevated, so it takes Program Files' permissions: others may only read.
    if let Err(e) = std::fs::create_dir_all(&base) {
        return Some(Err(format!("couldn't create {} ({e})", base.display())));
    }
    if let Err(e) = real_folders(&base) {
        return Some(Err(e));
    }
    clear_old_runs(&base);
    let dir = run_dir(&base, &run);
    // A new folder, never one that already exists: its files are then ours alone to change.
    if let Err(e) = std::fs::create_dir(&dir) {
        return Some(Err(format!("couldn't create {} ({e})", dir.display())));
    }
    let lock = match std::fs::File::options().write(true).create_new(true).share_mode(0).open(dir.join(LOCK)) {
        Ok(lock) => lock,
        Err(e) => return Some(Err(format!("couldn't create the installer helper's lock ({e})"))),
    };
    // Every account on the machine may read this folder (Program Files), so the logs here,
    // msiexec's verbose ones included, show this person's folders and installer properties to
    // other users of the machine. Accepted for a diagnostic folder that holds no secrets: making
    // it private would need the person's account, and the helper may run as another one.
    log_to(&dir);
    log::info!("installer helper: {} from {} to {}", job.app, job.previous.version, job.chosen.version);
    let done = downgrade(&dir, &job, facts);
    match &done {
        Ok(()) => log::info!("installer helper: done"),
        Err(e) => log::warn!("installer helper: {e}"),
    }
    if let Err(e) = write_result(&dir, &done) {
        log::warn!("couldn't record the installer helper's result ({e})");
    }
    drop(lock);
    Some(done)
}

/// Checks the folder and both packages, stages them, then removes, installs and if needed puts back.
#[cfg(windows)]
fn downgrade(dir: &Path, job: &Job, facts: &dyn super::identity::PackageFacts) -> Result<(), String> {
    use super::{MsiScope, identity, msi_downgrade, windows};

    let app = artcraft_launcher_core::catalog::CraftApp::by_id(&job.app).ok_or_else(|| format!("{} isn't a Craft app", job.app))?;
    let installs: Vec<_> = crate::detect::windows::msi_installs(app).into_iter().filter(|m| m.scope == MsiScope::PerMachine).collect();
    // The products the removal step takes away: both packages must belong to every one of them.
    let removed: Vec<String> = installs.iter().map(|m| m.product_code.clone()).collect();
    let registered: Vec<PathBuf> = installs.into_iter().filter_map(|m| m.exe.and_then(|exe| exe.parent().map(Path::to_path_buf))).collect();
    let folder = checked_folder(job.folder.as_deref(), &registered, app.name)?;
    let me = std::env::current_exe().map_err(|e| format!("couldn't find this program ({e})"))?;
    // Every staged copy goes when this returns, whatever stopped it: Windows Installer keeps its
    // own copy of what it installed, and the logs stay for diagnosis.
    let staged = Staged(vec![dir.join(CHOSEN), dir.join(PREVIOUS)]);
    let chosen = stage(&job.chosen, dir, CHOSEN)?;
    let previous = stage(&job.previous, dir, PREVIOUS)?;
    for staged in [&chosen, &previous] {
        signed_like(&me, staged)?;
        identity::check(facts, staged, app.name, &removed, identity::linked_upgrade_code)?;
    }
    let scope = Some(MsiScope::PerMachine);
    let done = msi_downgrade::run(msi_downgrade::Steps {
        app: app.name,
        chosen: &job.chosen.version,
        previous: &job.previous.version,
        // Only the machine's registrations: an elevated `msiexec /x` must never take a product
        // code from HKCU, which the person's own programs can write.
        remove: &mut || windows::remove_registrations(app, scope),
        install_chosen: &mut || windows::run_direct(&chosen, folder, scope),
        reinstall_previous: &mut || windows::run_direct(&previous, folder, scope),
    });
    drop(staged);
    done
}

/// Removes the files it names when dropped.
struct Staged(Vec<PathBuf>);

impl Drop for Staged {
    fn drop(&mut self) {
        for file in &self.0 {
            let _ = std::fs::remove_file(file);
        }
    }
}

#[cfg(test)]
#[path = "elevated_tests.rs"]
mod tests;
