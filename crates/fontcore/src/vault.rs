use std::collections::HashSet;
use std::fs::{self, OpenOptions, TryLockError};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Map;
use sha2::{Digest, Sha256};

use crate::error::IoContext;
use crate::manifest::write_atomic;
use crate::{AppId, Error, FontEntry, License, MANIFEST_FILE, Manifest, Result, Timestamp, inspect};

/// Environment variable that overrides the default vault location.
pub const HOME_ENV: &str = "FONTCORE_HOME";

/// Directory inside the vault that holds the font files.
pub const FONTS_DIR: &str = "fonts";

const LOCK_FILE: &str = ".fontcore.lock";
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// A font vault on disk: a `fonts.json` manifest plus a `fonts/` directory.
#[derive(Debug, Clone)]
pub struct Vault {
    root: PathBuf,
}

/// Metadata attached to fonts on import.
#[derive(Debug, Clone, Default)]
pub struct AddOptions {
    pub apps: Vec<AppId>,
    pub tags: Vec<String>,
    pub license: Option<License>,
}

#[derive(Debug, Clone)]
pub enum AddOutcome {
    Added(FontEntry),
    /// A byte-identical file is already in the vault; nothing was changed.
    AlreadyPresent(FontEntry),
}

impl AddOutcome {
    /// The manifest entry for the font, whether it was just added or already there.
    pub fn entry(&self) -> &FontEntry {
        match self {
            Self::Added(e) | Self::AlreadyPresent(e) => e,
        }
    }
}

/// A problem found by [`Vault::verify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Issue {
    UnsafePath {
        id: String,
        file: String,
    },
    MissingFile {
        id: String,
        path: PathBuf,
    },
    HashMismatch {
        id: String,
        path: PathBuf,
    },
    /// A file in `fonts/` that no manifest entry references.
    Orphan {
        path: PathBuf,
    },
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsafePath { id, file } => write!(f, "{id}: unsafe path '{file}'"),
            Self::MissingFile { id, path } => write!(f, "{id}: missing file {}", path.display()),
            Self::HashMismatch { id, path } => {
                write!(f, "{id}: contents of {} do not match recorded hash", path.display())
            }
            Self::Orphan { path } => write!(f, "orphaned file {}", path.display()),
        }
    }
}

impl Vault {
    /// `$FONTCORE_HOME`, or the platform data directory
    /// (e.g. `~/.local/share/fontcore`, `%APPDATA%\storytold\fontcore\data`).
    pub fn default_root() -> Result<PathBuf> {
        if let Some(home) = std::env::var_os(HOME_ENV).filter(|v| !v.is_empty()) {
            return Ok(PathBuf::from(home));
        }
        directories::ProjectDirs::from("", "storytold", "fontcore").map(|dirs| dirs.data_dir().to_owned()).ok_or(Error::NoHome)
    }

