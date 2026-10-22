//! How much disk space an installed copy takes.
//!
//! Only copies whose recorded location is a path on disk can be measured: a launcher install's
//! folder (a Windows launcher install records its exe, and the folder around it is the install),
//! an MSI install's folder (the folder its exe sits in), a macOS `.app` bundle, an
//! AppImage file, and a Flatpak app's deploy folder (asked of `flatpak info`). Packages a system
//! package manager owns and bare system binaries report nothing: the launcher doesn't own them,
//! and one binary says nothing about the app's real footprint.

use std::path::{Path, PathBuf};
use std::time::Duration;

use artcraft_launcher_core::{InstallSource, Installed};

/// The folder or file whose size says how much space `installed` takes; `None` when there is
/// nothing whose size would be honest.
pub fn target(installed: &Installed) -> Option<PathBuf> {
    match installed.source {
        // The location is the app's bundle id; where it really lives is Flatpak's own answer.
        InstallSource::Flatpak => {
            let id = installed.location.trim();
            (!id.is_empty()).then_some(id).and_then(flatpak_folder)
        }
        InstallSource::Deb | InstallSource::Rpm | InstallSource::Pacman | InstallSource::Binary => None,
        // An MSI records an exe, not its folder; the folder is where the exe is.
        InstallSource::Msi => {
            let exe = Path::new(&installed.location);
            if exe.is_file() { folder_of(exe) } else { None }
        }
        // Everything else records the path itself: a folder, a bundle, or a single file.
        _ => {
            let path = Path::new(&installed.location);
            if path.is_dir() {
                return Some(path.to_path_buf());
            }
            if !path.is_file() {
                return None;
            }
            // A Windows launcher install records its exe, not its folder: the install is the
            // folder the exe sits in, with the rest of the app's files beside it. Elsewhere a
            // recorded file is the app itself (an AppImage), and is measured as it is.
            if cfg!(windows) { folder_of(path) } else { Some(path.to_path_buf()) }
        }
    }
}

/// The folder a file sits in. A drive root is never an app's folder: measuring it would count
/// the whole disk.
fn folder_of(file: &Path) -> Option<PathBuf> {
    file.parent().filter(|folder| folder.file_name().is_some()).map(Path::to_path_buf)
}

/// Where Flatpak keeps an app's files; `None` when Flatpak doesn't know the app (or isn't here).
fn flatpak_folder(id: &str) -> Option<PathBuf> {
    let shown = crate::process::capture_flatpak(&["info", "--show-location", id], Duration::from_secs(5))?;
    let path = PathBuf::from(shown.trim());
    path.is_dir().then_some(path)
}

/// The size of a file, or of every file under a folder. Symlinks are counted as the link itself
/// and never followed. Anything unreadable is skipped: the answer is an estimate, never an error.
pub fn path_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return 0 };
    if !meta.is_dir() {
        return meta.len();
    }
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            match entry.metadata() {
                // `DirEntry::metadata` doesn't traverse symlinks, so this can't loop.
                Ok(m) if m.is_dir() => stack.push(entry.path()),
                Ok(m) => total = total.saturating_add(m.len()),
                Err(_) => {}
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launcher(location: &Path) -> Installed {
        Installed { version: None, source: InstallSource::Launcher, location: location.display().to_string(), package: None }
    }

    #[test]
    fn a_launcher_folder_is_measured_where_it_is() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(target(&launcher(dir.path())).as_deref(), Some(dir.path()));
    }

    #[test]
    fn a_location_that_is_not_on_disk_is_not_measured() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("photocraft");
        assert_eq!(target(&launcher(&missing)), None);
    }

    #[cfg(windows)]
    #[test]
    fn a_windows_launcher_exe_is_measured_by_the_folder_around_it() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("photocraft.exe");
        std::fs::write(&exe, b"program").unwrap();
        std::fs::write(dir.path().join("runtime.dll"), b"library").unwrap();
        assert_eq!(target(&launcher(&exe)).as_deref(), Some(dir.path()));
        // The whole install is measured, not just the exe.
        assert_eq!(path_size(&target(&launcher(&exe)).unwrap()), 14);
    }

    #[cfg(windows)]
    #[test]
    fn a_drive_root_is_never_an_apps_folder() {
        assert_eq!(folder_of(Path::new(r"D:\photocraft.exe")), None);
        assert_eq!(folder_of(Path::new(r"D:\Apps\PhotoCraft\photocraft.exe")), Some(PathBuf::from(r"D:\Apps\PhotoCraft")));
    }
}
