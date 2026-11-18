//! One permission prompt for a batch of per-machine Windows Installer packages.
//!
//! [`super::windows::run_batched`] gathers the members of an "update all" into one call of the
//! installer queue; the one whose call it is writes what the session is to do into a manifest
//! beside the checked packages and starts the launcher's own exe elevated (`ShellExecuteEx`,
//! verb `runas`). No PowerShell, nothing encoded, nothing hidden: the one prompt names ArtCraft
//! Launcher and its signed publisher, and each package still shows its own installer progress
//! window. A hidden, elevated, encoded PowerShell route did this before and was withdrawn after
//! behavioural antivirus flagged it (see `docs/incident-2026-10-windows-antivirus.md`).
//!
//! The elevated copy re-checks every package against the manifest's checksum — with the file held
//! open, so the checked bytes are the installed ones (CWE-367) — and installs it only when its
//! Authenticode signature names the launcher's own publisher. It builds each installer's
//! arguments from that held package itself (the manifest says where and in which context a
//! package installs, never *which* file installs), runs Windows Installer on each in turn, and
//! writes each app's exit code to a results file beside the manifest. A declined prompt cancels
//! the whole batch; a session that cannot start sends the members back to installing one by one,
//! the way a single app always installs, and so does a package from another publisher.
//!
//! The trust chain: the manifest is in a folder every process of the user can write, and any of
//! them can start the elevated copy on one, so the signature is the check the elevated side makes
//! on its own. The manifest of a real session is created anew (`create_new`) and then held open
//! unmodified for the whole session. The elevated side re-hashes every package through a held
//! handle before Windows Installer touches it, and runs the installer on that same held file. It
//! writes its process id and its results only to new files. The results file is bound to its
//! session by a random token (written as the file's first line), so no leftover or planted file
//! can pass for a session's report; the
//! launcher's own re-detection after each job is the backstop against a report that lies. And
//! the waiting side falls back to one-by-one installs only when the elevated session is *shown*
//! to be over — its process waited out or its process id gone — never while it may still be
//! installing.

#![cfg_attr(not(windows), allow(dead_code))]

use std::ffi::OsStr;
use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

use super::MsiScope;

/// What a member hears when the batch's one session could not run at all: install it by itself
/// (see [`super::windows::run_batched`], which recognises this and does exactly that). Never
/// shown to anyone.
pub(super) const NO_SESSION: &str = "no batch session";

/// How long the waiting side looks for the elevated copy's first act, writing its process id.
/// A behavioural antivirus may hold a start-up up for minutes; falling back before the elevated
/// copy has even begun would race its installers, so the wait is patient and the failure is an
/// error (never a fallback — see [`wait_until_done`]).
#[cfg(windows)]
const STARTED_WITHIN: std::time::Duration = std::time::Duration::from_secs(300);

/// How long the installer queue is kept for a session that has started but cannot be opened or
/// waited for ([`Seen::Unwatched`]). It ends sooner when the session reports.
#[cfg(windows)]
const UNWATCHED_LIMIT: std::time::Duration = std::time::Duration::from_secs(4 * 60 * 60);

/// One package of the batch, as both sides of the session see it. It carries no command line:
/// the elevated side builds the installer's arguments from the held package itself
/// ([`run_elevated`]), so the file Windows Installer runs is always the file that was checked.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Entry {
    /// The app the package belongs to: its key in the results.
    app: String,
    /// The checked package, in the launcher's downloads folder.
    msi: PathBuf,
    /// The SHA-256 the package was downloaded against; the elevated side checks it again before
    /// Windows Installer touches the file.
    sha256: String,
    /// Where the package installs, when a single install passes a folder (`INSTALLFOLDER`).
    destination: Option<PathBuf>,
    /// The Windows Installer context to keep; `None` takes the package's own default.
    scope: Option<MsiScope>,
}

/// What the elevated copy is to do, written beside the packages and held unchanged while the
/// session runs. The results and the process id arrive in files beside it (see [`sibling`]).
#[derive(Serialize, Deserialize)]
struct Manifest {
    /// Binds the results file to this session: the elevated copy writes it as the results'
    /// first line, so no file from any other session (left over, or put in place) can pass as
    /// this one's report.
    token: String,
    msis: Vec<Entry>,
}

