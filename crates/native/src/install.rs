//! Installing, updating, removing and starting Craft Apps.
//!
//! Apps go where their platform keeps applications, installed the platform's own way:
//!
//! - macOS: the signed `.app` from the release DMG into `/Applications`.
//! - Windows: the release MSI (Windows Installer) into `C:\Program Files\<App>`.
//! - Linux: the distro's own package (`.deb` through apt on the Debian family, `.rpm` through
//!   dnf/zypper/rpm on the Fedora, Red Hat and SUSE families, read from `os-release`) with the
//!   system's polkit password prompt; otherwise a Flatpak, otherwise an AppImage in
//!   `~/Applications` (the AppImage convention), with a menu entry and icon in
//!   `~/.local/share` so it shows up like any other app. A Flatpak or AppImage launcher prefers
//!   its own format.
//! - FreeBSD: the release's `/usr/local` tree, with a polkit prompt (or `~/.local` without one).
//! - Linux, inside the Flatpak launcher with no usable host Flatpak (none installed, or
//!   `flatpak-spawn` not allowed): the release tarball, unpacked into the launcher's own folder
//!   (`<data>/apps/<app>`) and run inside the same sandbox with `HOME` pointed at a `home/`
//!   folder beside it, so the app's settings stay in the sandbox too.
//!
//! Every download is checked against its published SHA-256 before anything is unpacked, a
//! running app is never replaced, and a new copy is fully in place before the old one goes.

use std::path::{Path, PathBuf};

use artcraft_launcher_core::protocol::{CANCELLED, INSTALL_CANCELLED, InstallLocations, JobPhase};
use artcraft_launcher_core::release::{self, Asset, Release};
use artcraft_launcher_core::{CraftApp, InstallSource, Installed, PackageKind, version};

use crate::http::{Http, Stop, Transfer};
use crate::manifest::Record;
use crate::paths::Paths;
use crate::process;

/// What an install needs from the backend.
pub struct Job<'a> {
    pub http: &'a Http,
    pub paths: &'a Paths,
    pub stop: &'a Stop,
    pub phase: &'a mut dyn FnMut(JobPhase),
    pub install_locations: &'a InstallLocations,
    /// Write over whatever [`occupied`] found.
    pub replace: bool,
}

/// Downloads and installs `asset` of `release`. Returns a record when the launcher owns the
/// result (portable ZIP, AppImage, macOS bundle, tarball); MSI installs are found in the registry.
/// `previous` is the launcher's record of the copy being replaced, if it installed it.
pub fn install(
    app: &CraftApp,
    release: &Release,
    asset: &Asset,
    kind: PackageKind,
    existing: Option<&Installed>,
    previous: Option<&Record>,
    job: &mut Job<'_>,
) -> Result<Option<Record>, String> {
    install_at(app, release, asset, kind, existing, previous, job, None)
}

type RecordCommit<'a> = Option<&'a mut dyn FnMut(&Record) -> Result<(), String>>;

#[allow(clippy::too_many_arguments)]
pub fn install_at(
    app: &CraftApp,
    release: &Release,
    asset: &Asset,
    kind: PackageKind,
    existing: Option<&Installed>,
    previous: Option<&Record>,
    job: &mut Job<'_>,
    commit: RecordCommit<'_>,
) -> Result<Option<Record>, String> {
    #[cfg(windows)]
    let mut commit = commit;
    #[cfg(not(windows))]
    let _ = commit;
    #[allow(unused_variables)]
    let destination = destination(app, kind, existing, previous, job.install_locations)?;
    if let Some(inst) = existing
        && inst.source.launcher_can_update()
        && process::is_running(Path::new(&inst.location))
    {
        return Err(format!("{} is running. Quit it, then try again.", app.name));
    }
    if !job.replace
        && let Some(inst) = existing
        && let (Some(rel_v), Some(inst_v)) = (release.version.as_ref(), inst.version.as_ref())
        && version::is_newer(inst_v, rel_v)
    {
        return Err(format!("Cannot update {} to {}: installed version {} is newer", app.name, release.version_label(), inst_v));
    }
    log::info!("{}: starting {} to {} ({})", app.name, if existing.is_some() { "update" } else { "install" }, release.version_label(), kind.label());
    let file = download(release, asset, job)?;
    log::info!("{}: download ready ({}), running installer...", app.name, file.display());
    // Past this point the install can't be interrupted halfway.
    (job.phase)(JobPhase::Installing);
    #[allow(unused_variables)]
    let version = release.version_label();
    let result = match kind {
        #[cfg(target_os = "macos")]
        PackageKind::Dmg => {
            macos::install_dmg(app, &file, existing, destination.as_deref(), job.replace, job.paths, &macos::Expect::release(release))
                .map(|path| Some(record(&version, kind, path, Vec::new(), false)))
        }
        #[cfg(windows)]
        PackageKind::Msi => windows::run_msi(&file, destination.as_deref(), job.stop, &mut *job.phase).map(|_| None),
        #[cfg(windows)]
        PackageKind::PortableZip => {
            crate::portable::install(app, &file, &version, previous, job.paths, destination.as_deref(), job.replace, &mut |record| {
                if release.version.is_some() && crate::detect::windows::executable_version(&record.path).as_ref() != release.version.as_ref() {
                    return Err("The portable executable does not match the selected version. The installation was rolled back.".into());
                }
                match commit.as_deref_mut() {
                    Some(commit) => commit(record),
                    None => Ok(()),
                }
            })
            .map(Some)
        }
        #[cfg(target_os = "linux")]
        PackageKind::Deb | PackageKind::Rpm | PackageKind::ArchPkg | PackageKind::Flatpak => {
            linux::install_package(kind, &file, job.stop, &mut *job.phase).map(|_| None)
        }
        #[cfg(target_os = "linux")]
        PackageKind::AppImage => {
            linux::install_appimage(&file, &asset.name, destination.as_deref(), previous, job.replace).map(|(path, replaced)| {
                let files = linux::integrate(app, &path, &replaced, previous);
                Some(record(&version, kind, path, files, false))
            })
        }
        #[cfg(target_os = "freebsd")]
        PackageKind::TarGz => freebsd::install_tree(app, &file, job.paths).map(|(bin, files, root)| Some(record(&version, kind, bin, files, root))),
        #[cfg(target_os = "linux")]
        PackageKind::TarGz => linux::install_tree(app, &file, job.paths).map(|path| Some(record(&version, kind, path, Vec::new(), false))),
        other => Err(format!("{} isn't supported on this system", other.label())),
    };
    match &result {
        Ok(_) => log::info!("{}: successfully installed/updated to {}", app.name, release.version_label()),
        Err(e) if e == CANCELLED => log::info!("{}: installation cancelled", app.name),
        Err(e) => log::error!("{}: installation failed: {e}", app.name),
    }
    // A failed install keeps the download: the next try checks it against its published checksum
    // again and skips the transfer.
    if result.is_ok() {
        let _ = std::fs::remove_file(&file);
    }
    // The system's own prompt was dismissed: the download itself wasn't cancelled.
    result.map_err(|e| if e == CANCELLED { INSTALL_CANCELLED.to_string() } else { e })
}

/// What a new install would write over: something already at its destination that the launcher
/// didn't put there.
pub fn occupied(
    app: &CraftApp,
    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))] asset: &Asset,
    kind: PackageKind,
    existing: Option<&Installed>,
    previous: Option<&Record>,
    locations: &InstallLocations,
    paths: &Paths,
) -> Result<Option<PathBuf>, String> {
    if existing.is_some() || previous.is_some() {
        return Ok(None);
    }
    let folder = destination(app, kind, None, None, locations)?;
    let target = match kind {
        PackageKind::Msi => folder,
        PackageKind::PortableZip => Some(folder.unwrap_or_else(|| paths.apps()).join(app.id)),
        PackageKind::Dmg => folder.map(|f| f.join(format!("{}.app", app.name))),
        #[cfg(target_os = "linux")]
        PackageKind::AppImage => folder.or_else(crate::detect::linux::appimage_dir).map(|f| f.join(&asset.name)),
        _ => None,
    };
    Ok(target.filter(|t| std::fs::symlink_metadata(t).is_ok()))
}

/// The folder a new copy goes in: beside the launcher's copy it replaces, else the user's choice,
/// else the platform default (`None`). macOS updates find their bundle's folder themselves.
fn destination(
    app: &CraftApp,
    kind: PackageKind,
    existing: Option<&Installed>,
    previous: Option<&Record>,
    locations: &InstallLocations,
) -> Result<Option<PathBuf>, String> {
    if kind == PackageKind::PortableZip && previous.is_some_and(|record| record.kind == PackageKind::PortableZip) {
        return Ok(None);
    }
    if let Some(inst) = existing {
        let exe = Path::new(&inst.location);
        return Ok(match kind {
            PackageKind::AppImage => previous.filter(|r| r.kind == kind).and_then(|r| r.path.parent()).map(Path::to_path_buf),
            PackageKind::Msi => exe.parent().filter(|_| exe.is_absolute()).map(Path::to_path_buf),
            _ => None,
        });
    }
    let Some(folder) = locations.folder(app.id) else { return Ok(None) };
    if !matches!(kind, PackageKind::Dmg | PackageKind::AppImage | PackageKind::Msi | PackageKind::PortableZip) || process::in_sandbox() {
        return Err("custom install folders aren't supported on this system".into());
    }
    let parent = selected_folder(folder, kind != PackageKind::Msi)?;
    Ok(Some(if kind == PackageKind::Msi { parent.join(app.name) } else { parent }))
}

fn selected_folder(folder: &str, writable: bool) -> Result<PathBuf, String> {
    let path = if folder == "~" {
        dirs::home_dir().ok_or("no home folder")?
    } else if let Some(relative) = folder.strip_prefix("~/") {
        dirs::home_dir().ok_or("no home folder")?.join(relative)
    } else {
        PathBuf::from(folder)
    };
    let invalid = |reason: &str| format!("couldn't use install folder {}: {reason}", path.display());
    if folder.chars().any(|c| c.is_control() || (cfg!(windows) && c == '"')) {
        return Err(invalid("the path contains invalid characters"));
    }
    if !path.is_absolute() {
        return Err(invalid("choose an absolute path"));
    }
    if !path.is_dir() {
        return Err(invalid("the folder is unavailable. Create it or reconnect its drive first"));
    }
    if writable {
        let probe = path.join(format!(
            ".artcraft-launcher-write-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())
        ));
        let output = std::fs::OpenOptions::new().write(true).create_new(true).open(&probe).map_err(|e| invalid(&e.to_string()))?;
        drop(output);
        std::fs::remove_file(&probe).map_err(|e| invalid(&e.to_string()))?;
    }
    Ok(path)
}

#[cfg(any(windows, test))]
fn msi_folder_argument(path: &Path) -> Result<String, String> {
    let value = path.to_str().ok_or("unusual characters in a path")?;
    if value.chars().any(|c| c == '"' || c.is_control()) {
        return Err(format!("couldn't use install folder {value}: the path contains invalid characters"));
    }
    Ok(format!("INSTALLFOLDER=\"{value}\""))
}

/// Where `asset` is downloaded to (and its partial file kept while unfinished).
pub fn download_path(paths: &Paths, asset: &Asset) -> PathBuf {
    paths.downloads().join(&asset.name)
}

#[allow(dead_code)]
fn record(version: &str, kind: PackageKind, path: PathBuf, files: Vec<PathBuf>, needs_root: bool) -> Record {
    Record { version: version.to_string(), kind, path, installed_at: crate::now(), files, needs_root }
}

/// The package format to install `app` with: the format of the copy that's there (an update
/// stays a `.deb` update), else the user's choice, else the best this system supports.
pub fn choose_kinds(existing: Option<&Installed>, record: Option<&Record>, preferred: &[PackageKind]) -> Vec<PackageKind> {
    let available = available_kinds();
    let can_pacman = which("pacman").is_some();
    if let Some(inst) = existing {
        let kind = match inst.source {
            InstallSource::Launcher => record.map(|r| r.kind),
            InstallSource::MacApp => Some(PackageKind::Dmg),
            InstallSource::Msi => Some(PackageKind::Msi),
            InstallSource::Deb => Some(PackageKind::Deb),
            InstallSource::Rpm => Some(PackageKind::Rpm),
            InstallSource::Pacman => Some(PackageKind::ArchPkg),
            InstallSource::Flatpak => Some(PackageKind::Flatpak),
            InstallSource::AppImage => Some(PackageKind::AppImage),
            InstallSource::Binary => Some(PackageKind::TarGz),
        };
        if let Some(k) = kind.filter(|k| available.contains(k) || (*k == PackageKind::ArchPkg && can_pacman)) {
            return vec![k];
        }
    }
    let mut kinds: Vec<PackageKind> =
        preferred.iter().copied().filter(|k| available.contains(k) || (*k == PackageKind::ArchPkg && can_pacman)).collect();
    for k in available {
        if !kinds.contains(&k) {
            kinds.push(k);
        }
    }
    kinds
}

/// Package formats this machine can install, best first.
pub fn available_kinds() -> Vec<PackageKind> {
    #[cfg(target_os = "macos")]
    return vec![PackageKind::Dmg];
    #[cfg(windows)]
    return vec![PackageKind::Msi, PackageKind::PortableZip];
    #[cfg(target_os = "linux")]
    return linux::available_kinds();
    #[cfg(target_os = "freebsd")]
    return vec![PackageKind::TarGz];
    #[allow(unreachable_code)]
    Vec::new()
}

