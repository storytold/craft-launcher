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
pub(crate) mod tests;

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

/// The XDG base directory spec: an unset, empty or relative value means the default under
/// `$HOME`. The effective absolute directory is still checked for overlap by the caller.
#[cfg(any(target_os = "linux", test))]
fn xdg_root(value: Option<std::ffi::OsString>, home: &Path, fallback: &str) -> PathBuf {
    value.map(PathBuf::from).filter(|path| path.is_absolute()).unwrap_or_else(|| home.join(fallback))
}

pub(crate) fn validate_namespaces(paths: &Paths) -> Result<(), String> {
    let home = dirs::home_dir().ok_or("No home directory is available for the nightly isolation check.")?;
    let mut protected = Vec::new();
    #[cfg(target_os = "linux")]
    for (variable, fallback) in
        [("XDG_CONFIG_HOME", ".config"), ("XDG_DATA_HOME", ".local/share"), ("XDG_CACHE_HOME", ".cache"), ("XDG_STATE_HOME", ".local/state")]
    {
        let base = xdg_root(std::env::var_os(variable), &home, fallback);
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
        // The notarization ticket `stapler staple` writes into a notarized bundle.
        "CodeResources",
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

fn write_entry(
    input: &mut impl Read,
    size: u64,
    destination: &Path,
    preparation: &mut crate::nightly_transaction::Preparation<'_>,
    mode: u32,
) -> Result<(), String> {
    let mut output = preparation.create(&Path::new("unpacked").join(destination))?;
    let copied = std::io::copy(&mut input.take(size.saturating_add(1)), &mut output).map_err(|e| e.to_string())?;
    if copied != size {
        return Err("The nightly archive has an unexpected unpacked size.".into());
    }
    output.flush().map_err(|e| e.to_string())?;
    output.set_permissions(std::fs::Permissions::from_mode(if mode & 0o111 != 0 { 0o755 } else { 0o644 })).map_err(|e| e.to_string())?;
    output.sync_all().map_err(|e| e.to_string())
}

fn extract_tar(file: &Path, preparation: &mut crate::nightly_transaction::Preparation<'_>) -> Result<Inventory, String> {
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
            preparation.directories(&Path::new("unpacked").join(path))?;
        } else {
            write_entry(&mut entry, size, &path, preparation, mode)?;
        }
    }
    Ok(inventory)
}

