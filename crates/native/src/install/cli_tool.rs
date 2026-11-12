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
//! The tool must be signed by the app's own Developer ID team and notarized, the checks the app
//! itself passes (`macos_verify`) apart from the Gatekeeper assessment, which only judges apps.

use std::path::{Path, PathBuf};
use std::time::Duration;

use artcraft_launcher_core::platform::{self, Platform};
use artcraft_launcher_core::protocol::JobPhase;
use artcraft_launcher_core::release::{Asset, Release};
use artcraft_launcher_core::{CraftApp, version};

use super::{Job, bind_download_url, download, swap_into_place};
use crate::manifest::CliRecord;
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

/// Downloads, checks and installs the tool of `release` for `app`.
pub(crate) fn install(app: &CraftApp, release: &Release, asset: &Asset, job: &mut Job<'_>, run: Run<'_>) -> Result<CliRecord, String> {
    let team = app.mac_team.ok_or("ArtCraft Launcher has no macOS signing team recorded for this app, so its command-line tool wasn't installed")?;
    bind(app, release, asset)?;
    let (file, _) = download(release, asset, job)?;
    (job.phase)(JobPhase::Installing);
    let record = place(app, &release.version_label(), &file, team, job.paths, run)?;
    let _ = std::fs::remove_file(&file);
    Ok(record)
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

/// Unpacks `zip`, checks the tool inside against `team`, and puts it in place with its link.
pub(super) fn place(app: &CraftApp, version: &str, zip: &Path, team: &str, paths: &Paths, run: Run<'_>) -> Result<CliRecord, String> {
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
    let folder = root.join(app.id);
    swap_into_place(&top, &folder)?;
    let link = link(paths, &name, &folder.join(&name))?;
    Ok(CliRecord { version: version.to_string(), folder, link, installed_at: crate::now() })
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

/// `<data>/cli/bin/<name>` pointing at the tool: replaced if it is a link, never over a file of the user's.
#[cfg(unix)]
fn link(paths: &Paths, name: &str, target: &Path) -> Result<PathBuf, String> {
    let dir = paths.cli_bin();
    std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't create {} ({e})", dir.display()))?;
    let link = dir.join(name);
    match std::fs::symlink_metadata(&link) {
        Ok(m) if m.file_type().is_symlink() => std::fs::remove_file(&link).map_err(|e| format!("couldn't replace {} ({e})", link.display()))?,
        Ok(_) => return Err(format!("{} exists and isn't a link ArtCraft Launcher made, so it was left alone", link.display())),
        Err(_) => {}
    }
    std::os::unix::fs::symlink(target, &link).map_err(|e| format!("couldn't link {} ({e})", link.display()))?;
    Ok(link)
}

/// Takes away what `record` says the launcher installed: its link (when it still points at the
/// tool) and its folder (when it is inside the launcher's own `cli` folder).
pub(crate) fn remove(paths: &Paths, record: &CliRecord) -> Result<(), String> {
    if std::fs::symlink_metadata(&record.link).is_ok_and(|m| m.file_type().is_symlink())
        && std::fs::read_link(&record.link).is_ok_and(|to| to.starts_with(&record.folder))
    {
        std::fs::remove_file(&record.link).map_err(|e| format!("couldn't remove {} ({e})", record.link.display()))?;
    }
    let inside = record.folder.starts_with(paths.cli()) && record.folder != paths.cli() && record.folder != paths.cli_bin();
    if inside && std::fs::symlink_metadata(&record.folder).is_ok_and(|m| m.is_dir()) {
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
        let record = place(app(), "0.20.0", Path::new("/x/pdfcraft-cli-0.20.0-macos-universal.zip"), "DJ6XS33FX8", &paths, &mut run).unwrap();
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
            place(app(), "0.19.0", zip, "DJ6XS33FX8", &paths, &mut runner(&calls, &[("pdfcraft-cli", "old"), ("OLD-ONLY", "x")], &[])).unwrap();
        let second = place(app(), "0.20.0", zip, "DJ6XS33FX8", &paths, &mut runner(&calls, &[("pdfcraft-cli", "new")], &[])).unwrap();
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
            let old = place(app(), "0.19.0", zip, "DJ6XS33FX8", &paths, &mut runner(&calls, &[("pdfcraft-cli", "old")], &[])).unwrap();
            let mut verdicts = [true; 3];
            verdicts[failing] = false;
            let calls = RefCell::new(Vec::new());
            let err = place(app(), "0.20.0", zip, "DJ6XS33FX8", &paths, &mut runner(&calls, &[("pdfcraft-cli", "new")], &verdicts)).unwrap_err();
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
        let err = place(app(), "0.20.0", Path::new("/x/z.zip"), "DJ6XS33FX8", &paths, &mut runner(&calls, &[("LICENSE", "x")], &[])).unwrap_err();
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
        let err = place(app(), "0.20.0", Path::new("/x/z.zip"), "DJ6XS33FX8", &paths, &mut with_link).unwrap_err();
        assert!(err.contains("contains a link"), "{err}");
    }

    #[test]
    fn a_link_that_is_a_file_of_the_users_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        std::fs::create_dir_all(paths.cli_bin()).unwrap();
        std::fs::write(paths.cli_bin().join("pdfcraft-cli"), "mine").unwrap();
        let calls = RefCell::new(Vec::new());
        let err =
            place(app(), "0.20.0", Path::new("/x/z.zip"), "DJ6XS33FX8", &paths, &mut runner(&calls, &[("pdfcraft-cli", "tool")], &[])).unwrap_err();
        assert!(err.contains("isn't a link ArtCraft Launcher made"), "{err}");
        assert_eq!(std::fs::read_to_string(paths.cli_bin().join("pdfcraft-cli")).unwrap(), "mine");
    }

    #[test]
    fn removal_takes_only_what_the_launcher_installed() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let calls = RefCell::new(Vec::new());
        let record =
            place(app(), "0.20.0", Path::new("/x/z.zip"), "DJ6XS33FX8", &paths, &mut runner(&calls, &[("pdfcraft-cli", "tool")], &[])).unwrap();
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
