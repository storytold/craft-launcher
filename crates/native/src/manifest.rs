//! The launcher's record of what it installed itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use artcraft_launcher_core::PackageKind;
use serde::{Deserialize, Serialize};

/// One app the launcher installed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub version: String,
    pub kind: PackageKind,
    /// The `.app`, the exe, the AppImage, or the folder of an unpacked tarball.
    pub path: PathBuf,
    /// Unix seconds.
    pub installed_at: i64,
    /// Tree-install files (FreeBSD), or AppImage desktop/icon ownership candidates. AppImage
    /// resources must also match the exact native contents before replacement or removal.
    #[serde(default)]
    pub files: Vec<PathBuf>,
    /// Expected generated contents, independent of the current binary path or catalog metadata.
    /// Older manifests lack this snapshot and use conservative legacy-content validation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appimage_desktop: Option<AppImageDesktopOwnership>,
    /// The files are in a system prefix and removing them needs root.
    #[serde(default)]
    pub needs_root: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppImageDesktopOwnership {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_sha256: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub apps: BTreeMap<String, Record>,
}

impl Manifest {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        crate::paths::write_atomic(path, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_survives_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("installed.json");
        assert_eq!(Manifest::load(&path), Manifest::default());
        let mut m = Manifest::default();
        m.apps.insert(
            "photocraft".into(),
            Record {
                version: "0.5.0".into(),
                kind: PackageKind::AppImage,
                path: "/x/y".into(),
                installed_at: 1,
                files: vec![],
                appimage_desktop: None,
                needs_root: false,
            },
        );
        m.save(&path).unwrap();
        assert_eq!(Manifest::load(&path), m);
        let mut legacy: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        legacy["apps"]["photocraft"].as_object_mut().unwrap().remove("appimage_desktop");
        std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert_eq!(Manifest::load(&path), m, "old records without the snapshot remain readable");
        m.apps.get_mut("photocraft").unwrap().appimage_desktop = Some(AppImageDesktopOwnership {
            desktop_text: Some("generated desktop contents from an earlier catalog".into()),
            icon_sha256: Some("expected icon digest".into()),
        });
        m.save(&path).unwrap();
        assert_eq!(Manifest::load(&path), m);
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(Manifest::load(&path), Manifest::default());
    }
}