    /// Opens an existing vault.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let vault = Self { root: root.into() };
        if !vault.manifest_path().is_file() {
            return Err(Error::NotInitialized(vault.root));
        }
        Ok(vault)
    }

    /// Opens the vault at `root`, creating it if needed.
    pub fn init(root: impl Into<PathBuf>) -> Result<Self> {
        let vault = Self { root: root.into() };
        let fonts = vault.root.join(FONTS_DIR);
        fs::create_dir_all(&fonts).at(&fonts)?;
        let manifest = vault.manifest_path();
        // Check and create under the lock: otherwise a concurrent init could find no manifest,
        // wait while another writer creates one and adds a font, then save an empty one over it.
        let _lock = vault.lock()?;
        if !manifest.exists() {
            Manifest::default().save(&manifest)?;
        }
        Ok(vault)
    }

    /// The vault's directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where the vault's `fonts.json` lives.
    pub fn manifest_path(&self) -> PathBuf {
        self.root.join(MANIFEST_FILE)
    }

    /// Reads the current manifest. Takes no lock: writes are atomic, so it is never half-written.
    pub fn manifest(&self) -> Result<Manifest> {
        Manifest::load(&self.manifest_path())
    }

    /// Looks up one entry by id or unique id prefix.
    pub fn get(&self, query: &str) -> Result<FontEntry> {
        let mut manifest = self.manifest()?;
        let i = manifest.find(query)?;
        Ok(manifest.fonts.swap_remove(i))
    }

    /// Copies a font file into the vault and records it in the manifest.
    pub fn add(&self, path: &Path, options: &AddOptions) -> Result<AddOutcome> {
        let data = fs::read(path).at(path)?;
        let (format, faces) = inspect(&data)?;
        let sha256 = hex(&Sha256::digest(&data));
        let id = sha256[..16].to_owned();
        let file = format!("{FONTS_DIR}/{id}.{}", format.extension());

        self.update(|manifest| {
            if let Some(existing) = manifest.fonts.iter().find(|f| f.sha256 == sha256) {
                return Ok(AddOutcome::AlreadyPresent(existing.clone()));
            }
            let dest = self.root.join(&file);
            write_atomic(&dest, &data)?;
            let entry = FontEntry {
                id,
                file,
                format,
                sha256,
                size: data.len() as u64,
                faces,
                enabled: true,
                apps: dedup(options.apps.clone()),
                tags: options.tags.clone(),
                license: options.license.clone(),
                source: path.file_name().map(|n| n.to_string_lossy().into_owned()),
                added_at: Timestamp::now(),
                extra: Map::new(),
            };
            manifest.fonts.push(entry.clone());
            Ok(AddOutcome::Added(entry))
        })
    }

    /// Removes a font from the manifest and deletes its file.
    ///
    /// The file is deleted while the vault lock is held: once the lock is released, an add of the
    /// same bytes may recreate this exact path, and deleting after that would lose its file. If
    /// saving the manifest then fails, the entry is left pointing at a missing file, which the
    /// loader reports as unavailable and [`Vault::verify`] reports as missing.
    pub fn remove(&self, query: &str) -> Result<FontEntry> {
        self.update(|manifest| {
            let i = manifest.find(query)?;
            let entry = manifest.fonts.remove(i);
            if let Some(path) = entry.path_in(&self.root) {
                match fs::remove_file(&path) {
                    Err(e) if e.kind() != ErrorKind::NotFound => return Err(e).at(path),
                    _ => {}
                }
            }
            Ok(entry)
        })
    }

    /// Shows or hides a whole font file from apps; its faces keep their own flags.
    pub fn set_enabled(&self, query: &str, enabled: bool) -> Result<FontEntry> {
        self.modify(query, |entry| entry.enabled = enabled)
    }

    /// Restricts a font to the given apps; an empty list makes it global.
    pub fn set_apps(&self, query: &str, apps: Vec<AppId>) -> Result<FontEntry> {
        self.modify(query, |entry| entry.apps = dedup(apps))
    }

    /// Enables or disables one face of a font, by its collection index.
    pub fn set_face_enabled(&self, query: &str, index: u32, enabled: bool) -> Result<FontEntry> {
        self.update(|manifest| {
            let i = manifest.find(query)?;
            let entry = &mut manifest.fonts[i];
            let face = entry.faces.iter_mut().find(|f| f.index == index).ok_or_else(|| Error::NoSuchFace { id: entry.id.clone(), index })?;
            face.enabled = enabled;
            Ok(entry.clone())
        })
    }

    /// Checks that every entry's file exists and matches its hash, and that
    /// `fonts/` holds no unreferenced files.
    pub fn verify(&self) -> Result<Vec<Issue>> {
        let manifest = self.manifest()?;
        let mut issues = Vec::new();
        let mut referenced = HashSet::new();
        for entry in &manifest.fonts {
            let Some(path) = entry.path_in(&self.root) else {
                issues.push(Issue::UnsafePath { id: entry.id.clone(), file: entry.file.clone() });
                continue;
            };
            referenced.insert(path.clone());
            match fs::read(&path) {
                Ok(data) if hex(&Sha256::digest(&data)) == entry.sha256 => {}
                Ok(_) => issues.push(Issue::HashMismatch { id: entry.id.clone(), path }),
                Err(e) if e.kind() == ErrorKind::NotFound => issues.push(Issue::MissingFile { id: entry.id.clone(), path }),
                Err(e) => return Err(e).at(path),
            }
        }
        let fonts_dir = self.root.join(FONTS_DIR);
        if fonts_dir.is_dir() {
            for dirent in fs::read_dir(&fonts_dir).at(&fonts_dir)? {
                let path = dirent.at(&fonts_dir)?.path();
                if path.is_file() && !referenced.contains(&path) {
                    issues.push(Issue::Orphan { path });
                }
            }
        }
        Ok(issues)
    }

    fn modify(&self, query: &str, f: impl FnOnce(&mut FontEntry)) -> Result<FontEntry> {
        self.update(|manifest| {
            let i = manifest.find(query)?;
            f(&mut manifest.fonts[i]);
            Ok(manifest.fonts[i].clone())
        })
    }

    /// Runs a read-modify-write of the manifest under the vault lock. The
    /// manifest is saved only if `f` succeeds.
    fn update<T>(&self, f: impl FnOnce(&mut Manifest) -> Result<T>) -> Result<T> {
        let _lock = self.lock()?;
        let path = self.manifest_path();
        let mut manifest = Manifest::load(&path)?;
        let before = manifest.clone();
        let out = f(&mut manifest)?;
        if manifest != before {
            manifest.save(&path)?;
        }
        Ok(out)
    }

    /// Takes the vault's write lock, waiting up to [`LOCK_TIMEOUT`] for another writer.
    fn lock(&self) -> Result<LockGuard> {
        let path = self.root.join(LOCK_FILE);
        let file = OpenOptions::new().write(true).create(true).truncate(false).open(&path).at(&path)?;
        let deadline = Instant::now() + LOCK_TIMEOUT;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(LockGuard(file)),
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
                Err(TryLockError::WouldBlock) => return Err(Error::Locked(path)),
                Err(TryLockError::Error(e)) => return Err(e).at(path),
            }
        }
    }
}

/// The vault's write lock: an OS file lock (`flock`, `LockFileEx`) on `.fontcore.lock`, released
/// when dropped. The OS also releases it when its holder exits, crash included, so a lock is never
/// taken over on a guess while its holder is still working. The file itself stays between writes:
/// deleting it could let two writers lock two different files under the same name.
struct LockGuard(fs::File);

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Drops repeated app ids, keeping the first occurrence of each.
fn dedup(apps: Vec<AppId>) -> Vec<AppId> {
    let mut seen = HashSet::new();
    apps.into_iter().filter(|a| seen.insert(a.clone())).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
