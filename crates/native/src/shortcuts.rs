//! Per-app and global desktop and Start menu / application menu shortcut management.
//!
//! Supports creating, updating, detecting, and safely removing shortcuts across Windows (`.lnk` files in
//! user Desktop and user Start Menu Programs), Linux (freedesktop `.desktop` files in `~/Desktop` and
//! `~/.local/share/applications`), and macOS (symbolic links in `~/Desktop` and `~/Applications`).
//!
//! All shortcuts created by the launcher are recorded in [`ShortcutManifest`] (`shortcuts.json`),
//! ensuring uninstalls and disabling settings only delete files the launcher itself created and
//! never touch unrecorded files, third-party shortcuts, or system directories like `ProgramData`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use artcraft_launcher_core::protocol::{AppShortcutState, BackendOptions};
use artcraft_launcher_core::{CraftApp, Installed, SUITE};
use serde::{Deserialize, Serialize};

use crate::paths::write_atomic;

/// Recorded shortcuts created by the launcher for an application.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortcutRecord {
    /// The desktop shortcut path, if created by the launcher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop: Option<PathBuf>,
    /// The Start menu / application menu shortcut path, if created by the launcher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_menu: Option<PathBuf>,
}

/// The persistent manifest of all shortcuts created by the launcher across the suite.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortcutManifest {
    /// Mapping of app ID to recorded shortcuts.
    #[serde(default)]
    pub apps: BTreeMap<String, ShortcutRecord>,
}

impl ShortcutManifest {
    /// Loads the shortcut manifest from `path`, or returns default if missing or unreadable.
    pub fn load(path: &Path) -> Self {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    /// Saves the shortcut manifest to `path` atomically.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        write_atomic(path, &bytes)
    }
}

/// Returns the default shortcut manifest path (`<data>/shortcuts.json`).
pub fn default_manifest_path() -> Option<PathBuf> {
    crate::paths::Paths::new().ok().map(|p| p.shortcuts())
}

/// Resolves the effective `(desktop, start_menu)` shortcut preferences for `app_id`, combining
/// per-app overrides with the global defaults in [`BackendOptions`].
pub fn resolve_prefs(app_id: &str, options: &BackendOptions) -> (bool, bool) {
    let prefs = options.app_shortcuts.get(app_id).copied().unwrap_or_default();
    let desktop = prefs.desktop.unwrap_or(options.create_desktop_shortcut);
    let start_menu = prefs.start_menu.unwrap_or(options.create_start_menu_shortcut);
    (desktop, start_menu)
}

/// Detects current shortcut existence on disk for all apps in [`SUITE`].
pub fn detect_all(installed: &HashMap<String, Installed>) -> BTreeMap<String, AppShortcutState> {
    let mut out = BTreeMap::new();
    for app in &SUITE {
        if installed.contains_key(app.id) {
            out.insert(app.id.to_string(), detect_one(app));
        }
    }
    out
}

/// Detects whether desktop and Start menu / application menu shortcuts exist on disk for `app`.
pub fn detect_one(app: &CraftApp) -> AppShortcutState {
    #[cfg(windows)]
    {
        let desktop = windows_desktop_lnk(app).is_some_and(|p| p.is_file());
        let start_menu = windows_start_menu_lnks(app).into_iter().any(|p| p.is_file());
        AppShortcutState { desktop, start_menu }
    }
    #[cfg(target_os = "linux")]
    {
        let bundle = format!("{}.desktop", app.bundle_id());
        let desktop = dirs::desktop_dir().is_some_and(|d| d.join(&bundle).is_file() || d.join(format!("{}.desktop", app.name)).is_file());
        let start_menu = linux_menu_dirs().into_iter().any(|d| d.join(&bundle).is_file());
        AppShortcutState { desktop, start_menu }
    }
    #[cfg(target_os = "macos")]
    {
        let bundle = format!("{}.app", app.name);
        let desktop = dirs::desktop_dir().is_some_and(|d| d.join(&bundle).exists() || d.join(app.name).exists());
        let start_menu =
            Path::new("/Applications").join(&bundle).exists() || dirs::home_dir().is_some_and(|h| h.join("Applications").join(&bundle).exists());
        AppShortcutState { desktop, start_menu }
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = app;
        AppShortcutState::default()
    }
}

/// Synchronizes the desktop and Start menu / application menu shortcuts for an installed `app`
/// so they match `(desktop, start_menu)` and point to `installed.location`.
pub fn sync_shortcuts(app: &CraftApp, installed: &Installed, desktop: bool, start_menu: bool) -> Result<AppShortcutState, String> {
    let m_path = default_manifest_path();
    sync_shortcuts_with_manifest(app, installed, desktop, start_menu, m_path.as_deref())
}

