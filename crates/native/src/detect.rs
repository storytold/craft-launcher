//! Finding Craft Apps that are already installed, however they got there.
//!
//! - macOS: `<App>.app` in `/Applications` or `~/Applications`, read from its `Info.plist`.
//! - Windows: the Windows Installer's uninstall entries (`Learning Machines LLC`), and portable
//!   copies the launcher unpacked.
//! - Linux: AppImages in `~/Applications` (where the launcher and AppImageLauncher put them),
//!   `~/AppImages` (Gear Lever) and `~/.local/bin`, Debian/RPM packages, Flatpaks, and tarball
//!   installs.
//! - FreeBSD: tarball installs and `/usr/local/bin/<app>`.

use std::path::{Path, PathBuf};

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
    if rec.kind == artcraft_launcher_core::PackageKind::Dmg {
        return macos::read_bundle(app, &rec.path);
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

/// Who our Windows Installer packages name as their publisher. A community app's names its own
/// ([`CraftApp::msi_publisher`]).
#[cfg_attr(not(windows), allow(dead_code))]
const PUBLISHER: &str = "Learning Machines LLC";

/// Whether an uninstall entry's `DisplayName` and `Publisher` are `app`'s, under any name its
/// installers have registered.
#[cfg_attr(not(windows), allow(dead_code))]
fn is_installer_entry(app: &CraftApp, display_name: &str, publisher: &str) -> bool {
    publisher == app.msi_publisher.unwrap_or(PUBLISHER) && app.names().any(|n| n == display_name)
}

/// Where a Windows install of `app` may keep its exe, in the order to try: the entry's
/// `InstallLocation`, then `<App>` under each install root (the `ArtCraft` suite folder first, then the
/// root itself, for Program Files and the per-user Programs folder) for each name the app has had,
/// and in each folder `<stem>.exe` for each stem, current first. Doesn't touch the disk.
#[cfg_attr(not(windows), allow(dead_code))]
fn exe_candidates(app: &CraftApp, location: Option<&Path>, program_files: &[PathBuf]) -> Vec<PathBuf> {
    let in_program_files = program_files.iter().flat_map(|root| app.names().map(move |n| root.join(n)));
    let dirs = location.map(Path::to_path_buf).into_iter().chain(in_program_files);
    dirs.flat_map(|d| app.stems().map(move |s| d.join(format!("{s}.exe")))).collect()
}

/// The folder under `ArtCraft` that the MSI installs to when it is not the display name: the launcher
/// installs to `ArtCraft\Launcher`, the apps to `ArtCraft\<App>`.
#[cfg_attr(not(windows), allow(dead_code))]
fn suite_folders(app: &CraftApp, roots: &[PathBuf]) -> Vec<PathBuf> {
    if app.id != artcraft_launcher_core::catalog::LAUNCHER_ID {
        return Vec::new();
    }
    roots.iter().map(|root| root.join("ArtCraft").join("Launcher")).collect()
}

/// The registry keys an MSI records its install folder in (value `InstallDir`). The launcher's package
/// keeps its own key, under the publisher name.
#[cfg_attr(not(windows), allow(dead_code))]
fn install_dir_keys(app: &CraftApp) -> Vec<String> {
    let mut keys = vec![format!(r"SOFTWARE\{}", app.name)];
    if app.id == artcraft_launcher_core::catalog::LAUNCHER_ID {
        keys.push(format!(r"SOFTWARE\{PUBLISHER}\{}", app.name));
    }
    keys
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

    use super::{CraftApp, InstallSource, Installed, version};

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
        // Its current bundle id or an older one (SolidCraft was ai.storyteller.solidcraft).
        let id = dict.get("CFBundleIdentifier")?.as_string()?;
        if !app.bundle_ids().any(|b| b == id) {
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

    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};
    use winreg::{HKEY, RegKey};

    use super::{CraftApp, InstallSource, Installed, exe_candidates, install_dir_keys, is_installer_entry, suite_folders, version};

    const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";

    /// Reads the ProductVersion string from a program's VERSIONINFO, in the first language that
    /// has one, without running the program or any other process.
    pub fn executable_version(path: &std::path::Path) -> Option<artcraft_launcher_core::version::Version> {
        let path = dunce::canonicalize(path).ok()?;
        let info = winsafe::HVERSIONINFO::GetFileVersionInfo(path.to_str()?).ok()?;
        let listed = info.langs_and_cps().map(<[_]>::to_vec).unwrap_or_default();
        let product = string_tables(&listed).into_iter().find_map(|table| info.str_val(table, "ProductVersion").ok())?;
        version::parse(product.trim())
    }

    /// The string tables to look in: the languages the file lists, then U.S. English in UTF-16,
    /// Windows-1252 and no code page, as .NET's `FileVersionInfo` also tries when a file lists none
    /// or its listed one has no strings.
    pub(crate) fn string_tables(listed: &[(winsafe::LANGID, winsafe::co::CP)]) -> Vec<(winsafe::LANGID, winsafe::co::CP)> {
        use winsafe::{LANGID, co};
        let us = LANGID::new(co::LANG::ENGLISH, co::SUBLANG::ENGLISH_US);
        listed.iter().copied().chain([(us, co::CP::UTF16), (us, co::CP::WINDOWS_1252), (us, co::CP::ACP)]).collect()
    }

    /// An MSI install found in the registry.
    pub struct MsiInstall {
        pub product_code: String,
        pub version: Option<artcraft_launcher_core::version::Version>,
        pub exe: Option<PathBuf>,
        /// The context of the uninstall record that named it: the machine's records are per-machine,
        /// the current account's own are per-user.
        pub scope: crate::install::MsiScope,
    }

    /// Where the uninstall records of an MSI are kept: the machine's (both registry views) and the account's own.
    const UNINSTALL_ROOTS: [(HKEY, u32); 3] = [(HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY), (HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY), (HKEY_CURRENT_USER, 0)];

    fn record_context(hive: HKEY) -> crate::install::MsiScope {
        if hive == HKEY_CURRENT_USER { crate::install::MsiScope::PerUser } else { crate::install::MsiScope::PerMachine }
    }

    /// The context every uninstall record of `product_code` places it in, when Windows Installer cannot be
    /// asked. All records are read, not the deduplicated list, so a copy registered both per-user and
    /// per-machine gives no answer and the update stops.
    pub fn record_scope(app: &CraftApp, product_code: &str) -> Option<crate::install::MsiScope> {
        let scopes = UNINSTALL_ROOTS.into_iter().filter_map(|(hive, flags)| {
            let root = RegKey::predef(hive).open_subkey_with_flags(UNINSTALL, KEY_READ | flags).ok()?;
            let key = root.open_subkey_with_flags(product_code, KEY_READ | flags).ok()?;
            let name: String = key.get_value("DisplayName").unwrap_or_default();
            let publisher: String = key.get_value("Publisher").unwrap_or_default();
            is_installer_entry(app, &name, &publisher).then(|| record_context(hive))
        });
        crate::msi_context::single_scope(scopes)
    }

    /// How an exe path is stored in `Installed::location`. One function builds it for `detect` and for
    /// the comparison below, so the two can never disagree on spelling.
    fn location_text(exe: &std::path::Path) -> String {
        exe.display().to_string()
    }

    /// Whether `location` is the exe the record `product_code` resolves in its own scope. False for
    /// a path that only the launch fallback (`exe_in_any_scope`) found, so an update never takes its
    /// folder from another scope.
    pub fn location_is_in_record_scope(app: &CraftApp, product_code: &str, location: &str) -> bool {
        msi_installs(app).into_iter().find(|m| m.product_code == product_code).and_then(|m| m.exe).is_some_and(|exe| location_text(&exe) == location)
    }

    /// Every Windows Installer entry for `app`, newest first.
    pub fn msi_installs(app: &CraftApp) -> Vec<MsiInstall> {
        let mut out = Vec::new();
        for (hive, flags) in UNINSTALL_ROOTS {
            let Ok(root) = RegKey::predef(hive).open_subkey_with_flags(UNINSTALL, KEY_READ | flags) else { continue };
            for code in root.enum_keys().flatten() {
                // App-authored uninstall rows are not MSI product identities.
                if !crate::msi_context::valid_product_code(&code) {
                    continue;
                }
                let Ok(key) = root.open_subkey_with_flags(&code, KEY_READ | flags) else { continue };
                let name: String = key.get_value("DisplayName").unwrap_or_default();
                let publisher: String = key.get_value("Publisher").unwrap_or_default();
                if !is_installer_entry(app, &name, &publisher) || out.iter().any(|m: &MsiInstall| m.product_code == code) {
                    continue;
                }
                let version = key.get_value::<String, _>("DisplayVersion").ok().and_then(|v| version::parse(&v));
                let location: String = key.get_value("InstallLocation").unwrap_or_default();
                let exe =
                    exe_path(app, (!location.is_empty()).then(|| PathBuf::from(location)).or_else(|| install_dir(app, hive, flags)), hive, flags);
                out.push(MsiInstall { product_code: code, version, exe, scope: record_context(hive) });
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

    /// The folder the app's MSI remembers for its next upgrade, read from the same registry view as the
    /// uninstall record that named the install. A per-machine record (HKLM) therefore reads HKLM, and a
    /// per-user record (HKCU, dual-scope packages) reads HKCU, so one scope never resolves to the other's folder.
    fn install_dir(app: &CraftApp, hive: HKEY, flags: u32) -> Option<PathBuf> {
        install_dir_keys(app).into_iter().find_map(|path| {
            let key = RegKey::predef(hive).open_subkey_with_flags(path, KEY_READ | flags).ok()?;
            let dir = key.get_value::<String, _>("InstallDir").ok()?;
            (!dir.is_empty()).then(|| PathBuf::from(dir))
        })
    }

    /// The app's exe for one uninstall record, found only in that record's own scope: the recorded
    /// folder first, then `App Paths` in the record's hive and registry view, then the usual folders
    /// for that scope (`ArtCraft` suite folder first, then the bare root, for older installs). A
    /// per-machine entry therefore never stands in for a per-user record, or the other way round.
    fn exe_path(app: &CraftApp, location: Option<PathBuf>, hive: HKEY, flags: u32) -> Option<PathBuf> {
        if let Some(found) = exe_candidates(app, location.as_deref(), &[]).into_iter().find(|p| p.exists()) {
            return Some(found);
        }
        for stem in app.stems() {
            let app_paths = format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{stem}.exe");
            if let Ok(key) = RegKey::predef(hive).open_subkey_with_flags(&app_paths, KEY_READ | flags)
                && let Ok(path) = key.get_value::<String, _>("")
            {
                let p = PathBuf::from(path.trim_matches('"'));
                if p.exists() {
                    return Some(p);
                }
            }
        }
        let roots: Vec<PathBuf> = if hive == HKEY_CURRENT_USER {
            // Per-user installs (dual-scope packages) live in the user's own Programs folder.
            std::env::var_os("LOCALAPPDATA").map(|local| PathBuf::from(local).join("Programs")).into_iter().collect()
        } else {
            let mut roots: Vec<PathBuf> = Vec::new();
            for var in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
                if let Some(pf) = std::env::var_os(var).map(PathBuf::from)
                    && !roots.contains(&pf)
                {
                    roots.push(pf);
                }
            }
            roots
        };
        exe_in_roots(app, roots)
    }

    /// Where the exe may be under the given install roots, best first: the launcher's own folder, then
    /// the `ArtCraft` suite folder of each root, then the bare roots (older installs). Not checked on
    /// disk.
    fn candidates_in_roots(app: &CraftApp, roots: &[PathBuf]) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = suite_folders(app, roots).iter().flat_map(|dir| exe_candidates(app, Some(dir), &[])).collect();
        let mut dirs: Vec<PathBuf> = roots.iter().map(|root| root.join("ArtCraft")).collect();
        dirs.extend(roots.iter().cloned());
        out.extend(exe_candidates(app, None, &dirs));
        out
    }

    /// The first exe under the given install roots.
    pub(super) fn exe_in_roots(app: &CraftApp, roots: Vec<PathBuf>) -> Option<PathBuf> {
        candidates_in_roots(app, &roots).into_iter().find(|p| p.exists())
    }

    /// Every exe under the given install roots, in the order `exe_in_roots` prefers them.
    pub(super) fn exe_all_in_roots(app: &CraftApp, roots: Vec<PathBuf>) -> Vec<PathBuf> {
        candidates_in_roots(app, &roots).into_iter().filter(|p| p.exists()).collect()
    }

    /// The app's exe directly in `folder`, under any name it has shipped as.
    pub(super) fn exe_in_folder(app: &CraftApp, folder: &std::path::Path) -> Option<PathBuf> {
        exe_candidates(app, Some(folder), &[]).into_iter().find(|p| p.exists())
    }

    /// Last resort for opening an app whose uninstall record names no folder and whose exe is not in
    /// the record's own scope. The package records its folder (`InstallDir`) in the hive of the account
    /// that installed it, which need not be the hive of its uninstall record: a per-user install can
    /// leave a per-machine record. So the folder any hive or view recorded is tried first (this finds
    /// a custom folder), then the usual folders of every scope. This only decides which exe "Open"
    /// runs and where "Show in folder" goes. An update never takes its folder from here
    /// (`location_is_in_record_scope`), and the record's scope is unchanged. Every candidate is
    /// collected and the one whose own version is the record's is chosen, so a leftover older copy in
    /// another scope is not opened under the newer record's version (`pick_candidate`).
    fn exe_in_any_scope(app: &CraftApp, version: Option<&version::Version>) -> Option<PathBuf> {
        let mut found: Vec<PathBuf> =
            UNINSTALL_ROOTS.into_iter().filter_map(|(hive, flags)| exe_in_folder(app, &install_dir(app, hive, flags)?)).collect();
        let mut roots: Vec<PathBuf> = std::env::var_os("LOCALAPPDATA").map(|local| PathBuf::from(local).join("Programs")).into_iter().collect();
        for var in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(pf) = std::env::var_os(var).map(PathBuf::from)
                && !roots.contains(&pf)
            {
                roots.push(pf);
            }
        }
        found.extend(exe_all_in_roots(app, roots));
        pick_candidate(found, version, executable_version)
    }

    /// The first candidate whose version is `wanted`; with no match (or no version to match, or exes
    /// without version info) the first candidate, in the order they were found.
    pub(super) fn pick_candidate(
        candidates: Vec<PathBuf>,
        wanted: Option<&version::Version>,
        version_of: impl Fn(&std::path::Path) -> Option<version::Version>,
    ) -> Option<PathBuf> {
        let matching = wanted.and_then(|want| candidates.iter().find(|c| version_of(c).as_ref() == Some(want)));
        matching.or_else(|| candidates.first()).cloned()
    }

    pub fn detect(app: &CraftApp) -> Option<Installed> {
        let m = msi_installs(app).into_iter().next()?;
        let location = m.exe.or_else(|| exe_in_any_scope(app, m.version.as_ref())).map_or_else(|| app.name.to_string(), |p| location_text(&p));
        Some(Installed { version: m.version, source: InstallSource::Msi, location, package: Some(m.product_code) })
    }
}

#[cfg(target_os = "linux")]
pub mod linux {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use super::{CraftApp, InstallSource, Installed, Snapshot, appimage_version, parse_cli_version, version};
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
        if has_pacman_db()
            && let Some(list) = capture("pacman", &["-Q"], t)
        {
            add(InstallSource::Pacman, packages::parse_pacman(&list));
        }
        let arch = format!("--arch={}", std::env::consts::ARCH);
        if let Some(list) = crate::process::capture_flatpak(&["list", "--app", &arch, "--columns=application,version"], t) {
            let mut flatpaks = packages::parse_flatpak_list(&list);
            // Bundles built from a release tarball often list no version: ask Flatpak directly.
            for (id, version) in
                flatpaks.iter_mut().filter(|(id, v)| v.is_empty() && artcraft_launcher_core::SUITE.iter().any(|a| a.bundle_ids().any(|b| b == *id)))
            {
                if let Some(v) = crate::process::capture_flatpak(&["info", &arch, id], t).as_deref().and_then(packages::parse_flatpak_info) {
                    *version = v;
                }
            }
            add(InstallSource::Flatpak, flatpaks);
        }
        out
    }

    /// The local package database of Arch's pacman.
    pub(crate) const PACMAN_DB: &str = "/var/lib/pacman/local";

    /// True when `db` is a directory, i.e. this system really uses pacman.
    pub(crate) fn pacman_db_present(db: &Path) -> bool {
        db.is_dir()
    }

    /// True when the system's default pacman database directory exists.
    pub(crate) fn has_pacman_db() -> bool {
        pacman_db_present(Path::new(PACMAN_DB))
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

    pub fn detect(app: &CraftApp, snapshot: &Snapshot) -> Option<Installed> {
        if let Some((path, version)) = appimages(app).into_iter().next() {
            return Some(Installed { version, source: InstallSource::AppImage, location: path.display().to_string(), package: None });
        }
        let order = [InstallSource::Deb, InstallSource::Rpm, InstallSource::Pacman, InstallSource::Flatpak];
        if let Some(p) = packages::find(app, &snapshot.packages, &order) {
            // A Flatpak runs under the id it was found under, which may be an older one.
            let location = if p.manager == InstallSource::Flatpak { p.name.clone() } else { binary_on_path(app.id) };
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

    use super::{CraftApp, InstallSource, Installed, parse_cli_version, version};

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
        assert!(ours("io.tangencylabs.solidcraft"));
        assert!(ours("ai.storyteller.solidcraft"));
        assert!(ours("ai.storyteller.solvecraft"));
        assert!(!ours("unrelated-package"));
    }

    #[test]
    fn pacman_is_only_queried_where_its_database_exists() {
        let root = tempfile::tempdir().unwrap();
        assert!(!linux::pacman_db_present(&root.path().join("local")));
        std::fs::create_dir(root.path().join("local")).unwrap();
        assert!(linux::pacman_db_present(&root.path().join("local")));
    }

    /// `linux::binary_version` on a script the test just wrote. Another test thread that forks
    /// while the script is still open for writing holds that descriptor until it execs, and running
    /// the script in that window fails with ETXTBSY ("Text file busy"), so a miss is retried.
    fn binary_version_of_new_script(bin: &Path, app: &CraftApp) -> Option<version::Version> {
        for _ in 0..20 {
            if let Some(v) = linux::binary_version(bin, app) {
                return Some(v);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        None
    }

    #[test]
    fn reads_binary_version_from_cli() {
        let photo = CraftApp::by_id("photocraft").unwrap();
        let temp_dir = tempfile::tempdir().unwrap();
        let bin = temp_dir.path().join("photocraft");
        std::fs::write(&bin, "#!/bin/sh\necho 'photocraft 0.6.0 (0c72d9542, 2026-10-10)'\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let ver = binary_version_of_new_script(&bin, photo);
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
        assert_eq!(binary_version_of_new_script(&sound, CraftApp::by_id("soundcraft").unwrap()).unwrap().to_string(), "0.5.0");
        let pdf = write("pdfcraft", "#!/bin/sh\necho 'printcraft 0.2.1'\n");
        assert_eq!(binary_version_of_new_script(&pdf, CraftApp::by_id("pdfcraft").unwrap()).unwrap().to_string(), "0.2.1");
    }
}

#[cfg(test)]
mod tests {
    use artcraft_launcher_core::SUITE;

    use super::*;

    /// A file without VERSIONINFO, or one that isn't there, has no version.
    #[cfg(windows)]
    #[test]
    fn a_file_without_version_info_has_no_version() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("photocraft.exe");
        std::fs::write(&text, "not a program").unwrap();
        assert_eq!(windows::executable_version(&text), None);
        assert_eq!(windows::executable_version(&dir.path().join("missing.exe")), None);
    }

    /// Without a listed language, the version is still looked for in the string tables .NET's
    /// `FileVersionInfo` falls back to (040904B0, 040904E4, 04090000), after any listed one.
    #[cfg(windows)]
    #[test]
    fn the_version_is_looked_for_in_the_usual_string_tables_too() {
        use winsafe::{LANGID, co};
        let key = |(lang, cp): (LANGID, co::CP)| format!("{:04x}{:04x}", u16::from(lang), u16::from(cp));
        let keys = |listed: &[(LANGID, co::CP)]| windows::string_tables(listed).into_iter().map(key).collect::<Vec<_>>();
        assert_eq!(keys(&[]), ["040904b0", "040904e4", "04090000"]);
        let german = (LANGID::new(co::LANG::GERMAN, co::SUBLANG::GERMAN), co::CP::UTF16);
        assert_eq!(keys(&[german]), ["040704b0", "040904b0", "040904e4", "04090000"]);
    }

    /// A program whose VERSIONINFO has a ProductVersion in the U.S. English UTF-16 table but no
    /// `\VarFileInfo\Translation` entry still has a version.
    #[cfg(windows)]
    #[test]
    fn a_version_without_a_translation_entry_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("photocraft.exe");
        std::fs::write(&exe, resource_only_program("1.2.3")).unwrap();
        assert_eq!(windows::executable_version(&exe), version::parse("1.2.3"));
    }

    /// A VERSIONINFO block node: length, value length, type, the key, then the value and the
    /// children, each aligned to 4 bytes.
    #[cfg(windows)]
    fn version_node(key: &str, value: &[u8], value_length: u16, text: bool, children: &[Vec<u8>]) -> Vec<u8> {
        let align = |bytes: &mut Vec<u8>| bytes.resize(bytes.len().next_multiple_of(4), 0);
        let mut node = vec![0, 0];
        node.extend(value_length.to_le_bytes());
        node.extend(u16::from(text).to_le_bytes());
        node.extend(key.encode_utf16().chain([0]).flat_map(u16::to_le_bytes));
        align(&mut node);
        node.extend(value);
        for child in children {
            align(&mut node);
            node.extend(child);
        }
        let length = u16::try_from(node.len()).unwrap();
        node[..2].copy_from_slice(&length.to_le_bytes());
        node
    }

    /// A 64-bit program file holding only a version resource (`ProductVersion` = `product`, in
    /// the 040904B0 string table) and no `\VarFileInfo\Translation` entry.
    #[cfg(windows)]
    fn resource_only_program(product: &str) -> Vec<u8> {
        let put = |bytes: &mut Vec<u8>, at: usize, value: &[u8]| bytes[at..at + value.len()].copy_from_slice(value);
        let text: Vec<u8> = product.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
        let string = version_node("ProductVersion", &text, u16::try_from(product.len() + 1).unwrap(), true, &[]);
        let table = version_node("040904b0", &[], 0, true, &[string]);
        let strings = version_node("StringFileInfo", &[], 0, true, &[table]);
        let mut fixed = vec![0u8; 52];
        put(&mut fixed, 0, &0xFEEF_04BDu32.to_le_bytes());
        put(&mut fixed, 4, &0x0001_0000u32.to_le_bytes());
        let info = version_node("VS_VERSION_INFO", &fixed, 52, false, &[strings]);

        // The resource section: type 16 (RT_VERSION) > id 1 > language 0x0409 > the data.
        const RVA: u32 = 0x1000;
        let mut rsrc = vec![0u8; 0x58];
        for (dir, id, next) in [(0x00, 16u32, 0x8000_0018u32), (0x18, 1, 0x8000_0030), (0x30, 0x0409, 0x48)] {
            put(&mut rsrc, dir + 14, &1u16.to_le_bytes());
            put(&mut rsrc, dir + 16, &id.to_le_bytes());
            put(&mut rsrc, dir + 20, &next.to_le_bytes());
        }
        put(&mut rsrc, 0x48, &(RVA + 0x58).to_le_bytes());
        put(&mut rsrc, 0x4C, &u32::try_from(info.len()).unwrap().to_le_bytes());
        rsrc.extend(&info);
        let raw = u32::try_from(rsrc.len().next_multiple_of(0x200)).unwrap();
        let rsrc_len = u32::try_from(rsrc.len()).unwrap();

        let mut file = vec![0u8; 0x200];
        put(&mut file, 0, b"MZ");
        put(&mut file, 0x3C, &0x40u32.to_le_bytes());
        put(&mut file, 0x40, b"PE\0\0");
        // COFF header: x64, one section, a 240-byte optional header, an executable DLL image.
        put(&mut file, 0x44, &0x8664u16.to_le_bytes());
        put(&mut file, 0x46, &1u16.to_le_bytes());
        put(&mut file, 0x54, &240u16.to_le_bytes());
        put(&mut file, 0x56, &0x2022u16.to_le_bytes());
        // PE32+ optional header.
        let opt = 0x58;
        put(&mut file, opt, &0x20Bu16.to_le_bytes());
        put(&mut file, opt + 8, &raw.to_le_bytes());
        put(&mut file, opt + 24, &0x1_4000_0000u64.to_le_bytes());
        put(&mut file, opt + 32, &0x1000u32.to_le_bytes());
        put(&mut file, opt + 36, &0x200u32.to_le_bytes());
        put(&mut file, opt + 40, &6u16.to_le_bytes());
        put(&mut file, opt + 48, &6u16.to_le_bytes());
        put(&mut file, opt + 56, &(RVA + rsrc_len.next_multiple_of(0x1000)).to_le_bytes());
        put(&mut file, opt + 60, &0x200u32.to_le_bytes());
        put(&mut file, opt + 68, &2u16.to_le_bytes());
        put(&mut file, opt + 72, &0x10_0000u64.to_le_bytes());
        put(&mut file, opt + 80, &0x1000u64.to_le_bytes());
        put(&mut file, opt + 88, &0x10_0000u64.to_le_bytes());
        put(&mut file, opt + 96, &0x1000u64.to_le_bytes());
        put(&mut file, opt + 108, &16u32.to_le_bytes());
        put(&mut file, opt + 112 + 2 * 8, &RVA.to_le_bytes());
        put(&mut file, opt + 112 + 2 * 8 + 4, &rsrc_len.to_le_bytes());
        // The .rsrc section header.
        let section = opt + 240;
        put(&mut file, section, b".rsrc\0\0\0");
        put(&mut file, section + 8, &rsrc_len.to_le_bytes());
        put(&mut file, section + 12, &RVA.to_le_bytes());
        put(&mut file, section + 16, &raw.to_le_bytes());
        put(&mut file, section + 20, &0x200u32.to_le_bytes());
        put(&mut file, section + 36, &0x4000_0040u32.to_le_bytes());
        rsrc.resize(usize::try_from(raw).unwrap(), 0);
        file.extend(rsrc);
        file
    }

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

    const LEARNING_MACHINES: &str = "Learning Machines LLC";

    fn roots() -> Vec<PathBuf> {
        vec![PathBuf::from(r"C:\Program Files"), PathBuf::from(r"C:\Program Files (x86)")]
    }

    #[test]
    fn pdfcraft_matches_its_former_printcraft_installer_name() {
        let pdf = CraftApp::by_id("pdfcraft").unwrap();
        assert!(is_installer_entry(pdf, "PrintCraft", LEARNING_MACHINES), "a PrintCraft entry from {LEARNING_MACHINES} should be PdfCraft's");
    }

    #[test]
    fn pdfcraft_still_matches_its_own_installer_name() {
        let pdf = CraftApp::by_id("pdfcraft").unwrap();
        assert!(is_installer_entry(pdf, "PdfCraft", LEARNING_MACHINES));
    }

    #[test]
    fn printcraft_entries_match_nothing_else() {
        let pdf = CraftApp::by_id("pdfcraft").unwrap();
        assert!(!is_installer_entry(pdf, "PrintCraft", "Some Other Publisher"));
        assert!(!is_installer_entry(pdf, "PhotoCraft", LEARNING_MACHINES));
        for a in SUITE.iter().filter(|a| a.id != "pdfcraft") {
            assert!(!is_installer_entry(a, "PrintCraft", LEARNING_MACHINES), "PrintCraft matched {}", a.name);
        }
    }

    #[test]
    fn solidcraft_matches_its_own_installer_publisher_only() {
        let sc = CraftApp::by_id("solidcraft").unwrap();
        assert!(is_installer_entry(sc, "SolidCraft", "SolidCraft contributors"));
        assert!(!is_installer_entry(sc, "SolidCraft", LEARNING_MACHINES));
        assert!(!is_installer_entry(CraftApp::by_id("cadcraft").unwrap(), "CADCraft", "SolidCraft contributors"));
    }

    #[test]
    fn pdfcraft_exe_is_sought_under_every_name_it_has_shipped_as() {
        let pdf = CraftApp::by_id("pdfcraft").unwrap();
        let location = PathBuf::from(r"D:\Apps\PrintCraft");
        let roots = roots();
        let mut want = vec![location.join("pdfcraft.exe"), location.join("printcraft.exe")];
        for root in &roots {
            for dir in ["PdfCraft", "PrintCraft"] {
                want.push(root.join(dir).join("pdfcraft.exe"));
                want.push(root.join(dir).join("printcraft.exe"));
            }
        }
        assert_eq!(exe_candidates(pdf, Some(&location), &roots), want);
    }

    #[test]
    fn apps_without_former_names_match_and_find_their_exe_as_before() {
        let roots = roots();
        let location = PathBuf::from(r"D:\Apps\Somewhere");
        for a in SUITE.iter().filter(|a| a.former_ids.is_empty() && a.former_names.is_empty()) {
            let publisher = a.msi_publisher.unwrap_or(LEARNING_MACHINES);
            assert!(is_installer_entry(a, a.name, publisher), "{}", a.name);
            assert!(!is_installer_entry(a, a.name, "Some Other Publisher"), "{}", a.name);
            assert!(!is_installer_entry(a, &a.name.to_lowercase(), publisher), "{}", a.name);
            let exe = format!("{}.exe", a.id);
            let in_program_files: Vec<PathBuf> = roots.iter().map(|r| r.join(a.name).join(&exe)).collect();
            assert_eq!(exe_candidates(a, None, &roots), in_program_files);
            let with_location: Vec<PathBuf> = std::iter::once(location.join(&exe)).chain(in_program_files).collect();
            assert_eq!(exe_candidates(a, Some(&location), &roots), with_location);
        }
    }

    /// An app unpacked into the suite folder of a root is found from that root alone, so "Open" works
    /// for a record whose folder is not the one its scope would name.
    #[cfg(windows)]
    #[test]
    fn an_exe_in_the_suite_folder_of_any_root_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let deck = CraftApp::by_id("deckcraft").unwrap();
        let folder = dir.path().join("ArtCraft").join("DeckCraft");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("deckcraft.exe"), "x").unwrap();
        assert_eq!(windows::exe_in_roots(deck, vec![dir.path().to_path_buf()]), Some(folder.join("deckcraft.exe")));
        assert_eq!(windows::exe_in_roots(deck, vec![dir.path().join("elsewhere")]), None);
    }

    /// A folder the package recorded is searched as it is, whatever it is called, so an app installed
    /// to a folder of the user's choosing is found.
    #[cfg(windows)]
    #[test]
    fn an_exe_in_a_custom_recorded_folder_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let deck = CraftApp::by_id("deckcraft").unwrap();
        let custom = dir.path().join("My Slides");
        std::fs::create_dir_all(&custom).unwrap();
        assert_eq!(windows::exe_in_folder(deck, &custom), None);
        std::fs::write(custom.join("deckcraft.exe"), "x").unwrap();
        assert_eq!(windows::exe_in_folder(deck, &custom), Some(custom.join("deckcraft.exe")));
    }

    /// With two copies in different scopes, the one that is the record's version is chosen, wherever it
    /// was found; with no match, the first found.
    #[cfg(windows)]
    #[test]
    fn the_copy_with_the_records_version_is_the_one_opened() {
        let v = |s: &str| version::parse(s);
        let old = PathBuf::from(r"D:\Old\deckcraft.exe");
        let new = PathBuf::from(r"C:\Users\a\AppData\Local\Programs\ArtCraft\DeckCraft\deckcraft.exe");
        let version_of = |p: &std::path::Path| if p == old { v("0.11.1") } else { v("0.12.0") };
        let both = vec![old.clone(), new.clone()];
        assert_eq!(windows::pick_candidate(both.clone(), v("0.12.0").as_ref(), version_of), Some(new.clone()));
        assert_eq!(windows::pick_candidate(both.clone(), v("0.11.1").as_ref(), version_of), Some(old.clone()));
        assert_eq!(windows::pick_candidate(both.clone(), v("0.99.0").as_ref(), version_of), Some(old.clone()));
        assert_eq!(windows::pick_candidate(both, None, version_of), Some(old.clone()));
        assert_eq!(windows::pick_candidate(Vec::new(), v("0.12.0").as_ref(), version_of), None);
    }

    /// Every exe under the given roots, in the order `exe_in_roots` prefers them, so a version match
    /// can see a copy that an earlier root's older copy would otherwise hide.
    #[cfg(windows)]
    #[test]
    fn every_root_is_searched_for_a_version_match() {
        let dir = tempfile::tempdir().unwrap();
        let deck = CraftApp::by_id("deckcraft").unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        let exes: Vec<PathBuf> = [&a, &b].iter().map(|r| r.join("ArtCraft").join("DeckCraft").join("deckcraft.exe")).collect();
        for exe in &exes {
            std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
            std::fs::write(exe, "x").unwrap();
        }
        assert_eq!(windows::exe_all_in_roots(deck, vec![a.clone(), b.clone()]), exes);
        assert_eq!(windows::exe_in_roots(deck, vec![a, b]), exes.first().cloned());
        assert!(windows::exe_all_in_roots(deck, vec![dir.path().join("none")]).is_empty());
    }

    #[test]
    fn the_launcher_msi_installs_to_artcraft_launcher_and_the_apps_do_not() {
        let roots = roots();
        let want: Vec<PathBuf> = roots.iter().map(|root| root.join("ArtCraft").join("Launcher")).collect();
        assert_eq!(suite_folders(&artcraft_launcher_core::catalog::LAUNCHER, &roots), want);
        let photo = CraftApp::by_id("photocraft").unwrap();
        assert!(suite_folders(photo, &roots).is_empty());
    }

    #[test]
    fn the_launcher_records_its_folder_under_the_publisher_key() {
        assert_eq!(
            install_dir_keys(&artcraft_launcher_core::catalog::LAUNCHER),
            [r"SOFTWARE\ArtCraft Launcher".to_string(), format!(r"SOFTWARE\{LEARNING_MACHINES}\ArtCraft Launcher")]
        );
        let photo = CraftApp::by_id("photocraft").unwrap();
        assert_eq!(install_dir_keys(photo), [r"SOFTWARE\PhotoCraft".to_string()]);
    }
}