/// The command every per-machine batch member waits under: the installer queue's merging key
/// for the one call the batch shares.
pub(super) fn command() -> Vec<OsString> {
    vec![OsString::from("msiexec")]
}

/// One member's place in the session, as the installer queue's `target`. `Err` for a package the
/// session cannot express (a package path that cannot be resolved); the member then installs by
/// itself.
#[cfg(windows)]
pub(super) fn entry(
    app: &super::CraftApp,
    package: &super::windows::Package<'_>,
    destination: Option<&Path>,
    scope: Option<MsiScope>,
) -> Result<OsString, String> {
    let entry = Entry {
        app: app.id.to_string(),
        msi: super::msi_path(package.file)?,
        sha256: package.sha256.to_string(),
        destination: destination.map(Path::to_path_buf),
        scope,
    };
    serde_json::to_string(&entry).map(OsString::from).map_err(|e| format!("couldn't describe the package for a shared prompt ({e})"))
}

/// The queue's `target` back into an [`Entry`].
fn parse(target: &OsStr) -> Option<Entry> {
    target.to_str().and_then(|text| serde_json::from_str(text).ok())
}

/// Where the session keeps a file beside its manifest.
fn sibling(manifest: &Path, extension: &str) -> PathBuf {
    manifest.with_extension(extension)
}

/// The one call the batch shares: what the member whose turn it is runs. Every `target` is an
/// [`entry`]. One prompt is asked; every member then hears its own installer's exit code, a
/// declined prompt cancels the batch, and a session that cannot start sends the members back to
/// installing one by one.
#[cfg(windows)]
pub(super) fn call(_command: &[OsString], targets: &[OsString]) -> Vec<Result<(), String>> {
    use artcraft_launcher_core::protocol::CANCELLED;

    let (mut outcomes, joined, mut held) = join(targets, &signed_by_this_launcher);
    let Some((_, first)) = joined.first() else {
        return outcomes;
    };
    let Some(dir) = first.msi.parent() else {
        return without_session(outcomes, &joined);
    };
    let (manifest, token) = match prepare(&joined, dir, &mut held) {
        Ok(prepared) => prepared,
        Err(e) => {
            log::warn!("the shared install session could not be prepared ({e}); installing one by one");
            return without_session(outcomes, &joined);
        }
    };
    let (results, pid_file) = (sibling(&manifest, "results"), sibling(&manifest, "pid"));
    match ask_once(dir, &manifest, joined.len()) {
        Prompt::Declined => {
            log::info!("the shared permission prompt was declined");
            for (at, _) in &joined {
                outcomes[*at] = Err(CANCELLED.to_string());
            }
        }
        Prompt::Failed(e) => {
            log::warn!("the elevated session could not start ({e}); installing one by one");
            outcomes = without_session(outcomes, &joined);
        }
        Prompt::Given => match wait_until_done(&pid_file, &results, &token) {
            SessionEnd::Done(codes) => {
                for (at, entry) in &joined {
                    outcomes[*at] = match codes.iter().find(|(app, _)| app == &entry.app) {
                        // A negative code is "no exit code" (see [`run_installer`]): no installer
                        // ran, so there is no log to point at.
                        Some((_, code)) => super::explain(Some(*code)).map_err(|e| {
                            if *code == 1602 || *code < 0 { e } else { format!("{e}. Its log is at {}", super::msi_log(&entry.msi).display()) }
                        }),
                        None => Err(format!("the shared install session never reported {}'s result", entry.app)),
                    };
                }
            }
            // The session is over and said nothing: nothing of it is running, so the members may
            // safely install one by one.
            SessionEnd::OverWithoutReport => {
                log::warn!("the elevated session ended without reporting; installing one by one");
                outcomes = without_session(outcomes, &joined);
            }
            // The session was launched and cannot be shown to be over: falling back would race
            // whatever it may still install, so the members hear an error instead.
            SessionEnd::Unknown => {
                log::warn!("the elevated session could not be watched; nothing was retried here");
                for (at, _) in &joined {
                    outcomes[*at] = Err("the shared install session couldn't be watched. Try again.".into());
                }
            }
        },
    }
    // The held files close before the session's own are removed: an open handle would keep the
    // manifest undeletable on Windows (its share mode allows reading only).
    drop(held);
    for file in [&manifest, &pid_file, &results] {
        let _ = std::fs::remove_file(file);
    }
    outcomes
}