#[cfg(target_os = "macos")]
fn extract_zip(file: &Path, preparation: &mut crate::nightly_transaction::Preparation<'_>) -> Result<Inventory, String> {
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
        let path = PathBuf::from(if is_dir { name.trim_end_matches('/') } else { &*name });
        let size = entry.size();
        let path = inventory.add(path, size, is_dir)?;
        if is_dir {
            preparation.directories(&Path::new("unpacked").join(path))?;
        } else {
            write_entry(&mut entry, size, &path, preparation, mode)?;
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
fn verify_mac_metadata(root: &Path, version: &str) -> Result<(), String> {
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
    Ok(())
}
#[cfg(target_os = "macos")]
fn verify_mac(root: &Path, version: &str) -> Result<(), String> {
    verify_mac_metadata(root, version)?;
    let bundle = root.join("PhotoCraft Nightly.app");
    let executable = root.join(Adapter::PhotoCraftMacV1.executable());
    let path = executable.to_str().ok_or("The nightly executable path cannot be checked losslessly.")?;
    let architecture = crate::process::run_with_output("/usr/bin/lipo", &["-archs", path], std::time::Duration::from_secs(60))?;
    if !architecture.success || architecture.stdout.trim() != "arm64" {
        return Err("The nightly bundle is not the source-owned aarch64 target.".into());
    }
    verify_mac_signature(&bundle, crate::process::run_with_output)
}

/// The stable Mac checks (integrity, Developer ID identity, notarization, Gatekeeper policy),
/// with the source-owned Nightly team and only the Nightly bundle identifier.
#[cfg(target_os = "macos")]
fn verify_mac_signature(
    bundle: &Path,
    run: impl FnMut(&str, &[&str], std::time::Duration) -> Result<crate::process::RunOutput, String>,
) -> Result<(), String> {
    let app = artcraft_launcher_core::CraftApp::by_id("photocraft").ok_or("Unknown nightly app.")?;
    let team = crate::nightly_profile::mac_team()?;
    // Pin only the Nightly identifier. Never accept the stable/former identifiers here.
    let identity = artcraft_launcher_core::CraftApp { bundle: Some("ai.storyteller.photocraft.nightly"), former_bundles: &[], ..*app };
    crate::install::macos_verify::verify_with(&identity, bundle, team, run)
}

fn verify_content(adapter: Adapter, root: &Path, files: &[PathBuf]) -> Result<(), String> {
    for file in files {
        if file != adapter.executable() && regular(&root.join(file))?.permissions().mode() & 0o111 != 0 {
            return Err("The nightly archive contains an unexpected executable notice or resource.".into());
        }
    }
    Ok(())
}

fn verify_structure(adapter: Adapter, root: &Path, version: &str) -> Result<(), String> {
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
        Adapter::PhotoCraftMacV1 => verify_mac_metadata(root, version),
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
    verify_structure(adapter, &root, &record.version)?;
    // Ownership does not depend on today's Gatekeeper/trust approval. Intact revoked copies
    // remain detectable, revealable and removable. Install and every Open approve separately.
    Ok(())
}

fn verify_program(adapter: Adapter, root: &Path, version: &str) -> Result<(), String> {
    verify_structure(adapter, root, version)?;
    #[cfg(target_os = "macos")]
    if adapter == Adapter::PhotoCraftMacV1 {
        verify_mac(root, version)?;
    }
    Ok(())
}

pub(crate) fn approve(adapter: Adapter, paths: &Paths, record: &Record) -> Result<(), String> {
    verify_program(adapter, &paths.apps().join(adapter.app()), &record.version)
}

pub(crate) fn validate_layout(adapter: Adapter, files: &[PathBuf]) -> Result<(), String> {
    layout(adapter, files, files)
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
    pub lease: Option<&'a crate::nightly_transaction::Lease>,
}

pub(crate) use crate::nightly_transaction::Checkpoint;

pub(crate) fn install(package: Package<'_>, commit: &mut dyn FnMut(&Record) -> Result<(), String>) -> Result<Record, String> {
    install_with_checkpoints(package, commit, &mut |_| Ok(()))
}

pub(crate) fn install_with_checkpoints(
    package: Package<'_>,
    commit: &mut dyn FnMut(&Record) -> Result<(), String>,
    checkpoint: &mut dyn FnMut(Checkpoint) -> Result<(), String>,
) -> Result<Record, String> {
    let Package { adapter, file, version, previous, paths, archive_sha256, lease } = package;
    verify_archive_digest(file, &archive_sha256)?;
    adapter.validate_profile(paths)?;
    if let Some(previous) = previous {
        adapter.validate_record(paths, previous)?;
    }
    let own_lease;
    let lease = match lease {
        Some(lease) => lease,
        None => {
            own_lease = crate::nightly_transaction::Lease::acquire(paths, adapter)?;
            &own_lease
        }
    };
    let mut preparation = lease.prepare()?;
    let root = paths.apps().join(adapter.app());
    let prepared = (|| {
        preparation.directories(Path::new("unpacked"))?;
        let inventory = match adapter {
            Adapter::PhotoCraftLinuxV1 if cfg!(target_os = "linux") => extract_tar(file, &mut preparation)?,
            #[cfg(target_os = "macos")]
            Adapter::PhotoCraftMacV1 => extract_zip(file, &mut preparation)?,
            _ => return Err("The Nightly archive format is unsupported on this platform.".into()),
        };
        let unpacked = preparation.path().join("unpacked");
        layout(adapter, &inventory.files, &inventory.entries)?;
        verify_content(adapter, &unpacked, &inventory.files)?;
        verify_program(adapter, &unpacked, version)?;
        checkpoint(Checkpoint::ArchiveExtracted)?;
        verify_archive_digest(file, &archive_sha256)?;
        let record = Record {
            version: version.into(),
            kind: adapter.kind(),
            path: root.join(adapter.executable()),
            installed_at: crate::now(),
            files: inventory.files.iter().map(|file| root.join(file)).collect(),
            needs_root: false,
        };
        preparation.seal(previous, Some(&record))?;
        Ok(record)
    })();
    let record = match prepared {
        Ok(record) => record,
        Err(error) => return Err(preparation.abort(error)),
    };
    let mut held = None;
    let mut committed = false;
    let transaction = (|| {
        preparation.apply(&mut held, checkpoint)?;
        adapter.validate_record(paths, &record)?;
        approve(adapter, paths, &record)?; // staged AND installed approval, not just ownership
        checkpoint(Checkpoint::BeforeCommit)?;
        preparation.verify_installed()?;
        commit(&record)?;
        committed = true;
        checkpoint(Checkpoint::AfterCommit)
    })();
    preparation.finish(transaction, held, committed)?;
    Ok(record)
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn remove_with_checkpoints(
    adapter: Adapter,
    record: &Record,
    paths: &Paths,
    checkpoint: &mut dyn FnMut(Checkpoint) -> Result<(), String>,
) -> Result<(), String> {
    adapter.validate_record(paths, record)?;
    let lease = crate::nightly_transaction::Lease::acquire(paths, adapter)?;
    crate::nightly_transaction::remove(&lease, record, &mut || Ok(()), checkpoint)
}
