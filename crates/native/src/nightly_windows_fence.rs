//! The Windows stable-data fence: the folders an app's Nightly data and cache must stay clear of.
//!
//! The Nightly copies live in their own versioned namespace, but a Nightly folder that overlaps
//! the stable app's data, settings or installation would let one channel read or replace the
//! other. Every protected folder is resolved through links first, and a folder that does not
//! exist yet is checked by where it would be created.
//!
//! Compiled in tests on every platform, so Linux CI keeps this list and its comparison honest;
//! only Windows reads the real `%APPDATA%` and `%LOCALAPPDATA%`.

use std::path::{Component, Path, PathBuf};

use crate::nightly_profile::Adapter;
use crate::paths::Paths;

/// The per-user programs folder a stable portable install defaults to, under `%LOCALAPPDATA%`.
const PORTABLE_FOLDER: [&str; 2] = ["Programs", "ArtCraft"];

/// The folders the stable app's copy and data use on Windows, by every name it is stored under:
/// its display name (`PhotoCraft`) and its catalog id (`photocraft`). The local app data folder
/// holds both those names and the portable program default.
pub(crate) fn windows_stable_paths(adapter: Adapter, appdata: &Path, local_appdata: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for base in [appdata, local_appdata] {
        for name in [adapter.display(), adapter.app()] {
            paths.push(base.join(name));
        }
    }
    paths.push(PORTABLE_FOLDER.iter().fold(local_appdata.to_path_buf(), |path, part| path.join(part)).join(adapter.display()));
    paths.sort();
    paths.dedup();
    paths
}

/// Resolve without the verbatim prefix that MSI short paths cannot carry, as the portable
/// installer already does; other platforms resolve with the standard call, so this fence runs
/// unchanged in their tests.
#[cfg(windows)]
fn canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    dunce::canonicalize(path)
}

#[cfg(not(windows))]
fn canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(path)
}