/// Fetches `asset` into the download folder, checked against its published SHA-256: the feed's
/// or GitHub's per-file digest, else the release's `SHA256SUMS.txt`. A release with neither is
/// refused before anything is fetched: a file nothing vouches for is never installed (#90).
pub(crate) fn download(release: &Release, asset: &Asset, job: &mut Job<'_>) -> Result<PathBuf, String> {
    if !release::valid_file_name(&asset.name) {
        return Err("the release has an unexpected file name".into());
    }
    let sha = match &asset.sha256 {
        Some(s) => Some(s.clone()),
        None => published_checksum(release, asset, job.http)?,
    };
    let Some(sha) = sha else {
        return Err(format!(
            "the release publishes no checksum for {}, so it can't be verified and wasn't installed. Try again later, or report it on GitHub.",
            asset.name
        ));
    };
    let dest = download_path(job.paths, asset);
    let phase = &mut *job.phase;
    job.http.download(&asset.url, &dest, asset.size, &sha, job.stop, &mut |t| match t {
        Transfer::Bytes { done, total } => phase(JobPhase::Downloading { done, total }),
        Transfer::Reconnecting { done, total } => phase(JobPhase::Reconnecting { done, total }),
    })?;
    (job.phase)(JobPhase::Verifying);
    Ok(dest)
}

/// The file's line in the release's `SHA256SUMS.txt`. `Ok(None)` only when the release really
/// publishes no checksum for it; a list that exists but can't be fetched is an error, so a
/// network hiccup never downgrades the check.
fn published_checksum(release: &Release, asset: &Asset, http: &Http) -> Result<Option<String>, String> {
    let Some(sums) = release.sums_asset() else { return Ok(None) };
    let Some(text) = http.get_text(&sums.url).map_err(|e| format!("couldn't fetch the release's checksums ({e}). Try again"))? else {
        return Ok(None);
    };
    Ok(release::parse_sha256sums(&text).into_iter().find(|(_, n)| *n == asset.name).map(|(h, _)| h))
}

/// When `installed` is owned by something the launcher doesn't drive, what to tell the user
/// instead of installing a second copy. AUR packages are removed through pacman like any system
/// package, but updated by the user's AUR helper, which builds as the user and can't run behind
/// the password prompt.
pub fn managed_elsewhere(app: &CraftApp, installed: &Installed, uninstall: bool) -> Option<String> {
    match installed.source {
        InstallSource::Pacman if !uninstall => {
            let pkg = installed.package.clone().unwrap_or_else(|| app.id.to_string());
            if pkg.ends_with("-git") || which("pacman").is_none() {
                let helper = ["paru", "yay"].into_iter().find(|h| which(h).is_some());
                let cmd = match helper {
                    Some(h) => format!("{h} -S {pkg}"),
                    None => format!("your AUR helper (for example: yay -S {pkg})"),
                };
                Some(format!("{} was installed from the AUR ({pkg}). Update it with {cmd}", app.name))
            } else {
                None
            }
        }
        InstallSource::Binary => Some(format!("{} was installed outside the launcher. Update or remove it the way it was installed.", app.name)),
        _ => None,
    }
}

/// Removes an installed copy. `phase` hears when the removal has to wait for another install;
/// `stop` cancels it while it waits.
pub fn uninstall(
    app: &CraftApp,
    installed: &Installed,
    record: Option<&Record>,
    paths: &Paths,
    stop: &Stop,
    phase: &mut dyn FnMut(JobPhase),
) -> Result<(), String> {
    let _ = (record, stop, &phase, paths);
    let path = PathBuf::from(&installed.location);
    if let Some(msg) = managed_elsewhere(app, installed, true) {
        return Err(msg);
    }
    if matches!(installed.source, InstallSource::MacApp | InstallSource::Msi | InstallSource::AppImage | InstallSource::Launcher)
        && process::is_running(&path)
    {
        return Err(format!("{} is running. Quit it, then try again.", app.name));
    }
    match installed.source {
        #[cfg(windows)]
        InstallSource::Launcher if record.is_some_and(|r| r.kind == PackageKind::PortableZip) => {
            crate::portable::remove(app, record.ok_or("the launcher has no record of these files")?)
        }
        #[cfg(windows)]
        InstallSource::Msi => windows::uninstall_msi(app, stop, phase),
        #[cfg(target_os = "macos")]
        InstallSource::MacApp | InstallSource::Launcher => macos::move_to_trash(&path),
        #[cfg(target_os = "linux")]
        InstallSource::Deb | InstallSource::Rpm | InstallSource::Pacman | InstallSource::Flatpak => {
            linux::remove_package(app, installed, stop, phase)
        }
        #[cfg(target_os = "freebsd")]
        InstallSource::Launcher => match record {
            Some(r) => freebsd::remove_files(&r.files, r.needs_root),
            None => Err("the launcher has no record of these files".into()),
        },
        #[cfg(target_os = "linux")]
        InstallSource::AppImage => remove_appimage(app, &path, record),
        // A tarball the launcher unpacked into its own folder: take the tree out and leave the
        // app's settings (`home/`) alone, so a reinstall finds its projects where they were.
        #[cfg(target_os = "linux")]
        InstallSource::Launcher if record.is_some_and(|r| r.kind == PackageKind::TarGz) => linux::remove_tree(app, paths),
        #[cfg(target_os = "linux")]
        InstallSource::Launcher => remove_appimage(app, &path, record),
        other => Err(format!("{} can't be removed from here", other.label())),
    }
}

/// Deletes an AppImage, the menu entry and icon the launcher wrote for it, and the menu entry
/// the app may have written for itself on first run.
#[cfg(target_os = "linux")]
fn remove_appimage(app: &CraftApp, path: &Path, record: Option<&Record>) -> Result<(), String> {
    if path.extension().is_none_or(|e| !e.eq_ignore_ascii_case("AppImage")) || !path.is_file() {
        return Err(format!("{} isn't an AppImage, so it was left alone", path.display()));
    }
    // AppImages found elsewhere (Gear Lever's ~/AppImages, ~/.local/bin) are the user's own:
    // remove only the launcher's folder or a copy the launcher recorded installing.
    let ours =
        path.parent().is_some_and(|dir| crate::detect::linux::appimage_dir().is_some_and(|own| own == dir)) || record.is_some_and(|r| r.path == path);
    if !ours {
        return Err(format!("{} wasn't installed by the launcher. Remove it the way it was added.", path.display()));
    }
    std::fs::remove_file(path).map_err(|e| format!("couldn't remove {} ({e})", path.display()))?;
    linux::unintegrate(app, path, record);
    Ok(())
}

/// Starts an installed app.
pub fn launch(app: &CraftApp, installed: &Installed, paths: &Paths) -> Result<(), String> {
    let _ = paths;
    let path = Path::new(&installed.location);
    match installed.source {
        InstallSource::Flatpak => {
            let mut cmd = process::flatpak_command();
            cmd.args(["run", &app.bundle_id()]);
            process::spawn_detached(cmd)
        }
        InstallSource::MacApp => process::open(path.as_os_str()),
        // A tarball the launcher unpacked into its own folder: run it here, against its own
        // home, rather than on the host through `flatpak-spawn` (which is what `launch_binary`
        // would do inside the sandbox). Its settings then land beside it instead of in the
        // launcher's or the host's home folder.
        #[cfg(target_os = "linux")]
        InstallSource::Launcher if path.starts_with(paths.apps()) => {
            let home = paths.app_home(app.id);
            prepare_home(&home);
            let mut cmd = process::command(path);
            for (name, value) in home_env(&home) {
                cmd.env(name, value);
            }
            if let Some(dir) = path.parent() {
                cmd.current_dir(dir);
            }
            process::spawn_detached(cmd)
        }
        _ if cfg!(target_os = "macos") && path.extension().is_some_and(|e| e == "app") => process::open(path.as_os_str()),
        _ => {
            if !path.exists() {
                return Err(format!("{} isn't where it was installed ({})", app.name, path.display()));
            }
            process::launch_binary(path, &[])
        }
    }
    .map_err(|e| format!("Couldn't open {}: {e}", app.name))
}

/// The environment a Craft App unpacked by the launcher runs with: its own `HOME` and the XDG
/// base directories inside it. Apps read their settings from these, so everything they keep
/// (settings, projects, caches) stays in that one folder.
#[cfg(target_os = "linux")]
fn home_env(home: &Path) -> [(&'static str, PathBuf); 5] {
    [
        ("HOME", home.to_path_buf()),
        ("XDG_CONFIG_HOME", home.join(".config")),
        ("XDG_DATA_HOME", home.join(".local/share")),
        ("XDG_CACHE_HOME", home.join(".cache")),
        ("XDG_STATE_HOME", home.join(".local/state")),
    ]
}

/// Creates the folders an app unpacked by the launcher expects to find, and links the host's
/// `~/.vst3` and `~/.clap` (SoundCraft's plugin folders) in when they exist, so an app running
/// inside the sandbox still sees the plugins installed on the host. Every failure here is
/// ignored: the app is worth starting even if its home is not quite ready.
#[cfg(target_os = "linux")]
fn prepare_home(home: &Path) {
    for (_, dir) in home_env(home).iter().skip(1) {
        let _ = std::fs::create_dir_all(dir);
    }
    let Some(real) = dirs::home_dir() else { return };
    if real == home {
        return;
    }
    for name in [".vst3", ".clap"] {
        let link = home.join(name);
        if link.symlink_metadata().is_ok() {
            continue;
        }
        if real.join(name).is_dir() {
            let _ = std::os::unix::fs::symlink(real.join(name), link);
        }
    }
}

/// A new folder of our own inside `dir`, removed when dropped: a predictable name there may be
/// the user's.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn staging_dir(dir: &Path) -> Result<tempfile::TempDir, String> {
    tempfile::Builder::new()
        .prefix(".artcraft-launcher-")
        .tempdir_in(dir)
        .map_err(|e| format!("couldn't create a staging folder in {} ({e})", dir.display()))
}

/// Moves `staged` into place at `dest`, keeping the old copy until the new one is there.
#[allow(dead_code)]
fn swap_into_place(staged: &Path, dest: &Path) -> Result<(), String> {
    swap_into_place_via(staged, dest, |from, to| std::fs::rename(from, to))
}

fn swap_into_place_via(staged: &Path, dest: &Path, rename: impl Fn(&Path, &Path) -> std::io::Result<()>) -> Result<(), String> {
    // Reserve a directory of our own beside the destination. A predictable sibling such as
    // `app.old` may be a user's backup, and must never be removed or overwritten.
    let backup = if dest.exists() {
        let parent = dest.parent().ok_or("the installed app has no parent folder")?;
        let backup = tempfile::Builder::new()
            .prefix(".artcraft-launcher-backup-")
            .tempdir_in(parent)
            .map_err(|e| format!("couldn't create a backup folder in {} ({e})", parent.display()))?;
        rename(dest, &backup.path().join("old")).map_err(|e| format!("couldn't move the old copy aside ({e})"))?;
        Some(backup)
    } else {
        None
    };
    if let Err(e) = rename(staged, dest) {
        if let Some(backup) = backup
            && let Err(restore) = rename(&backup.path().join("old"), dest)
        {
            // A failed rollback must not let TempDir's cleanup delete the only old copy.
            let saved = backup.keep().join("old");
            return Err(format!(
                "couldn't put the new copy in place ({e}); couldn't restore the old copy ({restore}); it was kept at {}",
                saved.display()
            ));
        }
        return Err(format!("couldn't put the new copy in place ({e})"));
    }
    Ok(())
}

/// After unpacking an archive into `dir`: its single top-level folder, if it has one.
#[allow(dead_code)]
fn single_top_folder(dir: &Path) -> Result<PathBuf, String> {
    let entries: Vec<_> = std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten().collect();
    match entries.as_slice() {
        [only] if only.path().is_dir() => Ok(only.path()),
        _ => Ok(dir.to_path_buf()),
    }
}

/// The app binary inside an unpacked archive: `bin/<id>`, `<id>` or `<id>.exe`.
#[allow(dead_code)]
fn find_binary(root: &Path, app: &CraftApp) -> Option<PathBuf> {
    let exe = if cfg!(windows) { format!("{}.exe", app.id) } else { app.id.to_string() };
    [root.join("bin").join(&exe), root.join(&exe)].into_iter().find(|p| p.is_file())
}

/// Finds a program on `PATH`.
#[allow(dead_code)]
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
}

/// Runs a command as root through polkit (`pkexec`), which shows the desktop's password prompt.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn run_as_root(args: &[&std::ffi::OsStr]) -> Result<(), String> {
    let pkexec = which("pkexec").ok_or("installing system-wide needs polkit (pkexec), which isn't installed")?;
    let status = process::command(pkexec).args(args).status().map_err(|e| format!("couldn't ask for permission ({e})"))?;
    root_outcome(status.code())
}

/// What `pkexec`'s exit code says about the command it ran.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn root_outcome(code: Option<i32>) -> Result<(), String> {
    match code {
        Some(0) => Ok(()),
        // pkexec: 126 = the password prompt was dismissed, 127 = not authorized.
        Some(126 | 127) => Err("cancelled".into()),
        Some(c) => Err(format!("the system installer stopped with error {c}")),
        None => Err("the system installer was interrupted".into()),
    }
}

/// How the error of an installer call shared by several apps ends (see `linux::shared_outcome`).
const SHARED_CALL: &str = "apps together. Try them one at a time.";

