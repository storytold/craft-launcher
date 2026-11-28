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
    /// The files are in a system prefix and removing them needs root.
    #[serde(default)]
    pub needs_root: bool,
}

/// One command-line tool the launcher installed beside an app (macOS).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliRecord {
    pub version: String,
    /// The folder the tool was installed into, under the launcher's own `cli` folder.
    pub folder: PathBuf,
    /// The link to the tool in the launcher's `cli/bin` folder, the one place a PATH line needs.
    pub link: PathBuf,
    /// Unix seconds.
    pub installed_at: i64,
}

/// The release behind a Windows Installer copy the launcher installed. Windows Installer keeps
/// only the numbers of a version, so `0.24.0-rc.1` reads back as `0.24.0`; this keeps the release
/// the launcher checked and installed, tied to the one registration (`package`, its product code)
/// that install made. Every package build gets a new product code, so a copy installed any other
/// way never matches it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledRelease {
    pub tag: String,
    /// The full version, pre-release label included.
    pub version: String,
    pub package: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub apps: BTreeMap<String, Record>,
    /// Command-line tools, by app id.
    #[serde(default)]
    pub cli: BTreeMap<String, CliRecord>,
    /// For copies the system's installer keeps the record of (`apps` has none for them).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub releases: BTreeMap<String, InstalledRelease>,
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
            Record { version: "0.5.0".into(), kind: PackageKind::AppImage, path: "/x/y".into(), installed_at: 1, files: vec![], needs_root: false },
        );
        m.releases.insert(
            "photocraft".into(),
            InstalledRelease { tag: "v0.6.0-rc.1".into(), version: "0.6.0-rc.1".into(), package: "{00000000-0000-0000-0000-000000000001}".into() },
        );
        m.save(&path).unwrap();
        assert_eq!(Manifest::load(&path), m);
        // A record written before `releases` existed still reads.
        std::fs::write(&path, br#"{"apps":{}}"#).unwrap();
        assert_eq!(Manifest::load(&path), Manifest::default());
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(Manifest::load(&path), Manifest::default());
    }
}
