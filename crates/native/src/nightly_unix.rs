//! Dedicated Unix nightly flavors. Stable tree/DMG installation is deliberately not reused:
//! it has integration hooks and different ownership/layout rules. This follows portable.rs's
//! bounded extraction, recorded-file backup, create-new writes and commit/rollback pattern.
//! Nothing in this module launches an app to inspect it. Authentication precedes extraction.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use crate::manifest::Record;
use crate::nightly_profile::Adapter;
use crate::paths::Paths;

#[cfg(test)]
#[path = "nightly_unix_tests.rs"]
mod tests;

const MAX_FILES: usize = 10_000;
const MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;

fn safe_relative(path: &Path) -> Result<(), String> {
    let text = path.to_str().ok_or("The nightly archive contains a non-textual path.")?;
    if text.is_empty()
        || text.contains(['\\', ':', '\0'])
        || path.components().any(|c| !matches!(c, Component::Normal(_)))
        || text.split('/').any(|part| part.is_empty() || part == "." || part == ".." || part.ends_with(['.', ' ']))
    {
        return Err("The nightly archive contains an unsafe path or alias.".into());
    }
    Ok(())
}

fn directory(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("The nightly directory is not absolute.".into());
    }
    for part in path.ancestors() {
        match std::fs::symlink_metadata(part) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                return Err("The nightly directory contains an alias or non-directory.".into());
            }
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.to_string()),
            _ => {}
        }
    }
    Ok(())
}

fn regular(path: &Path) -> Result<std::fs::Metadata, String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.nlink() != 1 {
        return Err("The nightly file is an alias, hardlink or non-regular file.".into());
    }
    Ok(meta)
}

fn same_file(path: &Path, identity: &std::fs::Metadata) -> bool {
    regular(path).is_ok_and(|meta| meta.dev() == identity.dev() && meta.ino() == identity.ino())
}