/// Whether an install failed in one installer call shared with other apps (Linux packages
/// installed together). Such a call can fail after it has installed some of its apps; any other
/// failure is the app's own.
pub fn failed_in_shared_call(error: &str) -> bool {
    cfg!(target_os = "linux") && error.ends_with(SHARED_CALL)
}

/// The system's installer runs one job at a time; the others wait their turn here.
#[cfg(any(target_os = "linux", windows))]
static INSTALLER: crate::queue::Queue = crate::queue::Queue::new();

/// For [`INSTALLER`]: reports [`JobPhase::Waiting`] while a job waits, then `working` again.
#[cfg(any(target_os = "linux", windows))]
fn turn<'a>(phase: &'a mut dyn FnMut(JobPhase), working: JobPhase) -> impl FnMut(bool) + 'a {
    move |waiting| phase(if waiting { JobPhase::Waiting } else { working })
}

/// For [`INSTALLER`]: whether the user cancelled the job while it waits.
#[cfg(any(target_os = "linux", windows))]
fn cancelled(stop: &Stop) -> impl Fn() -> bool + '_ {
    || stop.check().is_err_and(|e| e == artcraft_launcher_core::protocol::CANCELLED)
}

#[cfg(target_os = "linux")]
pub(crate) mod linux {
    use std::ffi::{OsStr, OsString};
    use std::os::unix::fs::PermissionsExt;

    use artcraft_launcher_core::distro;
    use artcraft_launcher_core::protocol::CANCELLED;

    use super::*;

    /// apt waits this long for another package tool (a software centre, automatic updates) to
    /// let go of its lock, instead of failing at once.
    const APT_WAITS: &str = "DPkg::Lock::Timeout=60";

    /// Where commands that need root go: the installer's queue, then `pkexec`.
    pub struct Root<'a> {
        pub queue: &'a crate::queue::Queue,
        pub run: &'a (dyn Fn(&[&OsStr]) -> Result<(), String> + Sync),
    }

    const SYSTEM: Root<'static> = Root { queue: &INSTALLER, run: &run_as_root };

    /// Runs `command target` as root. Jobs waiting for the same command go into one call
    /// (`apt-get install a.deb b.deb`), so the password is asked for once.
    fn as_root(
        root: &Root<'_>,
        command: &[&str],
        target: &OsStr,
        stop: &Stop,
        phase: &mut dyn FnMut(JobPhase),
        working: JobPhase,
    ) -> Result<(), String> {
        let command = command.iter().map(OsString::from).collect();
        root.queue.batched(command, target.to_os_string(), &cancelled(stop), &mut turn(phase, working), &|command, targets| {
            let args: Vec<&OsStr> = command.iter().chain(targets).map(OsString::as_os_str).collect();
            shared_outcome((root.run)(&args), targets.len())
        })
    }

    /// What one call's result means for each of the `count` apps it handled: a failure can't be
    /// pinned on one of them, and trying each again by itself would ask for the password each time.
    pub fn shared_outcome(result: Result<(), String>, count: usize) -> Result<(), String> {
        match result {
            Err(e) if count > 1 && e != CANCELLED => Err(format!("{e} while handling {count} {SHARED_CALL}")),
            other => other,
        }
    }

    /// The command that installs (or upgrades) a downloaded system package.
    pub fn install_command(kind: PackageKind) -> Option<&'static [&'static str]> {
        match kind {
            // apt resolves dependencies and records the package like any other.
            PackageKind::Deb => Some(&["apt-get", "-o", APT_WAITS, "install", "-y", "--allow-downgrades"]),
            PackageKind::Rpm if which("dnf").is_some() => Some(&["dnf", "install", "-y"]),
            PackageKind::Rpm if which("zypper").is_some() => Some(&["zypper", "--non-interactive", "install", "--allow-unsigned-rpm"]),
            PackageKind::Rpm => Some(&["rpm", "-U", "--replacepkgs"]),
            PackageKind::ArchPkg => Some(&["pacman", "-U", "--noconfirm"]),
            _ => None,
        }
    }

    /// Fedora Atomic (Silverblue, Kinoite, Bazzite…): rpm-ostree manages the root filesystem and
    /// `dnf install` of a local package is refused (Bazzite wraps `/usr/bin/dnf` to say so).
    fn immutable_ostree() -> bool {
        Path::new("/run/ostree-booted").exists()
    }

    /// What this system can install, ordered with the best default first.
    ///
    /// The default package format aligns with how the user installed the launcher itself:
    /// - Running as a Flatpak: prefers Flatpak (per-user, desktop integration). When the host
    ///   has no usable Flatpak (none installed, or `flatpak-spawn` refused) it prefers the
    ///   release tarball instead, which the launcher unpacks and runs inside the sandbox; an
    ///   AppImage cannot mount there and a distro package needs root, so both are dead ends.
    /// - Running as an AppImage: prefers AppImage.
    /// - Otherwise (system install): prefers the distro's own package format (`.deb` on the
    ///   Debian family, `.rpm` on the Fedora, Red Hat and SUSE families, `.pkg.tar.zst` on Arch,
    ///   read from `os-release` rather than from whichever tools are on `PATH`), then Flatpak if
    ///   available, then AppImage.
    pub fn available_kinds() -> Vec<PackageKind> {
        let flatpak = process::is_flatpak_available();
        let is_flatpak = process::in_sandbox();
        let is_appimage = std::env::var_os("APPIMAGE").is_some();
        let root = which("pkexec").is_some();
        let os = ["/etc/os-release", "/usr/lib/os-release"].iter().find_map(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
        let native = distro::native_package(&distro::parse_os_release(&os));
        // An ostree system can't take .deb or .rpm installs, whatever os-release says.
        let root = root && !immutable_ostree();
        let can_deb = root && native == Some(PackageKind::Deb) && which("apt-get").is_some() && which("dpkg").is_some();
        let can_rpm = root && native == Some(PackageKind::Rpm) && which("rpm").is_some();
        let can_arch = root && native == Some(PackageKind::ArchPkg) && which("pacman").is_some();
        order_available_kinds(is_flatpak, is_appimage, flatpak, can_deb, can_rpm, can_arch)
    }

    pub(crate) fn order_available_kinds(
        is_flatpak: bool,
        is_appimage: bool,
        flatpak_available: bool,
        can_deb: bool,
        can_rpm: bool,
        can_arch: bool,
    ) -> Vec<PackageKind> {
        let mut kinds = Vec::new();
        if is_flatpak && flatpak_available {
            kinds.push(PackageKind::Flatpak);
        } else if is_flatpak {
            kinds.push(PackageKind::TarGz);
        } else if is_appimage {
            kinds.push(PackageKind::AppImage);
        }

        if can_deb {
            kinds.push(PackageKind::Deb);
        }
        if can_rpm {
            kinds.push(PackageKind::Rpm);
        }
        if can_arch {
            kinds.push(PackageKind::ArchPkg);
        }

        if flatpak_available && !kinds.contains(&PackageKind::Flatpak) {
            kinds.push(PackageKind::Flatpak);
        }
        // The tarball runs inside the sandbox, so it is only ever offered there.
        if is_flatpak && !kinds.contains(&PackageKind::TarGz) {
            kinds.push(PackageKind::TarGz);
        }
        if !kinds.contains(&PackageKind::AppImage) {
            kinds.push(PackageKind::AppImage);
        }
        kinds
    }

    /// Installs (or upgrades) a downloaded package with the system's own tools.
    pub fn install_package(kind: PackageKind, file: &Path, stop: &Stop, phase: &mut dyn FnMut(JobPhase)) -> Result<(), String> {
        if kind == PackageKind::Deb && which("apt-get").is_none() {
            return Err("this system has no apt-get to install a Debian package with".into());
        }
        if kind == PackageKind::ArchPkg && which("pacman").is_none() {
            return Err("this system has no pacman to install an Arch package with".into());
        }
        install_package_via(&SYSTEM, kind, file, stop, phase)
    }

    pub fn install_package_via(root: &Root<'_>, kind: PackageKind, file: &Path, stop: &Stop, phase: &mut dyn FnMut(JobPhase)) -> Result<(), String> {
        let f = file.as_os_str();
        if let Some(command) = install_command(kind) {
            return as_root(root, command, f, stop, phase, JobPhase::Installing);
        }
        match kind {
            PackageKind::Flatpak => {
                let install_one = || {
                    log::info!("executing flatpak install --reinstall --bundle for {}", file.display());
                    let output = process::flatpak_command()
                        .args(["install", "--user", "-y", "--noninteractive", "--reinstall", "--bundle"])
                        .arg(f)
                        .output()
                        .map_err(|e| format!("couldn't start Flatpak ({e})"))?;
                    if output.status.success() {
                        log::info!("flatpak install succeeded for {}", file.display());
                        Ok(())
                    } else {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        let err = stderr.trim();
                        let err = if err.is_empty() { String::from_utf8_lossy(&output.stdout).trim().to_string() } else { err.to_string() };
                        if err.is_empty() {
                            Err("Flatpak couldn't install the bundle (it needs the Flathub remote for its runtime)".into())
                        } else {
                            let clean_err = err.strip_prefix("error: ").or_else(|| err.strip_prefix("Error: ")).unwrap_or(&err);
                            Err(format!("Flatpak couldn't install the bundle: {clean_err}"))
                        }
                    }
                };
                root.queue.alone(&cancelled(stop), &mut turn(phase, JobPhase::Installing), install_one).unwrap_or(Err(CANCELLED.into()))
            }
            other => Err(format!("{} isn't a system package", other.label())),
        }
    }

    pub fn remove_package(app: &CraftApp, installed: &Installed, stop: &Stop, phase: &mut dyn FnMut(JobPhase)) -> Result<(), String> {
        let mut remove = |command: &[&str], package: &str| as_root(&SYSTEM, command, OsStr::new(package), stop, phase, JobPhase::Removing);
        match installed.source {
            InstallSource::Deb if which("apt-get").is_some() => remove(&["apt-get", "-o", APT_WAITS, "remove", "-y"], app.id),
            InstallSource::Deb => remove(&["dpkg", "-r"], app.id),
            InstallSource::Rpm => {
                if which("dnf").is_some() {
                    remove(&["dnf", "remove", "-y"], app.id)
                } else {
                    remove(&["rpm", "-e"], app.id)
                }
            }
            // The package detected (photocraft, photocraft-bin or photocraft-git).
            InstallSource::Pacman => remove(&["pacman", "-R", "--noconfirm"], installed.package.as_deref().unwrap_or(app.id)),
            InstallSource::Flatpak => {
                let id = app.bundle_id();
                let user = process::flatpak_command().args(["uninstall", "--user", "-y", "--noninteractive", &id]).status();
                if user.is_ok_and(|s| s.success()) {
                    return Ok(());
                }
                if process::in_sandbox() {
                    let system = process::flatpak_command().args(["uninstall", "--system", "-y", "--noninteractive", &id]).status();
                    if system.is_ok_and(|s| s.success()) {
                        return Ok(());
                    }
                    return Err(format!("couldn't uninstall {id}"));
                }
                remove(&["flatpak", "uninstall", "--system", "-y", "--noninteractive"], &id)
            }
            other => Err(format!("{} isn't a system package", other.label())),
        }
    }

    pub fn install_appimage(
        file: &Path,
        name: &str,
        destination: Option<&Path>,
        previous: Option<&Record>,
        replace: bool,
    ) -> Result<(PathBuf, Vec<PathBuf>), String> {
        let dir = match destination {
            Some(dir) => dir.to_path_buf(),
            None => {
                let dir = crate::detect::linux::appimage_dir().ok_or("no home folder")?;
                std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't create {} ({e})", dir.display()))?;
                dir
            }
        };
        let dest = dir.join(name);
        let ours = previous.is_some_and(|r| r.kind == PackageKind::AppImage && r.path == dest);
        if std::fs::symlink_metadata(&dest).is_ok_and(|m| m.is_dir() || !(replace || ours && m.is_file())) {
            return Err(format!("{} already exists and wasn't installed by ArtCraft Launcher", dest.display()));
        }
        let staging = super::staging_dir(&dir)?;
        let partial = staging.path().join(name);
        std::fs::copy(file, &partial).map_err(|e| format!("couldn't copy the AppImage ({e})"))?;
        std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        std::fs::rename(&partial, &dest).map_err(|e| format!("couldn't put the AppImage in place ({e})"))?;
        let mut removed = Vec::new();
        if let Some(old) = previous.filter(|r| r.kind == PackageKind::AppImage && r.path != dest)
            && std::fs::remove_file(&old.path).is_ok()
        {
            removed.push(old.path.clone());
        }
        Ok((dest, removed))
    }

    /// Unpacks a release tarball (`bin/<app>`, `share/…`) into `<data>/apps/<app>`, a folder
    /// only the launcher owns, and returns the app's binary. This is the fallback for the
    /// Flatpak launcher when the host has no usable Flatpak: the app then runs inside the
    /// sandbox instead of on the host. `bin` and `share` are swapped in one at a time, so an
    /// update leaves the app's own `home/` beside them untouched.
    pub fn install_tree(app: &CraftApp, file: &Path, paths: &Paths) -> Result<PathBuf, String> {
        let staged = paths.cache.join(format!("{}.new", app.id));
        let result = unpack_tree(app, file, &staged, paths);
        let _ = std::fs::remove_dir_all(&staged);
        result
    }

    fn unpack_tree(app: &CraftApp, file: &Path, staged: &Path, paths: &Paths) -> Result<PathBuf, String> {
        let tree = tree::unpack(app, file, staged)?;
        let root = paths.apps().join(app.id);
        std::fs::create_dir_all(&root).map_err(|e| format!("couldn't create {} ({e})", root.display()))?;
        for dir in ["bin", "share"] {
            if tree.join(dir).is_dir() {
                swap_into_place(&tree.join(dir), &root.join(dir))?;
            }
        }
        let bin = root.join("bin").join(app.id);
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("couldn't make {} runnable ({e})", bin.display()))?;
        Ok(bin)
    }

    /// Removes a tree the launcher unpacked in [`install_tree`], keeping the app's `home/`: the
    /// settings and projects in it are the user's, not the installer's. Only the two folders the
    /// launcher put there are removed, and only inside the launcher's own apps folder.
    pub fn remove_tree(app: &CraftApp, paths: &Paths) -> Result<(), String> {
        let root = paths.apps().join(app.id);
        let mut removed = false;
        for dir in ["bin", "share"] {
            let path = root.join(dir);
            if path.starts_with(paths.apps()) && path.is_dir() {
                std::fs::remove_dir_all(&path).map_err(|e| format!("couldn't remove {} ({e})", path.display()))?;
                removed = true;
            }
        }
        if removed { Ok(()) } else { Err(format!("{} isn't installed in this folder", app.name)) }
    }

    /// Where menu entries and icons go: `$XDG_DATA_HOME`, else `~/.local/share`. Inside the
    /// launcher's own Flatpak that variable points into the sandbox, so the host's is used.
    pub fn host_data_dir() -> Option<PathBuf> {
        if Path::new("/.flatpak-info").exists() {
            if let Some(dir) = std::env::var_os("HOST_XDG_DATA_HOME").map(PathBuf::from).filter(|p| p.is_absolute()) {
                return Some(dir);
            }
            return dirs::home_dir().map(|h| h.join(".local").join("share"));
        }
        dirs::data_dir()
    }

    /// The app's 128 px icon, as bundled in `assets/app-icons`.
    fn icon_png(app: &CraftApp) -> Option<&'static [u8]> {
        Some(match app.id {
            "photocraft" => include_bytes!("../../../assets/app-icons/photocraft.png"),
            "vectorcraft" => include_bytes!("../../../assets/app-icons/vectorcraft.png"),
            "filmcraft" => include_bytes!("../../../assets/app-icons/filmcraft.png"),
            "lightcraft" => include_bytes!("../../../assets/app-icons/lightcraft.png"),
            "pdfcraft" => include_bytes!("../../../assets/app-icons/pdfcraft.png"),
            "effectcraft" => include_bytes!("../../../assets/app-icons/effectcraft.png"),
            "designcraft" => include_bytes!("../../../assets/app-icons/designcraft.png"),
            "wordcraft" => include_bytes!("../../../assets/app-icons/wordcraft.png"),
            "deckcraft" => include_bytes!("../../../assets/app-icons/deckcraft.png"),
            "gridcraft" => include_bytes!("../../../assets/app-icons/gridcraft.png"),
            "cadcraft" => include_bytes!("../../../assets/app-icons/cadcraft.png"),
            "soundcraft" => include_bytes!("../../../assets/app-icons/soundcraft.png"),
            "figcraft" => include_bytes!("../../../assets/app-icons/figcraft.png"),
            _ => return None,
        })
    }

    fn desktop_path(data: &Path, app: &CraftApp) -> PathBuf {
        data.join("applications").join(format!("{}.desktop", app.bundle_id()))
    }

    fn icon_path(data: &Path, app: &CraftApp) -> PathBuf {
        data.join("icons/hicolor/128x128/apps").join(format!("{}.png", app.bundle_id()))
    }

    /// Adds `appimage` to the desktop's menu. Best effort: a failure here never fails the
    /// install. Returns the files written, for the record.
    pub fn integrate(app: &CraftApp, appimage: &Path, replaced: &[PathBuf], previous: Option<&Record>) -> Vec<PathBuf> {
        match host_data_dir() {
            Some(data) => integrate_into(&data, app, appimage, replaced, previous),
            None => Vec::new(),
        }
    }

    /// [`integrate`] into the data folder `data`. Only writes over files the launcher wrote
    /// before: its own marked menu entry, an entry pointing at a copy this install replaced, and
    /// an icon listed in `previous`.
    pub(crate) fn integrate_into(data: &Path, app: &CraftApp, appimage: &Path, replaced: &[PathBuf], previous: Option<&Record>) -> Vec<PathBuf> {
        let ours_before = |p: &Path| previous.is_some_and(|r| r.files.iter().any(|f| f == p));
        let mut written = Vec::new();
        let icon = icon_path(data, app);
        let has_icon = match icon_png(app) {
            Some(png) if !icon.exists() || ours_before(&icon) => match crate::paths::write_atomic(&icon, png) {
                Ok(()) => {
                    written.push(icon.clone());
                    true
                }
                Err(e) => {
                    log::warn!("{}: couldn't install the menu icon: {e}", app.name);
                    false
                }
            },
            // Someone else's icon under the app's id (the app's own AppImage puts one there).
            _ => icon.is_file(),
        };
        let Some(text) = crate::desktop_entry::render(app, appimage, has_icon) else {
            log::warn!("{}: {} can't be written into a menu entry; skipping it", app.name, appimage.display());
            return written;
        };
        let desktop = desktop_path(data, app);
        let replace = match std::fs::read_to_string(&desktop) {
            Ok(old) => {
                crate::desktop_entry::is_ours(&old)
                    || crate::desktop_entry::exec_program(&old)
                        .is_some_and(|p| Path::new(&p) == appimage || replaced.iter().any(|r| r == Path::new(&p)))
            }
            Err(_) => !desktop.exists(),
        };
        if !replace {
            log::info!("{}: leaving the existing menu entry {} alone", app.name, desktop.display());
            return written;
        }
        match crate::paths::write_atomic(&desktop, text.as_bytes()) {
            Ok(()) => written.push(desktop),
            Err(e) => log::warn!("{}: couldn't add the menu entry: {e}", app.name),
        }
        written
    }

    /// Takes away what [`integrate`] added for the removed AppImage `appimage`.
    pub fn unintegrate(app: &CraftApp, appimage: &Path, record: Option<&Record>) {
        if let Some(data) = host_data_dir() {
            unintegrate_from(&data, app, appimage, record);
        }
    }

    /// [`unintegrate`] from the data folder `data`: the menu entry when it runs the removed copy,
    /// or when the launcher wrote it and what it runs is gone; the icon only when the
    /// launcher's record lists it.
    pub(crate) fn unintegrate_from(data: &Path, app: &CraftApp, appimage: &Path, record: Option<&Record>) {
        let desktop = desktop_path(data, app);
        if let Ok(text) = std::fs::read_to_string(&desktop) {
            let program = crate::desktop_entry::exec_program(&text).map(PathBuf::from);
            let runs_removed = program.as_deref() == Some(appimage);
            let ours_and_dangling = crate::desktop_entry::is_ours(&text) && program.is_none_or(|p| !p.exists());
            if runs_removed || ours_and_dangling {
                let _ = std::fs::remove_file(&desktop);
            }
        }
        let icon = icon_path(data, app);
        if record.is_some_and(|r| r.files.contains(&icon)) {
            let _ = std::fs::remove_file(&icon);
        }
    }
}

