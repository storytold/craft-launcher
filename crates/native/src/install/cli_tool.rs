//! The command-line tool of an app, installed beside it on request (macOS).
//!
//! A macOS disk image holds only the app; each release publishes the tool as a zip of its own
//! (`<app>-cli-<version>-macos-universal.zip`, checked against the release's `SHA256SUMS.txt` like
//! every download). The tool goes into `<data>/cli/<app>/`, replaced whole on an update (the new
//! folder is put in place, then the old one goes: macOS kills a tool that was written over in
//! place), with a link in `<data>/cli/bin/` so one `PATH` line covers every app. The path
//! never carries a version, so a program that stored it (an MCP client's configuration) keeps
//! working after an update. Only what the launcher put there is ever removed.
//!
//! An install is a transaction: the old folder is kept until the new link is made and the record is
//! saved ([`Placed::commit`]), and any step that fails puts the old tool back ([`Placed::rollback`]).
//!
//! The tool must be signed by the app's own Developer ID team and notarized, the checks the app
//! itself passes (`macos_verify`) apart from the Gatekeeper assessment, which only judges apps.

use std::path::{Path, PathBuf};
use std::time::Duration;

use artcraft_launcher_core::platform::{self, Platform};
use artcraft_launcher_core::protocol::JobPhase;
use artcraft_launcher_core::release::{Asset, Release};
use artcraft_launcher_core::{CraftApp, version};

use super::{Job, bind_download_url, download, swap_into_place_keeping};
use crate::manifest::{CliRecord, Manifest};
use crate::paths::Paths;
use crate::process::RunOutput;

const CODESIGN: &str = "/usr/bin/codesign";
const DITTO: &str = "/usr/bin/ditto";
const TIMEOUT: Duration = Duration::from_secs(120);

/// Runs a system tool (`process::run_with_output`; a fake in tests).
pub(crate) type Run<'a> = &'a mut dyn FnMut(&str, &[&str], Duration) -> Result<RunOutput, String>;

/// The release's command-line tool for `platform`, if it publishes one.
pub(crate) fn asset<'a>(app: &CraftApp, release: &'a Release, platform: Platform) -> Option<&'a Asset> {
    let stems: Vec<&str> = app.stems().collect();
    platform::pick_cli_asset(release, &stems, platform)
}

/// The tool's file name inside its folder.
pub(crate) fn binary_name(app: &CraftApp) -> String {
    format!("{}-cli", app.id)
}

/// Downloads, checks and installs the tool of `release` for `app`. The result is not final until [`record`] has saved it.
/// `existing` is the record of the tool the launcher installed earlier, if any: it alone says the
/// folder and link at the destination are the launcher's to replace.
pub(crate) fn install(
    app: &CraftApp,
    release: &Release,
    asset: &Asset,
    existing: Option<&CliRecord>,
    job: &mut Job<'_>,
    run: Run<'_>,
) -> Result<Placed, String> {
    let team = app.mac_team.ok_or("ArtCraft Launcher has no macOS signing team recorded for this app, so its command-line tool wasn't installed")?;
    bind(app, release, asset)?;
    // Before the download: nothing here is worth fetching if the destination isn't ours to replace.
    ensure_ours(job.paths, app, existing)?;
    let (file, _) = download(release, asset, job)?;
    (job.phase)(JobPhase::Installing);
    let placed = place_keeping(app, &release.version_label(), &file, team, job.paths, existing, run, link)?;
    let _ = std::fs::remove_file(&file);
    Ok(placed)
}

/// The version in the file's name and the tag in its URL must be the release's: a feed can't
/// advertise one version and point at another's tool.
fn bind(app: &CraftApp, release: &Release, asset: &Asset) -> Result<(), String> {
    let mismatch = || format!("{}'s command-line tool download does not match the advertised version {}.", app.name, release.version_label());
    if let Some(release_version) = release.version.as_ref() {
        let stems: Vec<&str> = app.stems().collect();
        let (named, _) = platform::parse_cli_asset_name(&asset.name, &stems).ok_or_else(mismatch)?;
        if version::parse(&named).as_ref() != Some(release_version) {
            return Err(mismatch());
        }
    }
    bind_download_url(app, release, asset)
}

/// Whether the folder and the link the tool would go to are free or the launcher's own: a folder
/// is the launcher's when `existing` names it, a link when it points into the launcher's `cli`
/// folder. Anything else is a user's, and is left alone.
fn ensure_ours(paths: &Paths, app: &CraftApp, existing: Option<&CliRecord>) -> Result<(), String> {
    let conflict =
        |path: &Path| format!("{} already exists and wasn't made by ArtCraft Launcher, so the command-line tool wasn't installed", path.display());
    // A link in place of either folder would lead the install (staging, the swap, the new link) out of them.
    for dir in [paths.cli(), paths.cli_bin()] {
        if std::fs::symlink_metadata(&dir).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(format!("{} is a link, so the command-line tool wasn't installed", dir.display()));
        }
    }
    let folder = paths.cli().join(app.id);
    if std::fs::symlink_metadata(&folder).is_ok() && !existing.is_some_and(|record| record.folder == folder) {
        return Err(conflict(&folder));
    }
    let link = paths.cli_bin().join(binary_name(app));
    if link_is_ours(paths, &link) == Some(false) {
        return Err(conflict(&link));
    }
    Ok(())
}

