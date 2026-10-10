//! Finding Craft Apps that are already installed, however they got there.
//!
//! - macOS: `<App>.app` in `/Applications` or `~/Applications`, read from its `Info.plist`.
//! - Windows: the Windows Installer's uninstall entries (`Learning Machines LLC`), and portable
//!   copies the launcher unpacked.
//! - Linux: AppImages in `~/Applications`, Debian/RPM packages, Flatpaks, and tarball installs.
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

    use artcraft_launcher_core::platform::parse_asset_name;

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
        if let Some(list) = capture("pacman", &["-Q"], t) {
            add(InstallSource::Pacman, packages::parse_pacman(&list));
        }
        if let Some(list) = crate::process::capture_flatpak(&["list", "--app", "--columns=application,version"], t) {
            let mut flatpaks = packages::parse_flatpak_list(&list);
            // Bundles built from a release tarball often list no version: ask Flatpak directly.
            for (id, version) in flatpaks.iter_mut().filter(|(id, v)| v.is_empty() && id.starts_with("ai.storyteller.")) {
                if let Some(v) = crate::process::capture_flatpak(&["info", id], t).as_deref().and_then(packages::parse_flatpak_info) {
                    *version = v;
                }
            }
            add(InstallSource::Flatpak, flatpaks);
        }
        out
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

    /// AppImages of `app` in `~/Applications`, newest first.
    pub fn appimages(app: &CraftApp) -> Vec<(PathBuf, Option<version::Version>)> {
        let Some(dir) = appimage_dir() else { return Vec::new() };
        let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
        let stems: Vec<&str> = app.stems().collect();
        let mut out: Vec<(PathBuf, Option<version::Version>)> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let parsed = parse_asset_name(&name, &stems)?;
                (parsed.kind == artcraft_launcher_core::PackageKind::AppImage).then(|| (e.path(), version::parse(&parsed.version)))
            })
            .collect();
        out.sort_by(|a, b| match (&b.1, &a.1) {
            (Some(x), Some(y)) => version::compare(x, y),
            _ => std::cmp::Ordering::Equal,
        });
        out
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
        Some(Installed { version: binary_version(&bin, app.id), source: InstallSource::Binary, location: bin.display().to_string(), package: None })
    }

    fn find_binary(id: &str) -> Option<PathBuf> {
        let on_path = std::env::var_os("PATH").and_then(|path| std::env::split_paths(&path).map(|d| d.join(id)).find(|p| p.is_file()));
        let usual = ["/usr/bin", "/usr/local/bin"].into_iter().map(PathBuf::from).chain(dirs::home_dir().map(|h| h.join(".local/bin")));
        on_path.or_else(|| usual.map(|d| d.join(id)).find(|p| p.is_file()))
    }

    pub(crate) fn binary_version(bin: &Path, id: &str) -> Option<version::Version> {
        let out = crate::process::capture(bin.to_str()?, &["--version"], Duration::from_secs(5))?;
        let rest = out.trim().strip_prefix(id)?.trim_start();
        version::parse(rest.split_whitespace().next()?)
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
        Some(Installed { version: binary_version(&bin, app.id), source: InstallSource::Binary, location: bin.display().to_string(), package: None })
    }

    /// `<app> --version` prints `<app> 0.5.0 (sha, date)`.
    fn binary_version(bin: &Path, id: &str) -> Option<version::Version> {
        let out = crate::process::capture(bin.to_str()?, &["--version"], Duration::from_secs(5))?;
        let rest = out.trim().strip_prefix(id)?.trim_start();
        version::parse(rest.split_whitespace().next()?)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
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
    fn reads_binary_version_from_cli() {
        let photo = CraftApp::by_id("photocraft").unwrap();
        let temp_dir = tempfile::tempdir().unwrap();
        let bin = temp_dir.path().join("photocraft");
        std::fs::write(&bin, "#!/bin/sh\necho 'photocraft 0.6.0 (0c72d9542, 2026-10-10)'\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let ver = linux::binary_version(&bin, photo.id);
        assert_eq!(ver.unwrap().to_string(), "0.6.0");
    }
}