#[cfg(target_os = "macos")]
pub mod macos {
    use std::time::Duration;

    use artcraft_launcher_core::version::Version;

    use super::*;

    /// `/Applications` when we can write there, else `~/Applications`.
    fn applications_dir(existing: Option<&Installed>, destination: Option<&Path>) -> Result<PathBuf, String> {
        if let Some(parent) =
            existing.filter(|i| i.source == InstallSource::MacApp).and_then(|i| Path::new(&i.location).parent().map(Path::to_path_buf))
        {
            return Ok(parent);
        }
        if let Some(dir) = destination {
            return Ok(dir.to_path_buf());
        }
        let system = PathBuf::from("/Applications");
        let probe = system.join(".artcraft-launcher-write-test");
        if std::fs::write(&probe, b"").is_ok() {
            let _ = std::fs::remove_file(&probe);
            return Ok(system);
        }
        let user = dirs::home_dir().ok_or("no home folder")?.join("Applications");
        std::fs::create_dir_all(&user).map_err(|e| format!("couldn't create {} ({e})", user.display()))?;
        Ok(user)
    }

    /// The Apple Developer team that signs every Craft App and ArtCraft Launcher (Learning
    /// Machines LLC): the macOS side of the Windows publisher check.
    pub const TEAM_ID: &str = "DJ6XS33FX8";

    /// What a new copy must prove before it replaces anything. The checksum shows the download is
    /// the file the release lists; these show the app inside is the one the release promises.
    pub struct Expect<'a> {
        /// The team whose Developer ID signature the bundle must carry. `None` only in tests,
        /// which build unsigned bundles.
        pub team: Option<&'a str>,
        /// The release's version, which the bundle must report. `None` when the tag isn't one.
        pub version: Option<&'a Version>,
    }

    impl<'a> Expect<'a> {
        pub fn release(release: &'a Release) -> Self {
            Self { team: Some(TEAM_ID), version: release.version.as_ref() }
        }
    }

    /// Held while this process asks diskimages-helper for something (an attach here; the tests'
    /// `hdiutil create` too), since it turns concurrent requests away.
    pub(crate) static DISK_IMAGES: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Apple's tools, by full path: a launcher started from Finder or at login gets a minimal
    /// `PATH`, and nothing earlier on it should stand in for these.
    const HDIUTIL: &str = "/usr/bin/hdiutil";
    const DITTO: &str = "/usr/bin/ditto";
    const CODESIGN: &str = "/usr/bin/codesign";

    pub fn install_dmg(
        app: &CraftApp,
        dmg: &Path,
        existing: Option<&Installed>,
        destination: Option<&Path>,
        replace: bool,
        paths: &Paths,
        expect: &Expect<'_>,
    ) -> Result<PathBuf, String> {
        // A new, empty mount point for every install. A fixed one can still hold an image from an
        // attach that was interrupted or timed out, and hdiutil won't mount another over it.
        let attempt = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
        let mount = paths.cache.join(format!("mount-{}-{}-{attempt}", app.id, std::process::id()));
        std::fs::create_dir_all(&mount).map_err(|e| format!("couldn't create a folder to open the disk image in ({e})"))?;
        let (Some(dmg_s), Some(mount_s)) = (dmg.to_str(), mount.to_str()) else {
            let _ = std::fs::remove_dir(&mount);
            return Err("unusual characters in the cache folder's path".into());
        };
        // diskimages-helper refuses an attach while it is busy with another image ("Resource
        // temporarily unavailable"): attach one image at a time (several apps may be updating at
        // once), and wait a little and try again when something outside the launcher holds it.
        // The download's SHA-256 is already checked, so hdiutil's own checksum pass (-noverify)
        // would only add minutes on a large image.
        let attached = {
            let _one_at_a_time = DISK_IMAGES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let attach = || {
                process::run(
                    HDIUTIL,
                    &["attach", "-nobrowse", "-readonly", "-noautoopen", "-noverify", "-mountpoint", mount_s, dmg_s],
                    Duration::from_secs(300),
                )
            };
            let mut attached = attach();
            for wait in [1, 2, 4, 8] {
                if attached.is_ok() {
                    break;
                }
                let _ = detach(&mount);
                std::thread::sleep(Duration::from_secs(wait));
                attached = attach();
            }
            attached
        };
        if let Err(reason) = attached {
            let _ = std::fs::remove_dir(&mount);
            return Err(format!("couldn't open the disk image: {reason}"));
        }
        let result = copy_app(app, &mount, existing, destination, replace, expect);
        let _ = detach(&mount);
        let _ = std::fs::remove_dir(&mount);
        result
    }

    fn copy_app(
        app: &CraftApp,
        mount: &Path,
        existing: Option<&Installed>,
        destination: Option<&Path>,
        replace: bool,
        expect: &Expect<'_>,
    ) -> Result<PathBuf, String> {
        let bundle = format!("{}.app", app.name);
        let src = mount.join(&bundle);
        crate::detect::macos::read_bundle(app, &src).ok_or("the disk image doesn't contain the app")?;
        let dir = applications_dir(existing, destination)?;
        let dest = existing.filter(|i| i.source == InstallSource::MacApp).map_or_else(|| dir.join(&bundle), |i| PathBuf::from(&i.location));
        if !replace
            && std::fs::symlink_metadata(&dest).is_ok()
            && (std::fs::symlink_metadata(&dest).is_ok_and(|m| m.file_type().is_symlink()) || crate::detect::macos::read_bundle(app, &dest).is_none())
        {
            return Err(format!("{} already exists and wasn't installed by ArtCraft Launcher", dest.display()));
        }
        let staging = super::staging_dir(&dir)?;
        let staged = staging.path().join(&bundle);
        let (Some(src_s), Some(staged_s)) = (src.to_str(), staged.to_str()) else { return Err("unusual characters in a path".into()) };
        // ditto keeps the code signature and symlinks intact. The disk image's volume adds Finder
        // metadata (com.apple.FinderInfo) to the files; leave it and the extended attributes
        // behind, or `codesign --verify --strict` rejects the bundle as carrying "detritus". A
        // bundle's signature lives in its files, not in extended attributes.
        process::run(DITTO, &["--norsrc", "--noextattr", "--noacl", src_s, staged_s], Duration::from_secs(600))
            .map_err(|reason| format!("couldn't copy the app: {reason}"))?;
        let checked = crate::detect::macos::read_bundle(app, &staged)
            .ok_or_else(|| "the copied app is incomplete".to_string())
            .and_then(|found| check(&staged, &found, expect));
        checked?;
        swap_into_place(&staged, &dest)?;
        Ok(dest)
    }

    /// Checks a copied bundle against what its release promises, before it replaces anything.
    fn check(bundle: &Path, found: &Installed, expect: &Expect<'_>) -> Result<(), String> {
        if let Some(team) = expect.team
            && !signed_by(bundle, team)
        {
            return Err("the app isn't signed by its developer, so it wasn't installed".into());
        }
        if let Some(want) = expect.version
            && !found.version.as_ref().is_some_and(|v| same_release(v, want))
        {
            return Err(format!("the app inside isn't version {want}, so it wasn't installed"));
        }
        Ok(())
    }

    /// Whether `bundle` has a valid Developer ID signature from `team`, nested code included. The
    /// requirement pins the signer, so a valid ad-hoc or other developer's signature fails it.
    /// Unlike a Gatekeeper assessment it needs no network.
    pub fn signed_by(bundle: &Path, team: &str) -> bool {
        let Some(path) = bundle.to_str() else { return false };
        if team.is_empty() || !team.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return false;
        }
        let requirement = format!("=anchor apple generic and certificate leaf[subject.OU] = \"{team}\"");
        process::capture(CODESIGN, &["--verify", "--deep", "-R", &requirement, path], Duration::from_secs(120)).is_some()
    }

    /// Whether a bundle's version is the release's. A bundle that reports only the numbers
    /// (`CFBundleShortVersionString`) matches a pre-release with the same numbers.
    fn same_release(found: &Version, release: &Version) -> bool {
        (found.major, found.minor, found.patch) == (release.major, release.minor, release.patch) && (found.pre.is_empty() || found.pre == release.pre)
    }

    fn detach(mount: &Path) -> Option<String> {
        let m = mount.to_str()?;
        if !mount.join(".").exists() {
            return None;
        }
        process::capture(HDIUTIL, &["detach", m, "-quiet"], Duration::from_secs(60))
            .or_else(|| process::capture(HDIUTIL, &["detach", m, "-force", "-quiet"], Duration::from_secs(60)))
    }

    /// Moves an app to the user's Trash (so it can be put back).
    pub fn move_to_trash(path: &Path) -> Result<(), String> {
        let trash = dirs::home_dir().ok_or("no home folder")?.join(".Trash");
        let name = path.file_name().ok_or("not an app")?;
        let mut dest = trash.join(name);
        if dest.exists() {
            dest = trash.join(format!("{} {}", name.to_string_lossy(), crate::now()));
        }
        std::fs::rename(path, &dest).map_err(|e| format!("couldn't move {} to Trash ({e}). Remove it in Finder instead.", path.display()))
    }
}