/// Whether what is at `link` is the launcher's: a link into its own `cli` folder. `None` when nothing is there.
fn link_is_ours(paths: &Paths, link: &Path) -> Option<bool> {
    let meta = std::fs::symlink_metadata(link).ok()?;
    Some(meta.file_type().is_symlink() && std::fs::read_link(link).is_ok_and(|to| to.starts_with(paths.cli())))
}

/// A tool in place whose record isn't saved yet. [`Placed::commit`] lets the old copy go;
/// [`Placed::rollback`] puts things back as they were.
pub(crate) struct Placed {
    record: CliRecord,
    /// The folder the tool replaced, kept beside it until the install is final.
    backup: Option<tempfile::TempDir>,
    /// Whether the install made the link (a link of the launcher's own that it replaced already names the stable path).
    link_is_new: bool,
}

impl Placed {
    pub(crate) fn record(&self) -> &CliRecord {
        &self.record
    }

    /// The install is final: the old copy goes.
    pub(crate) fn commit(self) {
        drop(self);
    }

    /// Puts the previous state back: the old folder (the link names the stable path, so it works again),
    /// or nothing for a first install. A copy that can't be put back is kept and named in the error.
    pub(crate) fn rollback(self, paths: &Paths) -> Result<(), String> {
        let Placed { record, backup, link_is_new } = self;
        let kept = |backup: Option<tempfile::TempDir>| backup.map(|b| format!("; the previous tool was kept at {}", b.keep().join("old").display()));
        if !is_plain_dir(&paths.cli()) {
            return Err(format!(
                "{} isn't a real folder, so the previous command-line tool wasn't put back{}",
                paths.cli().display(),
                kept(backup).unwrap_or_default()
            ));
        }
        if link_is_new && std::fs::symlink_metadata(&record.link).is_ok_and(|m| m.file_type().is_symlink()) && is_plain_dir(&paths.cli_bin()) {
            let _ = std::fs::remove_file(&record.link);
        }
        if is_plain_dir(&record.folder)
            && let Err(e) = std::fs::remove_dir_all(&record.folder)
        {
            return Err(format!(
                "couldn't remove the new command-line tool at {} ({e}){}",
                record.folder.display(),
                kept(backup).unwrap_or_default()
            ));
        }
        if let Some(backup) = backup
            && let Err(e) = std::fs::rename(backup.path().join("old"), &record.folder)
        {
            let saved = backup.keep().join("old");
            return Err(format!("couldn't put the previous command-line tool back ({e}); it was kept at {}", saved.display()));
        }
        Ok(())
    }
}

/// Records `placed` in `manifest` and saves it with `save`. Saved: the install is final. Not saved:
/// the manifest and the disk go back to the previous tool (`previous`), or to none, so the tool that
/// worked before still does and is still recorded.
pub(crate) fn record(
    manifest: &mut Manifest,
    id: &str,
    placed: Placed,
    previous: Option<CliRecord>,
    paths: &Paths,
    save: impl FnOnce(&Manifest) -> Result<(), String>,
) -> Result<(), String> {
    manifest.cli.insert(id.to_string(), placed.record().clone());
    match save(manifest) {
        Ok(()) => {
            placed.commit();
            Ok(())
        }
        Err(e) => {
            match previous {
                Some(previous) => manifest.cli.insert(id.to_string(), previous),
                None => manifest.cli.remove(id),
            };
            match placed.rollback(paths) {
                Ok(()) => Err(format!("couldn't save its record ({e})")),
                Err(restore) => Err(format!("couldn't save its record ({e}); {restore}")),
            }
        }
    }
}

/// Unpacks `zip`, checks the tool inside against `team`, and puts it in place with its link.
/// Nothing is replaced unless every check passed and the destination is free or the launcher's own
/// (see [`ensure_ours`]). The old tool stays recoverable until the result is committed, and a
/// failure at the link puts it back at once, so a failure leaves the old tool as it was.
#[allow(clippy::too_many_arguments)] // The inputs are independent, and `make_link` is only swapped out by tests.
pub(super) fn place_keeping(
    app: &CraftApp,
    version: &str,
    zip: &Path,
    team: &str,
    paths: &Paths,
    existing: Option<&CliRecord>,
    run: Run<'_>,
    make_link: impl FnOnce(&Paths, &str, &Path) -> Result<(PathBuf, bool), String>,
) -> Result<Placed, String> {
    // First of all: staging is created inside `cli`, so a link there must be found before it is.
    ensure_ours(paths, app, existing)?;
    let root = paths.cli();
    std::fs::create_dir_all(&root).map_err(|e| format!("couldn't create {} ({e})", root.display()))?;
    // A folder of our own: a predictable name there may be the user's.
    let staging = tempfile::Builder::new()
        .prefix(".artcraft-launcher-")
        .tempdir_in(&root)
        .map_err(|e| format!("couldn't create a staging folder in {} ({e})", root.display()))?;
    let unpacked = staging.path().join("unpacked");
    let (zip_s, out_s) = (zip.to_str(), unpacked.to_str());
    let (Some(zip_s), Some(out_s)) = (zip_s, out_s) else { return Err("unusual characters in a path".into()) };
    check(run(DITTO, &["-x", "-k", zip_s, out_s], TIMEOUT), "unpacking the command-line tool")?;
    let top = super::single_top_folder(&unpacked)?;
    let name = binary_name(app);
    let bin = top.join(&name);
    refuse_links(&top)?;
    if !std::fs::symlink_metadata(&bin).is_ok_and(|m| m.is_file()) {
        return Err(format!("the download doesn't contain {name}"));
    }
    verify(team, &bin, run)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).map_err(|e| format!("couldn't make {name} runnable ({e})"))?;
    }
    // Checked again here, and before the swap: the link must be settled while the old tool is still there.
    ensure_ours(paths, app, existing)?;
    // What the link needs is checked before the old tool is touched: a file where its folder should be stops the install here.
    let bin_dir = paths.cli_bin();
    std::fs::create_dir_all(&bin_dir).map_err(|e| format!("couldn't create {} ({e})", bin_dir.display()))?;
    if !is_plain_dir(&bin_dir) {
        return Err(format!("{} isn't a real folder, so the command-line tool wasn't installed", bin_dir.display()));
    }
    let folder = root.join(app.id);
    let backup = swap_into_place_keeping(&top, &folder)?;
    let record = CliRecord { version: version.to_string(), folder: folder.clone(), link: bin_dir.join(&name), installed_at: crate::now() };
    match make_link(paths, &name, &folder.join(&name)) {
        Ok((_, link_is_new)) => Ok(Placed { record, backup, link_is_new }),
        Err(e) => {
            // The old folder goes back before anything is reported: a failed update must not cost the working tool.
            let undo = Placed { record, backup, link_is_new: false }.rollback(paths);
            Err(match undo {
                Ok(()) => e,
                Err(restore) => format!("{e}; {restore}"),
            })
        }
    }
}

