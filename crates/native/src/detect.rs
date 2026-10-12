//! Finding Craft Apps that are already installed, however they got there.
//!
//! - macOS: `<App>.app` in `/Applications` or `~/Applications`, read from its `Info.plist`.
//! - Windows: the Windows Installer's uninstall entries (`Learning Machines LLC`), and portable
//!   copies the launcher unpacked.
//! - Linux: AppImages in `~/Applications` (where the launcher and AppImageLauncher put them),
//!   `~/AppImages` (Gear Lever) and `~/.local/bin`, Debian/RPM packages, Flatpaks, and tarball
//!   installs.
//! - FreeBSD: tarball installs and `/usr/local/bin/<app>`.

use artcraft_launcher_core::{CraftApp, InstallSource, Installed, version};

use crate::manifest::Manifest;

/// One look at the system, shared by every app's detection (package lists are read once).
#[derive(Default)]
pub struct Snapshot {
    #[allow(dead_code)]
    packages: Vec<crate::packages::Package>,
}

impl Snapshot {
    pub fn take() -> Self {
        #[cfg(target_os = "linux")]
        {
            Self { packages: linux::packages() }
        }
        #[cfg(not(target_os = "linux"))]
        {
            Self::default()
        }
    }
}

/// What's installed of `app`, best source first.
pub fn detect(app: &CraftApp, manifest: &Manifest, snapshot: &Snapshot) -> Option<Installed> {
    let _ = snapshot;
    if let Some(found) = from_manifest(app, manifest) {
        return Some(found);
    }
    #[cfg(target_os = "macos")]
    return macos::detect(app);
    #[cfg(windows)]
    return windows::detect(app);
    #[cfg(target_os = "linux")]
    return linux::detect(app, snapshot);
    #[cfg(target_os = "freebsd")]
    return freebsd::detect(app);
    #[allow(unreachable_code)]
    None
}

fn from_manifest(app: &CraftApp, manifest: &Manifest) -> Option<Installed> {
    let rec = manifest.apps.get(app.id)?;
    if !rec.path.exists() {
        return None;
    }
    // On macOS the bundle itself is the truth (the app may have updated itself).
    #[cfg(target_os = "macos")]
    if let Some(found) = macos::read_bundle(app, &rec.path) {
        return Some(found);
    }
    Some(Installed {
        version: version::parse(&rec.version),
        source: InstallSource::Launcher,
        location: rec.path.display().to_string(),
        package: None,
    })
}

/// Whether a file name is an AppImage of the app with asset stems `stems`, and the version its
/// name gives. Matches only the names a release AppImage ends up with: the release's own
/// (`photocraft-0.6.0-linux-x86_64.AppImage`) and the bare app name Gear Lever renames it to
/// (`photocraft.appimage`, version unknown). The extension's case doesn't matter.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn appimage_version(name: &str, stems: &[&str]) -> Option<Option<String>> {
    let split = name.len().checked_sub(".appimage".len())?;
    let (base, ext) = (name.get(..split)?, name.get(split..)?);
    if !ext.eq_ignore_ascii_case(".appimage") {
        return None;
    }
    if stems.iter().any(|s| base.eq_ignore_ascii_case(s)) {
        return Some(None);
    }
    let parsed = artcraft_launcher_core::platform::parse_asset_name(&format!("{base}.AppImage"), stems)?;
    (parsed.kind == artcraft_launcher_core::PackageKind::AppImage && parsed.token.starts_with("linux-")).then_some(Some(parsed.version))
}

/// Version from `<app> --version` output. Binaries print the machine id (`photocraft 0.7.0`),
/// the display name (`SoundCraft 0.5.0`), or a former id (`printcraft 0.2.1` for PdfCraft).
#[cfg_attr(not(any(target_os = "linux", target_os = "freebsd")), allow(dead_code))]
fn parse_cli_version(line: &str, app: &CraftApp) -> Option<version::Version> {
    let line = line.trim();
    for prefix in app.stems().chain(std::iter::once(app.name)) {
        if let Some(rest) = strip_cli_prefix(line, prefix)
            && let Some(tok) = rest.split_whitespace().next()
            && let Some(v) = version::parse(tok)
        {
            return Some(v);
        }
    }
    version::parse_package(line)
}