/// Each target's outcome so far, the members in the session, and the files held for them.
#[cfg(windows)]
type Joined = (Vec<Result<(), String>>, Vec<(usize, Entry)>, Vec<super::msi_batch::Held>);

/// A member joins the session only if its package is held, checks out again and passes `signed`,
/// the check the elevated copy makes too. A package from another publisher installs by itself,
/// under its own prompt; any other failure is that member's own error.
#[cfg(windows)]
fn join(targets: &[OsString], signed: &dyn Fn(&Path) -> Result<(), String>) -> Joined {
    let mut outcomes: Vec<Result<(), String>> = Vec::with_capacity(targets.len());
    let mut joined: Vec<(usize, Entry)> = Vec::new();
    let mut held: Vec<super::msi_batch::Held> = Vec::new();
    for (at, target) in targets.iter().enumerate() {
        match parse(target.as_os_str()) {
            Some(entry) => match super::msi_batch::hold(&entry.msi, &entry.sha256) {
                Ok(file) => match signed(&entry.msi) {
                    Ok(()) => {
                        held.push(file);
                        outcomes.push(Ok(()));
                        joined.push((at, entry));
                    }
                    Err(e) => {
                        log::info!("{}: {e}; installing it by itself", entry.app);
                        outcomes.push(Err(NO_SESSION.into()));
                    }
                },
                Err(e) => outcomes.push(Err(e)),
            },
            None => outcomes.push(Err(NO_SESSION.into())),
        }
    }
    (outcomes, joined, held)
}

/// Accepts `file` when it carries a valid Authenticode signature from the same publisher as
/// `reference`. Refuses an unsigned file, another publisher's, and every file when `reference`
/// itself isn't signed.
#[cfg(windows)]
fn signed_like(reference: &Path, file: &Path) -> Result<(), String> {
    let publisher = |path: &Path| verifysign::verify_file(path).map(|signature| signature.subject_name());
    let ours = publisher(reference).map_err(|e| format!("this copy of the launcher isn't signed ({e})"))?;
    let theirs = publisher(file).map_err(|e| format!("{} isn't signed ({e})", file.display()))?;
    if ours == theirs { Ok(()) } else { Err(format!("{} isn't signed by the launcher's publisher", file.display())) }
}

/// [`signed_like`] with this running program as the reference.
#[cfg(windows)]
fn signed_by_this_launcher(file: &Path) -> Result<(), String> {
    let me = std::env::current_exe().map_err(|e| format!("couldn't find this program ({e})"))?;
    signed_like(&me, file)
}

/// Writes the manifest the elevated copy reads, holding it and every package unchanged for as
/// long as the session runs. The name is only taken once its results and process-id files are
/// free too: a leftover of another session must never be mistaken for this one's.
#[cfg(windows)]
fn prepare(joined: &[(usize, Entry)], dir: &Path, held: &mut Vec<super::msi_batch::Held>) -> Result<(PathBuf, String), String> {
    use std::io::Write as _;
    use std::os::windows::fs::OpenOptionsExt;

    /// Others may read a held file, but not write, rename or delete it (see [`msi_batch::hold`]).
    const FILE_SHARE_READ: u32 = 0x1;

    let base = format!("batch-{}-{}", std::process::id(), crate::now());
    let manifest = (0..)
        .map(|n| dir.join(format!("{base}{}.json", if n == 0 { String::new() } else { n.to_string() })))
        .find(|manifest| [manifest, &sibling(manifest, "results"), &sibling(manifest, "pid")].iter().all(|p| !p.exists()))
        .ok_or("no free name for the batch manifest")?;
    let token = session_token();
    let text = serde_json::to_string(&Manifest { token: token.clone(), msis: joined.iter().map(|(_, entry)| entry.clone()).collect() })
        .map_err(|e| format!("couldn't describe the batch ({e})"))?;
    let mut file = std::fs::File::options()
        .write(true)
        .create_new(true)
        .share_mode(FILE_SHARE_READ)
        .open(&manifest)
        .map_err(|e| format!("couldn't write the batch manifest ({e})"))?;
    file.write_all(text.as_bytes()).map_err(|e| format!("couldn't write the batch manifest ({e})"))?;
    held.push(super::msi_batch::Held::keep(file));
    Ok((manifest, token))
}

