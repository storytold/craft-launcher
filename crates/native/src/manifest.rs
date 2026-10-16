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
    /// Every file installed, for tree installs (FreeBSD), so removal takes exactly those.
    #[serde(default)]
    pub files: Vec<PathBuf>,
    /// The folder a portable install went into, so removal takes exactly it even after the
    /// install location changes. Older records leave this empty and fall back to the default.
    #[serde(default)]
    pub root: Option<PathBuf>,
    /// The files are in a system prefix and removing them needs root.
    #[serde(default)]
    pub needs_root: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub apps: BTreeMap<String, Record>,
}

impl Manifest {
    pub fn load(path: &Path) -> Self {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if path.exists() => {
                log::error!("couldn't read the install records ({e}); starting fresh");
                return Self::default();
            }
            Err(_) => return Self::default(),
        };
        match serde_json::from_slice(&bytes) {
            Ok(manifest) => manifest,
            Err(e) => {
                // Never silently adopt an empty manifest over an unreadable file: keep
                // the evidence beside it, so the next mystery has something to read.
                let backup = path.with_extension(format!("corrupt-{}", crate::now()));
                let _ = std::fs::write(&backup, &bytes);
                log::error!("couldn't read the install records ({e}); kept a copy at {} and started fresh", backup.display());
                Self::default()
            }
        }
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
                needs_root: false,
                root: None,
            },
        );
        m.save(&path).unwrap();
        assert_eq!(Manifest::load(&path), m);
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(Manifest::load(&path), Manifest::default());
        // The evidence stays beside it for the next investigation.
        let backups: Vec<_> =
            std::fs::read_dir(dir.path()).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("installed.corrupt-")).collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read(backups[0].path()).unwrap(), b"{not json");
    }
}
