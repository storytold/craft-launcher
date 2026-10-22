//! What a portable ZIP's own layout means, apart from installing it: pure path reasoning, so it
//! is compiled and tested on every platform while [`crate::portable`] stays Windows-only.

use std::path::{Path, PathBuf};

/// Whether two paths name the same file to Windows: case is ignored, a trailing separator does
/// not count, and the `\\?\` prefix is not part of the name.
pub fn same_windows_path(left: &Path, right: &Path) -> bool {
    fn normalized(path: &Path) -> String {
        let text = path.to_string_lossy();
        let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
        text.trim_end_matches(['\\', '/']).to_lowercase()
    }
    normalized(left) == normalized(right)
}

/// The one folder a ZIP wraps everything in, when it has one: every entry lies inside it, and no
/// entry sits beside it at the root. `None` for a ZIP whose files are already at the root, or one
/// with several top-level entries, where there is nothing unambiguous to strip.
///
/// Entries are compared as Windows names them, so `App/app.exe` and `app/lib.dll` are one folder:
/// that is the folder they extract into on a case-insensitive volume, and `extract` already
/// refuses a ZIP whose names collide only in case.
fn wrapper_folder(files: &[PathBuf]) -> Option<PathBuf> {
    let mut folders = files.iter().map(|relative| {
        let mut components = relative.components();
        match (components.next(), components.next()) {
            // A first component with something below it: the candidate folder.
            (Some(first), Some(_)) => Some(PathBuf::from(first.as_os_str())),
            // A file at the root: nothing may be stripped.
            _ => None,
        }
    });
    let first = folders.next().flatten()?;
    folders.all(|folder| folder.as_deref().is_some_and(|f| same_windows_path(f, &first))).then_some(first)
}

/// `unpacked` and the entries inside it, with one wrapping folder dropped when the ZIP has one:
/// the folder becomes the place the files are read from, and the entries lose its name, so a ZIP
/// that wraps its program installs exactly as one packed without the wrapper does.
pub fn unwrap_single_folder(unpacked: PathBuf, files: Vec<PathBuf>) -> (PathBuf, Vec<PathBuf>) {
    let Some(folder) = wrapper_folder(&files) else { return (unpacked, files) };
    // The first component goes by position, not by name: `strip_prefix` compares exactly, and the
    // entries may spell the folder in another case than the one chosen above.
    let inside = files.iter().map(|entry| entry.components().skip(1).collect()).collect();
    (unpacked.join(folder), inside)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A release ZIP that wraps everything in one folder installs what is inside it, so the
    /// program lands flat rather than being refused for having no executable at the root.
    #[test]
    fn one_wrapping_folder_is_stripped_and_nothing_else_is() {
        let p = |s: &str| PathBuf::from(s);
        // Everything inside one folder: that folder is the wrapper.
        let wrapped = [p("VectorCraft-1.2.3/vectorcraft.exe"), p("VectorCraft-1.2.3/data/fonts.bin")];
        assert_eq!(wrapper_folder(&wrapped), Some(p("VectorCraft-1.2.3")));

        // Already flat: there is nothing to strip, and the executable is found as before.
        assert_eq!(wrapper_folder(&[p("vectorcraft.exe"), p("data/fonts.bin")]), None);
        assert_eq!(wrapper_folder(&[p("vectorcraft.exe")]), None);

        // A file beside the folder at the root: stripping would lose it.
        assert_eq!(wrapper_folder(&[p("VectorCraft-1.2.3/vectorcraft.exe"), p("README.txt")]), None);

        // Two top-level folders: nothing unambiguous to strip.
        assert_eq!(wrapper_folder(&[p("a/vectorcraft.exe"), p("b/fonts.bin")]), None);

        // An empty list strips nothing.
        assert_eq!(wrapper_folder(&[]), None);

        // Only the outermost folder goes, however deep the tree is.
        let deep = [p("App/bin/app.exe"), p("App/bin/lib/x.dll")];
        assert_eq!(wrapper_folder(&deep), Some(p("App")));
        let stripped: Vec<PathBuf> = deep.iter().map(|f| f.strip_prefix("App").unwrap().to_path_buf()).collect();
        assert_eq!(wrapper_folder(&stripped), Some(p("bin")), "the next level is only stripped if asked for again");
    }

    /// Windows treats `App` and `app` as one folder, and a ZIP spelling the wrapper both ways
    /// extracts into one folder there, so it is one wrapper here too. `extract` already refuses a
    /// ZIP whose names collide only in case, so this cannot hide two real folders.
    #[test]
    fn a_wrapper_spelled_in_different_cases_is_one_folder() {
        let p = |s: &str| PathBuf::from(s);
        let mixed = [p("App-1.2.3/photocraft.exe"), p("app-1.2.3/lib.dll"), p("APP-1.2.3/data/x.bin")];
        assert_eq!(wrapper_folder(&mixed), Some(p("App-1.2.3")), "the first spelling names it");

        // And the entries lose it however they spell it, so the executable reaches the root.
        let (from, files) = unwrap_single_folder(p("/staging/unpacked"), mixed.to_vec());
        assert_eq!(from, p("/staging/unpacked/App-1.2.3"));
        assert_eq!(files, vec![p("photocraft.exe"), p("lib.dll"), p("data/x.bin")]);

        // Genuinely different folders are still different.
        assert_eq!(wrapper_folder(&[p("App/a.exe"), p("Apple/b.dll")]), None);
    }

    #[test]
    fn windows_names_the_same_file_whatever_the_case_or_trailing_slash() {
        let p = |s: &str| PathBuf::from(s);
        assert!(same_windows_path(&p("App/app.exe"), &p("app/APP.EXE")));
        assert!(same_windows_path(&p("App"), &p("App/")));
        assert!(!same_windows_path(&p("App"), &p("Apple")));
    }

    /// What the installer uses: where to read the files from, and what to call them.
    #[test]
    fn unwrapping_moves_the_source_folder_and_the_entry_names_together() {
        let p = |s: &str| PathBuf::from(s);
        let unpacked = p("/staging/unpacked");

        let (from, files) = unwrap_single_folder(unpacked.clone(), vec![p("App-1.2.3/app.exe"), p("App-1.2.3/data/fonts.bin")]);
        assert_eq!(from, p("/staging/unpacked/App-1.2.3"), "read from inside the wrapper");
        assert_eq!(files, vec![p("app.exe"), p("data/fonts.bin")], "installed without it");
        // The executable is now at the root, which is what the installer looks for.
        assert!(files.iter().any(|f| f.parent().is_some_and(|parent| parent.as_os_str().is_empty()) && f.ends_with("app.exe")));

        // A flat ZIP is untouched, as are the ambiguous layouts.
        let flat = vec![p("app.exe"), p("data/fonts.bin")];
        assert_eq!(unwrap_single_folder(unpacked.clone(), flat.clone()), (unpacked.clone(), flat));
        let two = vec![p("a/app.exe"), p("b/fonts.bin")];
        assert_eq!(unwrap_single_folder(unpacked.clone(), two.clone()), (unpacked.clone(), two));
        let beside = vec![p("App/app.exe"), p("README.txt")];
        assert_eq!(unwrap_single_folder(unpacked.clone(), beside.clone()), (unpacked, beside));
    }
}
