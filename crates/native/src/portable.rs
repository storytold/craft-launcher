//! Windows portable mode: apps unpacked from their release's portable zip into
//! `<launcher folder>\apps\<app>`, instead of installed from the MSI.
//!
//! Each copy carries [`FILE`], written by the launcher, which says what was installed: the version
//! shown and compared against the latest release comes from there. An update unpacks the new
//! copy beside the old one, swaps the folders, and moves across anything of the old copy the new
//! one doesn't ship (settings or data a portable app keeps beside itself).
//!
//! The file system work is plain `std::fs`, so the tests run on every OS.

use std::io::Read;
use std::path::{Path, PathBuf};

use artcraft_launcher_core::{CraftApp, InstallSource, Installed, version};
use serde::{Deserialize, Serialize};

/// The launcher's record inside each portable copy.
pub const FILE: &str = "artcraft-launcher-install.json";

/// [`FILE`] is a few hundred bytes; anything much bigger isn't ours.
const MAX_FILE: u64 = 4 * 1024;

/// What the launcher unpacked into a portable copy's folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallFile {
    /// The app's id (`photocraft`); a file naming another app is ignored.
    pub app: String,
    /// `0.6.0`, `0.6.0-rc.1`
    pub version: String,
    /// The release tag (`v0.6.0`).
    pub tag: String,
    /// The build's architecture, as its file name gives it (`x64`, `x86`, `arm64`).
    pub arch: String,
    /// The zip it came from.
    pub asset: String,
    /// Unix seconds.
    pub installed_at: i64,
}

/// Writes [`FILE`] into `dir`.
pub fn write(dir: &Path, file: &InstallFile) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(file).map_err(|e| e.to_string())?;
    crate::paths::write_atomic(&dir.join(FILE), &bytes)
}

/// Reads [`FILE`] from `dir`; `None` when it's missing, unreadable, too big or about another app.
pub fn read(dir: &Path, app: &str) -> Option<InstallFile> {
    let f = std::fs::File::open(dir.join(FILE)).ok()?;
    let mut bytes = Vec::new();
    f.take(MAX_FILE + 1).read_to_end(&mut bytes).ok()?;
    if u64::try_from(bytes.len()).ok()? > MAX_FILE {
        return None;
    }
    let file: InstallFile = serde_json::from_slice(&bytes).ok()?;
    (file.app == app).then_some(file)
}

/// The app's exe in a portable copy: `<apps>\<id>\<id>.exe`.
pub fn exe_path(apps: &Path, app: &CraftApp) -> PathBuf {
    apps.join(app.id).join(exe_name(app))
}

fn exe_name(app: &CraftApp) -> String {
    format!("{}.exe", app.id)
}

/// The portable copy of `app` in `apps`, if its exe is there. Its version is the one [`FILE`]
/// records; without a readable file the copy still shows, with an unknown version.
pub fn find(apps: &Path, app: &CraftApp) -> Option<Installed> {
    let exe = exe_path(apps, app);
    if !exe.is_file() {
        return None;
    }
    let version = exe.parent().and_then(|dir| read(dir, app.id)).and_then(|f| version::parse(&f.version));
    Some(Installed { version, source: InstallSource::Launcher, location: exe.display().to_string(), package: None })
}

/// The folder of the portable copy whose exe is `exe`, when that is exactly `<apps>\<id>\<id>.exe`:
/// the only folder the launcher ever deletes in portable mode.
pub fn owned_folder(apps: &Path, app: &CraftApp, exe: &Path) -> Option<PathBuf> {
    if exe.file_name()?.to_str()? != exe_name(app) {
        return None;
    }
    let folder = exe.parent()?;
    let expected = apps.join(app.id);
    // Some volumes (RAM disks, some network shares) can't resolve paths: compare them as given.
    let (folder, expected) = match (folder.canonicalize(), expected.canonicalize()) {
        (Ok(f), Ok(e)) => (f, e),
        _ => (std::path::absolute(folder).ok()?, std::path::absolute(&expected).ok()?),
    };
    (folder == expected && folder.is_dir()).then_some(folder)
}

