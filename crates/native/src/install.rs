//! Installing, updating, removing and starting Craft Apps.
//!
//! Apps go where their platform keeps applications, installed the platform's own way:
//!
//! - macOS: the signed `.app` from the release DMG into `/Applications`.
//! - Windows: the release MSI (Windows Installer) into `C:\Program Files\<App>`.
//! - Linux: the distro's package (`.deb` through apt, `.rpm` through dnf/zypper/rpm) with the
//!   system's polkit password prompt; otherwise a Flatpak, otherwise an AppImage in
//!   `~/Applications` (the AppImage convention).
//! - FreeBSD: the release's `/usr/local` tree, with a polkit prompt (or `~/.local` without one).
//!
//! Every download is checked against its published SHA-256 before anything is unpacked, a
//! running app is never replaced, and a new copy is fully in place before the old one goes.

use std::path::{Path, PathBuf};

use artcraft_launcher_core::protocol::JobPhase;
use artcraft_launcher_core::release::{self, Asset, Release};
use artcraft_launcher_core::{CraftApp, InstallSource, Installed, PackageKind};

use crate::http::{Http, Stop, Transfer, Verified};
use crate::manifest::Record;
use crate::paths::Paths;
use crate::process;

/// What an install needs from the backend.
pub struct Job<'a> {
    pub http: &'a Http,
    pub paths: &'a Paths,
    pub stop: &'a Stop,
    pub phase: &'a mut dyn FnMut(JobPhase),
}

/// Downloads and installs `asset` of `release`. Returns a record when the launcher owns the
/// result (portable zip, AppImage, tarball); MSI and macOS installs are found by detection.
pub fn install(
    app: &CraftApp,
    release: &Release,
    asset: &Asset,
    kind: PackageKind,
    existing: Option<&Installed>,
    job: &mut Job<'_>,
) -> Result<(Option<Record>, Verified), String> {
    if let Some(inst) = existing
        && inst.source.launcher_can_update()
        && process::is_running(Path::new(&inst.location))
    {
        return Err(format!("{} is running. Quit it, then try again.", app.name));
    }
    let (file, verified) = download(release, asset, job)?;
    // Past this point the install can't be interrupted halfway.
    (job.phase)(JobPhase::Installing);
    #[allow(unused_variables)]
    let version = release.version_label();
    let result = match kind {
        #[cfg(target_os = "macos")]
        PackageKind::Dmg => macos::install_dmg(app, &file, existing, job.paths).map(|_| None),
        #[cfg(windows)]
        PackageKind::Msi => windows::run_msi(&file).map(|_| None),
        #[cfg(target_os = "linux")]
        PackageKind::Deb | PackageKind::Rpm | PackageKind::Flatpak => linux::install_package(kind, &file).map(|_| None),
        #[cfg(target_os = "linux")]
        PackageKind::AppImage => {
            linux::install_appimage(app, &file, &asset.name, existing).map(|path| Some(record(&version, kind, path, Vec::new(), false)))
        }
        #[cfg(target_os = "freebsd")]
        PackageKind::TarGz => freebsd::install_tree(app, &file, job.paths).map(|(bin, files, root)| Some(record(&version, kind, bin, files, root))),
        other => Err(format!("{} isn't supported on this system", other.label())),
    };
    // The download has served its purpose either way; a failed install starts fresh next time.
    let _ = std::fs::remove_file(&file);
    result.map(|r| (r, verified))
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
    if let Some(inst) = existing {
        let kind = match inst.source {
            InstallSource::Launcher => record.map(|r| r.kind),
            InstallSource::MacApp => Some(PackageKind::Dmg),
            InstallSource::Msi => Some(PackageKind::Msi),
            InstallSource::Deb => Some(PackageKind::Deb),
            InstallSource::Rpm => Some(PackageKind::Rpm),
            // Arch/AUR installs are owned by pacman and updated by the user's AUR helper (see
            // `managed_elsewhere`); the launcher opens and removes them but never replaces them.
            InstallSource::Pacman => None,
            InstallSource::Flatpak => Some(PackageKind::Flatpak),
            InstallSource::AppImage => Some(PackageKind::AppImage),
            InstallSource::Binary => Some(PackageKind::TarGz),
        };
        if let Some(k) = kind.filter(|k| available.contains(k)) {
            return vec![k];
        }
    }
    let mut kinds: Vec<PackageKind> = preferred.iter().copied().filter(|k| available.contains(k)).collect();
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
    return vec![PackageKind::Msi];
    #[cfg(target_os = "linux")]
    return linux::available_kinds();
    #[cfg(target_os = "freebsd")]
    return vec![PackageKind::TarGz];
    #[allow(unreachable_code)]
    Vec::new()
}