/// Synchronizes shortcuts using an explicit `manifest_path` to record and track created files.
pub fn sync_shortcuts_with_manifest(
    app: &CraftApp,
    installed: &Installed,
    desktop: bool,
    start_menu: bool,
    manifest_path: Option<&Path>,
) -> Result<AppShortcutState, String> {
    let mut manifest = manifest_path.map(ShortcutManifest::load).unwrap_or_default();
    let mut record = manifest.apps.get(app.id).cloned().unwrap_or_default();
    let target = Path::new(&installed.location);

    #[cfg(windows)]
    {
        sync_windows_shortcuts(app, target, desktop, start_menu, &mut record)?;
    }
    #[cfg(target_os = "linux")]
    {
        let desktop_dir = dirs::desktop_dir();
        let data_dir = crate::install::linux::host_data_dir();
        sync_linux_shortcuts_in(desktop_dir.as_deref(), data_dir.as_deref(), app, target, desktop, start_menu, &mut record)?;
    }
    #[cfg(target_os = "macos")]
    {
        sync_macos_shortcuts(app, target, desktop, start_menu, &mut record)?;
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = (app, target, desktop, start_menu, &mut record);
    }

    if let Some(m_path) = manifest_path {
        if record.desktop.is_none() && record.start_menu.is_none() {
            manifest.apps.remove(app.id);
        } else {
            manifest.apps.insert(app.id.to_string(), record);
        }
        let _ = manifest.save(m_path);
    }

    Ok(detect_one(app))
}

/// Applies the configured shortcut preferences for `app` right after an install or update.
pub fn apply_after_install(app: &CraftApp, installed: Option<&Installed>, options: &BackendOptions) {
    let Some(inst) = installed else { return };
    let (desktop, start_menu) = resolve_prefs(app.id, options);
    if let Err(e) = sync_shortcuts(app, inst, desktop, start_menu) {
        log::warn!("{}: couldn't sync shortcuts after install: {e}", app.name);
    }
}

/// Removes any launcher-managed shortcuts for `app` when the application is uninstalled.
pub fn remove_all(app: &CraftApp) {
    let m_path = default_manifest_path();
    remove_all_with_manifest(app, m_path.as_deref());
}

/// Removes any launcher-managed shortcuts for `app` recorded in `manifest_path` when uninstalled.
pub fn remove_all_with_manifest(app: &CraftApp, manifest_path: Option<&Path>) {
    let mut manifest = manifest_path.map(ShortcutManifest::load).unwrap_or_default();
    let record = manifest.apps.remove(app.id);

    #[cfg(windows)]
    {
        if let Some(r) = &record {
            if let Some(desktop_lnk) = &r.desktop
                && is_safe_user_lnk(desktop_lnk)
                && desktop_lnk.is_file()
            {
                let _ = std::fs::remove_file(desktop_lnk);
            }
            if let Some(start_menu_lnk) = &r.start_menu
                && is_safe_user_lnk(start_menu_lnk)
                && start_menu_lnk.is_file()
            {
                let _ = std::fs::remove_file(start_menu_lnk);
                if let Some(parent) = start_menu_lnk.parent()
                    && parent.file_name().is_some_and(|n| n.eq_ignore_ascii_case(app.name))
                {
                    let _ = std::fs::remove_dir(parent);
                }
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        let desktop_dir = dirs::desktop_dir();
        let data_dir = crate::install::linux::host_data_dir();
        let _ = remove_linux_shortcuts_in(desktop_dir.as_deref(), data_dir.as_deref(), app);
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(r) = &record {
            if let Some(desktop_link) = &r.desktop {
                if desktop_link.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) {
                    let _ = std::fs::remove_file(desktop_link);
                }
            }
            if let Some(app_link) = &r.start_menu {
                if app_link.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) {
                    let _ = std::fs::remove_file(app_link);
                }
            }
        }
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = (app, &record);
    }

    if let Some(m_path) = manifest_path {
        let _ = manifest.save(m_path);
    }
}

// ------------------------------------------------------------------ Windows implementation

