use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::IoContext;
use crate::{AppId, Error, FaceInfo, FontFormat, License, Result, Timestamp};

/// File name of the manifest at the root of a vault.
pub const MANIFEST_FILE: &str = "fonts.json";

/// Highest `fonts.json` schema version this build reads and writes.
pub const SCHEMA_VERSION: u32 = 1;

/// The `fonts.json` document: the single source of truth for a vault.
///
/// Every object in the manifest keeps fields this build does not know about
/// in an `extra` map and writes them back unchanged, so newer tools can add
/// fields without older ones deleting them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub fonts: Vec<FontEntry>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One font file stored in the vault.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FontEntry {
    /// Stable id: the first 16 hex digits of the file's SHA-256.
    pub id: String,
    /// Path of the font file relative to the vault root, using `/` separators.
    pub file: String,
    pub format: FontFormat,
    pub sha256: String,
    pub size: u64,
    /// Every face in the file; collections have more than one.
    pub faces: Vec<FaceInfo>,
    /// `false` hides the whole file from apps, whatever its faces say.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Suite apps allowed to load this font. Empty means every app.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub apps: Vec<AppId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<License>,
    /// Original file name the font was imported from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub added_at: Timestamp,
    /// Fields this build does not know about, preserved on rewrite.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Serde default for the `enabled` flags, which are on unless the manifest says otherwise.
pub(crate) fn default_true() -> bool {
    true
}

impl FontEntry {
    /// Family name of the first face.
    pub fn family(&self) -> &str {
        self.faces.first().map_or("", |f| f.family.as_str())
    }

    /// Faces apps may load: none if the entry is disabled, otherwise the
    /// enabled faces.
    pub fn active_faces(&self) -> impl Iterator<Item = &FaceInfo> {
        self.faces.iter().filter(|f| self.enabled && f.enabled)
    }

    /// Whether `app` should load this font at launch.
    pub fn available_to(&self, app: &AppId) -> bool {
        self.active_faces().next().is_some() && (self.apps.is_empty() || self.apps.contains(app))
    }

    /// Resolves [`FontEntry::file`] under `root`. Returns `None` unless the
    /// path is `/`-separated, non-empty segments with no `.`, `..`, `\` or
    /// `:`, so a hand-edited manifest cannot point outside the vault on any
    /// platform.
    pub fn path_in(&self, root: &Path) -> Option<PathBuf> {
        let mut path = root.to_owned();
        for segment in self.file.split('/') {
            if segment.is_empty() || segment == "." || segment == ".." || segment.contains(['\\', ':']) {
                return None;
            }
            path.push(segment);
        }
        Some(path)
    }
}

impl Default for Manifest {
    fn default() -> Self {
        Self { schema_version: SCHEMA_VERSION, fonts: Vec::new(), extra: Map::new() }
    }
}

impl Manifest {
    /// Reads and parses the manifest at `path`; see [`Manifest::from_json`].
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).at(path)?;
        Self::parse(&text, path)
    }

    /// Parses manifest text, rejecting any schema version other than [`SCHEMA_VERSION`].
    pub fn from_json(text: &str) -> Result<Self> {
        Self::parse(text, Path::new(MANIFEST_FILE))
    }

    fn parse(text: &str, path: &Path) -> Result<Self> {
        let manifest_error = |source| Error::Manifest { path: path.to_owned(), source };
        // Check the version before the full parse so another version gets a
        // clear error instead of a field-level one. Version 1 is the only one
        // there has ever been; the schema pins it with `"const": 1`.
        #[derive(Deserialize)]
        struct Version {
            schema_version: u32,
        }
        let Version { schema_version } = serde_json::from_str(text).map_err(manifest_error)?;
        if schema_version != SCHEMA_VERSION {
            return Err(Error::UnsupportedSchema { found: schema_version, supported: SCHEMA_VERSION });
        }
        serde_json::from_str(text).map_err(manifest_error)
    }

    /// Writes the manifest atomically so readers never see a partial file.
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut json = serde_json::to_vec_pretty(self).map_err(|source| Error::Manifest { path: path.to_owned(), source })?;
        json.push(b'\n');
        write_atomic(path, &json)
    }

    /// Finds an entry by exact id or unique id prefix.
    pub fn find(&self, query: &str) -> Result<usize> {
        if let Some(i) = self.fonts.iter().position(|f| f.id == query) {
            return Ok(i);
        }
        let mut matches = self.fonts.iter().enumerate().filter(|(_, f)| !query.is_empty() && f.id.starts_with(query));
        match (matches.next(), matches.count()) {
            (Some((i, _)), 0) => Ok(i),
            (Some(_), rest) => Err(Error::Ambiguous(query.into(), rest + 1)),
            (None, _) => Err(Error::NotFound(query.into())),
        }
    }
}

/// Writes `data` to a sibling temp file, syncs it, then renames over `path`.
pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut file = fs::File::create(&tmp).at(&tmp)?;
    file.write_all(data).at(&tmp)?;
    file.sync_all().at(&tmp)?;
    drop(file);
    fs::rename(&tmp, path).at(path)
}