/// Nothing in the unpacked tool may be a link: it could lead out of the folder.
fn refuse_links(dir: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_symlink() {
            return Err("the command-line tool's download contains a link, so it wasn't installed".into());
        }
        if kind.is_dir() {
            refuse_links(&entry.path())?;
        }
    }
    Ok(())
}

fn check(result: Result<RunOutput, String>, what: &str) -> Result<(), String> {
    let output = result.map_err(|reason| format!("{what} did not succeed ({reason})"))?;
    if output.success {
        return Ok(());
    }
    let reason = output.stderr.trim();
    let reason = if reason.is_empty() { output.status } else { reason.to_string() };
    Err(format!("{what} did not succeed ({reason})"))
}

/// Signature integrity, the app's own Developer ID team, and notarization: each must hold before
/// anything is replaced.
fn verify(team: &str, bin: &Path, run: Run<'_>) -> Result<(), String> {
    let path = bin.to_str().ok_or("couldn't check the tool's unusual path, so it wasn't installed")?;
    if team.is_empty() || !team.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err("no valid macOS signing team is configured, so the command-line tool wasn't installed".into());
    }
    // Developer ID Application's standard requirement, as for the app itself.
    let requirement = format!(
        "=anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists \
         and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU]=\"{team}\""
    );
    for (what, args) in [
        ("signature integrity", vec!["--verify", "--all-architectures", "--strict", path]),
        ("developer identity", vec!["--verify", "--all-architectures", "--strict", "-R", &requirement, path]),
        ("notarization", vec!["--verify", "--check-notarization", "-R=notarized", path]),
    ] {
        check(run(CODESIGN, &args, TIMEOUT), &format!("macOS verification of the command-line tool ({what})"))
            .map_err(|e| format!("{e}, so it wasn't installed"))?;
    }
    Ok(())
}

/// `<data>/cli/bin/<name>` pointing at the tool, and whether the link is new. A link of the launcher's
/// own is replaced in one step (the new link is made beside it and renamed over it), so the old one
/// is never gone while the new one isn't there; anything else at the name is never touched (see [`ensure_ours`]).
#[cfg(unix)]
fn link(paths: &Paths, name: &str, target: &Path) -> Result<(PathBuf, bool), String> {
    let dir = paths.cli_bin();
    std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't create {} ({e})", dir.display()))?;
    let link = dir.join(name);
    // Decided here, at the replacement, and not only before the swap: only a link of ours is replaced, and a
    // new name is made with `symlink`, which never overwrites, so a file that appears after this check stops the install.
    match link_is_ours(paths, &link) {
        Some(true) => {
            let staged = dir.join(format!(".{name}.artcraft-launcher-new"));
            let _ = std::fs::remove_file(&staged);
            std::os::unix::fs::symlink(target, &staged).map_err(|e| format!("couldn't link {} ({e})", link.display()))?;
            if let Err(e) = std::fs::rename(&staged, &link) {
                let _ = std::fs::remove_file(&staged);
                return Err(format!("couldn't replace {} ({e})", link.display()));
            }
            Ok((link, false))
        }
        Some(false) => {
            Err(format!("{} already exists and wasn't made by ArtCraft Launcher, so the command-line tool wasn't installed", link.display()))
        }
        None => {
            std::os::unix::fs::symlink(target, &link).map_err(|e| format!("couldn't link {} ({e})", link.display()))?;
            Ok((link, true))
        }
    }
}

/// A real folder: not a link, whatever it points at.
fn is_plain_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