/// FreeBSD: the release tarball holds a `/usr/local`-style tree (`bin/<app>`,
/// `share/applications`, icons). Older releases had the files flat in one folder; those are laid
/// out the same way first.
#[cfg(unix)]
#[allow(dead_code)]
mod tree {
    use super::*;

    /// Unpacks `file` and returns a staging folder laid out like `/usr/local`.
    pub fn unpack(app: &CraftApp, file: &Path, staging: &Path) -> Result<PathBuf, String> {
        let _ = std::fs::remove_dir_all(staging);
        std::fs::create_dir_all(staging).map_err(|e| format!("couldn't create {} ({e})", staging.display()))?;
        let gz = std::fs::File::open(file).map_err(|e| e.to_string())?;
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(gz));
        archive.set_preserve_permissions(true);
        // `unpack` refuses entries that would land outside `staging` (`..`, absolute paths).
        archive.unpack(staging).map_err(|e| format!("couldn't unpack the download ({e})"))?;
        let top = single_top_folder(staging)?;
        if top.join("bin").join(app.id).is_file() {
            return Ok(top);
        }
        // Flat layout: put each file where /usr/local keeps it.
        let tree = staging.join("tree");
        let id = app.bundle_id();
        for (from, to) in [
            (top.join(app.id), tree.join("bin").join(app.id)),
            (top.join(format!("{}-cli", app.id)), tree.join("bin").join(format!("{}-cli", app.id))),
            (top.join(format!("{id}.desktop")), tree.join("share/applications").join(format!("{id}.desktop"))),
            (top.join(format!("{id}.png")), tree.join("share/pixmaps").join(format!("{id}.png"))),
        ] {
            if from.is_file() {
                if let Some(dir) = to.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                }
                std::fs::rename(&from, &to).map_err(|e| e.to_string())?;
            }
        }
        if !tree.join("bin").join(app.id).is_file() {
            return Err("the download doesn't contain the app".into());
        }
        Ok(tree)
    }

    /// Every file under `root`, relative to it.
    pub fn files(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Ok(rel) = p.strip_prefix(root) {
                    out.push(rel.to_path_buf());
                }
            }
        }
        out.sort();
        out
    }
}

#[cfg(target_os = "freebsd")]
mod freebsd {
    use std::ffi::OsStr;

    use super::*;

    /// Copies the release's tree into `/usr/local` (asking for permission through polkit), or
    /// into `~/.local` when polkit isn't installed. Returns the binary, every file installed,
    /// and whether removing them needs root.
    pub fn install_tree(app: &CraftApp, file: &Path, paths: &Paths) -> Result<(PathBuf, Vec<PathBuf>, bool), String> {
        let staging = paths.cache.join(format!("{}.new", app.id));
        let result = install_from(app, file, &staging);
        let _ = std::fs::remove_dir_all(&staging);
        result
    }

    fn install_from(app: &CraftApp, file: &Path, staging: &Path) -> Result<(PathBuf, Vec<PathBuf>, bool), String> {
        let tree = tree::unpack(app, file, staging)?;
        let rel = tree::files(&tree);
        let mut source = tree.into_os_string();
        source.push("/.");
        if which("pkexec").is_some() {
            let prefix = PathBuf::from("/usr/local");
            run_as_root(&[OsStr::new("/bin/cp"), OsStr::new("-R"), &source, prefix.as_os_str()])?;
            return Ok((prefix.join("bin").join(app.id), rel.iter().map(|r| prefix.join(r)).collect(), true));
        }
        let prefix = dirs::home_dir().ok_or("no home folder")?.join(".local");
        std::fs::create_dir_all(&prefix).map_err(|e| e.to_string())?;
        let status = process::command("/bin/cp").arg("-R").arg(&source).arg(&prefix).status().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("couldn't copy the app into ~/.local".into());
        }
        // ~/.local/bin is often not on PATH for the desktop: point the menu entry at the binary.
        let bin = prefix.join("bin").join(app.id);
        let desktop = prefix.join("share/applications").join(format!("{}.desktop", app.bundle_id()));
        if let Ok(text) = std::fs::read_to_string(&desktop) {
            let exec = format!("Exec=\"{}\"", bin.display());
            let fixed: String = text
                .lines()
                .map(|l| {
                    if let Some(rest) = l.strip_prefix("Exec=").and_then(|r| r.strip_prefix(app.id)) {
                        format!("{exec}{rest}\n")
                    } else {
                        format!("{l}\n")
                    }
                })
                .collect();
            let _ = std::fs::write(&desktop, fixed);
        }
        Ok((bin, rel.iter().map(|r| prefix.join(r)).collect(), false))
    }

    /// Removes the files a tree install put down.
    pub fn remove_files(files: &[PathBuf], needs_root: bool) -> Result<(), String> {
        if files.is_empty() {
            return Err("the launcher has no record of these files".into());
        }
        if needs_root {
            let mut args: Vec<&OsStr> = vec![OsStr::new("/bin/rm"), OsStr::new("-f"), OsStr::new("--")];
            args.extend(files.iter().map(|f| f.as_os_str()));
            return run_as_root(&args);
        }
        for f in files {
            let _ = std::fs::remove_file(f);
        }
        Ok(())
    }
}

/// Builds the Windows Installer command separately so its package argument can be checked
/// without starting an installer.
#[cfg_attr(not(windows), allow(dead_code))]
fn msi_command(program: &Path, msi: &Path) -> Result<std::process::Command, String> {
    let msi = msi_path(msi)?;
    let mut command = process::command(program);
    command.arg("/i").arg(msi).args(["/passive", "/norestart"]);
    Ok(command)
}

/// Windows Installer's storage APIs can fail on paths through junctions or symlinks (#338).
/// Keep ordinary drive/UNC syntax when possible: Windows canonicalization alone introduces
/// a `\\?\` prefix that older APIs also cannot handle.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn msi_path(msi: &Path) -> Result<PathBuf, String> {
    #[cfg(windows)]
    let resolved = dunce::canonicalize(msi);
    #[cfg(not(windows))]
    let resolved = std::fs::canonicalize(msi);
    resolved.map_err(|e| format!("couldn't resolve the Windows Installer package {} ({e})", msi.display()))
}

#[cfg(windows)]
pub mod windows {
    use artcraft_launcher_core::protocol::CANCELLED;

    use super::*;

