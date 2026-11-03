//! The list of files a system-wide (`/usr/local`) tree install put down, kept where only root can
//! change it.
//!
//! The launcher's own record of an install (`installed.json`) lives in the desktop user's data
//! folder, which anything running as that user can rewrite. Removing a system-wide install runs
//! `rm` as root, so its targets can't come from there alone: a rewritten record could add any file
//! under `/usr/local` to an uninstall the user approved (CWE-863). The install therefore copies a
//! list of its own files into the prefix along with them, owned by root, and removal deletes only
//! files that list names.

#![cfg_attr(not(target_os = "freebsd"), allow(dead_code))]

use std::collections::HashSet;
use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

/// Largest list read back: an install holds at most 50,000 files (see `tree::files`).
const MAX_BYTES: u64 = 16 << 20;

/// Where the list for `app_id` sits, relative to the install prefix.
pub(super) fn relative(app_id: &str) -> PathBuf {
    Path::new("share/artcraft-launcher/installed").join(format!("{app_id}.files"))
}

/// The list for `app_id` under `prefix` and every folder below `prefix` that holds it: the paths
/// `owned` requires nobody but root to be able to write, deepest last.
pub(super) fn protected_paths(prefix: &Path, app_id: &str) -> Vec<PathBuf> {
    let mut path = prefix.to_path_buf();
    relative(app_id)
        .components()
        .map(|part| {
            path.push(part);
            path.clone()
        })
        .collect()
}

/// A path relative to the prefix that stays inside it: no root, `..` or empty parts.
fn contained(rel: &Path) -> bool {
    !rel.as_os_str().is_empty() && rel.components().all(|c| matches!(c, Component::Normal(_)))
}

/// Writes the list for `app_id` into the staging `tree` that is about to be copied into the prefix,
/// naming every file in `files` (relative to the tree) and the list itself. Returns the list's
/// path relative to the tree, which the caller records with the other files.
pub(super) fn stage(tree: &Path, app_id: &str, files: &[PathBuf]) -> Result<PathBuf, String> {
    let rel = relative(app_id);
    let mut text = Vec::new();
    for file in files.iter().chain(std::iter::once(&rel)) {
        let bytes = file.as_os_str().as_bytes();
        if !contained(file) || bytes.contains(&b'\n') {
            return Err(format!("the download has a file the launcher can't track: {}", file.display()));
        }
        text.extend_from_slice(bytes);
        text.push(b'\n');
    }
    let dest = tree.join(&rel);
    // The staging tree came from an archive: never write through a symlink it planted.
    let mut check = tree.to_path_buf();
    for part in rel.components() {
        check.push(part);
        if std::fs::symlink_metadata(&check).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(format!("couldn't record the installed files: {} is a symlink", check.display()));
        }
    }
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("couldn't create {} ({e})", dir.display()))?;
    }
    std::fs::write(&dest, text).map_err(|e| format!("couldn't record the installed files ({e})"))?;
    Ok(rel)
}

/// Whether `meta` belongs to `owner` and nobody else can write to it.
fn protected(meta: &std::fs::Metadata, owner: u32) -> bool {
    meta.uid() == owner && meta.mode() & 0o022 == 0
}