/// This session's unguessable name for its results, so a file from any other session (or put in
/// one's place) cannot pass for this one's report.
fn session_token() -> String {
    let random = getrandom::u64().unwrap_or_else(|_| (std::process::id() as u64) ^ ((crate::now() as u64) << 32));
    format!("{random:016x}")
}

/// The members of the session are told it never happened: each installs by itself, the way a
/// single app always installs.
#[cfg(windows)]
fn without_session(mut outcomes: Vec<Result<(), String>>, joined: &[(usize, Entry)]) -> Vec<Result<(), String>> {
    for (at, _) in joined {
        outcomes[*at] = Err(NO_SESSION.into());
    }
    outcomes
}

/// What asking for the one prompt came to.
#[cfg(windows)]
enum Prompt {
    Given,
    /// The person answered the prompt with No.
    Declined,
    /// The elevated copy could not be started at all.
    Failed(String),
}

/// Starts the launcher's own exe elevated on the manifest: the one prompt of the batch.
#[cfg(windows)]
fn ask_once(dir: &Path, manifest: &Path, apps: usize) -> Prompt {
    use winsafe::SHELLEXECUTEINFO;
    use winsafe::ShellExecuteEx;
    use winsafe::co;

    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("artcraft-launcher.exe"));
    let (Some(exe), Some(manifest), Some(dir)) = (super::msi_batch::text(&exe).ok(), super::msi_batch::text(manifest).ok(), dir.to_str()) else {
        return Prompt::Failed("unusual characters in a path".into());
    };
    let parameters = format!("--install-msis \"{manifest}\"");
    log::info!("asking once for permission to install {apps} apps together");
    match ShellExecuteEx(&SHELLEXECUTEINFO {
        verb: Some("runas"),
        file: exe,
        parameters: Some(parameters.as_str()),
        directory: Some(dir),
        show: co::SW::SHOW,
        ..Default::default()
    }) {
        Ok(()) => Prompt::Given,
        Err(e) if e == co::ERROR::CANCELLED => Prompt::Declined,
        Err(e) => Prompt::Failed(e.to_string()),
    }
}

/// How the elevated session ended, as far as the waiting side can tell.
#[cfg(windows)]
enum SessionEnd {
    /// It finished and reported; each app's installer exit code is here.
    Done(Vec<(String, i32)>),
    /// It is definitely over (its process is gone) but said nothing: the members may safely
    /// install one by one, because nothing of the session is running any more.
    OverWithoutReport,
    /// It was launched and cannot be shown to be over within the limits above, so the members
    /// hear an error instead of falling back.
    Unknown,
}

/// What a look at the elevated copy's process found.
#[cfg(windows)]
enum Seen {
    /// The process ended, or its id no longer exists.
    Over,
    /// The process could not be opened or waited for, so it may still be running.
    Unwatched,
}