    fn msiexec() -> PathBuf {
        std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from).join(r"System32\msiexec.exe")
    }

    fn explain(code: Option<i32>) -> Result<(), String> {
        match code {
            Some(0 | 3010 | 1641) => Ok(()),
            Some(1602) => Err("cancelled".into()),
            Some(1618) => Err("another installation is in progress. Wait for it to finish, then try again.".into()),
            Some(1925 | 1303) => Err("Windows didn't allow the install (administrator permission is needed)".into()),
            Some(c) => Err(format!("the Windows Installer stopped with error {c}")),
            None => Err("the Windows Installer was interrupted".into()),
        }
    }

    /// Runs an MSI with a progress window and no restart. Windows asks for permission itself.
    pub fn run_msi(msi: &Path, destination: Option<&Path>, stop: &Stop, phase: &mut dyn FnMut(JobPhase)) -> Result<(), String> {
        in_turn(stop, phase, JobPhase::Installing, || {
            use std::os::windows::process::CommandExt;
            let mut command = msi_command(&msiexec(), msi)?;
            if let Some(dir) = destination {
                command.raw_arg(msi_folder_argument(dir)?);
            }
            let status = command.status().map_err(|e| format!("couldn't start the Windows Installer ({e})"))?;
            explain(status.code())
        })
    }

    /// Runs `work` once no other Windows Installer job of ours is running (a second one would
    /// stop with error 1618). Cancelled while it waits, it never runs.
    fn in_turn(stop: &Stop, phase: &mut dyn FnMut(JobPhase), working: JobPhase, work: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
        INSTALLER.alone(&cancelled(stop), &mut turn(phase, working), work).unwrap_or_else(|| Err(CANCELLED.into()))
    }

    pub fn uninstall_msi(app: &CraftApp, stop: &Stop, phase: &mut dyn FnMut(JobPhase)) -> Result<(), String> {
        let installs = crate::detect::windows::msi_installs(app);
        if installs.is_empty() {
            return Err(format!("{} isn't registered with the Windows Installer", app.name));
        }
        in_turn(stop, phase, JobPhase::Removing, || {
            for m in installs {
                let status = process::command(msiexec())
                    .args(["/x", &m.product_code, "/passive", "/norestart"])
                    .status()
                    .map_err(|e| format!("couldn't start the Windows Installer ({e})"))?;
                explain(status.code())?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn msi_install_uses_the_package_target_instead_of_a_symlinked_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("persistent cache");
        std::fs::create_dir(&cache).unwrap();
        let file = cache.join("PhotoCraft.msi");
        std::fs::write(&file, b"test package").unwrap();
        let link = dir.path().join("current cache");
        std::os::unix::fs::symlink(&cache, &link).unwrap();
        let command = msi_command(Path::new("msiexec.exe"), &link.join("PhotoCraft.msi")).unwrap();
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(
            args,
            [
                std::ffi::OsStr::new("/i"),
                file.canonicalize().unwrap().as_os_str(),
                std::ffi::OsStr::new("/passive"),
                std::ffi::OsStr::new("/norestart")
            ]
        );
    }

    #[test]
    fn an_unavailable_msi_is_refused_before_starting_the_installer() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.msi");
        let error = msi_command(Path::new("msiexec.exe"), &missing).unwrap_err();
        assert!(error.contains("couldn't resolve the Windows Installer package"), "{error}");
    }

    #[cfg(windows)]
    #[test]
    fn a_short_local_msi_path_keeps_ordinary_windows_syntax() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("package.msi");
        std::fs::write(&file, b"test package").unwrap();
        let resolved = msi_path(&file).unwrap();
        assert!(!resolved.as_os_str().to_string_lossy().starts_with(r"\\?\"), "{resolved:?}");
        assert_eq!(std::fs::read(resolved).unwrap(), b"test package");
    }

    #[test]
    fn selected_folders_must_exist_and_leave_no_probe_files() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(selected_folder(dir.path().to_str().unwrap(), true).unwrap(), dir.path());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        #[cfg(unix)]
        {
            let quoted = dir.path().join("Craft \"Apps\"");
            std::fs::create_dir(&quoted).unwrap();
            assert_eq!(selected_folder(quoted.to_str().unwrap(), true).unwrap(), quoted);
        }
        let missing = dir.path().join("unavailable/Apps");
        assert!(selected_folder(missing.to_str().unwrap(), true).is_err());
        assert!(!missing.exists());
        assert!(selected_folder("relative/folder", true).is_err());
        assert!(selected_folder("bad\nfolder", true).is_err());
        let app = CraftApp::by_id("photocraft").unwrap();
        let locations = InstallLocations { root: Some(dir.path().display().to_string()), ..Default::default() };
        assert_eq!(destination(app, PackageKind::Msi, None, None, &locations).unwrap(), Some(dir.path().join("PhotoCraft")));
        assert_eq!(destination(app, PackageKind::PortableZip, None, None, &locations).unwrap(), Some(dir.path().to_path_buf()));
        let old = record("0.5.0", PackageKind::PortableZip, dir.path().join("PhotoCraft/photocraft.exe"), vec![], false);
        let unavailable = InstallLocations { root: Some(dir.path().join("missing").display().to_string()), ..Default::default() };
        assert_eq!(destination(app, PackageKind::PortableZip, None, Some(&old), &unavailable).unwrap(), None);
        let msi = Installed {
            version: None,
            source: InstallSource::Msi,
            location: dir.path().join("MSI/PhotoCraft/photocraft.exe").display().to_string(),
            package: None,
        };
        assert_eq!(destination(app, PackageKind::Msi, Some(&msi), Some(&old), &unavailable).unwrap(), Some(dir.path().join("MSI/PhotoCraft")));
        assert!(destination(app, PackageKind::TarGz, None, None, &locations).unwrap_err().contains("aren't supported"));
    }

    #[test]
    fn updates_stay_beside_the_launchers_copy_and_new_installs_use_the_chosen_folder() {
        let dir = tempfile::tempdir().unwrap();
        let app = CraftApp::by_id("photocraft").unwrap();
        let path = dir.path().join("old.AppImage");
        let rec = record("0.5.0", PackageKind::AppImage, path.clone(), vec![], false);
        let launcher = Installed { version: None, source: InstallSource::Launcher, location: path.display().to_string(), package: None };
        let users = Installed { version: None, source: InstallSource::AppImage, location: "/home/me/AppImages/p.AppImage".into(), package: None };
        let chosen = dir.path().join("New apps");
        std::fs::create_dir(&chosen).unwrap();
        let mut locations = InstallLocations { root: Some(chosen.display().to_string()), ..Default::default() };
        assert_eq!(destination(app, PackageKind::AppImage, Some(&launcher), Some(&rec), &locations).unwrap(), Some(dir.path().into()));
        assert_eq!(destination(app, PackageKind::AppImage, Some(&users), None, &locations).unwrap(), None);
        if !process::in_sandbox() {
            assert_eq!(destination(app, PackageKind::AppImage, None, None, &locations).unwrap(), Some(chosen.clone()));
            locations.per_app.insert(app.id.into(), dir.path().display().to_string());
            assert_eq!(destination(app, PackageKind::Dmg, None, None, &locations).unwrap(), Some(dir.path().into()));
        }
    }

    #[test]
    fn msi_folder_assignment_preserves_spaces_unicode_and_root_separators() {
        for (value, argument) in [
            (r"D:\", r#"INSTALLFOLDER="D:\""#),
            (r"D:\Craft Apps\PhotoCraft", r#"INSTALLFOLDER="D:\Craft Apps\PhotoCraft""#),
            (r"\\server\apps\绘画", r#"INSTALLFOLDER="\\server\apps\绘画""#),
            (r"D:\Craft Apps\", r#"INSTALLFOLDER="D:\Craft Apps\""#),
        ] {
            assert_eq!(msi_folder_argument(Path::new(value)).unwrap(), argument);
        }
        assert!(msi_folder_argument(Path::new("D:\\bad\" TRANSFORMS=evil")).is_err());
        assert!(msi_folder_argument(Path::new("D:\\bad\nfolder")).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn appimage_updates_replace_only_the_recorded_file_in_the_same_folder() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Craft Apps");
        std::fs::create_dir(&folder).unwrap();
        let download = dir.path().join("download");
        std::fs::write(&download, "first").unwrap();
        let unrelated = folder.join("photocraft-unrelated.AppImage");
        std::fs::write(&unrelated, "user copy").unwrap();
        let (old, removed) = linux::install_appimage(&download, "photocraft-0.5.0.AppImage", Some(&folder), None, false).unwrap();
        assert!(removed.is_empty());
        assert_eq!(std::fs::read(&old).unwrap(), b"first");
        let rec = record("0.5.0", PackageKind::AppImage, old.clone(), vec![], false);
        std::fs::write(&download, "updated").unwrap();
        let (new, removed) = linux::install_appimage(&download, "photocraft-0.6.0.AppImage", Some(&folder), Some(&rec), false).unwrap();
        assert_eq!(new, folder.join("photocraft-0.6.0.AppImage"));
        assert_eq!(std::fs::read(&new).unwrap(), b"updated");
        assert_eq!(removed.as_slice(), std::slice::from_ref(&old));
        assert!(!old.exists());
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"user copy");
        assert!(linux::install_appimage(&download, "photocraft-unrelated.AppImage", Some(&folder), Some(&rec), false).is_err());
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"user copy");
        let new_rec = record("0.6.0", PackageKind::AppImage, new.clone(), vec![], false);
        assert!(linux::install_appimage(&download, "photocraft-0.6.0.AppImage", Some(&folder), Some(&new_rec), false).is_ok());
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 2);
        let link = folder.join("linked.AppImage");
        std::os::unix::fs::symlink(&unrelated, &link).unwrap();
        let link_rec = record("0.6.0", PackageKind::AppImage, link, vec![], false);
        assert!(linux::install_appimage(&download, "linked.AppImage", Some(&folder), Some(&link_rec), false).is_err());
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"user copy");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_appimage_writes_over_a_users_file_only_when_asked_to() {
        let dir = tempfile::tempdir().unwrap();
        let download = dir.path().join("download");
        std::fs::write(&download, "new").unwrap();
        let folder = dir.path().join("Apps");
        std::fs::create_dir(&folder).unwrap();
        let theirs = folder.join("photocraft.AppImage");
        let personal = dir.path().join("personal.txt");
        std::fs::write(&theirs, "user copy").unwrap();
        std::fs::write(&personal, "user file").unwrap();
        std::os::unix::fs::symlink(&personal, folder.join(".photocraft.AppImage.partial")).unwrap();
        assert!(linux::install_appimage(&download, "photocraft.AppImage", Some(&folder), None, false).is_err());
        assert_eq!(std::fs::read(&theirs).unwrap(), b"user copy");
        linux::install_appimage(&download, "photocraft.AppImage", Some(&folder), None, true).unwrap();
        assert_eq!(std::fs::read(&theirs).unwrap(), b"new");
        assert_eq!(std::fs::read(&personal).unwrap(), b"user file");
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 2, "staging was left behind");
    }

    #[test]
    fn a_new_install_reports_what_it_would_write_over() {
        let dir = tempfile::tempdir().unwrap();
        let app = CraftApp::by_id("photocraft").unwrap();
        let paths = Paths::under(dir.path());
        let locations = InstallLocations { root: Some(dir.path().display().to_string()), ..Default::default() };
        let asset = Asset { name: "photocraft.zip".into(), ..Default::default() };
        let occupied = |previous| occupied(app, &asset, PackageKind::PortableZip, None, previous, &locations, &paths).unwrap();
        assert_eq!(occupied(None), None);
        std::fs::create_dir(dir.path().join("photocraft")).unwrap();
        assert_eq!(occupied(None), Some(dir.path().join("photocraft")));
        let old = record("0.5.0", PackageKind::PortableZip, dir.path().join("photocraft/photocraft.exe"), vec![], false);
        assert_eq!(occupied(Some(&old)), None);
    }

    #[test]
    fn swap_keeps_the_old_copy_until_the_new_one_is_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("app");
        let staged = dir.path().join("app.new");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("v"), "old").unwrap();
        std::fs::create_dir(&staged).unwrap();
        std::fs::write(staged.join("v"), "new").unwrap();
        swap_into_place(&staged, &dest).unwrap();
        assert_eq!(std::fs::read_to_string(dest.join("v")).unwrap(), "new");
        assert!(!staged.exists() && !dir.path().join("app.old").exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        // A missing staged copy leaves the installed one alone.
        assert!(swap_into_place(&staged, &dest).is_err());
        assert_eq!(std::fs::read_to_string(dest.join("v")).unwrap(), "new");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn swap_preserves_an_unrelated_backup_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("app");
        let staged = dir.path().join("app.new");
        let unrelated = dest.with_extension("old");
        std::fs::write(&dest, b"installed app").unwrap();
        std::fs::write(&staged, b"updated app").unwrap();
        std::fs::write(&unrelated, b"user backup").unwrap();
        swap_into_place(&staged, &dest).unwrap();
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"user backup");
        assert_eq!(std::fs::read(&dest).unwrap(), b"updated app");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn swap_preserves_an_unrelated_backup_directory() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("app");
        let staged = dir.path().join("app.new");
        let unrelated = dest.with_extension("old");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("v"), b"installed app").unwrap();
        std::fs::create_dir(&staged).unwrap();
        std::fs::write(staged.join("v"), b"updated app").unwrap();
        std::fs::create_dir(&unrelated).unwrap();
        std::fs::write(unrelated.join("keep.txt"), b"user backup").unwrap();
        swap_into_place(&staged, &dest).unwrap();
        assert_eq!(std::fs::read(unrelated.join("keep.txt")).unwrap(), b"user backup");
        assert_eq!(std::fs::read(dest.join("v")).unwrap(), b"updated app");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn swap_preserves_a_dangling_backup_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("app");
        let staged = dir.path().join("app.new");
        let unrelated = dest.with_extension("old");
        let target = dir.path().join("missing");
        std::fs::write(&dest, b"installed app").unwrap();
        std::fs::write(&staged, b"updated app").unwrap();
        std::os::unix::fs::symlink(&target, &unrelated).unwrap();
        swap_into_place(&staged, &dest).unwrap();
        assert_eq!(std::fs::read_link(&unrelated).unwrap(), target);
        assert_eq!(std::fs::read(&dest).unwrap(), b"updated app");
    }

    #[test]
    fn a_failed_rollback_keeps_the_old_app_and_reports_its_backup_path() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("app");
        let staged = dir.path().join("app.new");
        std::fs::write(&dest, b"installed app").unwrap();
        std::fs::write(&staged, b"updated app").unwrap();
        let calls = std::cell::Cell::new(0);
        let err = swap_into_place_via(&staged, &dest, |from, to| {
            calls.set(calls.get() + 1);
            match calls.get() {
                1 => std::fs::rename(from, to),
                2 => Err(std::io::Error::other("staged move failed")),
                _ => Err(std::io::Error::other("restore failed")),
            }
        })
        .unwrap_err();
        assert_eq!(calls.get(), 3);
        assert!(err.contains("staged move failed") && err.contains("restore failed"));
        let backup = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().path()).find(|p| p.is_dir()).unwrap();
        let saved = backup.join("old");
        assert_eq!(std::fs::read(&saved).unwrap(), b"installed app");
        assert!(err.contains(&saved.display().to_string()));
        assert_eq!(std::fs::read(&staged).unwrap(), b"updated app");
    }

    #[test]
    fn a_failed_install_keeps_the_download_for_the_next_try() {
        use crate::http::tests::{Reply, body, serve};
        let app = CraftApp::by_id("wordcraft").unwrap();
        let (body, sha) = body();
        let (url, requests) = serve(body.clone(), vec![Reply::Full, Reply::Full]);
        let asset = Asset { name: "wordcraft-0.3.0-test.pkg".into(), size: body.len() as u64, url, sha256: Some(sha) };
        let release = Release { assets: vec![asset.clone()], ..Release::default() };
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let http = Http::new().unwrap();
        // A format no build of the launcher installs here stands in for any failing install step.
        let kind = if cfg!(windows) { PackageKind::Dmg } else { PackageKind::Msi };
        let mut phases = Vec::new();
        for _ in 0..2 {
            let mut phase = |p: JobPhase| phases.push(p);
            let mut job = Job {
                http: &http,
                paths: &paths,
                stop: &Stop::default(),
                phase: &mut phase,
                install_locations: &InstallLocations::default(),
                replace: false,
            };
            let err = install(app, &release, &asset, kind, None, None, &mut job).unwrap_err();
            assert!(err.contains("isn't supported"), "{err}");
            assert_eq!(std::fs::read(download_path(&paths, &asset)).unwrap(), body);
        }
        assert_eq!(requests.lock().unwrap().len(), 1, "the second try reuses the kept file");
        assert_eq!(phases.iter().filter(|p| **p == JobPhase::Installing).count(), 2);
    }

    #[test]
    fn an_unavailable_custom_folder_is_rejected_before_downloading() {
        use crate::http::tests::{Reply, body, serve};
        let app = CraftApp::by_id("wordcraft").unwrap();
        let (body, sha) = body();
        let (url, requests) = serve(body.clone(), vec![Reply::Full]);
        let asset = Asset { name: "wordcraft-0.3.0-linux-x86_64.AppImage".into(), size: body.len() as u64, url, sha256: Some(sha) };
        let release = Release { assets: vec![asset.clone()], ..Release::default() };
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("unavailable/Apps");
        let locations = InstallLocations { root: Some(missing.display().to_string()), ..Default::default() };
        let paths = Paths::under(dir.path());
        let http = Http::new().unwrap();
        let mut phases = vec![];
        let mut phase = |p| phases.push(p);
        let mut job = Job { http: &http, paths: &paths, stop: &Stop::default(), phase: &mut phase, install_locations: &locations, replace: false };
        let error = install(app, &release, &asset, PackageKind::AppImage, None, None, &mut job).unwrap_err();
        assert!(error.contains("folder is unavailable") || (process::in_sandbox() && error.contains("aren't supported")), "{error}");
        assert!(requests.lock().unwrap().is_empty());
        assert!(phases.is_empty());
        assert!(!missing.exists());
    }

    #[test]
    fn a_release_without_a_checksum_is_refused_before_anything_is_fetched() {
        use crate::http::tests::{Reply, body, serve};
        let app = CraftApp::by_id("wordcraft").unwrap();
        let (body, _) = body();
        let (url, requests) = serve(body.clone(), vec![Reply::Full]);
        // Neither a per-file digest nor a SHA256SUMS.txt in the release.
        let asset = Asset { name: "wordcraft-0.3.1-test.pkg".into(), size: body.len() as u64, url, sha256: None };
        let release = Release { assets: vec![asset.clone()], ..Release::default() };
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let http = Http::new().unwrap();
        let kind = if cfg!(windows) { PackageKind::Dmg } else { PackageKind::Msi };
        let mut phases = Vec::new();
        let mut phase = |p: JobPhase| phases.push(p);
        let mut job = Job {
            http: &http,
            paths: &paths,
            stop: &Stop::default(),
            phase: &mut phase,
            install_locations: &InstallLocations::default(),
            replace: false,
        };
        let err = install(app, &release, &asset, kind, None, None, &mut job).unwrap_err();
        assert!(err.contains("publishes no checksum"), "{err}");
        assert!(!download_path(&paths, &asset).exists());
        assert!(requests.lock().unwrap().is_empty(), "nothing was fetched");
        assert!(!phases.contains(&JobPhase::Installing));
    }

    #[cfg(unix)]
    fn tarball(path: &Path, entries: &[&str]) {
        let f = std::fs::File::create(path).unwrap();
        let mut b = tar::Builder::new(flate2::write::GzEncoder::new(f, flate2::Compression::fast()));
        for name in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(2);
            h.set_mode(0o755);
            h.set_cksum();
            b.append_data(&mut h, name, &b"hi"[..]).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn lays_out_both_freebsd_tarball_styles_like_usr_local() {
        let app = CraftApp::by_id("wordcraft").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let tree_style = dir.path().join("tree.tar.gz");
        tarball(
            &tree_style,
            &["wordcraft-0.3.0-freebsd-x86_64/bin/wordcraft", "wordcraft-0.3.0-freebsd-x86_64/share/applications/ai.storyteller.wordcraft.desktop"],
        );
        let flat_style = dir.path().join("flat.tar.gz");
        tarball(
            &flat_style,
            &[
                "wordcraft-0.3.0-freebsd-x86_64/wordcraft",
                "wordcraft-0.3.0-freebsd-x86_64/wordcraft-cli",
                "wordcraft-0.3.0-freebsd-x86_64/README.md",
                "wordcraft-0.3.0-freebsd-x86_64/ai.storyteller.wordcraft.desktop",
                "wordcraft-0.3.0-freebsd-x86_64/ai.storyteller.wordcraft.png",
            ],
        );
        for file in [tree_style, flat_style] {
            let root = tree::unpack(app, &file, &dir.path().join("staging")).unwrap();
            let files = tree::files(&root);
            assert!(files.contains(&PathBuf::from("bin/wordcraft")), "{files:?}");
            assert!(files.contains(&PathBuf::from("share/applications/ai.storyteller.wordcraft.desktop")), "{files:?}");
            assert!(!files.iter().any(|f| f.ends_with("README.md")), "{files:?}");
        }
        let bad = dir.path().join("bad.tar.gz");
        tarball(&bad, &["x/README.md"]);
        assert!(tree::unpack(app, &bad, &dir.path().join("staging")).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_tarball_install_lives_in_the_launchers_folder_and_keeps_the_apps_home() {
        let app = CraftApp::by_id("wordcraft").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let file = dir.path().join("wordcraft-0.3.0-linux-x86_64.tar.gz");
        tarball(
            &file,
            &[
                "wordcraft-0.3.0-linux-x86_64/bin/wordcraft",
                "wordcraft-0.3.0-linux-x86_64/bin/wordcraft-cli",
                "wordcraft-0.3.0-linux-x86_64/share/applications/ai.storyteller.wordcraft.desktop",
            ],
        );

        let bin = linux::install_tree(app, &file, &paths).unwrap();
        let root = paths.apps().join("wordcraft");
        assert_eq!(bin, root.join("bin").join("wordcraft"));
        assert!(bin.is_file(), "the app's binary wasn't unpacked");
        assert!(root.join("share/applications/ai.storyteller.wordcraft.desktop").is_file(), "share/ wasn't unpacked");
        assert!(!paths.cache.join("wordcraft.new").exists(), "the staging folder was left behind");

        // The app's own home sits beside the tree, and an update keeps it.
        let home = paths.app_home(app.id);
        std::fs::create_dir_all(home.join(".config/wordcraft")).unwrap();
        std::fs::write(home.join(".config/wordcraft/settings.toml"), "keep me").unwrap();
        linux::install_tree(app, &file, &paths).unwrap();
        assert_eq!(std::fs::read_to_string(home.join(".config/wordcraft/settings.toml")).unwrap(), "keep me");

        // Removing the install takes the tree and nothing else.
        linux::remove_tree(app, &paths).unwrap();
        assert!(!bin.exists() && !root.join("share").exists());
        assert!(home.join(".config/wordcraft/settings.toml").is_file(), "the app's settings were removed with it");
        assert!(linux::remove_tree(app, &paths).is_err(), "a second removal says there is nothing there");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_app_sees_its_own_home_and_the_xdg_folders_inside_it() {
        let home = PathBuf::from("/data/apps/photocraft/home");
        let env = home_env(&home);
        assert_eq!(env[0], ("HOME", home.clone()));
        for (name, dir) in &env[1..] {
            assert!(dir.starts_with(&home), "{name}: {}", dir.display());
        }
        for wanted in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"] {
            assert!(env.iter().any(|(name, _)| *name == wanted), "{wanted} isn't set");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn several_debs_fit_one_apt_call_and_share_its_result() {
        // The files are appended to this, so it must end where apt expects packages.
        assert_eq!(
            linux::install_command(PackageKind::Deb),
            Some(&["apt-get", "-o", "DPkg::Lock::Timeout=60", "install", "-y", "--allow-downgrades"][..])
        );
        assert_eq!(linux::install_command(PackageKind::Flatpak), None);
        let failed = || Err("the system installer stopped with error 100".to_string());
        assert_eq!(linux::shared_outcome(Ok(()), 3), Ok(()));
        assert_eq!(linux::shared_outcome(failed(), 1), failed());
        assert_eq!(
            linux::shared_outcome(failed(), 3),
            Err("the system installer stopped with error 100 while handling 3 apps together. Try them one at a time.".into())
        );
        // A dismissed password prompt stays a cancellation for every app, not an error.
        assert_eq!(linux::shared_outcome(Err("cancelled".into()), 3), Err("cancelled".into()));
    }

    /// Installs two `.deb` files that reach the installer together (both wait behind a running
    /// install), with a stand-in for `pkexec` that exits with `code`. Returns every argument
    /// list that reached it, and each job's result.
    #[cfg(target_os = "linux")]
    fn two_debs_through_pkexec(code: i32) -> (Vec<Vec<String>>, Vec<Result<(), String>>) {
        use std::sync::{Mutex, mpsc};
        let queue = crate::queue::Queue::new();
        let ran = Mutex::new(Vec::new());
        let run = |args: &[&std::ffi::OsStr]| {
            ran.lock().unwrap().push(args.iter().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>());
            root_outcome(Some(code))
        };
        let root = linux::Root { queue: &queue, run: &run };
        let (started, has_started) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let (waiting, waits) = mpsc::channel();
        let results = std::thread::scope(|scope| {
            let (queue, root) = (&queue, &root);
            scope.spawn(move || {
                queue.alone(&|| false, &mut |_| {}, || {
                    started.send(()).unwrap();
                    released.recv().unwrap();
                });
            });
            has_started.recv().unwrap();
            let jobs = ["/downloads/a.deb", "/downloads/b.deb"].map(|file| {
                let waiting = waiting.clone();
                scope.spawn(move || {
                    let mut phase = |p: JobPhase| {
                        if p == JobPhase::Waiting {
                            waiting.send(()).unwrap();
                        }
                    };
                    linux::install_package_via(root, PackageKind::Deb, Path::new(file), &Stop::default(), &mut phase)
                })
            });
            waits.recv().unwrap();
            waits.recv().unwrap();
            release.send(()).unwrap();
            jobs.map(|j| j.join().unwrap()).to_vec()
        });
        (ran.into_inner().unwrap(), results)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn two_debs_reach_root_as_one_apt_call() {
        let (ran, results) = two_debs_through_pkexec(0);
        assert_eq!(ran.len(), 1, "{ran:?}");
        let (command, files) = ran[0].split_at(6);
        assert_eq!(command, ["apt-get", "-o", "DPkg::Lock::Timeout=60", "install", "-y", "--allow-downgrades"]);
        let mut files = files.to_vec();
        files.sort();
        assert_eq!(files, ["/downloads/a.deb", "/downloads/b.deb"]);
        assert_eq!(results, [Ok(()), Ok(())]);

        // The password prompt was dismissed: one call, and both apps are cancelled, not failed.
        let (ran, results) = two_debs_through_pkexec(126);
        assert_eq!(ran.len(), 1, "{ran:?}");
        assert_eq!(results, vec![Err("cancelled".to_string()); 2]);

        // apt failed: both apps say so, and how to find the one at fault.
        let (ran, results) = two_debs_through_pkexec(100);
        assert_eq!(ran.len(), 1, "{ran:?}");
        let error = "the system installer stopped with error 100 while handling 2 apps together. Try them one at a time.";
        assert_eq!(results, vec![Err(error.to_string()); 2]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn flatpak_bundle_install_failure_reports_clean_error() {
        if !crate::process::is_flatpak_available() {
            return;
        }
        let queue = crate::queue::Queue::new();
        let root = linux::Root { queue: &queue, run: &|_| Ok(()) };
        let result = linux::install_package_via(&root, PackageKind::Flatpak, Path::new("/nonexistent/file.flatpak"), &Stop::default(), &mut |_| ());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.starts_with("Flatpak couldn't install the bundle: "), "unexpected error: {err}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn adds_and_removes_only_its_own_menu_entry_and_icon() {
        let app = CraftApp::by_id("wordcraft").unwrap();
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("share");
        let desktop = data.join("applications/ai.storyteller.wordcraft.desktop");
        let icon = data.join("icons/hicolor/128x128/apps/ai.storyteller.wordcraft.png");
        let old = root.path().join("My Apps/wordcraft-0.2.0-linux-x86_64.AppImage");
        let new = root.path().join("My Apps/wordcraft-0.3.0-linux-x86_64.AppImage");
        std::fs::create_dir_all(new.parent().unwrap()).unwrap();
        std::fs::write(&new, b"").unwrap();

        // Fresh install: entry and icon written and recorded.
        let files = linux::integrate_into(&data, app, &new, &[], None);
        assert_eq!(files, [icon.clone(), desktop.clone()]);
        let text = std::fs::read_to_string(&desktop).unwrap();
        assert!(text.contains("Exec=\"") && text.contains("My Apps/wordcraft-0.3.0-linux-x86_64.AppImage\" %F\n"), "{text}");
        assert!(text.contains("Icon=ai.storyteller.wordcraft\n"));
        assert!(std::fs::read(&icon).unwrap().starts_with(b"\x89PNG"));
        let rec = record("0.3.0", PackageKind::AppImage, new.clone(), files, false);

        // An update rewrites both.
        assert_eq!(linux::integrate_into(&data, app, &new, &[], Some(&rec)).len(), 2);

        // Someone else's entry and icon are left alone (and not recorded).
        std::fs::write(&desktop, "[Desktop Entry]\nExec=/usr/bin/wordcraft\n").unwrap();
        std::fs::write(&icon, b"theirs").unwrap();
        assert!(linux::integrate_into(&data, app, &new, &[], None).is_empty());
        assert_eq!(std::fs::read(&icon).unwrap(), b"theirs");
        // ...unless the entry runs a copy this install just replaced (the app wrote it itself).
        std::fs::write(&desktop, format!("[Desktop Entry]\nExec=\"{}\" %F\n", old.display())).unwrap();
        assert_eq!(linux::integrate_into(&data, app, &new, std::slice::from_ref(&old), None), std::slice::from_ref(&desktop));
        assert!(desktop_entry_is_ours(&desktop));

        // Uninstall: an icon the record doesn't list stays; the entry for the removed copy goes.
        std::fs::remove_file(&new).unwrap();
        linux::unintegrate_from(&data, app, &new, None);
        assert!(!desktop.exists());
        assert_eq!(std::fs::read(&icon).unwrap(), b"theirs");
        // With the record, the launcher's icon goes too.
        std::fs::remove_file(&icon).unwrap();
        std::fs::write(&new, b"").unwrap();
        let files = linux::integrate_into(&data, app, &new, &[], None);
        let rec = record("0.3.0", PackageKind::AppImage, new.clone(), files, false);
        std::fs::remove_file(&new).unwrap();
        linux::unintegrate_from(&data, app, &new, Some(&rec));
        assert!(!desktop.exists() && !icon.exists());
        // Someone else's entry survives an uninstall.
        std::fs::write(&desktop, "[Desktop Entry]\nExec=/usr/bin/wordcraft\n").unwrap();
        linux::unintegrate_from(&data, app, &new, Some(&rec));
        assert!(desktop.exists());
    }

    #[cfg(target_os = "linux")]
    fn desktop_entry_is_ours(path: &Path) -> bool {
        crate::desktop_entry::is_ours(&std::fs::read_to_string(path).unwrap())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn immutable_ostree_offers_no_deb_or_rpm() {
        if !Path::new("/run/ostree-booted").exists() {
            return;
        }
        let kinds = available_kinds();
        assert!(!kinds.contains(&PackageKind::Rpm), "{kinds:?}");
        assert!(!kinds.contains(&PackageKind::Deb), "{kinds:?}");
        assert!(kinds.contains(&PackageKind::AppImage), "{kinds:?}");
    }

    #[test]
    fn updates_keep_the_installed_format() {
        let inst = |source| Installed { version: None, source, location: String::new(), package: None };
        let available = available_kinds();
        if let Some(first) = available.first() {
            assert_eq!(choose_kinds(None, None, &[]).first(), Some(first));
        }
        #[cfg(target_os = "macos")]
        assert_eq!(choose_kinds(Some(&inst(InstallSource::MacApp)), None, &[]), [PackageKind::Dmg]);
        #[cfg(windows)]
        assert_eq!(choose_kinds(Some(&inst(InstallSource::Msi)), None, &[]), [PackageKind::Msi]);
        #[cfg(target_os = "linux")]
        assert_eq!(choose_kinds(Some(&inst(InstallSource::AppImage)), None, &[]), [PackageKind::AppImage]);
        let _ = inst;
    }

    #[test]
    fn install_refuses_to_downgrade_when_target_release_is_older() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let http = Http::new().unwrap();
        let phase = &mut |_| ();
        let locations = InstallLocations::default();
        let app = CraftApp::by_id("photocraft").unwrap();
        let release = Release { version: version::parse("0.4.0"), tag: "v0.4.0".into(), ..Release::default() };
        let asset = Asset { name: "photocraft-0.4.0-linux-x86_64.AppImage".into(), ..Asset::default() };
        let installed = Installed {
            version: version::parse("0.5.0"),
            source: InstallSource::Launcher,
            location: dir.path().join("PhotoCraft.AppImage").display().to_string(),
            package: None,
        };
        let stop = Stop::default();
        let mut job = Job { http: &http, paths: &paths, stop: &stop, phase, install_locations: &locations, replace: false };
        let result = install_at(app, &release, &asset, PackageKind::AppImage, Some(&installed), None, &mut job, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("installed version 0.5.0 is newer"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn package_kinds_order_matches_launcher_format() {
        use super::linux::order_available_kinds;

        // Flatpak launcher defaults to Flatpak if available
        let kinds = order_available_kinds(true, false, true, true, false, false);
        assert_eq!(kinds.first(), Some(&PackageKind::Flatpak));
        assert!(kinds.contains(&PackageKind::Deb));
        assert!(kinds.contains(&PackageKind::AppImage));

        // AppImage launcher defaults to AppImage
        let kinds = order_available_kinds(false, true, true, true, false, false);
        assert_eq!(kinds.first(), Some(&PackageKind::AppImage));
        assert!(kinds.contains(&PackageKind::Deb));
        assert!(kinds.contains(&PackageKind::Flatpak));

        // Base Linux launcher defaults to native package format (deb, rpm, or arch) if available
        let kinds = order_available_kinds(false, false, true, true, false, false);
        assert_eq!(kinds.first(), Some(&PackageKind::Deb));

        let kinds = order_available_kinds(false, false, true, false, true, false);
        assert_eq!(kinds.first(), Some(&PackageKind::Rpm));

        let kinds = order_available_kinds(false, false, true, false, false, true);
        assert_eq!(kinds.first(), Some(&PackageKind::ArchPkg));

        // Base Linux launcher without root/distro packaging falls back to Flatpak then AppImage
        let kinds = order_available_kinds(false, false, true, false, false, false);
        assert_eq!(kinds.first(), Some(&PackageKind::Flatpak));
        assert_eq!(kinds.get(1), Some(&PackageKind::AppImage));

        // The tarball runs inside the sandbox, so it is only offered there: with a usable host
        // Flatpak it stays an alternative, without one it is the default (an AppImage cannot
        // mount in a sandbox, and a distro package needs root).
        let kinds = order_available_kinds(true, false, true, false, false, false);
        assert_eq!(kinds.first(), Some(&PackageKind::Flatpak));
        assert_eq!(kinds.get(1), Some(&PackageKind::TarGz));
        let kinds = order_available_kinds(true, false, false, false, false, false);
        assert_eq!(kinds, [PackageKind::TarGz, PackageKind::AppImage]);
        for (appimage, flatpak) in [(true, true), (false, false), (false, true)] {
            assert!(!order_available_kinds(false, appimage, flatpak, true, true, false).contains(&PackageKind::TarGz));
        }
    }

    #[cfg(target_os = "macos")]
    mod macos_checks {
        use artcraft_launcher_core::{SUITE, version};

        use super::macos::{Expect, TEAM_ID, install_dmg, signed_by};
        use super::*;

        fn photocraft() -> &'static CraftApp {
            SUITE.iter().find(|a| a.id == "photocraft").unwrap()
        }

        /// A minimal `PhotoCraft.app` at `dir`, reporting `version`.
        fn bundle(dir: &Path, version: &str) -> PathBuf {
            let app = dir.join("PhotoCraft.app");
            let contents = app.join("Contents");
            std::fs::create_dir_all(contents.join("MacOS")).unwrap();
            std::fs::write(
                contents.join("Info.plist"),
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict>\
                     <key>CFBundleIdentifier</key><string>ai.storyteller.photocraft</string>\
                     <key>CFBundleExecutable</key><string>PhotoCraft</string>\
                     <key>CFBundleShortVersionString</key><string>{version}</string></dict></plist>"
                ),
            )
            .unwrap();
            std::fs::write(contents.join("MacOS/PhotoCraft"), version).unwrap();
            app
        }

        /// A release-style disk image holding a `PhotoCraft.app` that reports `version`.
        fn dmg(dir: &Path, version: &str) -> PathBuf {
            let src = dir.join(format!("src-{version}"));
            bundle(&src, version);
            let dmg = dir.join(format!("photocraft-{version}-macos-universal.dmg"));
            let _one_at_a_time = super::macos::DISK_IMAGES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let ok = std::process::Command::new("hdiutil")
                .args(["create", "-quiet", "-format", "UDZO", "-volname", "PhotoCraft", "-srcfolder"])
                .arg(&src)
                .arg(&dmg)
                .status()
                .unwrap()
                .success();
            assert!(ok, "hdiutil create failed");
            dmg
        }

        #[test]
        fn custom_bundles_are_detected_and_updates_stay_in_their_actual_location() {
            let dir = tempfile::tempdir().unwrap();
            let apps = dir.path().join("Craft Apps");
            std::fs::create_dir(&apps).unwrap();
            let paths = Paths::under(dir.path());
            let image = dmg(dir.path(), "0.6.0");
            let expect = Expect { team: None, version: None };
            let placed = install_dmg(photocraft(), &image, None, Some(&apps), false, &paths, &expect).unwrap();
            assert_eq!(placed, apps.join("PhotoCraft.app"));
            let mut manifest = crate::manifest::Manifest::default();
            manifest.apps.insert("photocraft".into(), record("0.6.0", PackageKind::Dmg, placed.clone(), vec![], false));
            let existing = crate::detect::detect(photocraft(), &manifest, &crate::detect::Snapshot::default()).unwrap();
            assert_eq!(existing.source, InstallSource::MacApp);
            assert_eq!(existing.location, placed.display().to_string());
            assert_eq!(existing.version.unwrap().to_string(), "0.6.0");
            let existing = crate::detect::detect(photocraft(), &manifest, &crate::detect::Snapshot::default()).unwrap();
            let new_default = dir.path().join("Other Apps");
            std::fs::create_dir(&new_default).unwrap();
            let update = dmg(dir.path(), "0.7.0");
            assert_eq!(install_dmg(photocraft(), &update, Some(&existing), Some(&new_default), false, &paths, &expect).unwrap(), placed);
            assert!(!new_default.join("PhotoCraft.app").exists());
            let detected = crate::detect::detect(photocraft(), &manifest, &crate::detect::Snapshot::default()).unwrap();
            assert_eq!(detected.version.unwrap().to_string(), "0.7.0");
            std::fs::write(placed.join("Contents/Info.plist"), "invalid").unwrap();
            assert_ne!(
                crate::detect::detect(photocraft(), &manifest, &crate::detect::Snapshot::default()).map(|i| i.source),
                Some(InstallSource::Launcher)
            );
        }

        #[test]
        fn an_unrelated_bundle_at_the_selected_destination_is_preserved() {
            let dir = tempfile::tempdir().unwrap();
            let placed = bundle(dir.path(), "0.5.0");
            let plist = placed.join("Contents/Info.plist");
            let unrelated = std::fs::read_to_string(&plist).unwrap().replace("ai.storyteller.photocraft", "com.example.other");
            std::fs::write(&plist, &unrelated).unwrap();
            let image = dmg(dir.path(), "0.6.0");
            let result =
                install_dmg(photocraft(), &image, None, Some(dir.path()), false, &Paths::under(dir.path()), &Expect { team: None, version: None });
            assert!(result.unwrap_err().contains("already exists"));
            assert_eq!(std::fs::read_to_string(plist).unwrap(), unrelated);
            assert_eq!(std::fs::read(placed.join("Contents/MacOS/PhotoCraft")).unwrap(), b"0.5.0");
        }

        #[test]
        fn only_the_teams_signature_passes() {
            let dir = tempfile::tempdir().unwrap();
            let app = bundle(dir.path(), "0.6.0");
            assert!(!signed_by(&app, TEAM_ID), "an unsigned bundle passed");

            // A valid signature, but not from the team.
            let ok = std::process::Command::new("codesign").args(["--force", "--sign", "-"]).arg(&app).status().unwrap().success();
            assert!(ok, "codesign failed");
            assert!(!signed_by(&app, TEAM_ID), "an ad-hoc signature passed");

            // A team id can't smuggle anything into the requirement.
            assert!(!signed_by(&app, "X\" or anchor apple generic or \"Y"));
            assert!(!signed_by(&app, ""));
        }

        #[test]
        fn the_bundle_must_be_the_releases_version() {
            let dir = tempfile::tempdir().unwrap();
            let apps = dir.path().join("Applications");
            let current = bundle(&apps, "0.5.0");
            let existing = Installed { version: None, source: InstallSource::MacApp, location: current.display().to_string(), package: None };
            let paths = Paths::under(dir.path());
            let image = dmg(dir.path(), "0.6.0");

            for (release, ok) in [("0.6.0", true), ("0.7.0", false), ("0.6.0-rc.1", true), ("0.5.0", false)] {
                std::fs::write(current.join("Contents/MacOS/PhotoCraft"), "0.5.0").unwrap();
                let want = version::parse(release).unwrap();
                let expect = Expect { team: None, version: Some(&want) };
                let result = install_dmg(photocraft(), &image, Some(&existing), None, false, &paths, &expect);
                assert_eq!(result.is_ok(), ok, "release {release}: {result:?}");
                let now = std::fs::read_to_string(current.join("Contents/MacOS/PhotoCraft")).unwrap();
                assert_eq!(now, if ok { "0.6.0" } else { "0.5.0" }, "release {release}");
                if !ok {
                    assert_eq!(result, Err(format!("the app inside isn't version {release}, so it wasn't installed")));
                }
                assert_eq!(
                    std::fs::read_dir(&apps).unwrap().map(|e| e.unwrap().file_name()).collect::<Vec<_>>(),
                    [std::ffi::OsString::from("PhotoCraft.app")]
                );
            }

            // A bundle that names a pre-release must name the release's.
            let rc = dmg(dir.path(), "0.8.0-rc.2");
            let want = version::parse("0.8.0-rc.1").unwrap();
            let expect = Expect { team: None, version: Some(&want) };
            assert!(install_dmg(photocraft(), &rc, Some(&existing), None, false, &paths, &expect).is_err());
        }

        #[test]
        fn an_unsigned_update_leaves_the_installed_copy_alone() {
            let dir = tempfile::tempdir().unwrap();
            let apps = dir.path().join("Applications");
            let current = bundle(&apps, "0.5.0");
            let existing = Installed { version: None, source: InstallSource::MacApp, location: current.display().to_string(), package: None };
            let image = dmg(dir.path(), "0.6.0");
            let release = Release { tag: "v0.6.0".into(), version: version::parse("0.6.0"), ..Release::default() };

            let result = install_dmg(photocraft(), &image, Some(&existing), None, false, &Paths::under(dir.path()), &Expect::release(&release));
            assert_eq!(result, Err("the app isn't signed by its developer, so it wasn't installed".into()));
            assert_eq!(std::fs::read_to_string(current.join("Contents/MacOS/PhotoCraft")).unwrap(), "0.5.0");
            assert_eq!(
                std::fs::read_dir(&apps).unwrap().map(|e| e.unwrap().file_name()).collect::<Vec<_>>(),
                [std::ffi::OsString::from("PhotoCraft.app")]
            );
        }

        /// A real release passes. Run with `CRAFT_APP=/path/to/PhotoCraft.app cargo test -p
        /// artcraft-launcher-native -- --ignored a_real_craft_app`.
        #[test]
        #[ignore = "needs a Craft App from a real release"]
        fn a_real_craft_app_carries_the_teams_signature() {
            let app = std::env::var("CRAFT_APP").expect("set CRAFT_APP to a Craft App bundle");
            assert!(signed_by(Path::new(&app), TEAM_ID), "{app} isn't signed by {TEAM_ID}");
        }
    }
}