fn strip_cli_prefix<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let (head, rest) = line.split_at_checked(prefix.len())?;
    if !head.eq_ignore_ascii_case(prefix) {
        return None;
    }
    match rest.chars().next() {
        None => Some(rest),
        Some(c) if c.is_ascii_whitespace() => Some(rest),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
pub mod macos {
    use std::path::{Path, PathBuf};

    use super::*;

    /// Where a `.app` might be: the system and the user's Applications folders.
    pub fn candidates(app: &CraftApp) -> Vec<PathBuf> {
        let bundle = format!("{}.app", app.name);
        let mut out = vec![Path::new("/Applications").join(&bundle)];
        if let Some(home) = dirs::home_dir() {
            out.push(home.join("Applications").join(&bundle));
        }
        out
    }

    pub fn detect(app: &CraftApp) -> Option<Installed> {
        candidates(app).iter().find_map(|p| read_bundle(app, p))
    }

    /// Reads a bundle's `Info.plist`; `None` unless it's really this app.
    pub fn read_bundle(app: &CraftApp, path: &Path) -> Option<Installed> {
        let plist = plist::Value::from_file(path.join("Contents/Info.plist")).ok()?;
        let dict = plist.as_dictionary()?;
        if dict.get("CFBundleIdentifier")?.as_string()? != app.bundle_id() {
            return None;
        }
        // `<App>Version` carries the full version including a pre-release suffix;
        // CFBundleShortVersionString only has the numbers.
        let full_key = format!("{}Version", app.name);
        let text =
            [full_key.as_str(), "CFBundleShortVersionString", "CFBundleVersion"].iter().find_map(|k| dict.get(k).and_then(plist::Value::as_string));
        Some(Installed { version: text.and_then(version::parse), source: InstallSource::MacApp, location: path.display().to_string(), package: None })
    }
}

#[cfg(windows)]
pub mod windows {
    use std::path::PathBuf;

    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};

    use super::*;

    const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";
    const PUBLISHER: &str = "Learning Machines LLC";

    /// An MSI install found in the registry.
    pub struct MsiInstall {
        pub product_code: String,
        pub version: Option<artcraft_launcher_core::version::Version>,
        pub exe: Option<PathBuf>,
    }

    /// Every Windows Installer entry for `app`, newest first.
    pub fn msi_installs(app: &CraftApp) -> Vec<MsiInstall> {
        let mut out = Vec::new();
        for (hive, flags) in [(HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY), (HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY), (HKEY_CURRENT_USER, 0)] {
            let Ok(root) = RegKey::predef(hive).open_subkey_with_flags(UNINSTALL, KEY_READ | flags) else { continue };
            for code in root.enum_keys().flatten() {
                let Ok(key) = root.open_subkey_with_flags(&code, KEY_READ | flags) else { continue };
                let name: String = key.get_value("DisplayName").unwrap_or_default();
                let publisher: String = key.get_value("Publisher").unwrap_or_default();
                if name != app.name || publisher != PUBLISHER || out.iter().any(|m: &MsiInstall| m.product_code == code) {
                    continue;
                }
                let version = key.get_value::<String, _>("DisplayVersion").ok().and_then(|v| version::parse(&v));
                let location: String = key.get_value("InstallLocation").unwrap_or_default();
                let exe = exe_path(app, (!location.is_empty()).then(|| PathBuf::from(location)));
                out.push(MsiInstall { product_code: code, version, exe });
            }
        }
        out.sort_by(|a, b| match (&b.version, &a.version) {
            (Some(x), Some(y)) => version::compare(x, y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
        out
    }

    /// The app's exe: from `App Paths`, the install folder, or Program Files.
    fn exe_path(app: &CraftApp, location: Option<PathBuf>) -> Option<PathBuf> {
        let exe = format!("{}.exe", app.id);
        let app_paths = format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{exe}");
        for flags in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
            if let Ok(key) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(&app_paths, KEY_READ | flags)
                && let Ok(path) = key.get_value::<String, _>("")
            {
                let p = PathBuf::from(path.trim_matches('"'));
                if p.exists() {
                    return Some(p);
                }
            }
        }
        let mut dirs: Vec<PathBuf> = location.into_iter().collect();
        for var in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(pf) = std::env::var_os(var) {
                dirs.push(PathBuf::from(pf).join(app.name));
            }
        }
        dirs.into_iter().map(|d| d.join(&exe)).find(|p| p.exists())
    }

    pub fn detect(app: &CraftApp) -> Option<Installed> {
        let m = msi_installs(app).into_iter().next()?;
        Some(Installed {
            version: m.version,
            source: InstallSource::Msi,
            location: m.exe.map_or_else(|| app.name.to_string(), |p| p.display().to_string()),
            package: Some(m.product_code),
        })
    }
}

#[cfg(target_os = "linux")]
pub mod linux {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use super::*;
    use crate::packages::{self, Package};
    use crate::process::capture;

    /// Installed Craft App packages from every package manager on this system (dpkg, rpm,
    /// pacman, Flatpak). Read once per detection pass.
    pub fn packages() -> Vec<Package> {
        let ours = |name: &str| {
            artcraft_launcher_core::SUITE.iter().any(|a| {
                [InstallSource::Deb, InstallSource::Rpm, InstallSource::Pacman, InstallSource::Flatpak]
                    .iter()
                    .any(|m| crate::packages::candidate_names(a, *m).iter().any(|n| n == name))
            })
        };
        let t = Duration::from_secs(10);
        let mut out = Vec::new();
        let mut add = |manager, list: Vec<(String, String)>| {
            out.extend(list.into_iter().filter(|(n, _)| ours(n)).map(|(name, version)| Package { manager, name, version }));
        };
        if let Some(list) = capture("dpkg-query", &["-W", "-f", "${Package}|${db:Status-Status}|${Version}\n"], t) {
            add(InstallSource::Deb, packages::parse_dpkg(&list));
        }
        if let Some(list) = capture("rpm", &["-qa", "--qf", "%{NAME}|%{VERSION}\n"], t) {
            add(InstallSource::Rpm, packages::parse_rpm(&list));
        }
        // Only run `pacman` where its database exists: elsewhere `pacman` on PATH may be an unrelated program
        // (the Pac-Man game in /usr/games on Debian-family systems).
        if pacman_db_present(Path::new(PACMAN_DB))
            && let Some(list) = capture("pacman", &["-Q"], t)
        {
            add(InstallSource::Pacman, packages::parse_pacman(&list));
        }
        let arch = format!("--arch={}", std::env::consts::ARCH);
        if let Some(list) = crate::process::capture_flatpak(&["list", "--app", &arch, "--columns=application,version"], t) {
            let mut flatpaks = packages::parse_flatpak_list(&list);
            // Bundles built from a release tarball often list no version: ask Flatpak directly.
            for (id, version) in flatpaks.iter_mut().filter(|(id, v)| v.is_empty() && id.starts_with("ai.storyteller.")) {
                if let Some(v) = crate::process::capture_flatpak(&["info", &arch, id], t).as_deref().and_then(packages::parse_flatpak_info) {
                    *version = v;
                }
            }
            add(InstallSource::Flatpak, flatpaks);
        }
        out
    }

    /// The local package database of Arch's pacman.
    const PACMAN_DB: &str = "/var/lib/pacman/local";

    /// True when `db` is a directory, i.e. this system really uses pacman.
    pub(crate) fn pacman_db_present(db: &Path) -> bool {
        db.is_dir()
    }

    /// Where a package put the app's binary: on `PATH`, else `/usr/bin/<id>`.
    fn binary_on_path(id: &str) -> String {
        std::env::var_os("PATH")
            .and_then(|path| std::env::split_paths(&path).map(|d| d.join(id)).find(|p| p.is_file()))
            .unwrap_or_else(|| PathBuf::from("/usr/bin").join(id))
            .display()
            .to_string()
    }

    /// `~/Applications`, where the launcher puts AppImages (the AppImageLauncher convention).
    pub fn appimage_dir() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join("Applications"))
    }

    /// Every folder searched for AppImages, the launcher's own first: `~/Applications`,
    /// `~/AppImages` (Gear Lever's default) and `~/.local/bin`.
    pub fn appimage_dirs() -> Vec<PathBuf> {
        let Some(home) = dirs::home_dir() else { return Vec::new() };
        vec![home.join("Applications"), home.join("AppImages"), home.join(".local").join("bin")]
    }

    /// AppImages of `app` in `dirs`, newest first (unknown versions last; earlier folders win
    /// ties).
    pub fn appimages_in(app: &CraftApp, dirs: &[PathBuf]) -> Vec<(PathBuf, Option<version::Version>)> {
        let stems: Vec<&str> = app.stems().collect();
        let mut out: Vec<(PathBuf, Option<version::Version>)> = Vec::new();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(dir) else { continue };
            let mut found: Vec<(PathBuf, Option<version::Version>)> = entries
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name().into_string().ok()?;
                    let version = appimage_version(&name, &stems)?;
                    let path = e.path();
                    path.is_file().then(|| (path, version.as_deref().and_then(version::parse)))
                })
                .collect();
            found.sort_by(|a, b| a.0.cmp(&b.0));
            out.extend(found);
        }
        // Stable sort: equal versions keep the folder order above.
        out.sort_by(|a, b| match (&b.1, &a.1) {
            (Some(x), Some(y)) => version::compare(x, y),
            (Some(_), None) => std::cmp::Ordering::Greater,
            (None, Some(_)) => std::cmp::Ordering::Less,
            (None, None) => std::cmp::Ordering::Equal,
        });
        out
    }

    /// AppImages of `app` in any of the usual folders, newest first.
    pub fn appimages(app: &CraftApp) -> Vec<(PathBuf, Option<version::Version>)> {
        appimages_in(app, &appimage_dirs())
    }

    /// AppImages of `app` in the launcher's own folder only (the ones an update replaces).
    pub fn launcher_appimages(app: &CraftApp) -> Vec<PathBuf> {
        appimage_dir().map(|d| appimages_in(app, &[d]).into_iter().map(|(p, _)| p).collect()).unwrap_or_default()
    }

    pub fn detect(app: &CraftApp, snapshot: &Snapshot) -> Option<Installed> {
        if let Some((path, version)) = appimages(app).into_iter().next() {
            return Some(Installed { version, source: InstallSource::AppImage, location: path.display().to_string(), package: None });
        }
        let order = [InstallSource::Deb, InstallSource::Rpm, InstallSource::Pacman, InstallSource::Flatpak];
        if let Some(p) = packages::find(app, &snapshot.packages, &order) {
            let location = if p.manager == InstallSource::Flatpak { app.bundle_id() } else { binary_on_path(app.id) };
            return Some(Installed { version: packages::version_of(p), source: p.manager, location, package: Some(p.name.clone()) });
        }
        detect_system_install(app)
    }

    /// When the package managers don't report the app (a query timed out or was locked, an
    /// unlisted package name, or a sandbox that can't see them) but its binary is there. It is
    /// reported as a system binary: the launcher can open it but leaves updating and removing it
    /// to whatever installed it, since it can't tell which package manager that was.
    fn detect_system_install(app: &CraftApp) -> Option<Installed> {
        let bin = find_binary(app.id)?;
        Some(Installed { version: binary_version(&bin, app), source: InstallSource::Binary, location: bin.display().to_string(), package: None })
    }

    fn find_binary(id: &str) -> Option<PathBuf> {
        let on_path = std::env::var_os("PATH").and_then(|path| std::env::split_paths(&path).map(|d| d.join(id)).find(|p| p.is_file()));
        let usual = ["/usr/bin", "/usr/local/bin"].into_iter().map(PathBuf::from).chain(dirs::home_dir().map(|h| h.join(".local/bin")));
        on_path.or_else(|| usual.map(|d| d.join(id)).find(|p| p.is_file()))
    }

    pub(crate) fn binary_version(bin: &Path, app: &CraftApp) -> Option<version::Version> {
        let out = crate::process::capture(bin.to_str()?, &["--version"], Duration::from_secs(5))?;
        parse_cli_version(out.trim(), app)
    }
}