/// Takes away what `record` says the launcher installed: its link (when it still points at the
/// tool) and its folder (when it is a folder directly inside the launcher's own `cli` folder).
/// The `cli` and `cli/bin` folders themselves must be real folders: a link there could lead
/// anywhere, and nothing is removed through it. An error leaves the record in place for a retry.
pub(crate) fn remove(paths: &Paths, record: &CliRecord) -> Result<(), String> {
    let (root, bin_dir) = (paths.cli(), paths.cli_bin());
    if std::fs::symlink_metadata(&record.link).is_ok_and(|m| m.file_type().is_symlink())
        && record.link.parent() == Some(bin_dir.as_path())
        && std::fs::read_link(&record.link).is_ok_and(|to| to.starts_with(&record.folder))
    {
        if !is_plain_dir(&root) || !is_plain_dir(&bin_dir) {
            return Err(format!("{} isn't a real folder, so nothing was removed through it", bin_dir.display()));
        }
        std::fs::remove_file(&record.link).map_err(|e| format!("couldn't remove {} ({e})", record.link.display()))?;
    }
    let inside = record.folder.parent() == Some(root.as_path()) && record.folder != bin_dir;
    if inside && std::fs::symlink_metadata(&record.folder).is_ok() {
        if !is_plain_dir(&root) || !is_plain_dir(&record.folder) {
            return Err(format!("{} isn't a real folder, so nothing was removed through it", record.folder.display()));
        }
        std::fs::remove_dir_all(&record.folder).map_err(|e| format!("couldn't remove {} ({e})", record.folder.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use artcraft_launcher_core::{Arch, Os};
    use std::cell::RefCell;

    fn ok() -> Result<RunOutput, String> {
        Ok(RunOutput { success: true, code: Some(0), stdout: String::new(), stderr: String::new(), status: "exit 0".into() })
    }

    fn app() -> &'static CraftApp {
        CraftApp::by_id("pdfcraft").unwrap()
    }

    /// An install that is final at once (what one is once its record is saved).
    fn place(
        app: &CraftApp,
        version: &str,
        zip: &Path,
        team: &str,
        paths: &Paths,
        existing: Option<&CliRecord>,
        run: Run<'_>,
    ) -> Result<CliRecord, String> {
        place_keeping(app, version, zip, team, paths, existing, run, link).map(|placed| {
            let record = placed.record().clone();
            placed.commit();
            record
        })
    }

    fn failing_link(_: &Paths, _: &str, _: &Path) -> Result<(PathBuf, bool), String> {
        Err("couldn't link the tool (disk full)".into())
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    /// A fake `ditto` that lays out the zip's usual folder, and a `codesign` that answers `verdicts` in order.
    fn runner<'a>(
        calls: &'a RefCell<Vec<String>>,
        files: &'a [(&'a str, &'a str)],
        verdicts: &'a [bool],
    ) -> impl FnMut(&str, &[&str], Duration) -> Result<RunOutput, String> + 'a {
        move |program, args, _| {
            calls.borrow_mut().push(format!("{} {}", program.rsplit('/').next().unwrap(), args.join(" ")));
            if program == DITTO {
                let out = Path::new(args[3]);
                let folder = out.join("pdfcraft-cli-0.20.0-macos-universal");
                std::fs::create_dir_all(&folder).unwrap();
                for (name, body) in files {
                    std::fs::write(folder.join(name), body).unwrap();
                }
                return ok();
            }
            let n = calls.borrow().iter().filter(|c| c.starts_with("codesign")).count();
            if verdicts.get(n - 1).copied().unwrap_or(true) {
                ok()
            } else {
                Ok(RunOutput {
                    success: false,
                    code: Some(3),
                    stdout: String::new(),
                    stderr: "code failed to satisfy specified code requirement(s)".into(),
                    status: "exit 3".into(),
                })
            }
        }
    }

    #[test]
    fn an_install_puts_the_tool_in_a_stable_folder_with_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let calls = RefCell::new(Vec::new());
        let mut run = runner(&calls, &[("pdfcraft-cli", "tool"), ("LICENSE", "x")], &[]);
        let record = place(app(), "0.20.0", Path::new("/x/pdfcraft-cli-0.20.0-macos-universal.zip"), "DJ6XS33FX8", &paths, None, &mut run).unwrap();
        assert_eq!(record.folder, paths.cli().join("pdfcraft"));
        assert_eq!(std::fs::read_to_string(record.folder.join("pdfcraft-cli")).unwrap(), "tool");
        assert_eq!(record.link, paths.cli_bin().join("pdfcraft-cli"));
        assert_eq!(std::fs::read_link(&record.link).unwrap(), record.folder.join("pdfcraft-cli"));
        let calls = calls.borrow();
        assert_eq!(calls.iter().filter(|c| c.starts_with("codesign")).count(), 3, "integrity, identity and notarization: {calls:?}");
        assert!(calls.iter().any(|c| c.contains("leaf[subject.OU]=\"DJ6XS33FX8\"")), "{calls:?}");
        // The staging folder is gone: only the app's folder and bin are left.
        let mut left: Vec<_> = std::fs::read_dir(paths.cli()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["bin", "pdfcraft"]);
    }

    #[test]
    fn an_update_replaces_the_folder_whole_and_keeps_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let calls = RefCell::new(Vec::new());
        let zip = Path::new("/x/z.zip");
        let first =
            place(app(), "0.19.0", zip, "DJ6XS33FX8", &paths, None, &mut runner(&calls, &[("pdfcraft-cli", "old"), ("OLD-ONLY", "x")], &[])).unwrap();
        let second = place(app(), "0.20.0", zip, "DJ6XS33FX8", &paths, Some(&first), &mut runner(&calls, &[("pdfcraft-cli", "new")], &[])).unwrap();
        assert_eq!((first.folder.clone(), first.link.clone()), (second.folder.clone(), second.link.clone()), "the path never carries a version");
        assert_eq!(std::fs::read_to_string(second.folder.join("pdfcraft-cli")).unwrap(), "new");
        assert!(!second.folder.join("OLD-ONLY").exists(), "the old folder went whole");
        assert_eq!(second.version, "0.20.0");
    }

    #[test]
    fn a_tool_that_fails_a_check_changes_nothing() {
        for (failing, what) in [(0, "signature integrity"), (1, "developer identity"), (2, "notarization")] {
            let dir = tempfile::tempdir().unwrap();
            let paths = Paths::under(dir.path());
            let calls = RefCell::new(Vec::new());
            let zip = Path::new("/x/z.zip");
            let old = place(app(), "0.19.0", zip, "DJ6XS33FX8", &paths, None, &mut runner(&calls, &[("pdfcraft-cli", "old")], &[])).unwrap();
            let mut verdicts = [true; 3];
            verdicts[failing] = false;
            let calls = RefCell::new(Vec::new());
            let err = place(app(), "0.20.0", zip, "DJ6XS33FX8", &paths, Some(&old), &mut runner(&calls, &[("pdfcraft-cli", "new")], &verdicts))
                .unwrap_err();
            assert!(err.contains(what) && err.contains("wasn't installed"), "{err}");
            assert_eq!(std::fs::read_to_string(old.folder.join("pdfcraft-cli")).unwrap(), "old", "the old tool stays after a failed {what}");
            assert!(std::fs::read_link(&old.link).is_ok());
        }
    }

    #[test]
    fn a_download_that_is_not_the_tool_or_holds_a_link_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let calls = RefCell::new(Vec::new());
        let err =
            place(app(), "0.20.0", Path::new("/x/z.zip"), "DJ6XS33FX8", &paths, None, &mut runner(&calls, &[("LICENSE", "x")], &[])).unwrap_err();
        assert!(err.contains("doesn't contain pdfcraft-cli"), "{err}");
        assert!(!paths.cli().join("pdfcraft").exists());

        let mut with_link = |program: &str, args: &[&str], _: Duration| -> Result<RunOutput, String> {
            assert_eq!(program, DITTO);
            let folder = Path::new(args[3]).join("pdfcraft-cli-0.20.0-macos-universal");
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("pdfcraft-cli"), "tool").unwrap();
            std::os::unix::fs::symlink("/etc/passwd", folder.join("evil")).unwrap();
            ok()
        };
        let err = place(app(), "0.20.0", Path::new("/x/z.zip"), "DJ6XS33FX8", &paths, None, &mut with_link).unwrap_err();
        assert!(err.contains("contains a link"), "{err}");
    }

    #[test]
    fn a_link_that_is_a_file_of_the_users_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        std::fs::create_dir_all(paths.cli_bin()).unwrap();
        std::fs::write(paths.cli_bin().join("pdfcraft-cli"), "mine").unwrap();
        let calls = RefCell::new(Vec::new());
        let err = place(app(), "0.20.0", Path::new("/x/z.zip"), "DJ6XS33FX8", &paths, None, &mut runner(&calls, &[("pdfcraft-cli", "tool")], &[]))
            .unwrap_err();
        assert!(err.contains("wasn't made by ArtCraft Launcher"), "{err}");
        assert_eq!(std::fs::read_to_string(paths.cli_bin().join("pdfcraft-cli")).unwrap(), "mine");
    }

    #[test]
    fn a_folder_of_the_users_at_the_destination_is_never_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        std::fs::create_dir_all(paths.cli().join("pdfcraft")).unwrap();
        std::fs::write(paths.cli().join("pdfcraft").join("keep"), "mine").unwrap();
        let calls = RefCell::new(Vec::new());
        let zip = Path::new("/x/z.zip");
        let err = place(app(), "0.20.0", zip, "DJ6XS33FX8", &paths, None, &mut runner(&calls, &[("pdfcraft-cli", "tool")], &[])).unwrap_err();
        assert!(err.contains("wasn't made by ArtCraft Launcher"), "{err}");
        assert_eq!(std::fs::read_to_string(paths.cli().join("pdfcraft").join("keep")).unwrap(), "mine");
        // A record naming that very folder makes it the launcher's to replace.
        let record =
            CliRecord { version: "0.19.0".into(), folder: paths.cli().join("pdfcraft"), link: paths.cli_bin().join("pdfcraft-cli"), installed_at: 0 };
        place(app(), "0.20.0", zip, "DJ6XS33FX8", &paths, Some(&record), &mut runner(&calls, &[("pdfcraft-cli", "tool")], &[])).unwrap();
    }

    #[test]
    fn a_link_of_the_users_at_the_name_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        std::fs::create_dir_all(paths.cli_bin()).unwrap();
        std::os::unix::fs::symlink("/etc/hosts", paths.cli_bin().join("pdfcraft-cli")).unwrap();
        let calls = RefCell::new(Vec::new());
        let err = place(app(), "0.20.0", Path::new("/x/z.zip"), "DJ6XS33FX8", &paths, None, &mut runner(&calls, &[("pdfcraft-cli", "tool")], &[]))
            .unwrap_err();
        assert!(err.contains("wasn't made by ArtCraft Launcher"), "{err}");
        assert_eq!(std::fs::read_link(paths.cli_bin().join("pdfcraft-cli")).unwrap(), Path::new("/etc/hosts"));
        assert!(!paths.cli().join("pdfcraft").exists(), "nothing was put in place");
    }

    #[test]
    fn an_update_blocked_at_the_link_keeps_the_old_tool() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let calls = RefCell::new(Vec::new());
        let zip = Path::new("/x/z.zip");
        let old = place(app(), "0.19.0", zip, "DJ6XS33FX8", &paths, None, &mut runner(&calls, &[("pdfcraft-cli", "old")], &[])).unwrap();
        // The user replaces the link with a file of their own.
        std::fs::remove_file(&old.link).unwrap();
        std::fs::write(&old.link, "mine").unwrap();
        let err = place(app(), "0.20.0", zip, "DJ6XS33FX8", &paths, Some(&old), &mut runner(&calls, &[("pdfcraft-cli", "new")], &[])).unwrap_err();
        assert!(err.contains("wasn't made by ArtCraft Launcher"), "{err}");
        assert_eq!(std::fs::read_to_string(old.folder.join("pdfcraft-cli")).unwrap(), "old", "the old tool is still there");
        assert_eq!(std::fs::read_to_string(&old.link).unwrap(), "mine");
    }

    #[test]
    fn nothing_is_removed_through_a_linked_cli_folder() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let calls = RefCell::new(Vec::new());
        let record =
            place(app(), "0.20.0", Path::new("/x/z.zip"), "DJ6XS33FX8", &paths, None, &mut runner(&calls, &[("pdfcraft-cli", "tool")], &[])).unwrap();
        // The launcher's `cli` folder is later replaced by a link to a folder that holds a `pdfcraft` of its own.
        let real = dir.path().join("data").join("real-cli");
        std::fs::rename(paths.cli(), &real).unwrap();
        std::os::unix::fs::symlink(&real, paths.cli()).unwrap();
        let err = remove(&paths, &record).unwrap_err();
        assert!(err.contains("isn't a real folder"), "{err}");
        assert!(real.join("pdfcraft").join("pdfcraft-cli").exists(), "the folder behind the link is untouched");
    }

    #[test]
    fn a_linked_cli_or_bin_folder_is_refused_before_anything_is_placed() {
        for linked in ["cli", "bin"] {
            let dir = tempfile::tempdir().unwrap();
            let paths = Paths::under(dir.path());
            let elsewhere = dir.path().join("elsewhere");
            std::fs::create_dir_all(&elsewhere).unwrap();
            if linked == "cli" {
                std::fs::create_dir_all(dir.path().join("data")).unwrap();
                std::os::unix::fs::symlink(&elsewhere, paths.cli()).unwrap();
            } else {
                std::fs::create_dir_all(paths.cli()).unwrap();
                std::os::unix::fs::symlink(&elsewhere, paths.cli_bin()).unwrap();
            }
            let calls = RefCell::new(Vec::new());
            let err =
                place(app(), "0.20.0", Path::new("/x/z.zip"), "DJ6XS33FX8", &paths, None, &mut runner(&calls, &[("pdfcraft-cli", "tool")], &[]))
                    .unwrap_err();
            assert!(err.contains("is a link"), "{linked}: {err}");
            assert!(calls.borrow().is_empty(), "nothing was unpacked or checked");
            assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0, "nothing was written through the {linked} link");
        }
    }

    #[test]
    fn the_link_step_itself_refuses_a_file_that_is_not_the_launchers() {
        // The check before the swap can be out of date by the time the link is made: link() decides again.
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        std::fs::create_dir_all(paths.cli_bin()).unwrap();
        std::fs::write(paths.cli_bin().join("pdfcraft-cli"), "appeared late").unwrap();
        let err = link(&paths, "pdfcraft-cli", &paths.cli().join("pdfcraft").join("pdfcraft-cli")).unwrap_err();
        assert!(err.contains("wasn't made by ArtCraft Launcher"), "{err}");
        assert_eq!(std::fs::read_to_string(paths.cli_bin().join("pdfcraft-cli")).unwrap(), "appeared late");
    }

    #[test]
    fn removal_takes_only_what_the_launcher_installed() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let calls = RefCell::new(Vec::new());
        let record =
            place(app(), "0.20.0", Path::new("/x/z.zip"), "DJ6XS33FX8", &paths, None, &mut runner(&calls, &[("pdfcraft-cli", "tool")], &[])).unwrap();
        let other = paths.cli_bin().join("other-cli");
        std::fs::write(&other, "not ours").unwrap();
        remove(&paths, &record).unwrap();
        assert!(!record.folder.exists());
        assert!(std::fs::symlink_metadata(&record.link).is_err());
        assert!(other.exists(), "a file beside the link stays");

        // A record pointing outside the launcher's folder removes nothing.
        let outside = dir.path().join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep"), "x").unwrap();
        let wild = CliRecord { version: "1".into(), folder: outside.clone(), link: paths.cli_bin().join("nolink"), installed_at: 0 };
        remove(&paths, &wild).unwrap();
        assert!(outside.join("keep").exists());
    }

    #[test]
    fn a_file_where_the_bin_folder_should_be_stops_the_install_before_the_swap() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let calls = RefCell::new(Vec::new());
        let zip = Path::new("/x/z.zip");
        // A first install: nothing is left behind.
        std::fs::create_dir_all(paths.cli()).unwrap();
        std::fs::write(paths.cli_bin(), "a file of the user's").unwrap();
        let err = place(app(), "0.20.0", zip, "DJ6XS33FX8", &paths, None, &mut runner(&calls, &[("pdfcraft-cli", "tool")], &[])).unwrap_err();
        assert!(err.contains("couldn't create") || err.contains("isn't a real folder"), "{err}");
        assert_eq!(names(&paths.cli()), ["bin"], "no folder, no staging, no backup left in cli");
        assert_eq!(std::fs::read_to_string(paths.cli_bin()).unwrap(), "a file of the user's");
        // An update: the old tool is still there when the install stops.
        std::fs::remove_file(paths.cli_bin()).unwrap();
        let old = place(app(), "0.19.0", zip, "DJ6XS33FX8", &paths, None, &mut runner(&calls, &[("pdfcraft-cli", "old")], &[])).unwrap();
        std::fs::remove_file(&old.link).unwrap();
        std::fs::remove_dir(paths.cli_bin()).unwrap();
        std::fs::write(paths.cli_bin(), "a file of the user's").unwrap();
        let err = place(app(), "0.20.0", zip, "DJ6XS33FX8", &paths, Some(&old), &mut runner(&calls, &[("pdfcraft-cli", "new")], &[])).unwrap_err();
        assert!(err.contains("couldn't create") || err.contains("isn't a real folder"), "{err}");
        assert_eq!(std::fs::read_to_string(old.folder.join("pdfcraft-cli")).unwrap(), "old", "the old tool wasn't touched");
        assert_eq!(names(&paths.cli()), ["bin", "pdfcraft"]);
    }

    #[test]
    fn a_first_install_that_fails_at_the_link_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let calls = RefCell::new(Vec::new());
        let err = place_keeping(
            app(),
            "0.20.0",
            Path::new("/x/z.zip"),
            "DJ6XS33FX8",
            &paths,
            None,
            &mut runner(&calls, &[("pdfcraft-cli", "tool")], &[]),
            failing_link,
        )
        .err()
        .unwrap();
        assert!(err.contains("disk full"), "{err}");
        assert!(!paths.cli().join("pdfcraft").exists(), "the new folder was taken away again");
        assert_eq!(names(&paths.cli()), ["bin"]);
    }

    #[test]
    fn an_update_that_fails_at_the_link_keeps_the_old_tool_and_its_link() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let calls = RefCell::new(Vec::new());
        let zip = Path::new("/x/z.zip");
        let old =
            place(app(), "0.19.0", zip, "DJ6XS33FX8", &paths, None, &mut runner(&calls, &[("pdfcraft-cli", "old"), ("OLD-ONLY", "x")], &[])).unwrap();
        let err =
            place_keeping(app(), "0.20.0", zip, "DJ6XS33FX8", &paths, Some(&old), &mut runner(&calls, &[("pdfcraft-cli", "new")], &[]), failing_link)
                .err()
                .unwrap();
        assert!(err.contains("disk full"), "{err}");
        assert_eq!(std::fs::read_to_string(old.folder.join("pdfcraft-cli")).unwrap(), "old");
        assert!(old.folder.join("OLD-ONLY").exists(), "the old folder came back whole");
        assert_eq!(std::fs::read_link(&old.link).unwrap(), old.folder.join("pdfcraft-cli"), "the old link is untouched");
        assert_eq!(names(&paths.cli()), ["bin", "pdfcraft"], "no backup or staging folder is left");
        assert_eq!(names(&paths.cli_bin()), ["pdfcraft-cli"], "no half-made link is left");
    }

    #[test]
    fn rolling_back_restores_the_previous_tool_and_a_first_install_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let calls = RefCell::new(Vec::new());
        let zip = Path::new("/x/z.zip");
        // A first install, rolled back (its record couldn't be saved).
        let first =
            place_keeping(app(), "0.19.0", zip, "DJ6XS33FX8", &paths, None, &mut runner(&calls, &[("pdfcraft-cli", "old")], &[]), link).unwrap();
        assert!(std::fs::symlink_metadata(&first.record().link).is_ok());
        first.rollback(&paths).unwrap();
        assert_eq!(names(&paths.cli()), ["bin"]);
        assert!(names(&paths.cli_bin()).is_empty(), "the link the install made is gone");
        // An update, rolled back: the old tool works again through the same link.
        let old = place(app(), "0.19.0", zip, "DJ6XS33FX8", &paths, None, &mut runner(&calls, &[("pdfcraft-cli", "old")], &[])).unwrap();
        let updated =
            place_keeping(app(), "0.20.0", zip, "DJ6XS33FX8", &paths, Some(&old), &mut runner(&calls, &[("pdfcraft-cli", "new")], &[]), link)
                .unwrap();
        assert_eq!(std::fs::read_to_string(old.folder.join("pdfcraft-cli")).unwrap(), "new");
        updated.rollback(&paths).unwrap();
        assert_eq!(std::fs::read_to_string(old.link.clone()).unwrap(), "old", "the link reaches the old tool again");
        assert_eq!(names(&paths.cli()), ["bin", "pdfcraft"]);
    }

    #[test]
    fn a_record_that_cannot_be_saved_leaves_the_previous_tool_and_record() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let calls = RefCell::new(Vec::new());
        let zip = Path::new("/x/z.zip");
        let mut manifest = Manifest::default();
        // First install: no record, no tool.
        let placed =
            place_keeping(app(), "0.19.0", zip, "DJ6XS33FX8", &paths, None, &mut runner(&calls, &[("pdfcraft-cli", "old")], &[]), link).unwrap();
        let err = record(&mut manifest, "pdfcraft", placed, None, &paths, |_| Err("disk full".into())).unwrap_err();
        assert!(err.contains("couldn't save its record (disk full)"), "{err}");
        assert!(manifest.cli.is_empty());
        assert_eq!(names(&paths.cli()), ["bin"]);
        // An update: the previous tool, its link and its record stay.
        let placed =
            place_keeping(app(), "0.19.0", zip, "DJ6XS33FX8", &paths, None, &mut runner(&calls, &[("pdfcraft-cli", "old")], &[]), link).unwrap();
        let old = placed.record().clone();
        record(&mut manifest, "pdfcraft", placed, None, &paths, |_| Ok(())).unwrap();
        let placed =
            place_keeping(app(), "0.20.0", zip, "DJ6XS33FX8", &paths, Some(&old), &mut runner(&calls, &[("pdfcraft-cli", "new")], &[]), link)
                .unwrap();
        let err = record(&mut manifest, "pdfcraft", placed, Some(old.clone()), &paths, |_| Err("disk full".into())).unwrap_err();
        assert!(err.contains("disk full"), "{err}");
        assert_eq!(manifest.cli.get("pdfcraft"), Some(&old), "the previous record is back");
        assert_eq!(std::fs::read_to_string(&old.link).unwrap(), "old", "the previous tool works through its link");
        assert_eq!(names(&paths.cli()), ["bin", "pdfcraft"]);
    }

    #[test]
    fn a_saved_record_lets_the_old_copy_go() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let calls = RefCell::new(Vec::new());
        let zip = Path::new("/x/z.zip");
        let old = place(app(), "0.19.0", zip, "DJ6XS33FX8", &paths, None, &mut runner(&calls, &[("pdfcraft-cli", "old")], &[])).unwrap();
        let placed =
            place_keeping(app(), "0.20.0", zip, "DJ6XS33FX8", &paths, Some(&old), &mut runner(&calls, &[("pdfcraft-cli", "new")], &[]), link)
                .unwrap();
        let mut manifest = Manifest::default();
        record(&mut manifest, "pdfcraft", placed, Some(old), &paths, |_| Ok(())).unwrap();
        assert_eq!(manifest.cli.get("pdfcraft").map(|r| r.version.as_str()), Some("0.20.0"));
        assert_eq!(names(&paths.cli()), ["bin", "pdfcraft"], "no backup folder is left");
    }

    #[test]
    fn the_version_in_the_name_and_the_url_must_be_the_releases() {
        let release = |name: &str, url: &str| Release {
            tag: "v0.20.0".into(),
            version: version::parse("0.20.0"),
            assets: vec![Asset { name: name.into(), size: 1, url: url.into(), sha256: None }],
            ..Release::default()
        };
        let good = release(
            "pdfcraft-cli-0.20.0-macos-universal.zip",
            "https://github.com/x/pdfcraft/releases/download/v0.20.0/pdfcraft-cli-0.20.0-macos-universal.zip",
        );
        assert!(bind(app(), &good, &good.assets[0]).is_ok());
        let stale = release(
            "pdfcraft-cli-0.19.0-macos-universal.zip",
            "https://github.com/x/pdfcraft/releases/download/v0.20.0/pdfcraft-cli-0.19.0-macos-universal.zip",
        );
        assert!(bind(app(), &stale, &stale.assets[0]).is_err(), "an older tool under the new release");
        let other_tag = release(
            "pdfcraft-cli-0.20.0-macos-universal.zip",
            "https://github.com/x/pdfcraft/releases/download/v0.19.0/pdfcraft-cli-0.20.0-macos-universal.zip",
        );
        assert!(bind(app(), &other_tag, &other_tag.assets[0]).is_err(), "a url from another release");
        assert_eq!(
            asset(app(), &good, Platform { os: Os::MacOs, arch: Arch::Arm64 }).map(|a| a.name.as_str()),
            Some("pdfcraft-cli-0.20.0-macos-universal.zip")
        );
    }

    #[test]
    fn the_path_line_the_interface_shows_names_the_link_folder() {
        // On macOS the launcher's data folder is `~/Library/Application Support/ArtCraft Launcher`.
        let paths = Paths { data: PathBuf::from("/Users/sam/Library/Application Support/ArtCraft Launcher"), cache: PathBuf::from("/c") };
        let line = artcraft_launcher_core::protocol::MAC_CLI_PATH_LINE.replace("$HOME", "/Users/sam");
        assert_eq!(line, format!("export PATH=\"{}:$PATH\"", paths.cli_bin().display()));
    }
}