/// `.<name>.<what>` beside `dest`.
pub fn sibling(dest: &Path, what: &str) -> Option<PathBuf> {
    let name = dest.file_name()?.to_str()?;
    Some(dest.with_file_name(format!(".{name}.{what}")))
}

/// Puts the unpacked copy `staged` in place at `dest`. The old copy is moved aside first and
/// comes back if the new one can't go in; once it has, whatever the old copy had that the new
/// one doesn't (the user's settings and data) moves across before the rest of the old copy goes.
pub fn replace_folder(staged: &Path, dest: &Path) -> Result<(), String> {
    let old = sibling(dest, "old").ok_or("unusual characters in the app's folder name")?;
    settle(&old, dest)?;
    let had_old = dest.exists();
    if had_old {
        std::fs::rename(dest, &old).map_err(|e| format!("couldn't move the old copy aside ({e}). Is the app still running?"))?;
    }
    if let Err(e) = std::fs::rename(staged, dest) {
        if had_old {
            let _ = std::fs::rename(&old, dest);
        }
        return Err(format!("couldn't put the new copy in place ({e})"));
    }
    if had_old {
        settle(&old, dest)?;
    }
    Ok(())
}

/// Moves what's left of an old copy `old` into `dest` (only entries `dest` doesn't have), then
/// deletes `old`. A copy whose entries couldn't all be moved stays, so nothing of the user's is
/// lost; the error says where it is.
fn settle(old: &Path, dest: &Path) -> Result<(), String> {
    if !old.exists() {
        return Ok(());
    }
    if dest.is_dir() && !carry_over(old, dest) {
        return Err(format!("couldn't move everything from the old copy into the new one. What's left is in {}", old.display()));
    }
    let _ = std::fs::remove_dir_all(old);
    Ok(())
}

/// Moves each top-level entry of `from` that `to` doesn't have into `to`. Whether all of them
/// moved.
fn carry_over(from: &Path, to: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(from) else { return false };
    let mut all = true;
    for entry in entries.flatten() {
        let target = to.join(entry.file_name());
        if target.symlink_metadata().is_ok() {
            continue;
        }
        if let Err(e) = std::fs::rename(entry.path(), &target) {
            log::warn!("couldn't keep {} from the old copy ({e})", entry.path().display());
            all = false;
        }
    }
    all
}

/// The most files a portable zip may hold.
#[cfg(any(windows, test))]
const MAX_ENTRIES: usize = 50_000;

/// The most a portable zip may unpack to.
#[cfg(any(windows, test))]
const MAX_UNPACKED: u64 = 8 * 1024 * 1024 * 1024;

/// Unpacks the zip `file` into `dest`. Entries that would land outside `dest`, links, and zips
/// with too many files or too much data are refused.
#[cfg(any(windows, test))]
pub fn unzip(file: &Path, dest: &Path) -> Result<(), String> {
    unzip_capped(file, dest, MAX_ENTRIES, MAX_UNPACKED)
}

