//! The user's list of third-party apps, saved beside the launcher's record of what it installed.
//!
//! ArtCraft provides no support for these apps: the launcher only keeps them up to date from
//! their GitHub releases, like the Craft App Suite (see `artcraft_launcher_core::custom`).

use std::path::Path;

use artcraft_launcher_core::custom::CustomList;

/// Reads the saved list. A missing, unreadable or damaged file is an empty list.
pub fn load(path: &Path) -> CustomList {
    match std::fs::read(path) {
        Ok(bytes) => CustomList::parse(&bytes),
        Err(_) => CustomList::default(),
    }
}

/// Saves the list (atomically, so a crash never leaves half a file).
pub fn save(list: &CustomList, path: &Path) -> Result<(), String> {
    let bytes = list.to_json()?;
    crate::paths::write_atomic(path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_loads_and_shrugs_off_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("third-party-apps.json");
        assert_eq!(load(&path), CustomList::default());
        let mut list = CustomList::default();
        list.add("octo/tool", 7).unwrap();
        save(&list, &path).unwrap();
        assert_eq!(load(&path), list);
        std::fs::write(&path, b"\xff\xfe nope").unwrap();
        assert_eq!(load(&path), CustomList::default());
    }
}