/// Waits for the elevated copy: it writes its process id as its first act and its results as its
/// last, so waiting for the process to end is waiting for the results. Only a session that is
/// *shown* to be over (its process waited out, or its process id gone) lets the members fall
/// back to installing one by one; one that merely cannot be seen is never fallen back from.
/// The installer queue stays taken until then: a session that cannot be watched keeps it until
/// it reports, its process ends, or [`UNWATCHED_LIMIT`] passes.
#[cfg(windows)]
fn wait_until_done(pid_file: &Path, results: &Path, token: &str) -> SessionEnd {
    let deadline = std::time::Instant::now() + STARTED_WITHIN;
    let pid = loop {
        if let Ok(text) = std::fs::read_to_string(pid_file)
            && let Ok(pid) = text.trim().parse::<u32>()
        {
            break pid;
        }
        if std::time::Instant::now() > deadline {
            // The prompt was answered and the copy launched, but it never even said it had
            // started: whether a slow antivirus still has it, or it died at once, is not ours
            // to guess. Falling back is not safe either way.
            return SessionEnd::Unknown;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    };
    let watching_until = std::time::Instant::now() + UNWATCHED_LIMIT;
    let mut pause = || {
        std::thread::sleep(std::time::Duration::from_millis(500));
        std::time::Instant::now() <= watching_until
    };
    watch(&mut || look_at_process(pid), pid_file, results, token, &mut pause)
}

/// Waits for process `pid` when it can be opened.
#[cfg(windows)]
fn look_at_process(pid: u32) -> Seen {
    use winsafe::HPROCESS;
    use winsafe::co;

    match HPROCESS::OpenProcess(co::PROCESS::SYNCHRONIZE, false, pid) {
        Ok(process) => {
            // None is INFINITE: the session ends when its last progress window closes.
            if process.WaitForSingleObject(None).is_ok_and(|wait| wait == co::WAIT::OBJECT_0) {
                Seen::Over
            } else {
                log::warn!("waiting for the elevated session failed");
                Seen::Unwatched
            }
        }
        // ERROR_INVALID_PARAMETER is what a process id that no longer exists answers.
        Err(e) if e == co::ERROR::INVALID_PARAMETER => Seen::Over,
        Err(e) => {
            log::debug!("the elevated session could not be opened to be waited for ({e})");
            Seen::Unwatched
        }
    }
}

/// Keeps looking until the session is over. The copy writes its results and then removes its
/// process id, so a session that cannot be watched is over once its id is gone and its results
/// are this session's. `pause` waits before the next look; `false` gives up.
#[cfg(windows)]
fn watch(look: &mut dyn FnMut() -> Seen, pid_file: &Path, results: &Path, token: &str, pause: &mut dyn FnMut() -> bool) -> SessionEnd {
    loop {
        match look() {
            Seen::Over => return report(results, token),
            Seen::Unwatched => {
                if !pid_file.exists()
                    && let end @ SessionEnd::Done(_) = report(results, token)
                {
                    return end;
                }
            }
        }
        if !pause() {
            return SessionEnd::Unknown;
        }
    }
}

/// What a finished session's results file says.
#[cfg(windows)]
fn report(results: &Path, token: &str) -> SessionEnd {
    match std::fs::read_to_string(results) {
        Ok(reported) => match parse_results(&reported, token) {
            codes if codes.is_empty() => SessionEnd::OverWithoutReport,
            codes => SessionEnd::Done(codes),
        },
        Err(_) => SessionEnd::OverWithoutReport,
    }
}

/// The results file: the session's token, then one app and its installer's exit code per line.
/// A file whose first line is not this session's token is not this session's and is ignored.
fn parse_results(text: &str, token: &str) -> Vec<(String, i32)> {
    let Some((first, rest)) = text.split_once('\n') else { return Vec::new() };
    if first != token {
        return Vec::new();
    }
    rest.lines()
        .filter_map(|line| {
            let (app, code) = line.split_once('\t')?;
            let code = code.trim().parse::<i32>().ok()?;
            (!app.is_empty()).then(|| (app.to_string(), code))
        })
        .collect()
}

/// Writes `text` to a new file. Anything already at `path`, a link included, is an error and is
/// left as it is.
fn write_new(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;

    std::fs::File::options().write(true).create_new(true).open(path)?.write_all(text.as_bytes())
}

/// Writes the results file, never over anything that already exists.
fn write_results(path: &Path, token: &str, codes: &[(String, i32)]) -> Result<(), String> {
    let mut text = String::from(token);
    for (app, code) in codes {
        text.push_str(&format!("\n{app}\t{code}"));
    }
    write_new(path, &text).map_err(|e| format!("couldn't write the batch results ({e})"))
}

/// The elevated half of the session: what the launcher runs as `--install-msis <manifest>`.
/// Re-checks every package against the manifest's checksum and against this program's publisher
/// (held open, so the checked bytes are the installed ones), builds each installer's arguments
/// from that held package, runs Windows Installer on each in turn, and writes each app's exit code
/// to the results file beside the manifest — every app's, however the others went, so one failed
/// member never sends the whole batch back to one-by-one installs and their prompts. `Ok` means
/// the session ran, whatever the installers themselves decided: their outcomes are the codes in
/// the file.
#[cfg(windows)]
pub fn run_elevated(manifest: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(manifest).map_err(|e| format!("couldn't read the batch manifest ({e})"))?;
    let session: Manifest = serde_json::from_str(&text).map_err(|e| format!("couldn't understand the batch manifest ({e})"))?;
    let dir = manifest.parent().ok_or("the batch manifest has no folder")?;
    // The session only ever touches packages beside its own manifest, in the launcher's
    // downloads folder.
    for entry in &session.msis {
        if entry.msi.parent() != Some(dir) {
            return Err(format!("{} is not beside the batch manifest", entry.msi.display()));
        }
    }
    let pid_file = sibling(manifest, "pid");
    write_new(&pid_file, &std::process::id().to_string()).map_err(|e| format!("couldn't say the batch session had started ({e})"))?;
    let mut codes = Vec::new();
    for entry in &session.msis {
        codes.push((entry.app.clone(), install_one(entry)));
    }
    write_results(&sibling(manifest, "results"), &session.token, &codes)?;
    let _ = std::fs::remove_file(&pid_file);
    Ok(())
}

/// One package of the session, held while its installer runs.
#[cfg(windows)]
fn install_one(entry: &Entry) -> i32 {
    checked_install(entry, &signed_by_this_launcher, &run_installer)
}

/// Holds `entry`'s package, checks its checksum and passes it to `signed`, and only then gives
/// `run` its installer's arguments, built here from the held package itself: a manifest can say
/// where and in which context the checked file installs, never which file installs. A package
/// that cannot be held, checked or described reports "no exit code" ([`run_installer`]'s `-1`);
/// the rest of the session carries on under the one prompt.
#[cfg(windows)]
fn checked_install(entry: &Entry, signed: &dyn Fn(&Path) -> Result<(), String>, run: &dyn Fn(&str) -> Result<i32, String>) -> i32 {
    let installed = (|| -> Result<i32, String> {
        // Every link in the path is resolved first, so the name that is held and checked is the
        // name the installer's arguments carry.
        let msi = super::msi_path(&entry.msi)?;
        // The hold is the point: it keeps the package unchanged through the installer run, so
        // the checked bytes are the installed ones. Its name is never read — the file staying
        // open until the end of this closure is the whole job.
        let _held = super::msi_batch::hold(&msi, &entry.sha256)?;
        signed(&msi)?;
        let line = super::msi_batch::line(&msi, entry.destination.as_deref(), entry.scope)?;
        log::info!("{}: installing under the shared prompt", entry.app);
        run(&line)
    })();
    match installed {
        Ok(status) => status,
        Err(e) => {
            log::warn!("{}: {e}", entry.app);
            -1
        }
    }
}

/// Windows Installer on one package of the session: a progress window and no restart, the same
/// arguments a single install runs. `Err` only when it couldn't be started at all.
#[cfg(windows)]
fn run_installer(line: &str) -> Result<i32, String> {
    use std::os::windows::process::CommandExt;

    let status = crate::process::command(super::windows::msiexec())
        .raw_arg(line)
        .status()
        .map_err(|e| format!("couldn't start the Windows Installer ({e})"))?;
    Ok(status.code().unwrap_or(-1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(app: &str, msi: &Path) -> Entry {
        Entry {
            app: app.to_string(),
            msi: msi.to_path_buf(),
            sha256: "abc123".into(),
            destination: Some(Path::new(r"C:\Program Files\Photocraft").to_path_buf()),
            scope: Some(MsiScope::PerMachine),
        }
    }

    #[test]
    fn an_entry_survives_the_queue_as_json() {
        let entry = entry("photocraft", Path::new(r"C:\Downloads\photocraft.msi"));
        let target = serde_json::to_string(&entry).unwrap();
        // An entry carries where and in which context to install, never a command line: the
        // elevated side builds the installer's arguments from the held package itself.
        assert!(!target.contains("line"));
        let back = parse(&OsString::from(target)).unwrap();
        assert_eq!(back.app, entry.app);
        assert_eq!(back.msi, entry.msi);
        assert_eq!(back.sha256, entry.sha256);
        assert_eq!(back.destination, entry.destination);
        assert_eq!(back.scope, entry.scope);
        assert!(parse(&OsString::from("not json")).is_none());
    }

    #[test]
    fn the_results_survive_their_file() {
        let dir = tempfile::tempdir().unwrap();
        let results = dir.path().join("batch.results");
        let token = "0123456789abcdef";
        let codes = vec![
            (String::from("photocraft"), 0),
            (String::from("lightcraft"), 1603),
            (String::from("filmcraft"), 3010),
            (String::from("wordcraft"), -1),
        ];
        write_results(&results, token, &codes).unwrap();
        assert_eq!(parse_results(&std::fs::read_to_string(&results).unwrap(), token), codes);
        // A results file is never written over anything: a second session with the same name
        // refuses rather than report for it.
        assert!(write_results(&results, token, &codes).is_err());
        assert_eq!(
            parse_results("photocraft\t0\n\njunk\nlightcraft\t3010\n", token),
            Vec::<(String, i32)>::new(),
            "a report without this session's token is not this session's"
        );
        // A leftover from another session (a different token) is never read as this one's.
        assert_eq!(parse_results(&std::fs::read_to_string(&results).unwrap(), "ffffffffffffffff"), Vec::<(String, i32)>::new());
        assert_eq!(
            parse_results(&format!("{token}\nphotocraft\t0\n\njunk\nlightcraft\t3010\n"), token),
            vec![(String::from("photocraft"), 0), (String::from("lightcraft"), 3010)]
        );
    }

    #[cfg(windows)]
    mod waiting {
        use super::*;

        const TOKEN: &str = "0123456789abcdef";

        fn session_files(dir: &Path) -> (PathBuf, PathBuf) {
            let pid_file = dir.join("batch.pid");
            std::fs::write(&pid_file, "4242").unwrap();
            (pid_file, dir.join("batch.results"))
        }

        /// The queue stays taken while the session cannot be watched, and frees once it reports.
        #[test]
        fn a_session_that_cannot_be_watched_keeps_the_queue_until_it_reports() {
            let dir = tempfile::tempdir().unwrap();
            let (pid_file, results) = session_files(dir.path());
            let codes = vec![(String::from("photocraft"), 0)];
            let mut pauses = 0;
            let mut pause = || {
                pauses += 1;
                if pauses == 3 {
                    write_results(&results, TOKEN, &codes).unwrap();
                    std::fs::remove_file(&pid_file).unwrap();
                }
                true
            };
            let end = watch(&mut || Seen::Unwatched, &pid_file, &results, TOKEN, &mut pause);
            assert!(matches!(&end, SessionEnd::Done(reported) if *reported == codes), "the session's report was not read");
            assert_eq!(pauses, 3, "the queue was freed before the session reported");
        }

        /// A report whose process id is still there is not read: the copy removes the id last.
        #[test]
        fn results_are_not_read_while_the_process_id_is_there() {
            let dir = tempfile::tempdir().unwrap();
            let (pid_file, results) = session_files(dir.path());
            write_results(&results, TOKEN, &[(String::from("photocraft"), 0)]).unwrap();
            let end = watch(&mut || Seen::Unwatched, &pid_file, &results, TOKEN, &mut || false);
            assert!(matches!(end, SessionEnd::Unknown));
        }

        #[test]
        fn a_session_that_is_over_is_read_at_once() {
            let dir = tempfile::tempdir().unwrap();
            let (pid_file, results) = session_files(dir.path());
            let end = watch(&mut || Seen::Over, &pid_file, &results, TOKEN, &mut || panic!("no wait is needed"));
            assert!(matches!(end, SessionEnd::OverWithoutReport));
        }

        #[test]
        fn a_session_that_stays_unwatched_past_the_limit_is_unknown() {
            let dir = tempfile::tempdir().unwrap();
            let (pid_file, results) = session_files(dir.path());
            let mut left = 2;
            let mut pause = || {
                left -= 1;
                left > 0
            };
            assert!(matches!(watch(&mut || Seen::Unwatched, &pid_file, &results, TOKEN, &mut pause), SessionEnd::Unknown));
        }
    }

    #[test]
    fn session_tokens_differ() {
        let seen: std::collections::HashSet<String> = (0..64).map(|_| session_token()).collect();
        assert_eq!(seen.len(), 64, "every session's token is its own");
    }

    #[test]
    fn the_session_files_sit_beside_the_manifest() {
        let manifest = Path::new(r"C:\Cache\downloads\batch-123-45.json");
        assert_eq!(sibling(manifest, "results"), Path::new(r"C:\Cache\downloads\batch-123-45.results"));
        assert_eq!(sibling(manifest, "pid"), Path::new(r"C:\Cache\downloads\batch-123-45.pid"));
    }

    #[cfg(windows)]
    mod elevated_checks {
        use super::*;

        fn system_program(name: &str) -> PathBuf {
            PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32").join(name)
        }

        /// A package with the checksum the entry names, in a folder of its own.
        fn package(dir: &Path) -> Entry {
            let msi = dir.join("app.msi");
            std::fs::write(&msi, b"the checked package").unwrap();
            Entry { sha256: crate::http::sha256_file(&msi).unwrap(), destination: None, ..entry("app", &msi) }
        }

        /// `cmd.exe` carries a Windows signature; `cargo test` binaries carry none.
        #[test]
        fn a_file_must_carry_the_reference_publishers_signature() {
            let cmd = system_program("cmd.exe");
            let unsigned = std::env::current_exe().unwrap();
            assert_eq!(signed_like(&cmd, &cmd), Ok(()));
            assert!(signed_like(&cmd, &unsigned).is_err_and(|e| e.contains("isn't signed")));
            assert!(signed_like(&unsigned, &cmd).is_err_and(|e| e.contains("launcher isn't signed")));
        }

        /// Windows Installer and the signature check both open a package the session holds.
        #[test]
        fn the_signature_of_a_held_package_can_be_read() {
            let cmd = system_program("cmd.exe");
            let _held = crate::install::msi_batch::hold(&cmd, &crate::http::sha256_file(&cmd).unwrap()).unwrap();
            assert_eq!(signed_like(&cmd, &cmd), Ok(()));
        }

        /// The elevated side's refusal: whatever the manifest says, an unsigned package is never
        /// handed to the Windows Installer.
        #[test]
        fn the_elevated_side_refuses_a_package_it_cannot_verify() {
            let dir = tempfile::tempdir().unwrap();
            let entry = package(dir.path());
            let run = std::cell::Cell::new(false);
            let code = checked_install(&entry, &|_| Err("not signed".to_string()), &|_| {
                run.set(true);
                Ok(0)
            });
            assert_eq!(code, -1);
            assert!(!run.get(), "the installer ran on a package with no publisher");
        }

        #[test]
        fn a_verified_package_reaches_the_installer_by_its_resolved_path() {
            let dir = tempfile::tempdir().unwrap();
            let entry = package(dir.path());
            let seen = std::cell::RefCell::new(String::new());
            let code = checked_install(&entry, &|_| Ok(()), &|line| {
                seen.replace(line.to_string());
                Ok(0)
            });
            assert_eq!(code, 0);
            let resolved = dunce::canonicalize(&entry.msi).unwrap();
            assert!(seen.borrow().contains(&format!("\"{}\"", resolved.display())), "{}", seen.borrow());
        }

        /// The launcher's side keeps a package from another publisher out of the session.
        #[test]
        fn another_publishers_package_does_not_join_the_session() {
            let dir = tempfile::tempdir().unwrap();
            let target = OsString::from(serde_json::to_string(&package(dir.path())).unwrap());
            let (outcomes, joined, held) = join(&[target], &|_| Err("another publisher".to_string()));
            assert_eq!(outcomes, vec![Err(NO_SESSION.to_string())]);
            assert!(joined.is_empty() && held.is_empty());
        }

        #[test]
        fn a_verified_package_joins_the_session() {
            let dir = tempfile::tempdir().unwrap();
            let target = OsString::from(serde_json::to_string(&package(dir.path())).unwrap());
            let (outcomes, joined, held) = join(&[target], &|_| Ok(()));
            assert_eq!(outcomes, vec![Ok(())]);
            assert_eq!(joined.len(), 1);
            assert_eq!(held.len(), 1);
        }

        /// An elevated write goes only to a new file, so nothing planted at the process-id name
        /// is written through.
        #[test]
        fn the_session_will_not_write_over_what_is_at_its_process_id_name() {
            let dir = tempfile::tempdir().unwrap();
            let manifest = dir.path().join("batch.json");
            std::fs::write(&manifest, r#"{"token":"t","msis":[]}"#).unwrap();
            let pid_file = sibling(&manifest, "pid");
            std::fs::write(&pid_file, "keep").unwrap();
            assert!(run_elevated(&manifest).is_err());
            assert_eq!(std::fs::read_to_string(&pid_file).unwrap(), "keep");
        }
    }
}
