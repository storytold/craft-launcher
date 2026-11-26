use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::artifact_paths::is_link as linked;
pub(super) use crate::artifact_paths::valid_path;

use super::{MAX_FILE_BYTES, Manifest, ManifestFile, ReleaseError, Result, lowercase_hex, validate_source_fields};
const MAX_SITE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_FILES: usize = 4096;
const MAX_ENTRIES: usize = 8192;

impl Manifest {
    pub(super) fn validate(&mut self) -> Result<u64> {
        validate_source_fields(&self.app, &self.source_commit, &self.source_tree)?;
        if self.files.is_empty() || self.files.len() > MAX_FILES {
            return Err(ReleaseError::Limit);
        }
        self.files.sort_unstable_by(|left, right| left.path.cmp(&right.path));
        let mut bytes = 0;
        for file in &self.files {
            if !valid_path(&file.path) || !lowercase_hex(&file.sha256, 64) {
                return Err(ReleaseError::Invalid);
            }
            if file.size > MAX_FILE_BYTES as u64 || file.size > MAX_SITE_BYTES - bytes {
                return Err(ReleaseError::Limit);
            }
            bytes += file.size;
        }
        if self.file("index.html").is_none_or(|entry| entry.size == 0) {
            return Err(ReleaseError::Invalid);
        }
        self.inventory()?;
        Ok(bytes)
    }

    pub(super) fn file(&self, path: &str) -> Option<&ManifestFile> {
        self.files.binary_search_by(|entry| entry.path.as_str().cmp(path)).ok().and_then(|index| self.files.get(index))
    }

    fn inventory(&self) -> Result<BTreeMap<String, (&str, bool)>> {
        let mut inventory = BTreeMap::new();
        for file in &self.files {
            let directories = file.path.match_indices('/').filter_map(|(end, _)| file.path.get(..end));
            for directory in directories {
                match inventory.entry(directory.to_ascii_lowercase()) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert((directory, false));
                    }
                    std::collections::btree_map::Entry::Occupied(entry) => {
                        if *entry.get() != (directory, false) {
                            return Err(ReleaseError::Invalid);
                        }
                    }
                }
            }
            if inventory.insert(file.path.to_ascii_lowercase(), (&file.path, true)).is_some() {
                return Err(ReleaseError::Invalid);
            }
            if inventory.len() > MAX_ENTRIES {
                return Err(ReleaseError::Limit);
            }
        }
        Ok(inventory)
    }
}

pub(super) fn check_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() && !linked(&metadata) { Ok(()) } else { Err(ReleaseError::Invalid) }
}

pub(super) fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

pub(super) fn create_directory(parent: &Path, name: &str) -> Result<PathBuf> {
    check_directory(parent)?;
    let directory = parent.join(name);
    match fs::create_dir(&directory) {
        Ok(()) => sync_directory(parent)?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    check_directory(&directory)?;
    Ok(directory)
}

pub(super) fn upload(build: &Path, site: &Path, entry: &ManifestFile, bytes: &[u8]) -> Result<()> {
    if bytes.len() as u64 != entry.size || format!("{:x}", Sha256::digest(bytes)) != entry.sha256 {
        return Err(ReleaseError::Invalid);
    }
    let (directories, name) = entry.path.rsplit_once('/').unwrap_or(("", &entry.path));
    let mut parent = site.to_path_buf();
    for part in directories.split('/').filter(|part| !part.is_empty()) {
        parent = create_directory(&parent, part)?;
    }
    let destination = parent.join(name);
    match fs::symlink_metadata(&destination) {
        Ok(_) => {
            verify_file(&destination, entry)?;
            sync_directory(&parent)?;
            sync_directory(build)?;
            return Ok(());
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    // A crash can leave this private temp file. It never enters the site's inventory.
    let temp = build.join("upload.tmp");
    match fs::symlink_metadata(&temp) {
        Ok(metadata) if metadata.is_file() && !linked(&metadata) => fs::remove_file(&temp)?,
        Ok(_) => return Err(ReleaseError::Invalid),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, &destination)?;
        sync_directory(&parent)?;
        sync_directory(build)?;
        Ok(())
    })();
    if result.is_err() {
        // The next upload or terminal collection retries cleanup if the filesystem is unavailable.
        let _ = fs::remove_file(&temp);
    }
    result
}

pub(super) fn inspect(site: &Path, manifest: &Manifest) -> Result<()> {
    let mut remaining = manifest.inventory()?;
    let mut directories = vec![(site.to_path_buf(), String::new())];
    while let Some((directory, prefix)) = directories.pop() {
        for next in fs::read_dir(&directory)? {
            let next = next?;
            let name = next.file_name();
            let name = name.to_str().ok_or(ReleaseError::Invalid)?;
            let relative = if prefix.is_empty() { name.into() } else { format!("{prefix}/{name}") };
            let (declared, file) = remaining.remove(&relative.to_ascii_lowercase()).ok_or(ReleaseError::Invalid)?;
            if relative != declared {
                return Err(ReleaseError::Invalid);
            }
            let path = next.path();
            if file {
                verify_file(&path, manifest.file(declared).ok_or(ReleaseError::Invalid)?)?;
            } else {
                check_directory(&path)?;
                directories.push((path, relative));
            }
        }
        sync_directory(&directory)?;
    }
    if remaining.is_empty() { Ok(()) } else { Err(ReleaseError::Incomplete) }
}

pub(super) fn asset_path(site: &Path, entry: &ManifestFile) -> Result<PathBuf> {
    let (directories, name) = entry.path.rsplit_once('/').unwrap_or(("", &entry.path));
    let mut path = site.to_path_buf();
    for part in directories.split('/').filter(|part| !part.is_empty()) {
        path.push(part);
        check_directory(&path)?;
    }
    path.push(name);
    check_file(&path, entry)?;
    Ok(path)
}

fn check_file(path: &Path, entry: &ManifestFile) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_file() && !linked(&metadata) && metadata.len() == entry.size { Ok(()) } else { Err(ReleaseError::Invalid) }
}

fn verify_file(path: &Path, entry: &ManifestFile) -> Result<()> {
    check_file(path, entry)?;
    let mut file = File::open(path)?.take(entry.size + 1);
    let mut hash = Sha256::new();
    let mut bytes = 0;
    let mut buffer = [0; 16 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(buffer.get(..read).ok_or(ReleaseError::Invalid)?);
        bytes += read as u64;
    }
    if bytes == entry.size && format!("{:x}", hash.finalize()) == entry.sha256 { Ok(()) } else { Err(ReleaseError::Invalid) }
}