fn resolved_missing(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() || path.components().any(|part| matches!(part, Component::CurDir | Component::ParentDir)) {
        return Err("A protected storage path is not absolute and unambiguous.".into());
    }
    for ancestor in path.ancestors() {
        match std::fs::canonicalize(ancestor) {
            Ok(resolved) => return Ok(resolved.join(path.strip_prefix(ancestor).map_err(|e| e.to_string())?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if std::fs::symlink_metadata(ancestor).is_ok() {
                    return Err("A protected storage path contains a dangling alias.".into());
                }
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("A protected storage path has no existing ancestor.".into())
}

fn disjoint(paths: &[PathBuf], protected: &[PathBuf]) -> Result<(), String> {
    let protected = protected.iter().map(|path| resolved_missing(path)).collect::<Result<Vec<_>, _>>()?;
    for path in paths {
        let path = resolved_missing(path)?;
        if protected.iter().any(|stable| path.starts_with(stable) || stable.starts_with(&path)) {
            return Err("The nightly namespace overlaps PhotoCraft's stable data or installation; nothing was changed.".into());
        }
    }
    Ok(())
}

pub(crate) fn validate_namespaces(paths: &Paths) -> Result<(), String> {
    let home = dirs::home_dir().ok_or("No home directory is available for the nightly isolation check.")?;
    let mut protected = Vec::new();
    #[cfg(target_os = "linux")]
    for (variable, fallback) in
        [("XDG_CONFIG_HOME", ".config"), ("XDG_DATA_HOME", ".local/share"), ("XDG_CACHE_HOME", ".cache"), ("XDG_STATE_HOME", ".local/state")]
    {
        let base = std::env::var_os(variable).map(PathBuf::from).unwrap_or_else(|| home.join(fallback));
        if !base.is_absolute() {
            return Err("A stable XDG root is relative; nightly isolation cannot be established.".into());
        }
        protected.push(base.join("photocraft"));
    }
    #[cfg(target_os = "linux")]
    protected.extend([PathBuf::from("/usr/bin/photocraft"), PathBuf::from("/usr/local/bin/photocraft"), PathBuf::from("/opt/photocraft")]);
    #[cfg(target_os = "macos")]
    {
        for relative in [
            "Library/Application Support/Photocraft",
            "Library/Application Support/PhotoCraft",
            "Library/Caches/Photocraft",
            "Library/Caches/ai.storyteller.photocraft",
            "Library/Preferences/Photocraft",
            "Library/Caches/PhotoCraft",
            "Library/Preferences/ai.storyteller.photocraft.plist",
            "Library/Saved Application State/ai.storyteller.photocraft.savedState",
            "Library/WebKit/ai.storyteller.photocraft",
            "Library/HTTPStorages/ai.storyteller.photocraft",
            "Applications/PhotoCraft.app",
        ] {
            protected.push(home.join(relative));
        }
        protected.push(PathBuf::from("/Applications/PhotoCraft.app"));
    }
    if !cfg!(any(target_os = "linux", target_os = "macos")) {
        let _ = &home;
        return Err("No nightly isolation contract is staged for this Unix platform.".into());
    }
    // Read only the stable Launcher's existing record for an additional custom installation
    // fence. Never adopt it, detect an OS package or use it as a fallback installation.
    let stable_data = paths.data.parent().and_then(Path::parent).ok_or("The nightly data folder has no stable namespace owner.")?;
    protected.push(stable_data.join("apps/photocraft"));
    let stable_manifest = stable_data.join("installed.json");
    match std::fs::symlink_metadata(&stable_manifest) {
        Ok(_) => {
            let meta = regular(&stable_manifest)?;
            if meta.len() > 1024 * 1024 {
                return Err("The stable ownership record is too large to check safely.".into());
            }
            let mut bytes = Vec::new();
            std::fs::File::open(stable_manifest).and_then(|file| file.take(1024 * 1024 + 1).read_to_end(&mut bytes)).map_err(|e| e.to_string())?;
            if bytes.len() > 1024 * 1024 {
                return Err("The stable ownership record grew beyond its check limit.".into());
            }
            let manifest: crate::manifest::Manifest =
                serde_json::from_slice(&bytes).map_err(|_| "The stable ownership record cannot be checked safely.")?;
            if let Some(record) = manifest.apps.get("photocraft") {
                protected.push(record.path.clone());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    disjoint(&[paths.data.clone(), paths.cache.clone()], &protected)
}

fn notice(text: &str) -> bool {
    ["README.md", "LICENSE", "LICENSE-MIT", "LICENSE-APACHE", "COPYRIGHT", "NOTICE", "ATTRIBUTION.md"].contains(&text)
        || (text.starts_with("OFL-") && text.ends_with(".txt") && !text.contains('/') && text.len() > "OFL-.txt".len())
}

// No arbitrary executable/resource tree is admitted. If the real signed package needs a
// native framework or dependency, its inventory must be reviewed before this private row grows.
fn mac_entry(path: &Path) -> bool {
    const CONTENTS: &str = "PhotoCraft Nightly.app/Contents";
    let text = path.to_str().unwrap_or("");
    if ["PhotoCraft Nightly.app", CONTENTS].contains(&text) {
        return true;
    }
    let Ok(relative) = path.strip_prefix(CONTENTS) else { return false };
    let Some(relative) = relative.to_str() else { return false };
    [
        "Info.plist",
        "PkgInfo",
        "MacOS",
        "MacOS/PhotoCraft",
        "Resources",
        "Resources/photocraft-nightly.icns",
        "Resources/docs",
        "_CodeSignature",
        "_CodeSignature/CodeResources",
    ]
    .contains(&relative)
        || relative.strip_prefix("Resources/docs/").is_some_and(notice)
}

fn layout(adapter: Adapter, files: &[PathBuf], entries: &[PathBuf]) -> Result<(), String> {
    if !files.iter().any(|file| file == adapter.executable()) {
        return Err("The nightly archive is missing its source-owned GUI executable.".into());
    }
    let badge = match adapter {
        Adapter::PhotoCraftLinuxV1 => Path::new("photocraft-nightly.png"),
        Adapter::PhotoCraftMacV1 => Path::new("PhotoCraft Nightly.app/Contents/Resources/photocraft-nightly.icns"),
        Adapter::PhotoCraftWindowsV1 => return Err("This is not a Unix nightly flavor.".into()),
    };
    if !files.iter().any(|file| file == badge) {
        return Err("The nightly archive is missing its declared badge resource.".into());
    }
    for entry in entries {
        safe_relative(entry)?;
        let text = entry.to_str().ok_or("Invalid nightly path.")?;
        let allowed = match adapter {
            Adapter::PhotoCraftLinuxV1 => {
                entry.components().count() == 1 && (text == "photocraft" || text == "photocraft-nightly.png" || notice(text))
            }
            Adapter::PhotoCraftMacV1 => mac_entry(entry),
            Adapter::PhotoCraftWindowsV1 => false,
        };
        if !allowed {
            return Err("The nightly archive contains content outside its dedicated GUI flavor.".into());
        }
    }
    if adapter == Adapter::PhotoCraftMacV1
        && files.iter().any(|file| file.starts_with("PhotoCraft Nightly.app/Contents/MacOS") && file != adapter.executable())
    {
        return Err("The nightly bundle contains an unexpected command-line executable.".into());
    }
    Ok(())
}

struct Inventory {
    files: Vec<PathBuf>,
    entries: Vec<PathBuf>,
    seen: BTreeSet<String>,
    bytes: u64,
}
impl Inventory {
    fn new() -> Self {
        Self { files: Vec::new(), entries: Vec::new(), seen: BTreeSet::new(), bytes: 0 }
    }
    fn add(&mut self, path: PathBuf, size: u64, is_dir: bool) -> Result<PathBuf, String> {
        safe_relative(&path)?;
        if self.entries.len() >= MAX_FILES || !self.seen.insert(path.to_string_lossy().to_lowercase()) {
            return Err("The nightly archive has too many entries or colliding paths.".into());
        }
        self.bytes = self.bytes.checked_add(size).filter(|size| *size <= MAX_BYTES).ok_or("The nightly archive is too large.")?;
        self.entries.push(path.clone());
        if !is_dir {
            self.files.push(path.clone());
        }
        Ok(path)
    }
}

fn write_entry(input: &mut impl Read, size: u64, destination: &Path, mode: u32) -> Result<(), String> {
    std::fs::create_dir_all(destination.parent().ok_or("Missing archive parent.")?).map_err(|e| e.to_string())?;
    let mut output = std::fs::OpenOptions::new().write(true).create_new(true).open(destination).map_err(|e| e.to_string())?;
    let copied = std::io::copy(&mut input.take(size.saturating_add(1)), &mut output).map_err(|e| e.to_string())?;
    if copied != size {
        return Err("The nightly archive has an unexpected unpacked size.".into());
    }
    output.flush().map_err(|e| e.to_string())?;
    output.set_permissions(std::fs::Permissions::from_mode(if mode & 0o111 != 0 { 0o755 } else { 0o644 })).map_err(|e| e.to_string())
}

fn extract_tar(file: &Path, destination: &Path) -> Result<Inventory, String> {
    let input = std::fs::File::open(file).map_err(|e| e.to_string())?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(input));
    let mut inventory = Inventory::new();
    // Raw iteration refuses GNU/PAX extension headers before their bodies can be allocated.
    for entry in archive.entries().map_err(|e| e.to_string())?.raw(true) {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            return Err("The nightly tarball contains a link or special file.".into());
        }
        let size = entry.size();
        let mode = entry.header().mode().map_err(|e| e.to_string())?;
        let path = inventory.add(entry.path().map_err(|e| e.to_string())?.into_owned(), size, kind.is_dir())?;
        if kind.is_dir() {
            std::fs::create_dir_all(destination.join(path)).map_err(|e| e.to_string())?;
        } else {
            write_entry(&mut entry, size, &destination.join(path), mode)?;
        }
    }
    Ok(inventory)
}

#[cfg(target_os = "macos")]
fn extract_zip(file: &Path, destination: &Path) -> Result<Inventory, String> {
    let input = std::fs::File::open(file).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(input).map_err(|e| e.to_string())?;
    if archive.len() > MAX_FILES {
        return Err("The nightly ZIP has too many entries.".into());
    }
    let mut inventory = Inventory::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|e| e.to_string())?;
        let mode = entry.unix_mode().unwrap_or(0o100644);
        let is_dir = entry.is_dir();
        let file_type = mode & 0o170000;
        if !matches!(file_type, 0 | 0o100000 | 0o040000) || (file_type == 0o040000 && !is_dir) {
            return Err("The nightly ZIP contains a link or special file.".into());
        }
        let name = entry.name().map_err(|e| e.to_string())?;
        let path = PathBuf::from(if is_dir { name.trim_end_matches('/') } else { name });
        let size = entry.size();
        let path = inventory.add(path, size, is_dir)?;
        if is_dir {
            std::fs::create_dir_all(destination.join(path)).map_err(|e| e.to_string())?;
        } else {
            write_entry(&mut entry, size, &destination.join(path), mode)?;
        }
    }
    Ok(inventory)
}

/// Full prerelease identity only: numeric CFBundle* values cannot identify a nightly build.
#[cfg(any(target_os = "macos", test))]
fn exact_full_version(found: Option<&str>, expected: &str) -> bool {
    let Some(version) = artcraft_launcher_core::version::parse(expected) else { return false };
    version.pre.as_str().split('.').next() == Some("nightly") && found == Some(version.to_string().as_str())
}

#[cfg(target_os = "macos")]
fn verify_mac(root: &Path, version: &str) -> Result<(), String> {
    let bundle = root.join("PhotoCraft Nightly.app");
    let plist_path = bundle.join("Contents/Info.plist");
    if regular(&plist_path)?.len() > 1024 * 1024 {
        return Err("The nightly Info.plist is too large to check safely.".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(plist_path).and_then(|file| file.take(1024 * 1024 + 1).read_to_end(&mut bytes)).map_err(|e| e.to_string())?;
    if bytes.len() > 1024 * 1024 {
        return Err("The nightly Info.plist grew beyond its check limit.".into());
    }
    let value = plist::Value::from_reader(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let dict = value.as_dictionary().ok_or("The nightly bundle has no plist dictionary.")?;
    if dict.get("CFBundleIdentifier").and_then(plist::Value::as_string) != Some("ai.storyteller.photocraft.nightly")
        || dict.get("CFBundleExecutable").and_then(plist::Value::as_string) != Some("PhotoCraft")
        || !exact_full_version(dict.get("PhotoCraftVersion").and_then(plist::Value::as_string), version)
        || dict.contains_key("CFBundleDocumentTypes")
        || dict.contains_key("CFBundleURLTypes")
    {
        return Err("The nightly bundle identity, full PhotoCraftVersion or association policy does not match.".into());
    }
    let executable = root.join(Adapter::PhotoCraftMacV1.executable());
    let path = executable.to_str().ok_or("The nightly executable path cannot be checked losslessly.")?;
    let architecture = crate::process::run_with_output("/usr/bin/lipo", &["-archs", path], std::time::Duration::from_secs(60))?;
    if !architecture.success || architecture.stdout.trim() != "arm64" {
        return Err("The nightly bundle is not the source-owned aarch64 target.".into());
    }
    let app = artcraft_launcher_core::CraftApp::by_id("photocraft").ok_or("Unknown nightly app.")?;
    let team = app.mac_team.ok_or("No production macOS signing team is recorded for this nightly; signing remains pending.")?;
    // Pin only the Nightly identifier. Never accept the stable/former identifiers here.
    let identity = artcraft_launcher_core::CraftApp { bundle: Some("ai.storyteller.photocraft.nightly"), former_bundles: &[], ..*app };
    crate::install::macos_verify::verify_with(&identity, &bundle, team, crate::process::run_with_output)
}

fn verify_content(adapter: Adapter, root: &Path, files: &[PathBuf]) -> Result<(), String> {
    for file in files {
        if file != adapter.executable() && regular(&root.join(file))?.permissions().mode() & 0o111 != 0 {
            return Err("The nightly archive contains an unexpected executable notice or resource.".into());
        }
    }
    Ok(())
}

fn verify_program(adapter: Adapter, root: &Path, version: &str) -> Result<(), String> {
    let executable = root.join(adapter.executable());
    let meta = regular(&executable)?;
    if meta.permissions().mode() & 0o111 == 0 {
        return Err("The nightly GUI program is not executable.".into());
    }
    match adapter {
        Adapter::PhotoCraftLinuxV1 if cfg!(target_os = "linux") => {
            let mut header = [0u8; 20];
            std::fs::File::open(executable).and_then(|mut file| file.read_exact(&mut header)).map_err(|e| e.to_string())?;
            if &header[..7] != b"\x7fELF\x02\x01\x01" || header[18..20] != [62, 0] || !matches!(header[16..18], [2, 0] | [3, 0]) {
                return Err("The nightly GUI is not an x86_64 Linux ELF executable.".into());
            }
            Ok(())
        }
        #[cfg(target_os = "macos")]
        Adapter::PhotoCraftMacV1 => verify_mac(root, version),
        _ => {
            let _ = version;
            Err("This nightly adapter is not available on the current platform.".into())
        }
    }
}

pub(crate) fn validate_record(adapter: Adapter, paths: &Paths, record: &Record) -> Result<(), String> {
    validate_namespaces(paths)?;
    let root = paths.apps().join(adapter.app());
    directory(&root)?;
    if record.path != root.join(adapter.executable()) || record.files.len() > MAX_FILES || !record.files.contains(&record.path) {
        return Err("The nightly record does not own this program location.".into());
    }
    let mut seen = BTreeSet::new();
    let mut relative = Vec::new();
    for file in &record.files {
        let path = file.strip_prefix(&root).map_err(|_| "The nightly record escapes its program folder.")?;
        safe_relative(path)?;
        if !seen.insert(path.to_string_lossy().to_lowercase()) {
            return Err("The nightly record contains colliding files.".into());
        }
        directory(file.parent().ok_or("Missing recorded parent.")?)?;
        regular(file)?;
        relative.push(path.to_path_buf());
    }
    layout(adapter, &relative, &relative)?;
    verify_content(adapter, &root, &relative)?;
    verify_program(adapter, &root, &record.version)
}

fn verify_archive_digest(file: &Path, expected: &str) -> Result<(), String> {
    if expected.len() != 64 || !crate::http::sha256_file(file)?.eq_ignore_ascii_case(expected) {
        return Err("The authenticated Nightly archive changed before replacement; the installed copy was left unchanged.".into());
    }
    Ok(())
}

pub(crate) struct Package<'a> {
    pub adapter: Adapter,
    pub file: &'a Path,
    pub version: &'a str,
    pub previous: Option<&'a Record>,
    pub paths: &'a Paths,
    pub archive_sha256: String,
}

/// Roll back only files this transaction created. Restore through create_new so an arriving
/// file is never overwritten. A failed restoration keeps the backup and reports its path.
pub(crate) fn install(package: Package<'_>, commit: &mut dyn FnMut(&Record) -> Result<(), String>) -> Result<Record, String> {
    install_with_checkpoints(package, commit, &mut |_| Ok(()))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Checkpoint {
    ArchiveExtracted,
    RootReserved,
    BackupsSaved,
    BeforeCommit,
}

// The production entry point supplies a no-op observer. Tests inject a rename at an exact
// filesystem boundary; no sleeps, shared environment, descriptor input or global hook is used.
fn install_with_checkpoints(
    package: Package<'_>,
    commit: &mut dyn FnMut(&Record) -> Result<(), String>,
    checkpoint: &mut dyn FnMut(Checkpoint) -> Result<(), String>,
) -> Result<Record, String> {
    let Package { adapter, file, version, previous, paths, archive_sha256 } = package;
    verify_archive_digest(file, &archive_sha256)?;
    adapter.validate_profile(paths)?;
    if let Some(previous) = previous {
        adapter.validate_record(paths, previous)?;
    }
    let root = paths.apps().join(adapter.app());
    directory(&root)?;
    if previous.is_none() && std::fs::symlink_metadata(&root).is_ok() {
        return Err("The unowned existing nightly directory was left unchanged.".into());
    }
    let staging_parent = paths.cache.join("staging");
    directory(&staging_parent)?;
    std::fs::create_dir_all(&staging_parent).map_err(|e| e.to_string())?;
    let staging = tempfile::Builder::new().prefix("photocraft-nightly-").tempdir_in(staging_parent).map_err(|e| e.to_string())?;
    let unpacked = staging.path().join("unpacked");
    std::fs::create_dir(&unpacked).map_err(|e| e.to_string())?;
    let inventory = match adapter {
        Adapter::PhotoCraftLinuxV1 if cfg!(target_os = "linux") => extract_tar(file, &unpacked)?,
        #[cfg(target_os = "macos")]
        Adapter::PhotoCraftMacV1 => extract_zip(file, &unpacked)?,
        _ => return Err("The nightly archive format is not supported on this platform.".into()),
    };
    layout(adapter, &inventory.files, &inventory.entries)?;
    verify_content(adapter, &unpacked, &inventory.files)?;
    verify_program(adapter, &unpacked, version)?;
    checkpoint(Checkpoint::ArchiveExtracted)?;
    // Detect any mutation/replacement between authentication and extraction, before touching old files.
    verify_archive_digest(file, &archive_sha256)?;
    let old_files: Vec<_> = previous.map(|record| record.files.clone()).unwrap_or_default();
    for relative in &inventory.files {
        let target = root.join(relative);
        directory(target.parent().ok_or("Missing destination parent.")?)?;
        if std::fs::symlink_metadata(&target).is_ok() && !old_files.contains(&target) {
            return Err("The nightly update would overwrite an unrecorded file.".into());
        }
    }
    std::fs::create_dir_all(root.parent().ok_or("Missing program parent.")?).map_err(|e| e.to_string())?;
    let created_root = match std::fs::create_dir(&root) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && previous.is_some() => {
            directory(&root)?;
            false
        }
        Err(error) => return Err(error.to_string()),
    };
    let root_identity = std::fs::symlink_metadata(&root).map_err(|e| e.to_string())?;
    let backup = staging.path().join("backup");
    let record = Record {
        version: version.into(),
        kind: adapter.kind(),
        path: root.join(adapter.executable()),
        installed_at: crate::now(),
        files: inventory.files.iter().map(|p| root.join(p)).collect(),
        needs_root: false,
    };
    let original_directories = original_directories(&root, &old_files)?;
    let mut saved = Vec::new();
    let mut created = Vec::new();
    let mut created_directories = if created_root { vec![(root.clone(), root_identity.clone())] } else { Vec::new() };
    let transaction = (|| {
        checkpoint(Checkpoint::RootReserved)?;
        require_program_root(&root, &root_identity)?;
        for old in &old_files {
            require_program_root(&root, &root_identity)?;
            let identity = regular(old)?;
            let relative = old.strip_prefix(&root).map_err(|e| e.to_string())?;
            let copy = backup.join(relative);
            std::fs::create_dir_all(copy.parent().ok_or("Missing backup parent.")?).map_err(|e| e.to_string())?;
            std::fs::copy(old, &copy).map_err(|e| e.to_string())?;
            require_program_root(&root, &root_identity)?;
            if !same_file(old, &identity) {
                return Err("A recorded file changed during backup; it was left unchanged.".into());
            }
            std::fs::remove_file(old).map_err(|e| e.to_string())?;
            saved.push(relative.to_path_buf());
        }
        checkpoint(Checkpoint::BackupsSaved)?;
        require_program_root(&root, &root_identity)?;
        for relative in &inventory.files {
            require_program_root(&root, &root_identity)?;
            let target = root.join(relative);
            directory(target.parent().ok_or("Missing program parent.")?)?;
            create_program_parents(&root, target.parent().ok_or("Missing program parent.")?, &original_directories, &mut created_directories)?;
            let mut input = std::fs::File::open(unpacked.join(relative)).map_err(|e| e.to_string())?;
            require_program_root(&root, &root_identity)?;
            let mut output = std::fs::OpenOptions::new().write(true).create_new(true).open(&target).map_err(|e| e.to_string())?;
            created.push((target.clone(), output.metadata().map_err(|e| e.to_string())?));
            require_program_root(&root, &root_identity)?;
            std::io::copy(&mut input, &mut output).map_err(|e| e.to_string())?;
            output.set_permissions(input.metadata().map_err(|e| e.to_string())?.permissions()).map_err(|e| e.to_string())?;
            require_program_root(&root, &root_identity)?;
        }
        require_program_root(&root, &root_identity)?;
        adapter.validate_record(paths, &record)?;
        checkpoint(Checkpoint::BeforeCommit)?;
        require_program_root(&root, &root_identity)?;
        commit(&record)
    })();
    if let Err(error) = transaction {
        let rollback = rollback(&root, &root_identity, &original_directories, &backup, &created, &saved);
        if let Err(restore) = rollback {
            let recovery = staging.keep();
            return Err(format!("{error}; rollback could not finish ({restore}). Recovery files: {}", recovery.display()));
        }
        // Only empty directories created by us may disappear. Foreign/arriving files survive.
        for (directory, identity) in created_directories.iter().rev() {
            if same_directory(directory, identity) {
                let _ = std::fs::remove_dir(directory);
            }
        }
        return Err(error);
    }
    Ok(record)
}

/// Point-in-time refusal, not an atomic directory capability: pathname operations still
/// have a check/use window. Never describe this guard as race-free against concurrent renames.
fn require_program_root(root: &Path, identity: &std::fs::Metadata) -> Result<(), String> {
    directory(root)?;
    if !same_directory(root, identity) {
        return Err("The reserved nightly program directory changed identity; the replacement was not adopted.".into());
    }
    Ok(())
}

fn same_directory(path: &Path, identity: &std::fs::Metadata) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink() && meta.dev() == identity.dev() && meta.ino() == identity.ino())
}

fn create_program_parents(
    root: &Path,
    parent: &Path,
    original: &std::collections::BTreeMap<PathBuf, std::fs::Metadata>,
    created: &mut Vec<(PathBuf, std::fs::Metadata)>,
) -> Result<(), String> {
    // The root is an ownership boundary even when parent == root and the loop is empty.
    let identity = original
        .get(root)
        .or_else(|| created.iter().find(|(path, _)| path == root).map(|(_, identity)| identity))
        .ok_or("The program root has no transaction ownership identity.")?;
    require_program_root(root, identity)?;
    let relative = parent.strip_prefix(root).map_err(|e| e.to_string())?;
    let mut path = root.to_path_buf();
    for component in relative.components() {
        path.push(component);
        match std::fs::create_dir(&path) {
            Ok(()) => created.push((path.clone(), std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                directory(&path)?;
                let identity = original.get(&path).or_else(|| created.iter().find(|(created, _)| created == &path).map(|(_, identity)| identity));
                if identity.is_none_or(|identity| !same_directory(&path, identity)) {
                    return Err("A program parent directory is unowned or changed identity; it was left unchanged.".into());
                }
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

fn original_directories(root: &Path, files: &[PathBuf]) -> Result<std::collections::BTreeMap<PathBuf, std::fs::Metadata>, String> {
    let mut directories = std::collections::BTreeMap::new();
    for file in files {
        for parent in file.ancestors().skip(1).take_while(|parent| parent.starts_with(root)) {
            directory(parent)?;
            if !directories.contains_key(parent) {
                directories.insert(parent.to_path_buf(), std::fs::symlink_metadata(parent).map_err(|e| e.to_string())?);
            }
        }
    }
    Ok(directories)
}

fn rollback(
    root: &Path,
    root_identity: &std::fs::Metadata,
    original_directories: &std::collections::BTreeMap<PathBuf, std::fs::Metadata>,
    backup: &Path,
    created: &[(PathBuf, std::fs::Metadata)],
    saved: &[PathBuf],
) -> Result<(), String> {
    if !same_directory(root, root_identity) {
        return Err("The program directory changed identity; the arriving directory was left untouched.".into());
    }
    if original_directories.iter().any(|(path, identity)| !same_directory(path, identity) || directory(path).is_err()) {
        return Err("A program parent directory changed identity; rollback did not write into it.".into());
    }
    for (target, identity) in created {
        if same_file(target, identity) {
            std::fs::remove_file(target).map_err(|e| e.to_string())?;
        }
    }
    for relative in saved {
        let source = backup.join(relative);
        let mut input = std::fs::File::open(&source).map_err(|e| e.to_string())?;
        let mut output = std::fs::OpenOptions::new().write(true).create_new(true).open(root.join(relative)).map_err(|e| e.to_string())?;
        std::io::copy(&mut input, &mut output).map_err(|e| e.to_string())?;
        output.set_permissions(input.metadata().map_err(|e| e.to_string())?.permissions()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn prune_parents(root: &Path, files: &[PathBuf], remove_root: bool) {
    for file in files {
        let mut directory = file.parent();
        while let Some(path) = directory.filter(|path| path.starts_with(root) && (remove_root || *path != root)) {
            if std::fs::remove_dir(path).is_err() || path == root {
                break;
            }
            directory = path.parent();
        }
    }
}

pub(crate) fn remove(adapter: Adapter, record: &Record, paths: &Paths) -> Result<(), String> {
    adapter.validate_record(paths, record)?;
    for file in &record.files {
        regular(file)?;
        std::fs::remove_file(file).map_err(|e| e.to_string())?;
    }
    prune_parents(&paths.apps().join(adapter.app()), &record.files, true);
    Ok(())
}