/// Fetches `asset` into the download folder, checked against its published SHA-256: GitHub's
/// per-file digest, else the release's `SHA256SUMS.txt`. A release with neither still installs,
/// checked by size only, and says so.
pub(crate) fn download(release: &Release, asset: &Asset, job: &mut Job<'_>) -> Result<(PathBuf, Verified), String> {
    if !release::valid_file_name(&asset.name) {
        return Err("the release has an unexpected file name".into());
    }
    let sha = match &asset.sha256 {
        Some(s) => Some(s.clone()),
        None => published_checksum(release, asset, job.http)?,
    };
    if sha.is_none() {
        log::warn!("{}: no published checksum; checking the size only", asset.name);
    }
    let dest = download_path(job.paths, asset);
    let phase = &mut *job.phase;
    let verified = job.http.download(&asset.url, &dest, asset.size, sha.as_deref(), job.stop, &mut |t| match t {
        Transfer::Bytes { done, total } => phase(JobPhase::Downloading { done, total }),
        Transfer::Reconnecting { done, total } => phase(JobPhase::Reconnecting { done, total }),
    })?;
    (job.phase)(JobPhase::Verifying);
    Ok((dest, verified))
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
            let helper = ["paru", "yay"].into_iter().find(|h| which(h).is_some());
            let cmd = match helper {
                Some(h) => format!("{h} -S {pkg}"),
                None => format!("your AUR helper (for example: yay -S {pkg})"),
            };
            Some(format!("{} was installed from the AUR ({pkg}). Update it with {cmd}", app.name))
        }
        InstallSource::Binary => Some(format!("{} was installed outside the launcher. Update or remove it the way it was installed.", app.name)),
        _ => None,
    }
}

/// Removes an installed copy.
pub fn uninstall(app: &CraftApp, installed: &Installed, record: Option<&Record>) -> Result<(), String> {
    let _ = record;
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
        InstallSource::Msi => windows::uninstall_msi(app),
        #[cfg(target_os = "macos")]
        InstallSource::MacApp | InstallSource::Launcher => macos::move_to_trash(&path),
        #[cfg(target_os = "linux")]
        InstallSource::Deb | InstallSource::Rpm | InstallSource::Pacman | InstallSource::Flatpak => linux::remove_package(app, installed),
        #[cfg(target_os = "freebsd")]
        InstallSource::Launcher => match record {
            Some(r) => freebsd::remove_files(&r.files, r.needs_root),
            None => Err("the launcher has no record of these files".into()),
        },
        #[cfg(target_os = "linux")]
        InstallSource::AppImage | InstallSource::Launcher => remove_appimage(app, &path),
        other => Err(format!("{} can't be removed from here", other.label())),
    }
}

#[allow(dead_code)]
/// Deletes an AppImage, and the menu entry it created for itself on first run.
fn remove_appimage(app: &CraftApp, path: &Path) -> Result<(), String> {
    if path.extension().is_none_or(|e| e != "AppImage") || !path.is_file() {
        return Err(format!("{} isn't an AppImage, so it was left alone", path.display()));
    }
    std::fs::remove_file(path).map_err(|e| format!("couldn't remove {} ({e})", path.display()))?;
    if let Some(data) = dirs::data_dir() {
        let desktop = data.join("applications").join(format!("{}.desktop", app.bundle_id()));
        if std::fs::read_to_string(&desktop).is_ok_and(|t| t.contains(&path.display().to_string())) {
            let _ = std::fs::remove_file(desktop);
        }
    }
    Ok(())
}

