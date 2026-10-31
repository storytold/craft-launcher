//! Keeps the shared version-choice file ([`craft_update::pins`]) in step with the launcher's
//! settings, so an app's own update check knows when the launcher keeps it on another version.
//!
//! Invariants:
//! - The launcher's settings are the source of truth for the apps it lists: an app absent from
//!   them follows stable releases, so a readable entry for it in the file is removed.
//! - An entry this launcher cannot read (written by a newer one) is kept as it is.
//! - The file is written only when something changed.

use std::collections::BTreeMap;
use std::path::Path;

use artcraft_launcher_core::protocol::VersionTrack;
use craft_update::pins::{Pins, VersionTrack as SharedTrack};

fn shared(track: &VersionTrack) -> SharedTrack {
    match track {
        VersionTrack::Stable => SharedTrack::Stable,
        VersionTrack::Nightly => SharedTrack::Nightly,
        VersionTrack::Pinned(tag) => SharedTrack::Pinned(tag.clone()),
    }
}

/// `pins` changed so it says what `versions` says, keeping entries this launcher cannot read.
pub fn merged(mut pins: Pins, versions: &BTreeMap<String, VersionTrack>) -> Pins {
    let stale: Vec<String> = pins.tracks().map(|(id, _)| id.to_string()).filter(|id| !versions.contains_key(id)).collect();
    for id in stale {
        pins.set(&id, &SharedTrack::Stable);
    }
    for (id, track) in versions {
        pins.set(id, &shared(track));
    }
    pins
}

/// Makes the file's folder when the launcher starts, before any app does. Flatpak mounts an app's
/// grant for that folder only if it exists when the app starts, so an app started before the
/// first pin would otherwise never see it until restarted.
pub fn prepare_folder(path: &Path) {
    if let Some(dir) = path.parent()
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        log::warn!("couldn't create {} ({e}); Flatpak apps won't see version choices", dir.display());
    }
}

/// Writes `path` so it matches `versions`. Returns whether the file changed.
pub fn sync(path: &Path, versions: &BTreeMap<String, VersionTrack>) -> Result<bool, String> {
    let before = Pins::load(path);
    let after = merged(before.clone(), versions);
    if after == before {
        return Ok(false);
    }
    after.save(path)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use craft_update::pins::Choice;

    fn versions(entries: &[(&str, VersionTrack)]) -> BTreeMap<String, VersionTrack> {
        entries.iter().map(|(id, t)| (id.to_string(), t.clone())).collect()
    }

    #[test]
    fn the_folder_exists_before_any_choice_is_saved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ArtCraft").join("versions").join("versions.json");
        prepare_folder(&path);
        assert!(path.parent().unwrap().is_dir());
        assert!(!path.exists(), "no file until there is a choice");
        prepare_folder(&path);
    }

    #[test]
    fn the_file_follows_the_launchers_choices() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("versions.json");
        let chosen = versions(&[("pdfcraft", VersionTrack::Pinned("v0.13.0".into())), ("filmcraft", VersionTrack::Nightly)]);
        assert_eq!(sync(&path, &chosen), Ok(true));
        let pins = Pins::load(&path);
        assert_eq!(pins.choice("pdfcraft"), Choice::Managed(SharedTrack::Pinned("v0.13.0".into())));
        assert_eq!(pins.choice("filmcraft"), Choice::Managed(SharedTrack::Nightly));
        assert_eq!(pins.choice("wordcraft"), Choice::Stable);
        assert_eq!(sync(&path, &chosen), Ok(false), "nothing changed, nothing written");
    }

    #[test]
    fn going_back_to_stable_removes_the_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("versions.json");
        sync(&path, &versions(&[("pdfcraft", VersionTrack::Pinned("v0.13.0".into()))])).unwrap();
        // The UI may drop the entry or keep it as Stable; both mean stable.
        assert_eq!(sync(&path, &versions(&[("pdfcraft", VersionTrack::Stable)])), Ok(true));
        assert_eq!(Pins::load(&path).choice("pdfcraft"), Choice::Stable);
        sync(&path, &versions(&[("pdfcraft", VersionTrack::Nightly)])).unwrap();
        assert_eq!(sync(&path, &BTreeMap::new()), Ok(true));
        assert_eq!(Pins::load(&path).choice("pdfcraft"), Choice::Stable);
    }

    #[test]
    fn an_entry_from_a_newer_launcher_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("versions.json");
        std::fs::write(&path, r#"{"apps":{"soundcraft":{"Beta":1}}}"#).unwrap();
        sync(&path, &versions(&[("pdfcraft", VersionTrack::Nightly)])).unwrap();
        let pins = Pins::load(&path);
        assert_eq!(pins.choice("soundcraft"), Choice::Unknown);
        assert_eq!(pins.choice("pdfcraft"), Choice::Managed(SharedTrack::Nightly));
    }

    #[test]
    fn a_damaged_file_is_replaced_by_the_launchers_choices() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("versions.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(sync(&path, &versions(&[("pdfcraft", VersionTrack::Nightly)])), Ok(true));
        assert_eq!(Pins::load(&path).choice("pdfcraft"), Choice::Managed(SharedTrack::Nightly));
    }
}
