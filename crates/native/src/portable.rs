//! Windows portable mode: apps unpacked from their release's portable zip into
//! `<data>\apps\<app>` (the launcher's own data folder, [`crate::paths::Paths::apps`]), instead of
//! installed from the MSI.
//!
//! Each copy carries [`FILE`], written by the launcher, which says what was installed: the version
//! shown and compared against the latest release comes from there, and so does the list of files
//! the zip shipped. An update unpacks the new copy beside the old one, swaps the folders, and
//! moves across whatever of the old copy the zip didn't ship (settings or data a portable app
//! keeps beside itself). Uninstalling deletes the listed files only; what the app saved stays.
//!
//! The file system work is plain `std::fs`, so the tests run on every OS.

use std::collections::{BTreeSet, HashSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use artcraft_launcher_core::{CraftApp, InstallSource, Installed, version};
use serde::{Deserialize, Serialize};

/// The launcher's record inside each portable copy.
pub const FILE: &str = "artcraft-launcher-install.json";

/// [`FILE`] lists every file of the zip: at most [`MAX_ENTRIES`] paths. Anything bigger isn't ours.
const MAX_FILE: u64 = 8 * 1024 * 1024;

/// How deep [`carry_over`] goes into an old copy's folders.
const MAX_DEPTH: usize = 64;

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
    /// Every file the zip put into the folder, relative to it, `/`-separated: what an update
    /// replaces and an uninstall deletes. Anything else in the folder is the app's or the user's.
    #[serde(default)]
    pub files: Vec<String>,
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

/// `path` (relative, plain names only) as [`InstallFile::files`] spells it: `lib/x.dll`.
pub fn list_entry(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for c in path.components() {
        match c {
            Component::Normal(name) => parts.push(name.to_str()?),
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// An [`InstallFile::files`] entry as a relative path, or `None` when it isn't plain names only
/// (`..`, a drive, a root): such an entry never touches anything.
fn entry_path(entry: &str) -> Option<PathBuf> {
    let path = PathBuf::from(entry.replace('\\', "/"));
    path.components().all(|c| matches!(c, Component::Normal(_))).then_some(path).filter(|p| p.components().next().is_some())
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
/// the only folder the launcher ever removes files from in portable mode.
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

/// Puts the unpacked copy `staged` of `app` in place at `dest`. The old copy is moved aside first
/// and comes back if the new one can't go in; once it has, whatever the old copy had that its zip
/// didn't ship (the user's settings and data) moves across before the rest of the old copy goes.
pub fn replace_folder(staged: &Path, dest: &Path, app: &str) -> Result<(), String> {
    let old = sibling(dest, "old").ok_or("unusual characters in the app's folder name")?;
    settle(&old, dest, app)?;
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
        settle(&old, dest, app)?;
    }
    Ok(())
}

/// Moves what's left of an old copy `old` into `dest` (see [`carry_over`]), then deletes `old`. A
/// copy whose entries couldn't all be moved stays, so nothing of the user's is lost; the error
/// says where it is.
fn settle(old: &Path, dest: &Path, app: &str) -> Result<(), String> {
    if !old.exists() {
        return Ok(());
    }
    if dest.is_dir() {
        // Without the old copy's list there is no telling its files from the user's: keep them all.
        let shipped = read(old, app).map(|f| Shipped::new(&f.files)).unwrap_or_default();
        if !carry_over(old, dest, "", &shipped, 0) {
            return Err(format!("couldn't move everything from the old copy into the new one. What's left is in {}", old.display()));
        }
    }
    let _ = std::fs::remove_dir_all(old);
    Ok(())
}

/// The files a zip shipped and the folders they sit in, lower-cased (Windows paths ignore case).
#[derive(Default)]
struct Shipped {
    files: HashSet<String>,
    dirs: HashSet<String>,
}

impl Shipped {
    fn new(files: &[String]) -> Self {
        let mut out = Self::default();
        for f in files {
            let f = f.replace('\\', "/").to_lowercase();
            let mut rest = f.as_str();
            while let Some((parent, _)) = rest.rsplit_once('/') {
                out.dirs.insert(parent.to_string());
                rest = parent;
            }
            out.files.insert(f);
        }
        out
    }
}

/// Moves each entry of `from` (at `rel` inside the copy) that the old zip didn't ship and `to`
/// doesn't have into `to`, going into folders that hold both. Shipped files stay behind, to be
/// deleted with the old copy. Links are moved as they are, never followed. Whether everything
/// that should move did.
fn carry_over(from: &Path, to: &Path, rel: &str, shipped: &Shipped, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        log::warn!("{} is nested too deeply to keep", from.display());
        return false;
    }
    let Ok(entries) = std::fs::read_dir(from) else { return false };
    let mut all = true;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if depth == 0 && name == FILE {
            continue;
        }
        let key = format!("{rel}{}", name.to_string_lossy().to_lowercase());
        let (path, target) = (entry.path(), to.join(&name));
        let Ok(meta) = path.symlink_metadata() else {
            all = false;
            continue;
        };
        let target_meta = target.symlink_metadata().ok();
        if meta.is_dir() {
            let ours = shipped.dirs.contains(&key);
            match target_meta {
                None if !ours => all &= move_entry(&path, &target),
                // A folder of the old version: rescue whatever the user put in it.
                None => {
                    if std::fs::create_dir(&target).is_err() {
                        all = false;
                        continue;
                    }
                    all &= carry_over(&path, &target, &format!("{key}/"), shipped, depth + 1);
                    // Nothing of the user's in it: no empty folder in the new copy.
                    let _ = std::fs::remove_dir(&target);
                }
                Some(t) if t.is_dir() => all &= carry_over(&path, &target, &format!("{key}/"), shipped, depth + 1),
                // The new copy has a file where the user has a folder: keep it in the old copy.
                Some(_) if !ours => {
                    log::warn!("couldn't keep {}: the new copy has a file there", path.display());
                    all = false;
                }
                Some(_) => {}
            }
        } else if !shipped.files.contains(&key) && target_meta.is_none() {
            all &= move_entry(&path, &target);
        }
    }
    all
}

fn move_entry(from: &Path, to: &Path) -> bool {
    match std::fs::rename(from, to) {
        Ok(()) => true,
        Err(e) => {
            log::warn!("couldn't keep {} from the old copy ({e})", from.display());
            false
        }
    }
}

/// An uninstall cut off halfway leaves `.<app>.removing` beside `dest`: put it back, so its files
/// are found again and the user's aren't lost. With `dest` taken, it stays where it is.
pub fn recover(dest: &Path) {
    let Some(removing) = sibling(dest, "removing") else { return };
    if removing.symlink_metadata().is_err() {
        return;
    }
    if dest.symlink_metadata().is_ok() {
        log::warn!("{} is left from an earlier uninstall; it was kept", removing.display());
        return;
    }
    if let Err(e) = std::fs::rename(&removing, dest) {
        log::warn!("couldn't put {} back ({e})", removing.display());
    }
}

/// Removes the portable copy of `app` in `apps`: the files [`FILE`] lists, the folders that leaves
/// empty, and [`FILE`] itself. Whatever else is in the folder (settings and data the app saved)
/// stays, and so does the folder if anything is left. A copy without a list is left alone.
///
/// The folder is moved aside first, which Windows refuses while anything in it is open (the
/// app running, a DLL a helper loaded): then nothing at all is deleted.
pub fn remove(apps: &Path, app: &CraftApp) -> Result<(), String> {
    let dest = apps.join(app.id);
    recover(&dest);
    let Some(record) = read(&dest, app.id).filter(|f| !f.files.is_empty()) else {
        return Err(format!(
            "The launcher has no record of what it unpacked into {}, so nothing was removed. Delete the folder yourself if you no longer need it.",
            dest.display()
        ));
    };
    let removing = sibling(&dest, "removing").ok_or("unusual characters in the app's folder name")?;
    if removing.symlink_metadata().is_ok() {
        return Err(format!("an earlier uninstall left files in {}. Move or delete them, then try again.", removing.display()));
    }
    std::fs::rename(&dest, &removing).map_err(|e| format!("couldn't remove {} ({e}). Is the app still running?", dest.display()))?;

    let mut failed = None;
    let mut dirs = BTreeSet::new();
    for rel in record.files.iter().filter_map(|f| entry_path(f)) {
        let mut parent = rel.parent();
        while let Some(dir) = parent.filter(|d| d.components().next().is_some()) {
            dirs.insert(dir.to_path_buf());
            parent = dir.parent();
        }
        let Some(path) = plain_path(&removing, &rel) else { continue };
        if path.symlink_metadata().is_ok_and(|m| m.is_file())
            && let Err(e) = std::fs::remove_file(&path)
        {
            failed.get_or_insert(format!("couldn't remove {} ({e})", path.display()));
        }
    }
    // Deepest first; a folder that still holds something of the user's stays.
    let mut dirs: Vec<PathBuf> = dirs.into_iter().collect();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for dir in dirs {
        if let Some(path) = plain_path(&removing, &dir) {
            let _ = std::fs::remove_dir(path);
        }
    }
    let _ = std::fs::remove_file(removing.join(FILE));

    let empty = std::fs::read_dir(&removing).is_ok_and(|mut e| e.next().is_none());
    if empty {
        let _ = std::fs::remove_dir(&removing);
    } else if let Err(e) = std::fs::rename(&removing, &dest) {
        return Err(format!("{} was removed, but what it saved is now in {} ({e})", app.name, removing.display()));
    }
    failed.map_or(Ok(()), Err)
}

/// `root` joined with `rel`, when every folder on the way is a real folder (not a link or a
/// junction that could lead somewhere else).
fn plain_path(root: &Path, rel: &Path) -> Option<PathBuf> {
    let mut path = root.to_path_buf();
    let mut parts = rel.components().peekable();
    while let Some(part) = parts.next() {
        path.push(part);
        if parts.peek().is_some() && !path.symlink_metadata().is_ok_and(|m| m.is_dir()) {
            return None;
        }
    }
    Some(path)
}

/// The most files a portable zip may hold.
#[cfg(any(windows, test))]
const MAX_ENTRIES: usize = 50_000;

/// The most a portable zip may unpack to.
#[cfg(any(windows, test))]
const MAX_UNPACKED: u64 = 8 * 1024 * 1024 * 1024;

/// Unpacks the zip `file` into `dest`, and returns the files it wrote, relative to `dest`.
/// Entries that would land outside `dest`, links, and zips with too many files or too much data
/// are refused.
#[cfg(any(windows, test))]
pub fn unzip(file: &Path, dest: &Path) -> Result<Vec<PathBuf>, String> {
    unzip_capped(file, dest, MAX_ENTRIES, MAX_UNPACKED)
}

#[cfg(any(windows, test))]
fn unzip_capped(file: &Path, dest: &Path, max_entries: usize, max_unpacked: u64) -> Result<Vec<PathBuf>, String> {
    let f = std::fs::File::open(file).map_err(|e| format!("couldn't open the download ({e})"))?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(f)).map_err(|e| format!("the download isn't a readable zip ({e})"))?;
    if archive.len() > max_entries {
        return Err("the download holds too many files, so it wasn't unpacked".into());
    }
    let too_big = || format!("the download unpacks to more than {} MB, so it wasn't unpacked", max_unpacked / (1024 * 1024));
    let mut total: u64 = 0;
    let mut files = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("couldn't read the download ({e})"))?;
        let Some(rel) = entry.enclosed_name() else {
            return Err("the download holds a file that would land outside its folder, so it wasn't unpacked".into());
        };
        if entry.is_symlink() {
            return Err("the download holds a link, so it wasn't unpacked".into());
        }
        let path = dest.join(&rel);
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
        files.push(rel);
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn photo() -> &'static CraftApp {
        CraftApp::by_id("photocraft").unwrap()
    }

    fn install_file(version: &str, files: &[&str]) -> InstallFile {
        InstallFile {
            app: "photocraft".into(),
            version: version.into(),
            tag: format!("v{version}"),
            arch: "x64".into(),
            asset: format!("photocraft-{version}-windows-x64-portable.zip"),
            installed_at: 1,
            files: files.iter().map(|f| f.to_string()).collect(),
        }
    }

    /// Writes each `path` (`/`-separated) under `root` with `body` as its contents.
    fn files(root: &Path, entries: &[(&str, &str)]) {
        for (path, body) in entries {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
    }

    fn text(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn the_install_file_round_trips_and_ignores_anything_else() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path(), "photocraft"), None);
        write(dir.path(), &install_file("0.6.0-rc.1", &["photocraft.exe", "lib/a.dll"])).unwrap();
        assert_eq!(read(dir.path(), "photocraft"), Some(install_file("0.6.0-rc.1", &["photocraft.exe", "lib/a.dll"])));
        assert_eq!(read(dir.path(), "wordcraft"), None);
        std::fs::write(dir.path().join(FILE), b"{not json").unwrap();
        assert_eq!(read(dir.path(), "photocraft"), None);
        let mut big = serde_json::to_string(&install_file("0.6.0", &[])).unwrap();
        big.insert_str(1, &" ".repeat(usize::try_from(MAX_FILE).unwrap() + 1));
        std::fs::write(dir.path().join(FILE), big).unwrap();
        assert_eq!(read(dir.path(), "photocraft"), None);
        // A file without a list (from an earlier build) still reads.
        let mut old = serde_json::to_value(install_file("0.5.0", &[])).unwrap();
        old.as_object_mut().unwrap().remove("files");
        std::fs::write(dir.path().join(FILE), old.to_string()).unwrap();
        assert_eq!(read(dir.path(), "photocraft"), Some(install_file("0.5.0", &[])));
    }

    #[test]
    fn list_entries_are_plain_relative_paths() {
        assert_eq!(list_entry(Path::new("lib").join("a.dll").as_path()).as_deref(), Some("lib/a.dll"));
        assert_eq!(list_entry(Path::new("../a.dll")), None);
        assert_eq!(list_entry(Path::new("")), None);
        assert_eq!(entry_path("lib/a.dll"), Some(Path::new("lib").join("a.dll")));
        assert_eq!(entry_path(r"lib\a.dll"), Some(Path::new("lib").join("a.dll")));
        for bad in ["../outside.txt", "lib/../../x", "/etc/x", ""] {
            assert_eq!(entry_path(bad), None, "{bad}");
        }
        #[cfg(windows)]
        for bad in ["C:/x", "C:x", "//server/share/x"] {
            assert_eq!(entry_path(bad), None, "{bad}");
        }
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
        write(&dir, &install_file("0.6.0", &["photocraft.exe"])).unwrap();
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
    fn an_update_drops_the_old_versions_files_and_keeps_the_users() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("photocraft");
        files(
            &dest,
            &[
                ("photocraft.exe", "old"),
                ("lib/old.dll", "old"),
                ("Plugins/Old.dll", "old"),
                ("lib/user.cfg", "mine"),
                ("Plugins/mine/user.dll", "mine"),
                ("PhotoCraftData/settings.json", "mine"),
                ("portable.txt", "mine too"),
            ],
        );
        // Case differs from the disk: Windows paths ignore it.
        write(&dest, &install_file("0.5.0", &["photocraft.exe", "LIB/old.dll", "plugins/old.dll"])).unwrap();
        let staged = root.path().join("new");
        files(&staged, &[("photocraft.exe", "new"), ("lib/new.dll", "new")]);
        write(&staged, &install_file("0.6.0", &["photocraft.exe", "lib/new.dll"])).unwrap();

        replace_folder(&staged, &dest, "photocraft").unwrap();
        assert_eq!(text(&dest.join("photocraft.exe")), "new");
        assert_eq!(text(&dest.join("lib/new.dll")), "new");
        assert!(!dest.join("lib/old.dll").exists(), "a file of the old version came across");
        assert_eq!(text(&dest.join("lib/user.cfg")), "mine");
        // A folder only the old version shipped comes across with just the user's files in it.
        assert!(!dest.join("Plugins/Old.dll").exists());
        assert_eq!(text(&dest.join("Plugins/mine/user.dll")), "mine");
        assert_eq!(text(&dest.join("PhotoCraftData/settings.json")), "mine");
        assert_eq!(text(&dest.join("portable.txt")), "mine too");
        assert_eq!(read(&dest, "photocraft").unwrap().version, "0.6.0");
        assert!(!staged.exists() && !root.path().join(".photocraft.old").exists());

        // A missing staged copy leaves the installed one alone.
        assert!(replace_folder(&staged, &dest, "photocraft").is_err());
        assert_eq!(text(&dest.join("photocraft.exe")), "new");
        assert!(!root.path().join(".photocraft.old").exists());
    }

    #[test]
    fn an_old_folder_of_the_zip_the_user_left_nothing_in_doesnt_come_across() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("photocraft");
        files(&dest, &[("photocraft.exe", "old"), ("plugins/a.dll", "old")]);
        write(&dest, &install_file("0.5.0", &["photocraft.exe", "plugins/a.dll"])).unwrap();
        let staged = root.path().join("new");
        files(&staged, &[("photocraft.exe", "new")]);
        replace_folder(&staged, &dest, "photocraft").unwrap();
        assert!(!dest.join("plugins").exists());
    }

    #[test]
    fn without_a_list_everything_the_new_copy_lacks_is_kept() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("photocraft");
        files(&dest, &[("photocraft.exe", "old"), ("lib/unknown.dll", "?"), ("notes.txt", "mine")]);
        let staged = root.path().join("new");
        files(&staged, &[("photocraft.exe", "new"), ("lib/new.dll", "new")]);
        replace_folder(&staged, &dest, "photocraft").unwrap();
        assert_eq!(text(&dest.join("photocraft.exe")), "new");
        assert_eq!(text(&dest.join("lib/unknown.dll")), "?");
        assert_eq!(text(&dest.join("notes.txt")), "mine");
    }

    #[test]
    fn a_first_install_just_moves_in() {
        let root = tempfile::tempdir().unwrap();
        let (staged, dest) = (root.path().join("new"), root.path().join("photocraft"));
        files(&staged, &[("photocraft.exe", "new")]);
        replace_folder(&staged, &dest, "photocraft").unwrap();
        assert_eq!(text(&dest.join("photocraft.exe")), "new");
    }

    #[test]
    fn leftovers_of_an_interrupted_update_are_kept() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("photocraft");
        let old = root.path().join(".photocraft.old");
        files(&dest, &[("photocraft.exe", "current")]);
        files(&old, &[("photocraft.exe", "older"), ("notes.txt", "mine")]);
        write(&old, &install_file("0.4.0", &["photocraft.exe"])).unwrap();
        let staged = root.path().join("new");
        files(&staged, &[("photocraft.exe", "new")]);
        replace_folder(&staged, &dest, "photocraft").unwrap();
        assert_eq!(text(&dest.join("photocraft.exe")), "new");
        assert_eq!(text(&dest.join("notes.txt")), "mine");
        assert!(!old.exists());
    }

    #[test]
    fn an_uninstall_removes_the_apps_files_and_keeps_the_users() {
        let apps = tempfile::tempdir().unwrap();
        let dest = apps.path().join("photocraft");
        files(
            &dest,
            &[
                ("photocraft.exe", "app"),
                ("lib/a.dll", "app"),
                ("lib/deep/b.dll", "app"),
                ("res/icon.png", "app"),
                ("lib/user.cfg", "mine"),
                ("PhotoCraftData/settings.json", "mine"),
            ],
        );
        write(&dest, &install_file("0.6.0", &["photocraft.exe", "lib/a.dll", "lib/deep/b.dll", "res/icon.png"])).unwrap();
        remove(apps.path(), photo()).unwrap();
        for gone in ["photocraft.exe", "lib/a.dll", "lib/deep", "res", FILE] {
            assert!(!dest.join(gone).exists(), "{gone} is still there");
        }
        assert_eq!(text(&dest.join("lib/user.cfg")), "mine");
        assert_eq!(text(&dest.join("PhotoCraftData/settings.json")), "mine");
        assert_eq!(find(apps.path(), photo()), None);
        assert!(!apps.path().join(".photocraft.removing").exists());

        // Installed again, the app finds what it saved.
        let staged = apps.path().join("new");
        files(&staged, &[("photocraft.exe", "new"), ("lib/a.dll", "new")]);
        write(&staged, &install_file("0.7.0", &["photocraft.exe", "lib/a.dll"])).unwrap();
        replace_folder(&staged, &dest, "photocraft").unwrap();
        assert_eq!(text(&dest.join("lib/a.dll")), "new");
        assert_eq!(text(&dest.join("lib/user.cfg")), "mine");
        assert_eq!(text(&dest.join("PhotoCraftData/settings.json")), "mine");
    }

    #[test]
    fn an_uninstall_of_only_the_apps_files_leaves_no_folder() {
        let apps = tempfile::tempdir().unwrap();
        let dest = apps.path().join("photocraft");
        files(&dest, &[("photocraft.exe", "app"), ("lib/a.dll", "app")]);
        write(&dest, &install_file("0.6.0", &["photocraft.exe", "lib/a.dll"])).unwrap();
        remove(apps.path(), photo()).unwrap();
        assert!(!dest.exists() && !apps.path().join(".photocraft.removing").exists());
    }

    #[test]
    fn an_uninstall_without_a_list_or_with_a_hostile_one_touches_nothing_else() {
        let apps = tempfile::tempdir().unwrap();
        let dest = apps.path().join("photocraft");
        files(&dest, &[("photocraft.exe", "app")]);
        let err = remove(apps.path(), photo()).unwrap_err();
        assert!(err.contains("no record"), "{err}");
        assert_eq!(text(&dest.join("photocraft.exe")), "app");

        std::fs::write(apps.path().join("outside.txt"), "keep").unwrap();
        write(&dest, &install_file("0.6.0", &["photocraft.exe", "../outside.txt", "..\\outside.txt"])).unwrap();
        remove(apps.path(), photo()).unwrap();
        assert_eq!(text(&apps.path().join("outside.txt")), "keep");
        assert!(!dest.exists());
    }

    #[test]
    fn an_interrupted_uninstall_is_put_back() {
        let apps = tempfile::tempdir().unwrap();
        let removing = apps.path().join(".photocraft.removing");
        files(&removing, &[("photocraft.exe", "app"), ("notes.txt", "mine")]);
        write(&removing, &install_file("0.6.0", &["photocraft.exe"])).unwrap();
        recover(&apps.path().join("photocraft"));
        assert!(find(apps.path(), photo()).is_some());
        remove(apps.path(), photo()).unwrap();
        assert_eq!(text(&apps.path().join("photocraft").join("notes.txt")), "mine");
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
    fn unpacks_a_portable_zip_and_lists_its_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("photocraft.zip");
        make_zip(
            &file,
            &[("PhotoCraft/", None), ("PhotoCraft/photocraft.exe", Some(b"exe")), ("PhotoCraft/resources/icon.png", Some(&[7u8; 10_000]))],
        );
        let out = dir.path().join("out");
        let listed = unzip(&file, &out).unwrap();
        assert_eq!(std::fs::read(out.join("PhotoCraft").join("photocraft.exe")).unwrap(), b"exe");
        assert_eq!(std::fs::read(out.join("PhotoCraft").join("resources").join("icon.png")).unwrap(), [7u8; 10_000]);
        let listed: Vec<String> = listed.iter().filter_map(|p| list_entry(p)).collect();
        assert_eq!(listed, ["PhotoCraft/photocraft.exe", "PhotoCraft/resources/icon.png"]);
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