/// Verifies whether `path` is safely located within the current user's profile folders
/// (Desktop or user Start Menu Programs) and is not inside system-wide directories such as `ProgramData`.
#[cfg(windows)]
pub fn is_safe_user_lnk(path: &Path) -> bool {
    let program_data = std::env::var_os("ProgramData").map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from);
    if path.starts_with(&program_data) {
        return false;
    }
    let is_desktop = dirs::desktop_dir().is_some_and(|d| path.starts_with(d));
    let is_user_start = dirs::data_dir().is_some_and(|d| path.starts_with(d.join(r"Microsoft\Windows\Start Menu\Programs")));
    (is_desktop || is_user_start) && path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
}

#[cfg(windows)]
fn windows_desktop_lnk(app: &CraftApp) -> Option<PathBuf> {
    dirs::desktop_dir().map(|d| d.join(format!("{}.lnk", app.name)))
}

#[cfg(windows)]
fn windows_user_start_menu_lnks(app: &CraftApp) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(data) = dirs::data_dir() {
        let programs = data.join(r"Microsoft\Windows\Start Menu\Programs");
        out.push(programs.join(format!("{}.lnk", app.name)));
        out.push(programs.join(app.name).join(format!("{}.lnk", app.name)));
    }
    out
}

#[cfg(windows)]
fn windows_start_menu_lnks(app: &CraftApp) -> Vec<PathBuf> {
    let mut out = windows_user_start_menu_lnks(app);
    let program_data = std::env::var_os("ProgramData").map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from);
    let common_programs = program_data.join(r"Microsoft\Windows\Start Menu\Programs");
    out.push(common_programs.join(format!("{}.lnk", app.name)));
    out.push(common_programs.join(app.name).join(format!("{}.lnk", app.name)));
    out
}

#[cfg(windows)]
fn sync_windows_shortcuts(app: &CraftApp, target: &Path, desktop: bool, start_menu: bool, record: &mut ShortcutRecord) -> Result<(), String> {
    if desktop {
        if let Some(desktop_lnk) = windows_desktop_lnk(app) {
            create_windows_lnk(&desktop_lnk, target, app.tagline)?;
            record.desktop = Some(desktop_lnk);
        }
    } else if let Some(desktop_lnk) = record.desktop.take()
        && is_safe_user_lnk(&desktop_lnk)
        && desktop_lnk.is_file()
    {
        std::fs::remove_file(&desktop_lnk).map_err(|e| format!("couldn't remove desktop shortcut ({e})"))?;
    }

    if start_menu {
        let user_lnks = windows_user_start_menu_lnks(app);
        if let Some(primary) = user_lnks.first() {
            create_windows_lnk(primary, target, app.tagline)?;
            record.start_menu = Some(primary.clone());
        }
    } else if let Some(start_lnk) = record.start_menu.take()
        && is_safe_user_lnk(&start_lnk)
        && start_lnk.is_file()
    {
        std::fs::remove_file(&start_lnk).map_err(|e| format!("couldn't remove Start menu shortcut ({e})"))?;
        if let Some(parent) = start_lnk.parent()
            && parent.file_name().is_some_and(|n| n.eq_ignore_ascii_case(app.name))
        {
            let _ = std::fs::remove_dir(parent);
        }
    }

    Ok(())
}

#[cfg(windows)]
fn create_windows_lnk(lnk_path: &Path, target: &Path, description: &str) -> Result<(), String> {
    if !is_safe_user_lnk(lnk_path) {
        return Err(format!("shortcut destination {} is outside safe user profile directories", lnk_path.display()));
    }
    if let Some(parent) = lnk_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("couldn't create shortcut folder {} ({e})", parent.display()))?;
    }
    let ps_quote = |s: &str| format!("'{}'", s.replace('\'', "''"));
    let lnk_s = lnk_path.to_str().ok_or("shortcut path is not valid UTF-8")?;
    let target_s = target.to_str().ok_or("target path is not valid UTF-8")?;
    let work_dir = target.parent().and_then(Path::to_str).unwrap_or("");
    let script = format!(
        "$ws = New-Object -ComObject WScript.Shell;          $s = $ws.CreateShortcut({});          $s.TargetPath = {};          $s.WorkingDirectory = {};          $s.Description = {};          $s.IconLocation = {};          $s.Save()",
        ps_quote(lnk_s),
        ps_quote(target_s),
        ps_quote(work_dir),
        ps_quote(description),
        ps_quote(&format!("{target_s},0")),
    );
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let powershell = std::path::Path::new(&root).join(r"System32\WindowsPowerShell1.0\powershell.exe");
    let mut cmd = crate::process::command(&powershell);
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &script]);
    crate::process::run_command(cmd, "powershell", std::time::Duration::from_secs(15)).map(|_| ())
}

// ------------------------------------------------------------------ Linux / XDG implementation