#[cfg(any(windows, test))]
fn unzip_capped(file: &Path, dest: &Path, max_entries: usize, max_unpacked: u64) -> Result<(), String> {
    let f = std::fs::File::open(file).map_err(|e| format!("couldn't open the download ({e})"))?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(f)).map_err(|e| format!("the download isn't a readable zip ({e})"))?;
    if archive.len() > max_entries {
        return Err("the download holds too many files, so it wasn't unpacked".into());
    }
    let too_big = || format!("the download unpacks to more than {} MB, so it wasn't unpacked", max_unpacked / (1024 * 1024));
    let mut total: u64 = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("couldn't read the download ({e})"))?;
        let Some(rel) = entry.enclosed_name() else {
            return Err("the download holds a file that would land outside its folder, so it wasn't unpacked".into());
        };
        if entry.is_symlink() {
            return Err("the download holds a link, so it wasn't unpacked".into());
        }
        let path = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&path).map_err(|e| format!("couldn't create {} ({e})", path.display()))?;
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("couldn't create {} ({e})", parent.display()))?;
        }
        // The sizes in the zip's headers may lie: count what actually comes out.
        let left = max_unpacked.checked_sub(total).ok_or_else(too_big)?;
        let mut out = std::fs::File::create(&path).map_err(|e| format!("couldn't write {} ({e})", path.display()))?;
        let copied =
            std::io::copy(&mut (&mut entry).take(left.saturating_add(1)), &mut out).map_err(|e| format!("couldn't unpack the download ({e})"))?;
        total = total.checked_add(copied).filter(|t| *t <= max_unpacked).ok_or_else(too_big)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn photo() -> &'static CraftApp {
        CraftApp::by_id("photocraft").unwrap()
    }

    fn install_file(version: &str) -> InstallFile {
        InstallFile {
            app: "photocraft".into(),
            version: version.into(),
            tag: format!("v{version}"),
            arch: "x64".into(),
            asset: format!("photocraft-{version}-windows-x64-portable.zip"),
            installed_at: 1,
        }
    }

    #[test]
    fn the_install_file_round_trips_and_ignores_anything_else() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path(), "photocraft"), None);
        write(dir.path(), &install_file("0.6.0-rc.1")).unwrap();
        assert_eq!(read(dir.path(), "photocraft"), Some(install_file("0.6.0-rc.1")));
        assert_eq!(read(dir.path(), "wordcraft"), None);
        std::fs::write(dir.path().join(FILE), b"{not json").unwrap();
        assert_eq!(read(dir.path(), "photocraft"), None);
        let mut big = serde_json::to_string(&install_file("0.6.0")).unwrap();
        big.insert_str(1, &" ".repeat(5000));
        std::fs::write(dir.path().join(FILE), big).unwrap();
        assert_eq!(read(dir.path(), "photocraft"), None);
    }

    #[test]
    fn finds_a_portable_copy_and_its_recorded_version() {
        let apps = tempfile::tempdir().unwrap();
        assert_eq!(find(apps.path(), photo()), None);
        let dir = apps.path().join("photocraft");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("photocraft.exe"), b"").unwrap();
        let found = find(apps.path(), photo()).unwrap();
        assert_eq!((found.version, found.source), (None, InstallSource::Launcher));
        assert_eq!(found.location, dir.join("photocraft.exe").display().to_string());
        write(&dir, &install_file("0.6.0")).unwrap();
        assert_eq!(find(apps.path(), photo()).unwrap().version, version::parse("0.6.0"));
    }

    #[test]
    fn only_the_apps_own_folder_is_owned() {
        let apps = tempfile::tempdir().unwrap();
        let dir = apps.path().join("photocraft");
        std::fs::create_dir_all(&dir).unwrap();
        let other = apps.path().join("elsewhere");
        std::fs::create_dir_all(&other).unwrap();
        assert!(owned_folder(apps.path(), photo(), &dir.join("photocraft.exe")).is_some());
        assert_eq!(owned_folder(apps.path(), photo(), &other.join("photocraft.exe")), None);
        assert_eq!(owned_folder(apps.path(), photo(), &dir.join("wordcraft.exe")), None);
        assert_eq!(owned_folder(apps.path(), photo(), &apps.path().join("photocraft.exe")), None);
    }

    #[test]
    fn an_update_keeps_the_users_files_and_replaces_the_apps() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("photocraft");
        std::fs::create_dir_all(dest.join("PhotoCraftData")).unwrap();
        std::fs::write(dest.join("photocraft.exe"), "old").unwrap();
        std::fs::write(dest.join("PhotoCraftData").join("settings.json"), "mine").unwrap();
        std::fs::write(dest.join("portable.txt"), "mine too").unwrap();
        write(&dest, &install_file("0.5.0")).unwrap();
        let staged = root.path().join("new");
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(staged.join("photocraft.exe"), "new").unwrap();
        write(&staged, &install_file("0.6.0")).unwrap();

        replace_folder(&staged, &dest).unwrap();
        assert_eq!(std::fs::read_to_string(dest.join("photocraft.exe")).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(dest.join("PhotoCraftData").join("settings.json")).unwrap(), "mine");
        assert_eq!(std::fs::read_to_string(dest.join("portable.txt")).unwrap(), "mine too");
        assert_eq!(read(&dest, "photocraft").unwrap().version, "0.6.0");
        assert!(!staged.exists() && !root.path().join(".photocraft.old").exists());

        // A missing staged copy leaves the installed one alone.
        assert!(replace_folder(&staged, &dest).is_err());
        assert_eq!(std::fs::read_to_string(dest.join("photocraft.exe")).unwrap(), "new");
        assert!(!root.path().join(".photocraft.old").exists());
    }

    #[test]
    fn a_first_install_just_moves_in() {
        let root = tempfile::tempdir().unwrap();
        let (staged, dest) = (root.path().join("new"), root.path().join("photocraft"));
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(staged.join("photocraft.exe"), "new").unwrap();
        replace_folder(&staged, &dest).unwrap();
        assert_eq!(std::fs::read_to_string(dest.join("photocraft.exe")).unwrap(), "new");
    }

    #[test]
    fn leftovers_of_an_interrupted_update_are_kept() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("photocraft");
        let old = root.path().join(".photocraft.old");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(dest.join("photocraft.exe"), "current").unwrap();
        std::fs::write(old.join("photocraft.exe"), "older").unwrap();
        std::fs::write(old.join("notes.txt"), "mine").unwrap();
        let staged = root.path().join("new");
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(staged.join("photocraft.exe"), "new").unwrap();
        replace_folder(&staged, &dest).unwrap();
        assert_eq!(std::fs::read_to_string(dest.join("photocraft.exe")).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(dest.join("notes.txt")).unwrap(), "mine");
        assert!(!old.exists());
    }

    fn make_zip(path: &Path, entries: &[(&str, Option<&[u8]>)]) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, body) in entries {
            match body {
                Some(body) => {
                    zip.start_file(*name, options).unwrap();
                    zip.write_all(body).unwrap();
                }
                None => zip.add_directory(*name, options).unwrap(),
            }
        }
        zip.finish().unwrap();
    }

    #[test]
    fn unpacks_a_portable_zip() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("photocraft.zip");
        make_zip(
            &file,
            &[("PhotoCraft/", None), ("PhotoCraft/photocraft.exe", Some(b"exe")), ("PhotoCraft/resources/icon.png", Some(&[7u8; 10_000]))],
        );
        let out = dir.path().join("out");
        unzip(&file, &out).unwrap();
        assert_eq!(std::fs::read(out.join("PhotoCraft").join("photocraft.exe")).unwrap(), b"exe");
        assert_eq!(std::fs::read(out.join("PhotoCraft").join("resources").join("icon.png")).unwrap(), [7u8; 10_000]);
    }

    #[test]
    fn refuses_zips_that_escape_or_are_too_big() {
        let dir = tempfile::tempdir().unwrap();
        let escape = dir.path().join("escape.zip");
        make_zip(&escape, &[("../evil.txt", Some(b"x"))]);
        assert!(unzip(&escape, &dir.path().join("out")).unwrap_err().contains("outside"));
        assert!(!dir.path().join("evil.txt").exists());

        let big = dir.path().join("big.zip");
        make_zip(&big, &[("a.bin", Some(&[0u8; 600])), ("b.bin", Some(&[0u8; 600]))]);
        assert!(unzip_capped(&big, &dir.path().join("big"), 10, 1000).unwrap_err().contains("more than"));
        assert!(unzip_capped(&big, &dir.path().join("many"), 1, 1_000_000).unwrap_err().contains("too many"));
        unzip_capped(&big, &dir.path().join("fits"), 2, 1200).unwrap();

        let junk = dir.path().join("junk.zip");
        std::fs::write(&junk, b"not a zip").unwrap();
        assert!(unzip(&junk, &dir.path().join("junk")).is_err());
    }
}
