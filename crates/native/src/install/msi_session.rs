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
//! The elevated copy re-checks every package against the checksum its release published — with
//! the file held open, so the checked bytes are the installed ones (CWE-367) — runs Windows
//! Installer on each in turn, and writes each app's exit code to a results file beside the
//! manifest. A declined prompt cancels the whole batch; a session that cannot start sends the
//! members back to installing one by one, the way a single app always installs.

#![cfg_attr(not(windows), allow(dead_code))]

use std::ffi::OsStr;
use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

#[cfg(windows)]
use super::MsiScope;

/// What a member hears when the batch's one session could not run at all: install it by itself
/// (see [`super::windows::run_batched`], which recognises this and does exactly that). Never
/// shown to anyone.
pub(super) const NO_SESSION: &str = "no batch session";

/// How long the waiting side looks for the elevated copy's first act, writing its process id.
/// A behavioural antivirus may hold a start-up up; past this, the session counts as not started.
#[cfg(windows)]
const STARTED_WITHIN: std::time::Duration = std::time::Duration::from_secs(30);

/// One package of the batch, as both sides of the session see it.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Entry {
    /// The app the package belongs to: its key in the results.
    app: String,
    /// The checked package, in the launcher's downloads folder.
    msi: PathBuf,
    /// The SHA-256 the package was downloaded against; the elevated side checks it again before
    /// Windows Installer touches the file.
    sha256: String,
    /// The `msiexec` arguments for this package, the same ones a single install runs.
    line: String,
}

/// What the elevated copy is to do, written beside the packages and held unchanged while the
/// session runs. The results and the process id arrive in files beside it (see [`sibling`]).
#[derive(Serialize, Deserialize)]
struct Manifest {
    msis: Vec<Entry>,
}

/// The command every per-machine batch member waits under: the installer queue's merging key
/// for the one call the batch shares.
pub(super) fn command() -> Vec<OsString> {
    vec![OsString::from("msiexec")]
}

/// One member's place in the session, as the installer queue's `target`. `Err` for a package the
/// session cannot express (unusual characters in a path); the member then installs by itself.
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
        line: super::msi_batch::line(package.file, destination, scope)?,
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

    use super::msi_batch;

    // A member joins the session only if its package is held and checks out again; anything
    // else falls back to installing by itself, or hears its own error.
    let mut outcomes: Vec<Result<(), String>> = Vec::with_capacity(targets.len());
    let mut joined: Vec<(usize, Entry)> = Vec::new();
    let mut held: Vec<msi_batch::Held> = Vec::new();
    for (at, target) in targets.iter().enumerate() {
        match parse(target.as_os_str()) {
            Some(entry) => match msi_batch::hold(&entry.msi, &entry.sha256) {
                Ok(file) => {
                    held.push(file);
                    outcomes.push(Ok(()));
                    joined.push((at, entry));
                }
                Err(e) => outcomes.push(Err(e)),
            },
            None => outcomes.push(Err(NO_SESSION.into())),
        }
    }
    let Some((_, first)) = joined.first() else {
        return outcomes;
    };
    let Some(dir) = first.msi.parent() else {
        return without_session(outcomes, &joined);
    };
    let manifest = match prepare(&joined, dir, &mut held) {
        Ok(manifest) => manifest,
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
        Prompt::Given => match wait_until_done(&pid_file, &results) {
            Some(codes) => {
                for (at, entry) in &joined {
                    outcomes[*at] = match codes.iter().find(|(app, _)| app == &entry.app) {
                        Some((_, code)) => super::explain(Some(*code))
                            .map_err(|e| if *code == 1602 { e } else { format!("{e}. Its log is at {}", super::msi_log(&entry.msi).display()) }),
                        None => Err(format!("the shared install session never reported {}'s result", entry.app)),
                    };
                }
            }
            None => {
                log::warn!("the elevated session didn't report; installing one by one");
                outcomes = without_session(outcomes, &joined);
            }
        },
    }
    for file in [&manifest, &pid_file, &results] {
        let _ = std::fs::remove_file(file);
    }
    outcomes
}