#[cfg(target_os = "freebsd")]
pub mod freebsd {
    use std::path::Path;
    use std::time::Duration;

    use super::*;

    pub fn detect(app: &CraftApp) -> Option<Installed> {
        let bin = Path::new("/usr/local/bin").join(app.id);
        if !bin.exists() {
            return None;
        }
        Some(Installed { version: binary_version(&bin, app), source: InstallSource::Binary, location: bin.display().to_string(), package: None })
    }

    /// `<app> --version` prints `<app> 0.5.0 (sha, date)`.
    fn binary_version(bin: &Path, app: &CraftApp) -> Option<version::Version> {
        let out = crate::process::capture(bin.to_str()?, &["--version"], Duration::from_secs(5))?;
        parse_cli_version(out.trim(), app)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;

    #[test]
    fn ours_filter_matches_rpm_packages() {
        let ours = |name: &str| {
            artcraft_launcher_core::SUITE.iter().any(|a| {
                [InstallSource::Deb, InstallSource::Rpm, InstallSource::Pacman, InstallSource::Flatpak]
                    .iter()
                    .any(|m| crate::packages::candidate_names(a, *m).iter().any(|n| n == name))
            })
        };
        assert!(ours("photocraft"));
        assert!(ours("ai.storyteller.photocraft"));
        assert!(!ours("unrelated-package"));
    }

    #[test]
    fn pacman_is_only_queried_where_its_database_exists() {
        let root = tempfile::tempdir().unwrap();
        assert!(!linux::pacman_db_present(&root.path().join("local")));
        std::fs::create_dir(root.path().join("local")).unwrap();
        assert!(linux::pacman_db_present(&root.path().join("local")));
    }

    #[test]
    fn reads_binary_version_from_cli() {
        let photo = CraftApp::by_id("photocraft").unwrap();
        let temp_dir = tempfile::tempdir().unwrap();
        let bin = temp_dir.path().join("photocraft");
        std::fs::write(&bin, "#!/bin/sh\necho 'photocraft 0.6.0 (0c72d9542, 2026-10-10)'\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let ver = linux::binary_version(&bin, photo);
        assert_eq!(ver.unwrap().to_string(), "0.6.0");
    }

    #[test]
    fn reads_binary_version_when_the_cli_uses_the_display_name_or_a_former_id() {
        let temp_dir = tempfile::tempdir().unwrap();
        use std::os::unix::fs::PermissionsExt;
        let write = |name: &str, script: &str| {
            let bin = temp_dir.path().join(name);
            std::fs::write(&bin, script).unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            bin
        };
        let sound = write("soundcraft", "#!/bin/sh\necho 'SoundCraft 0.5.0'\n");
        assert_eq!(linux::binary_version(&sound, CraftApp::by_id("soundcraft").unwrap()).unwrap().to_string(), "0.5.0");
        let pdf = write("pdfcraft", "#!/bin/sh\necho 'printcraft 0.2.1'\n");
        assert_eq!(linux::binary_version(&pdf, CraftApp::by_id("pdfcraft").unwrap()).unwrap().to_string(), "0.2.1");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_appimage_names_conservatively() {
        let pdf = ["pdfcraft", "printcraft"];
        let v = |n: &str| appimage_version(n, &pdf);
        assert_eq!(v("pdfcraft-0.3.0-linux-x86_64.AppImage"), Some(Some("0.3.0".into())));
        assert_eq!(v("printcraft-0.2.1-linux-aarch64.appimage"), Some(Some("0.2.1".into())));
        assert_eq!(v("pdfcraft-0.3.0-rc.1-linux-x86_64.AppImage"), Some(Some("0.3.0-rc.1".into())));
        // Gear Lever's rename, and a plain rename by hand.
        assert_eq!(v("pdfcraft.appimage"), Some(None));
        assert_eq!(v("PdfCraft.AppImage"), Some(None));
        for other in [
            "pdfcraft-0.3.0-linux-x86_64.AppImage.zsync",
            "pdfcraft-0.3.0-linux-x86_64.deb",
            "pdfcraft-0.3.0-windows-x64.AppImage",
            "pdfcraft-cli-0.3.0-linux-x86_64.AppImage",
            "photocraft-0.6.0-linux-x86_64.AppImage",
            "mypdfcraft.AppImage",
            "pdfcraft-latest.AppImage",
            ".appimage",
            "x",
        ] {
            assert_eq!(v(other), None, "{other}");
        }
    }

    #[test]
    fn parse_cli_version_accepts_id_display_name_and_former_id() {
        let photo = CraftApp::by_id("photocraft").unwrap();
        let sound = CraftApp::by_id("soundcraft").unwrap();
        let pdf = CraftApp::by_id("pdfcraft").unwrap();
        assert_eq!(parse_cli_version("photocraft 0.7.0 (dev build)", photo).unwrap().to_string(), "0.7.0");
        assert_eq!(parse_cli_version("SoundCraft 0.5.0", sound).unwrap().to_string(), "0.5.0");
        assert_eq!(parse_cli_version("printcraft 0.2.1", pdf).unwrap().to_string(), "0.2.1");
        assert_eq!(parse_cli_version("PdfCraft 0.6.0", pdf).unwrap().to_string(), "0.6.0");
        assert!(strip_cli_prefix("photocraft-cli 1.0.0", "photocraft").is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn finds_appimages_in_every_folder_newest_first() {
        let app = CraftApp::by_id("photocraft").unwrap();
        let root = tempfile::tempdir().unwrap();
        let (apps, gear, bin) = (root.path().join("Applications"), root.path().join("AppImages"), root.path().join("bin"));
        for d in [&apps, &gear, &bin] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(apps.join("photocraft-0.5.0-linux-x86_64.AppImage"), b"").unwrap();
        std::fs::write(apps.join("wordcraft-0.3.0-linux-x86_64.AppImage"), b"").unwrap();
        std::fs::write(gear.join("photocraft.appimage"), b"").unwrap();
        std::fs::write(bin.join("photocraft-0.6.0-linux-x86_64.AppImage"), b"").unwrap();
        std::fs::create_dir(bin.join("photocraft.AppImage")).unwrap();
        let found = linux::appimages_in(app, &[apps.clone(), gear.clone(), bin.clone(), root.path().join("missing")]);
        let names: Vec<_> = found.iter().map(|(p, v)| (p.clone(), v.as_ref().map(ToString::to_string))).collect();
        assert_eq!(
            names,
            [
                (bin.join("photocraft-0.6.0-linux-x86_64.AppImage"), Some("0.6.0".to_string())),
                (apps.join("photocraft-0.5.0-linux-x86_64.AppImage"), Some("0.5.0".to_string())),
                (gear.join("photocraft.appimage"), None),
            ]
        );
    }
}