/// Resolve a folder that need not exist yet: its nearest existing ancestor is resolved, so a
/// link to a stable folder is seen even when the leaf is still to be created.
fn resolved_missing(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() || path.components().any(|part| matches!(part, Component::CurDir | Component::ParentDir)) {
        return Err("A protected storage path is not absolute and unambiguous.".into());
    }
    for ancestor in path.ancestors() {
        match canonicalize(ancestor) {
            Ok(resolved) => return Ok(resolved.join(path.strip_prefix(ancestor).map_err(|error| error.to_string())?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if std::fs::symlink_metadata(ancestor).is_ok() {
                    return Err("A protected storage path contains a dangling alias.".into());
                }
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("A protected storage path has no existing ancestor.".into())
}

/// Windows compares these folder names without regard to ASCII case, so the fence must too.
/// Only the case fold is added: a name that is not valid UTF-8 folds through the lossy form,
/// which can make the fence refuse more, never less.
fn comparable(path: &Path, fold_case: bool) -> PathBuf {
    if !fold_case {
        return path.to_path_buf();
    }
    let mut folded = PathBuf::new();
    for part in path.components() {
        folded.push(part.as_os_str().to_string_lossy().to_lowercase());
    }
    folded
}

/// Neither folder may contain the other.
fn overlaps(nightly: &Path, stable: &Path, fold_case: bool) -> bool {
    let nightly = comparable(nightly, fold_case);
    let stable = comparable(stable, fold_case);
    nightly.starts_with(&stable) || stable.starts_with(&nightly)
}

/// `fold_case` is the platform's own comparison: Windows ignores case, and its tests ask for
/// that comparison on every platform.
fn disjoint(nightly: &[PathBuf], protected: &[PathBuf], fold_case: bool) -> Result<(), String> {
    let protected = protected.iter().map(|path| resolved_missing(path)).collect::<Result<Vec<_>, _>>()?;
    for path in nightly {
        let path = resolved_missing(path)?;
        if protected.iter().any(|stable| overlaps(&path, stable, fold_case)) {
            return Err("The nightly namespace overlaps the stable app's data or installation; nothing was changed.".into());
        }
    }
    Ok(())
}

/// The stable Launcher's own program folder for this app, and the path its ownership record
/// names if it has one: a custom stable installation is fenced like the defaults.
fn stable_installation(paths: &Paths, app: &str, protected: &mut Vec<PathBuf>) -> Result<(), String> {
    let stable_data = paths.data.parent().and_then(Path::parent).ok_or("The nightly data folder has no stable namespace owner.")?;
    protected.push(stable_data.join("apps").join(app));
    let record = stable_data.join("installed.json");
    // A link, shared hardlink or non-file record is refused rather than followed or adopted.
    crate::nightly_profile::validate_file(&record)?;
    // Read at most one byte past the limit, as Unix does: a huge record is refused before its
    // contents are allocated, never after.
    let mut bytes = Vec::new();
    match std::fs::File::open(&record) {
        Ok(file) => {
            use std::io::Read;
            file.take(1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    }
    if bytes.len() > 1024 * 1024 {
        return Err("The stable ownership record is too large to check safely.".into());
    }
    let manifest: crate::manifest::Manifest = serde_json::from_slice(&bytes).map_err(|_| "The stable ownership record cannot be checked safely.")?;
    if let Some(record) = manifest.apps.get(app) {
        protected.push(record.path.clone());
    }
    Ok(())
}

/// The Windows Nightly data and cache must not overlap any stable folder for this app.
/// A missing `%APPDATA%` or `%LOCALAPPDATA%` refuses: without both there is no fence to check.
#[cfg(any(windows, test))]
// Only Windows has a production caller; the tests check the environment rule through the seam.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn validate_windows_namespaces(adapter: Adapter, paths: &Paths) -> Result<(), String> {
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
    let local_appdata = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    validate_with(adapter, paths, appdata, local_appdata)
}

fn validate_with(adapter: Adapter, paths: &Paths, appdata: Option<PathBuf>, local_appdata: Option<PathBuf>) -> Result<(), String> {
    let appdata = appdata.ok_or("The stable app's roaming data folder is unknown; nightly isolation cannot be established.")?;
    let local_appdata = local_appdata.ok_or("The stable copy's local data folder is unknown; nightly isolation cannot be established.")?;
    let mut protected = windows_stable_paths(adapter, &appdata, &local_appdata);
    stable_installation(paths, adapter.app(), &mut protected)?;
    disjoint(&[paths.data.clone(), paths.cache.clone()], &protected, cfg!(windows))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(app: &str) -> Adapter {
        let app = artcraft_launcher_core::CraftApp::by_id(app).unwrap();
        let platform = artcraft_launcher_core::Platform { os: artcraft_launcher_core::Os::Windows, arch: artcraft_launcher_core::Arch::X64 };
        crate::nightly_profile::for_app(app, platform).unwrap()
    }

    fn nightly_paths(root: &Path) -> Paths {
        Paths::under(root).for_channel(artcraft_launcher_core::nightly::InstallChannel::Nightly)
    }

    /// The stable folders by both names, in both roots, and the portable default under local data.
    #[test]
    fn windows_stable_paths_name_every_folder_the_stable_app_uses() {
        let appdata = Path::new("roaming");
        let local = Path::new("local");
        let photo = windows_stable_paths(adapter("photocraft"), appdata, local);
        for expected in [
            appdata.join("PhotoCraft"),
            appdata.join("photocraft"),
            local.join("PhotoCraft"),
            local.join("photocraft"),
            local.join("Programs").join("ArtCraft").join("PhotoCraft"),
        ] {
            assert!(photo.contains(&expected), "{}", expected.display());
        }
        assert_eq!(photo.len(), 5);
        // Another app is fenced by its own names, never PhotoCraft's.
        let cad = windows_stable_paths(adapter("cadcraft"), appdata, local);
        for expected in
            [appdata.join("CADCraft"), appdata.join("cadcraft"), local.join("CADCraft"), local.join("Programs").join("ArtCraft").join("CADCraft")]
        {
            assert!(cad.contains(&expected), "{}", expected.display());
        }
        assert!(!cad.contains(&appdata.join("PhotoCraft")) && !cad.contains(&local.join("PhotoCraft")));
    }

    /// A Nightly folder inside, above or equal to a stable folder is refused; one beside them
    /// passes. Windows compares without regard to case, so a differently-cased name overlaps.
    #[test]
    fn a_nightly_folder_that_overlaps_a_stable_folder_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let (appdata, local) = (root.join("Roaming"), root.join("Local"));
        std::fs::create_dir_all(&appdata).unwrap();
        std::fs::create_dir_all(&local).unwrap();
        let protected = windows_stable_paths(adapter("cadcraft"), &appdata, &local);
        for nightly in [
            appdata.join("cadcraft").join("pre-release"),
            appdata.join("cadcraft"),
            appdata.join("CADCraft").join("pre-release"),
            local.join("Programs").join("ArtCraft").join("CADCraft").join("program"),
            appdata.clone(),
        ] {
            assert!(disjoint(std::slice::from_ref(&nightly), &protected, true).is_err(), "{}", nightly.display());
        }
        for nightly in
            [appdata.join("CADCraft Nightly"), local.join("ArtCraft").join("cadcraft-nightly"), local.join("Programs").join("ArtCraft Nightly")]
        {
            assert!(disjoint(std::slice::from_ref(&nightly), &protected, true).is_ok(), "{}", nightly.display());
        }
        // The exact spelling refuses without any case fold too: the fence never depends on it.
        assert!(disjoint(&[appdata.join("cadcraft").join("pre-release")], &protected, false).is_err());
        assert!(disjoint(&[appdata.join("cadcraft-nightly")], &protected, false).is_ok());
    }

    /// Missing either stable root refuses rather than checking half a fence.
    #[test]
    fn a_missing_stable_root_refuses_the_windows_fence() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let paths = nightly_paths(&root_path);
        let (appdata, local) = (root_path.join("Roaming"), root_path.join("Local"));
        let photo = adapter("photocraft");
        assert!(validate_with(photo, &paths, None, Some(local.clone())).is_err());
        assert!(validate_with(photo, &paths, Some(appdata.clone()), None).is_err());
        assert!(validate_with(photo, &paths, None, None).is_err());
        // A Nightly namespace beside the stable folders is fine where they exist.
        assert!(validate_with(photo, &paths, Some(appdata), Some(local)).is_ok());
    }

    /// A custom stable installation recorded by the Launcher is fenced too, and a record that
    /// cannot be read safely refuses the whole check.
    #[test]
    fn the_stable_installation_record_is_fenced_and_checked() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let stable = Paths::under(&root_path);
        let paths = nightly_paths(&root_path);
        let (appdata, local) = (root_path.join("Roaming"), root_path.join("Local"));
        // A chosen folder, not the launcher's own `apps/` default: only the record names it.
        let install = root_path.join("Chosen").join("photocraft");
        std::fs::create_dir_all(&install).unwrap();
        let mut manifest = crate::manifest::Manifest::default();
        manifest.apps.insert(
            "photocraft".into(),
            crate::manifest::Record {
                version: "0.5.0".into(),
                kind: artcraft_launcher_core::PackageKind::PortableZip,
                path: install.join("photocraft.exe"),
                installed_at: 1,
                files: vec![install.join("photocraft.exe")],
                needs_root: false,
            },
        );
        manifest.save(&stable.manifest()).unwrap();
        let photo = adapter("photocraft");
        assert!(validate_with(photo, &paths, Some(appdata.clone()), Some(local.clone())).is_ok());
        // The record fences the program file it names: a Nightly folder that contains it would
        // let one channel write over the other's copy, so it is refused.
        let mut containing = paths.clone();
        containing.cache = install.clone();
        assert!(validate_with(photo, &containing, Some(appdata.clone()), Some(local.clone())).is_err());
        // A Nightly folder inside the Launcher's own stable program folder is refused too.
        let mut inside_apps = paths.clone();
        inside_apps.cache = stable.data.join("apps").join("photocraft").join("pre-release").join("v1");
        assert!(validate_with(photo, &inside_apps, Some(appdata.clone()), Some(local.clone())).is_err());
        // One beside the recorded copy is fine.
        let mut beside = paths.clone();
        beside.cache = root_path.join("Side by side").join("photocraft");
        assert!(validate_with(photo, &beside, Some(appdata.clone()), Some(local.clone())).is_ok());
        // A record that is not readable JSON refuses rather than passing unnoticed.
        std::fs::write(stable.manifest(), b"{").unwrap();
        assert!(validate_with(photo, &paths, Some(appdata), Some(local)).is_err());
    }
}