#[cfg(target_os = "linux")]
fn linux_menu_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(data) = crate::install::linux::host_data_dir() {
        dirs.push(data.join("applications"));
    }
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local/share/flatpak/exports/share/applications"));
    }
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
    dirs.push(PathBuf::from("/usr/local/share/applications"));
    dirs.push(PathBuf::from("/usr/share/applications"));
    dirs
}

/// Synchronizes `.desktop` shortcuts inside `desktop_dir` and `data_dir` (testable on all OSes).
pub fn sync_linux_shortcuts_in(
    desktop_dir: Option<&Path>,
    data_dir: Option<&Path>,
    app: &CraftApp,
    target: &Path,
    desktop: bool,
    start_menu: bool,
    record: &mut ShortcutRecord,
) -> Result<(), String> {
    let bundle_desktop = format!("{}.desktop", app.bundle_id());
    let has_icon = data_dir.is_some_and(|d| d.join("icons/hicolor/128x128/apps").join(format!("{}.png", app.bundle_id())).is_file());
    let rendered =
        crate::desktop_entry::render(app, target, has_icon).ok_or_else(|| format!("{} cannot be written into a desktop entry", target.display()))?;

    if let Some(d_dir) = desktop_dir {
        let path = d_dir.join(&bundle_desktop);
        if desktop {
            write_atomic(&path, rendered.as_bytes())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
            }
            record.desktop = Some(path);
        } else {
            let to_remove = record.desktop.take().or_else(|| if path.is_file() { Some(path) } else { None });
            if let Some(p) = to_remove
                && let Ok(text) = std::fs::read_to_string(&p)
                && crate::desktop_entry::is_ours(&text)
            {
                let _ = std::fs::remove_file(&p);
            }
        }
    }

    if let Some(a_dir) = data_dir {
        let apps_dir = a_dir.join("applications");
        let path = apps_dir.join(&bundle_desktop);
        if start_menu {
            write_atomic(&path, rendered.as_bytes())?;
            record.start_menu = Some(path);
        } else {
            let to_remove = record.start_menu.take().or_else(|| if path.is_file() { Some(path) } else { None });
            if let Some(p) = to_remove
                && let Ok(text) = std::fs::read_to_string(&p)
                && crate::desktop_entry::is_ours(&text)
            {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
    Ok(())
}

/// Removes launcher-written `.desktop` shortcuts from `desktop_dir` and `data_dir`.
pub fn remove_linux_shortcuts_in(desktop_dir: Option<&Path>, data_dir: Option<&Path>, app: &CraftApp) -> Result<(), String> {
    let bundle_desktop = format!("{}.desktop", app.bundle_id());
    for dir in [desktop_dir, data_dir.map(|d| d.join("applications")).as_deref().map(Path::new)].into_iter().flatten() {
        let path = dir.join(&bundle_desktop);
        if let Ok(text) = std::fs::read_to_string(&path)
            && crate::desktop_entry::is_ours(&text)
        {
            let _ = std::fs::remove_file(&path);
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ macOS implementation

#[cfg(target_os = "macos")]
fn sync_macos_shortcuts(app: &CraftApp, target: &Path, desktop: bool, start_menu: bool, record: &mut ShortcutRecord) -> Result<(), String> {
    let bundle = format!("{}.app", app.name);
    if let Some(desktop_dir) = dirs::desktop_dir() {
        let link = desktop_dir.join(&bundle);
        if desktop {
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(target, &link).map_err(|e| format!("couldn't create desktop alias ({e})"))?;
            record.desktop = Some(link);
        } else if let Some(link) = record.desktop.take() {
            if link.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) {
                let _ = std::fs::remove_file(&link);
            }
        }
    }
    if let Some(home) = dirs::home_dir() {
        let user_apps = home.join("Applications");
        let link = user_apps.join(&bundle);
        if start_menu {
            if target != Path::new("/Applications").join(&bundle) && target != link {
                let _ = std::fs::create_dir_all(&user_apps);
                let _ = std::fs::remove_file(&link);
                let _ = std::os::unix::fs::symlink(target, &link);
                record.start_menu = Some(link);
            }
        } else if let Some(link) = record.start_menu.take() {
            if link.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) {
                let _ = std::fs::remove_file(&link);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use artcraft_launcher_core::protocol::AppShortcutPrefs;

    use super::*;

    #[test]
    fn resolves_global_defaults_and_per_app_overrides() {
        let mut options = BackendOptions { create_desktop_shortcut: false, create_start_menu_shortcut: true, ..BackendOptions::default() };
        assert_eq!(resolve_prefs("photocraft", &options), (false, true));

        options.app_shortcuts.insert("photocraft".to_string(), AppShortcutPrefs { desktop: Some(true), start_menu: Some(false) });
        assert_eq!(resolve_prefs("photocraft", &options), (true, false));
        assert_eq!(resolve_prefs("vectorcraft", &options), (false, true));
    }

    #[test]
    fn shortcut_manifest_roundtrips_and_survives_corrupted_json() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("shortcuts.json");
        assert_eq!(ShortcutManifest::load(&path), ShortcutManifest::default());

        let mut manifest = ShortcutManifest::default();
        manifest.apps.insert(
            "photocraft".into(),
            ShortcutRecord {
                desktop: Some(PathBuf::from("/home/user/Desktop/ai.storyteller.photocraft.desktop")),
                start_menu: Some(PathBuf::from("/home/user/.local/share/applications/ai.storyteller.photocraft.desktop")),
            },
        );
        manifest.save(&path).unwrap();
        assert_eq!(ShortcutManifest::load(&path), manifest);

        std::fs::write(&path, b"invalid json content").unwrap();
        assert_eq!(ShortcutManifest::load(&path), ShortcutManifest::default());
    }

    #[test]
    fn creates_updates_and_removes_desktop_and_menu_entries_across_folder_changes() {
        let tmp = std::env::temp_dir().join(format!("artcraft-launcher-shortcuts-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let desktop_dir = tmp.join("Desktop");
        let data_dir = tmp.join("data");
        let app = CraftApp::by_id("photocraft").unwrap();
        let mut record = ShortcutRecord::default();

        let first_bin = Path::new("/opt/ArtCraft/photocraft/photocraft.AppImage");
        sync_linux_shortcuts_in(Some(&desktop_dir), Some(&data_dir), app, first_bin, true, true, &mut record).unwrap();

        let desktop_file = desktop_dir.join("ai.storyteller.photocraft.desktop");
        let menu_file = data_dir.join("applications/ai.storyteller.photocraft.desktop");
        assert!(desktop_file.is_file());
        assert!(menu_file.is_file());
        assert_eq!(record.desktop.as_deref(), Some(desktop_file.as_path()));
        assert_eq!(record.start_menu.as_deref(), Some(menu_file.as_path()));
        assert_eq!(
            crate::desktop_entry::exec_program(&std::fs::read_to_string(&desktop_file).unwrap()).as_deref(),
            Some("/opt/ArtCraft/photocraft/photocraft.AppImage")
        );

        // Move app to a new installation directory and disable desktop shortcut while keeping menu shortcut.
        let moved_bin = Path::new("/mnt/work/CraftSuite/photocraft/photocraft.AppImage");
        sync_linux_shortcuts_in(Some(&desktop_dir), Some(&data_dir), app, moved_bin, false, true, &mut record).unwrap();
        assert!(!desktop_file.exists(), "disabled desktop shortcut should be removed");
        assert!(record.desktop.is_none());
        assert!(menu_file.is_file(), "enabled menu shortcut should stay");
        assert_eq!(record.start_menu.as_deref(), Some(menu_file.as_path()));
        assert_eq!(
            crate::desktop_entry::exec_program(&std::fs::read_to_string(&menu_file).unwrap()).as_deref(),
            Some("/mnt/work/CraftSuite/photocraft/photocraft.AppImage"),
            "menu shortcut should point to the updated installation location"
        );

        remove_linux_shortcuts_in(Some(&desktop_dir), Some(&data_dir), app).unwrap();
        assert!(!menu_file.exists(), "uninstall cleanup removes launcher-written menu shortcut");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(windows)]
    #[test]
    fn windows_safe_user_lnk_rejects_programdata_and_system_paths() {
        let program_data = std::env::var_os("ProgramData").map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from);
        let pd_lnk = program_data.join(r"Microsoft\Windows\Start Menu\Programs\PhotoCraft.lnk");
        assert!(!is_safe_user_lnk(&pd_lnk), "ProgramData shortcuts must never be considered user shortcuts");

        let sys_lnk = PathBuf::from(r"C:\Windows\System32\cmd.lnk");
        assert!(!is_safe_user_lnk(&sys_lnk), "System shortcuts must never be considered user shortcuts");

        if let Some(desktop) = dirs::desktop_dir() {
            assert!(is_safe_user_lnk(&desktop.join("PhotoCraft.lnk")));
            assert!(!is_safe_user_lnk(&desktop.join("PhotoCraft.exe")));
        }
    }
}