/// Starts an installed app.
pub fn launch(app: &CraftApp, installed: &Installed) -> Result<(), String> {
    let path = Path::new(&installed.location);
    match installed.source {
        InstallSource::Flatpak => {
            let mut cmd = process::command("flatpak");
            cmd.args(["run", &app.bundle_id()]);
            process::spawn_detached(cmd)
        }
        InstallSource::MacApp => process::open(path.as_os_str()),
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

/// Moves `staged` into place at `dest`, keeping the old copy until the new one is there.
#[allow(dead_code)]
fn swap_into_place(staged: &Path, dest: &Path) -> Result<(), String> {
    let old = dest.with_extension("old");
    if old.exists() {
        let _ = if old.is_dir() { std::fs::remove_dir_all(&old) } else { std::fs::remove_file(&old) };
    }
    let had_old = dest.exists();
    if had_old {
        std::fs::rename(dest, &old).map_err(|e| format!("couldn't move the old copy aside ({e})"))?;
    }
    if let Err(e) = std::fs::rename(staged, dest) {
        if had_old {
            let _ = std::fs::rename(&old, dest);
        }
        return Err(format!("couldn't put the new copy in place ({e})"));
    }
    if had_old {
        let _ = if old.is_dir() { std::fs::remove_dir_all(&old) } else { std::fs::remove_file(&old) };
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
    match status.code() {
        Some(0) => Ok(()),
        // pkexec: 126 = the password prompt was dismissed, 127 = not authorized.
        Some(126 | 127) => Err("cancelled".into()),
        Some(c) => Err(format!("the system installer stopped with error {c}")),
        None => Err("the system installer was interrupted".into()),
    }
}

#[cfg(target_os = "linux")]
pub(crate) mod linux {
    use std::ffi::OsStr;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// What this system can install: its own package format first (when polkit can ask for
    /// permission), then Flatpak, then AppImage, which works everywhere.
    pub fn available_kinds() -> Vec<PackageKind> {
        let mut kinds = Vec::new();
        let root = which("pkexec").is_some();
        if root && which("apt-get").is_some() && which("dpkg").is_some() {
            kinds.push(PackageKind::Deb);
        }
        if root && which("rpm").is_some() && which("dpkg").is_none() {
            kinds.push(PackageKind::Rpm);
        }
        if which("flatpak").is_some() {
            kinds.push(PackageKind::Flatpak);
        }
        kinds.push(PackageKind::AppImage);
        kinds
    }

    /// Installs (or upgrades) a downloaded package with the system's own tools.
    pub fn install_package(kind: PackageKind, file: &Path) -> Result<(), String> {
        let f = file.as_os_str();
        match kind {
            // apt resolves dependencies and records the package like any other.
            PackageKind::Deb => run_as_root(&[OsStr::new("apt-get"), OsStr::new("install"), OsStr::new("-y"), OsStr::new("--allow-downgrades"), f]),
            PackageKind::Rpm => {
                if which("dnf").is_some() {
                    run_as_root(&[OsStr::new("dnf"), OsStr::new("install"), OsStr::new("-y"), f])
                } else if which("zypper").is_some() {
                    run_as_root(&[
                        OsStr::new("zypper"),
                        OsStr::new("--non-interactive"),
                        OsStr::new("install"),
                        OsStr::new("--allow-unsigned-rpm"),
                        f,
                    ])
                } else {
                    run_as_root(&[OsStr::new("rpm"), OsStr::new("-U"), OsStr::new("--replacepkgs"), f])
                }
            }
            PackageKind::Flatpak => {
                let status = process::command("flatpak")
                    .args(["install", "--user", "-y", "--noninteractive", "--bundle"])
                    .arg(f)
                    .status()
                    .map_err(|e| format!("couldn't start Flatpak ({e})"))?;
                if status.success() {
                    Ok(())
                } else {
                    Err("Flatpak couldn't install the bundle (it needs the Flathub remote for its runtime)".into())
                }
            }
            other => Err(format!("{} isn't a system package", other.label())),
        }
    }

    pub fn remove_package(app: &CraftApp, installed: &Installed) -> Result<(), String> {
        match installed.source {
            InstallSource::Deb => run_as_root(&[OsStr::new("apt-get"), OsStr::new("remove"), OsStr::new("-y"), OsStr::new(app.id)]),
            InstallSource::Rpm => {
                if which("dnf").is_some() {
                    run_as_root(&[OsStr::new("dnf"), OsStr::new("remove"), OsStr::new("-y"), OsStr::new(app.id)])
                } else {
                    run_as_root(&[OsStr::new("rpm"), OsStr::new("-e"), OsStr::new(app.id)])
                }
            }
            // The package detected (photocraft, photocraft-bin or photocraft-git).
            InstallSource::Pacman => {
                let pkg = installed.package.as_deref().unwrap_or(app.id);
                run_as_root(&[OsStr::new("pacman"), OsStr::new("-R"), OsStr::new("--noconfirm"), OsStr::new(pkg)])
            }
            InstallSource::Flatpak => {
                let id = app.bundle_id();
                let user = process::command("flatpak").args(["uninstall", "--user", "-y", "--noninteractive", &id]).status();
                if user.is_ok_and(|s| s.success()) {
                    return Ok(());
                }
                run_as_root(&[
                    OsStr::new("flatpak"),
                    OsStr::new("uninstall"),
                    OsStr::new("--system"),
                    OsStr::new("-y"),
                    OsStr::new("--noninteractive"),
                    OsStr::new(&id),
                ])
            }
            other => Err(format!("{} isn't a system package", other.label())),
        }
    }

    /// Copies an AppImage into `~/Applications` and makes it executable; removes the copy it
    /// replaces.
    pub fn install_appimage(app: &CraftApp, file: &Path, name: &str, existing: Option<&Installed>) -> Result<PathBuf, String> {
        let dir = crate::detect::linux::appimage_dir().ok_or("no home folder")?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't create {} ({e})", dir.display()))?;
        let dest = dir.join(name);
        let partial = dir.join(format!(".{name}.partial"));
        std::fs::copy(file, &partial).map_err(|e| format!("couldn't copy the AppImage ({e})"))?;
        std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        std::fs::rename(&partial, &dest).map_err(|e| format!("couldn't put the AppImage in place ({e})"))?;
        // Older AppImages of this app (the one we replaced, or earlier downloads) go.
        let mut old: Vec<PathBuf> = crate::detect::linux::appimages(app).into_iter().map(|(p, _)| p).collect();
        if let Some(inst) = existing.filter(|i| matches!(i.source, InstallSource::AppImage | InstallSource::Launcher)) {
            old.push(PathBuf::from(&inst.location));
        }
        for p in old.into_iter().filter(|p| *p != dest && p.extension().is_some_and(|e| e == "AppImage")) {
            let _ = std::fs::remove_file(p);
        }
        Ok(dest)
    }
}

#[cfg(target_os = "macos")]
pub mod macos {
    use std::time::Duration;

    use super::*;

    /// `/Applications` when we can write there, else `~/Applications`.
    fn applications_dir(existing: Option<&Installed>) -> Result<PathBuf, String> {
        if let Some(parent) =
            existing.filter(|i| i.source == InstallSource::MacApp).and_then(|i| Path::new(&i.location).parent().map(Path::to_path_buf))
        {
            return Ok(parent);
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

    pub fn install_dmg(app: &CraftApp, dmg: &Path, existing: Option<&Installed>, paths: &Paths) -> Result<PathBuf, String> {
        let mount = paths.cache.join(format!("mount-{}", app.id));
        let _ = detach(&mount);
        std::fs::create_dir_all(&mount).map_err(|e| e.to_string())?;
        let (Some(dmg_s), Some(mount_s)) = (dmg.to_str(), mount.to_str()) else { return Err("unusual characters in the cache folder's path".into()) };
        process::capture("hdiutil", &["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint", mount_s, dmg_s], Duration::from_secs(120))
            .ok_or("couldn't open the disk image")?;
        let result = copy_app(app, &mount, existing);
        let _ = detach(&mount);
        let _ = std::fs::remove_dir(&mount);
        result
    }

    fn copy_app(app: &CraftApp, mount: &Path, existing: Option<&Installed>) -> Result<PathBuf, String> {
        let bundle = format!("{}.app", app.name);
        let src = mount.join(&bundle);
        crate::detect::macos::read_bundle(app, &src).ok_or("the disk image doesn't contain the app")?;
        let dir = applications_dir(existing)?;
        let dest = dir.join(&bundle);
        let staged = dir.join(format!(".{bundle}.partial"));
        let _ = std::fs::remove_dir_all(&staged);
        let (Some(src_s), Some(staged_s)) = (src.to_str(), staged.to_str()) else { return Err("unusual characters in a path".into()) };
        // ditto keeps the code signature and symlinks intact. The disk image's volume adds Finder
        // metadata (com.apple.FinderInfo) to the files; leave it and the extended attributes
        // behind, or `codesign --verify --strict` rejects the bundle as carrying "detritus". A
        // bundle's signature lives in its files, not in extended attributes.
        process::capture("ditto", &["--norsrc", "--noextattr", "--noacl", src_s, staged_s], Duration::from_secs(600))
            .ok_or("couldn't copy the app")?;
        if crate::detect::macos::read_bundle(app, &staged).is_none() {
            let _ = std::fs::remove_dir_all(&staged);
            return Err("the copied app is incomplete".into());
        }
        swap_into_place(&staged, &dest)?;
        Ok(dest)
    }

    fn detach(mount: &Path) -> Option<String> {
        let m = mount.to_str()?;
        if !mount.join(".").exists() {
            return None;
        }
        process::capture("hdiutil", &["detach", m, "-quiet"], Duration::from_secs(60))
            .or_else(|| process::capture("hdiutil", &["detach", m, "-force", "-quiet"], Duration::from_secs(60)))
    }

    /// Moves an app to the user's Trash (so it can be put back).
    pub fn move_to_trash(path: &Path) -> Result<(), String> {
        let trash = dirs::home_dir().ok_or("no home folder")?.join(".Trash");
        let name = path.file_name().ok_or("not an app")?;
        let mut dest = trash.join(name);
        if dest.exists() {
            dest = trash.join(format!("{} {}", name.to_string_lossy(), crate::now()));
        }
        if std::fs::rename(path, &dest).is_ok() {
            return Ok(());
        }
        std::fs::remove_dir_all(path).map_err(|e| format!("couldn't remove {} ({e})", path.display()))
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

#[cfg(windows)]
pub mod windows {
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
    pub fn run_msi(msi: &Path) -> Result<(), String> {
        let status = process::command(msiexec())
            .arg("/i")
            .arg(msi)
            .args(["/passive", "/norestart"])
            .status()
            .map_err(|e| format!("couldn't start the Windows Installer ({e})"))?;
        explain(status.code())
    }

    pub fn uninstall_msi(app: &CraftApp) -> Result<(), String> {
        let installs = crate::detect::windows::msi_installs(app);
        if installs.is_empty() {
            return Err(format!("{} isn't registered with the Windows Installer", app.name));
        }
        for m in installs {
            let status = process::command(msiexec())
                .args(["/x", &m.product_code, "/passive", "/norestart"])
                .status()
                .map_err(|e| format!("couldn't start the Windows Installer ({e})"))?;
            explain(status.code())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // A missing staged copy leaves the installed one alone.
        assert!(swap_into_place(&staged, &dest).is_err());
        assert_eq!(std::fs::read_to_string(dest.join("v")).unwrap(), "new");
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
}