/// Writes the manifest the elevated copy reads, holding it and every package unchanged for as
/// long as the session runs.
#[cfg(windows)]
fn prepare(joined: &[(usize, Entry)], dir: &Path, held: &mut Vec<super::msi_batch::Held>) -> Result<PathBuf, String> {
    use std::io::Write as _;
    use std::os::windows::fs::OpenOptionsExt;

    /// Others may read a held file, but not write, rename or delete it (see [`msi_batch::hold`]).
    const FILE_SHARE_READ: u32 = 0x1;

    let base = format!("batch-{}-{}", std::process::id(), crate::now());
    let manifest = (0..)
        .map(|n| dir.join(format!("{base}{}.json", if n == 0 { String::new() } else { n.to_string() })))
        .find(|p| !p.exists())
        .ok_or("no free name for the batch manifest")?;
    let text = serde_json::to_string(&Manifest { msis: joined.iter().map(|(_, entry)| entry.clone()).collect() })
        .map_err(|e| format!("couldn't describe the batch ({e})"))?;
    let mut file = std::fs::File::options()
        .write(true)
        .create_new(true)
        .share_mode(FILE_SHARE_READ)
        .open(&manifest)
        .map_err(|e| format!("couldn't write the batch manifest ({e})"))?;
    file.write_all(text.as_bytes()).map_err(|e| format!("couldn't write the batch manifest ({e})"))?;
    held.push(super::msi_batch::Held::keep(file));
    Ok(manifest)
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
        file: exe.as_str(),
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

/// Waits for the elevated copy: it writes its process id as its first act and its results as its
/// last, so waiting for the process to end is waiting for the results. `None` when it never
/// started or never reported: the members then install one by one.
#[cfg(windows)]
fn wait_until_done(pid_file: &Path, results: &Path) -> Option<Vec<(String, i32)>> {
    use winsafe::HPROCESS;
    use winsafe::co;

    let deadline = std::time::Instant::now() + STARTED_WITHIN;
    let pid = loop {
        if let Ok(text) = std::fs::read_to_string(pid_file)
            && let Ok(pid) = text.trim().parse::<u32>()
        {
            break pid;
        }
        if std::time::Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    };
    // None is INFINITE: the session ends when its last Windows Installer progress window closes.
    if let Ok(process) = HPROCESS::OpenProcess(co::PROCESS::SYNCHRONIZE, false, pid)
        && !process.WaitForSingleObject(None).is_ok_and(|wait| wait == co::WAIT::OBJECT_0)
    {
        log::warn!("the elevated session could not be waited for");
    }
    let reported = std::fs::read_to_string(results).ok()?;
    let codes = parse_results(&reported);
    (!codes.is_empty()).then_some(codes)
}

/// The results file: one app and its installer's exit code per line.
fn parse_results(text: &str) -> Vec<(String, i32)> {
    text.lines()
        .filter_map(|line| {
            let (app, code) = line.split_once('\t')?;
            let code = code.trim().parse::<i32>().ok()?;
            (!app.is_empty()).then(|| (app.to_string(), code))
        })
        .collect()
}

/// Writes the results file, never over anything that already exists.
fn write_results(path: &Path, codes: &[(String, i32)]) -> Result<(), String> {
    use std::io::Write as _;

    let text = codes.iter().map(|(app, code)| format!("{app}\t{code}")).collect::<Vec<_>>().join("\n");
    let mut file = std::fs::File::options().write(true).create_new(true).open(path).map_err(|e| format!("couldn't write the batch results ({e})"))?;
    file.write_all(text.as_bytes()).map_err(|e| format!("couldn't write the batch results ({e})"))
}

/// The elevated half of the session: what the launcher runs as `--install-msis <manifest>`.
/// Re-checks every package against its published checksum (held open, so the checked bytes are
/// the installed ones), runs Windows Installer on each in turn, and writes each app's exit code
/// to the results file beside the manifest. `Ok` means the session ran, whatever the installers
/// themselves decided: their outcomes are the codes in the file.
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
    std::fs::write(&pid_file, std::process::id().to_string()).map_err(|e| format!("couldn't say the batch session had started ({e})"))?;
    let mut codes = Vec::new();
    for entry in &session.msis {
        let held = super::msi_batch::hold(&entry.msi, &entry.sha256).map_err(|e| {
            let _ = std::fs::remove_file(&pid_file);
            e
        })?;
        log::info!("{}: installing under the shared prompt", entry.app);
        let status = run_installer(&entry.line).map_err(|e| {
            let _ = std::fs::remove_file(&pid_file);
            e
        })?;
        codes.push((entry.app.clone(), status));
        drop(held);
    }
    write_results(&sibling(manifest, "results"), &codes)?;
    let _ = std::fs::remove_file(&pid_file);
    Ok(())
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
        Entry { app: app.to_string(), msi: msi.to_path_buf(), sha256: "abc123".into(), line: "/i \"msi\" /passive /norestart".into() }
    }

    #[test]
    fn an_entry_survives_the_queue_as_json() {
        let entry = entry("photocraft", Path::new(r"C:\Downloads\photocraft.msi"));
        let target = serde_json::to_string(&entry).unwrap();
        let back = parse(&OsString::from(target)).unwrap();
        assert_eq!(back.app, entry.app);
        assert_eq!(back.msi, entry.msi);
        assert_eq!(back.sha256, entry.sha256);
        assert_eq!(back.line, entry.line);
        assert!(parse(&OsString::from("not json")).is_none());
    }

    #[test]
    fn the_results_survive_their_file() {
        let dir = tempfile::tempdir().unwrap();
        let results = dir.path().join("batch.results");
        let codes = vec![(String::from("photocraft"), 0), (String::from("lightcraft"), 1603), (String::from("filmcraft"), 3010)];
        write_results(&results, &codes).unwrap();
        assert_eq!(parse_results(&std::fs::read_to_string(&results).unwrap()), codes);
        // A results file is never written over anything: a second session with the same name
        // refuses rather than report for it.
        assert!(write_results(&results, &codes).is_err());
        assert_eq!(
            parse_results("photocraft\t0\n\njunk\nlightcraft\t3010\n"),
            vec![(String::from("photocraft"), 0), (String::from("lightcraft"), 3010)]
        );
    }

    #[test]
    fn the_session_files_sit_beside_the_manifest() {
        let manifest = Path::new(r"C:\Cache\downloads\batch-123-45.json");
        assert_eq!(sibling(manifest, "results"), Path::new(r"C:\Cache\downloads\batch-123-45.results"));
        assert_eq!(sibling(manifest, "pid"), Path::new(r"C:\Cache\downloads\batch-123-45.pid"));
    }
}