/// The files the list for `app_id` under `prefix` names, as full paths. The list, and every folder
/// from `prefix` down to it, must belong to `owner` (root, outside tests) and be writable by
/// nobody else, so the desktop user can't have written or swapped it.
pub(super) fn owned(prefix: &Path, app_id: &str, owner: u32) -> Result<HashSet<PathBuf>, String> {
    let rel = relative(app_id);
    let path = prefix.join(&rel);
    let unconfirmed = || {
        format!(
            "couldn't confirm which files under {} belong to this app, so none were removed. Reinstall it, then uninstall it again",
            prefix.display()
        )
    };
    let mut check = prefix.to_path_buf();
    let mut parts = rel.components().peekable();
    loop {
        let meta = std::fs::symlink_metadata(&check).map_err(|_| unconfirmed())?;
        if meta.file_type().is_symlink() || !protected(&meta, owner) {
            log::warn!("{} isn't protected; not trusting it for an uninstall", check.display());
            return Err(unconfirmed());
        }
        let Some(part) = parts.next() else { break };
        check.push(part);
        if parts.peek().is_none() && !std::fs::symlink_metadata(&check).is_ok_and(|m| m.file_type().is_file()) {
            return Err(unconfirmed());
        }
    }
    let file = std::fs::File::open(&path).map_err(|_| unconfirmed())?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes).map_err(|_| unconfirmed())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(unconfirmed());
    }
    let mut out = HashSet::new();
    for line in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let rel = Path::new(OsStr::from_bytes(line));
        if !contained(rel) {
            return Err(unconfirmed());
        }
        out.insert(prefix.join(rel));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me(dir: &Path) -> u32 {
        std::fs::metadata(dir).unwrap().uid()
    }

    /// A prefix laid out as an install leaves it, with the given files.
    fn installed(files: &[&str]) -> (tempfile::TempDir, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().join("local");
        let files: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
        std::fs::create_dir_all(&prefix).unwrap();
        stage(&prefix, "wordcraft", &files).unwrap();
        for d in [prefix.clone(), prefix.join("share"), prefix.join("share/artcraft-launcher"), prefix.join("share/artcraft-launcher/installed")] {
            std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::set_permissions(prefix.join(relative("wordcraft")), std::fs::Permissions::from_mode(0o644)).unwrap();
        (dir, prefix)
    }

    #[test]
    fn the_list_names_the_installed_files_and_itself() {
        let (dir, prefix) = installed(&["bin/wordcraft", "share/applications/ai.storyteller.wordcraft.desktop"]);
        let owned = owned(&prefix, "wordcraft", me(dir.path())).unwrap();
        let expected: HashSet<PathBuf> =
            ["bin/wordcraft", "share/applications/ai.storyteller.wordcraft.desktop", "share/artcraft-launcher/installed/wordcraft.files"]
                .iter()
                .map(|f| prefix.join(f))
                .collect();
        assert_eq!(owned, expected);
        // Another app's files, or a protected file the app never installed, are not in it.
        assert!(!owned.contains(&prefix.join("bin/sudo")));
    }

    #[test]
    fn protected_paths_are_the_list_and_its_folders_below_the_prefix() {
        let prefix = Path::new("/usr/local");
        let expected: Vec<PathBuf> = [
            "/usr/local/share",
            "/usr/local/share/artcraft-launcher",
            "/usr/local/share/artcraft-launcher/installed",
            "/usr/local/share/artcraft-launcher/installed/wordcraft.files",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        assert_eq!(protected_paths(prefix, "wordcraft"), expected);
    }

    #[test]
    fn clearing_write_on_the_protected_paths_makes_a_loosely_copied_list_trusted() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, prefix) = installed(&["bin/wordcraft"]);
        let uid = me(dir.path());
        // As a copy under a permissive umask, or of an archive with loose modes, could leave it.
        for p in protected_paths(&prefix, "wordcraft") {
            let mode = if p.is_dir() { 0o777 } else { 0o666 };
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        assert!(owned(&prefix, "wordcraft", uid).is_err());
        // What the install's `chmod go-w` does to them.
        for p in protected_paths(&prefix, "wordcraft") {
            let mode = std::fs::metadata(&p).unwrap().mode() & 0o7777 & !0o022;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        assert!(owned(&prefix, "wordcraft", uid).is_ok());
    }

    #[test]
    fn a_list_someone_else_can_write_is_not_trusted() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, prefix) = installed(&["bin/wordcraft"]);
        let uid = me(dir.path());
        // Not root's (here: not the test user's).
        assert!(owned(&prefix, "wordcraft", uid.wrapping_add(1)).is_err());
        // Writable by others, or in a folder writable by others.
        let list = prefix.join(relative("wordcraft"));
        std::fs::set_permissions(&list, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(owned(&prefix, "wordcraft", uid).is_err());
        std::fs::set_permissions(&list, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::set_permissions(prefix.join("share"), std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(owned(&prefix, "wordcraft", uid).is_err());
    }

    #[test]
    fn a_missing_symlinked_or_escaping_list_is_not_trusted() {
        let (dir, prefix) = installed(&["bin/wordcraft"]);
        let uid = me(dir.path());
        assert!(owned(&prefix, "photocraft", uid).is_err(), "no list for this app");

        let list = prefix.join(relative("wordcraft"));
        std::fs::write(&list, b"bin/wordcraft\n../../etc/passwd\n").unwrap();
        assert!(owned(&prefix, "wordcraft", uid).is_err(), "a list that climbs out of the prefix");
        std::fs::write(&list, b"/etc/passwd\n").unwrap();
        assert!(owned(&prefix, "wordcraft", uid).is_err(), "an absolute path");

        let elsewhere = dir.path().join("elsewhere.files");
        std::fs::write(&elsewhere, b"bin/sudo\n").unwrap();
        std::fs::remove_file(&list).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &list).unwrap();
        assert!(owned(&prefix, "wordcraft", uid).is_err(), "a symlinked list");
    }

    #[test]
    fn staging_refuses_a_symlink_planted_by_the_archive_and_untrackable_names() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(tree.join("share")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, tree.join("share/artcraft-launcher")).unwrap();
        assert!(stage(&tree, "wordcraft", &[PathBuf::from("bin/wordcraft")]).is_err());
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);

        let clean = dir.path().join("clean");
        assert!(stage(&clean, "wordcraft", &[PathBuf::from("bin/word\ncraft")]).is_err());
        assert!(stage(&clean, "wordcraft", &[PathBuf::from("../bin/wordcraft")]).is_err());
    }
}
