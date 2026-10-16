//! User-added sources use a separate namespace and only isolated, non-elevated packages.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use artcraft_launcher_core::community::{CommunityAction, CommunitySource, CommunityStatus};
use artcraft_launcher_core::protocol::JobPhase;
use artcraft_launcher_core::{Os, Platform, Release};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::http::{Fetched, Http, Stop, Transfer};
use crate::paths::Paths;

const MAX_EXPANDED: u64 = 4 * 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 20_000;

#[derive(Serialize, Deserialize)]
struct Receipt {
    repo: String,
    package: String,
    version: String,
    directory: String,
    executable: String,
}

fn root(paths: &Paths, source: &CommunitySource) -> PathBuf {
    let key = format!("{:x}", Sha256::digest(source.key().as_bytes()));
    paths.data.join("community").join(key)
}

fn receipt(root: &Path, source: &CommunitySource) -> Option<Receipt> {
    let file = fs::File::open(root.join("installed.json")).ok()?;
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 64 * 1024 {
        return None;
    }
    let r: Receipt = serde_json::from_slice(&bytes).ok()?;
    (r.repo == source.key()
        && r.package == source.package
        && r.directory.starts_with("version-")
        && artcraft_launcher_core::release::valid_file_name(&r.directory)
        && relative_path(&r.executable).is_ok()
        && root.join("versions").join(&r.directory).join("app").join(&r.executable).is_file())
    .then_some(r)
}

pub(crate) fn run(
    paths: &Paths,
    http: Option<&Http>,
    source: &CommunitySource,
    action: CommunityAction,
    stop: &Stop,
    phase: &mut dyn FnMut(JobPhase),
) -> CommunityStatus {
    let mut status = CommunityStatus::default();
    let result = (|| {
        source.validate()?;
        if !source.enabled {
            return Err("This source is disabled.".into());
        }
        let root = root(paths, source);
        status.installed = receipt(&root, source).map(|r| r.version);
        if action == CommunityAction::Launch {
            return launch(&root, source);
        }
        let http = http.ok_or("HTTPS is unavailable.")?;
        let release = latest(http, source)?;
        status.release = release.clone();
        if action == CommunityAction::Install {
            let release = release.ok_or("This repository has no published release.")?;
            install(&root, http, source, &release, stop, phase)?;
            status.installed = Some(release.version_label());
        }
        Ok(())
    })();
    status.error = result.err();
    status
}

fn latest(http: &Http, source: &CommunitySource) -> Result<Option<Release>, String> {
    match http.get_api(&format!("https://api.github.com/repos/{}/releases/latest", source.repo), None)? {
        Fetched::Body { text, .. } => source.parse_release(&text).map(Some),
        Fetched::NotFound => Ok(None),
        Fetched::RateLimited { .. } => Err("GitHub's request limit was reached. Try again later.".into()),
        Fetched::NotModified => Err("GitHub returned no release data.".into()),
    }
}

fn install(
    root: &Path,
    http: &Http,
    source: &CommunitySource,
    release: &Release,
    stop: &Stop,
    phase: &mut dyn FnMut(JobPhase),
) -> Result<(), String> {
    let platform = Platform::current().ok_or("This platform cannot install community packages.")?;
    let asset = source.package_asset(release, platform).ok_or("No compatible isolated package: use a Windows portable ZIP or Linux AppImage.")?;
    // The checksum must be published by this repository for this exact tag and filename.
    let sums_url = format!("{}/releases/download/{}/SHA256SUMS.txt", source.url(), release.tag);
    let sums = http.get_text(&sums_url)?.ok_or("The release must publish SHA256SUMS.txt.")?;
    let hash = checksum(&sums, &asset.name)?;
    fs::create_dir_all(root.join("versions")).map_err(|e| e.to_string())?;
    let staged = tempfile::Builder::new().prefix("install-").tempdir_in(root).map_err(|e| e.to_string())?;
    let download = staged.path().join("download");
    http.download(&asset.url, &download, asset.size, &hash, stop, &mut |transfer| {
        phase(match transfer {
            Transfer::Bytes { done, total } => JobPhase::Downloading { done, total },
            Transfer::Reconnecting { done, total } => JobPhase::Reconnecting { done, total },
        });
    })?;
    stop.check()?;
    phase(JobPhase::Installing);
    let files = staged.path().join("files");
    fs::create_dir(&files).map_err(|e| e.to_string())?;
    let executable = match platform.os {
        Os::Windows => unpack_zip(&download, &files, &source.package, stop)?,
        Os::Linux => {
            let name = format!("{}.AppImage", source.package);
            fs::copy(&download, files.join(&name)).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(files.join(&name), fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
            }
            name
        }
        _ => return Err("This platform cannot install community packages.".into()),
    };
    stop.check()?;
    let record =
        Receipt { repo: source.key(), package: source.package.clone(), version: release.version_label(), directory: String::new(), executable };
    publish(root, &files, record)
}

fn publish(root: &Path, files: &Path, mut record: Receipt) -> Result<(), String> {
    // A unique owned directory keeps interrupted attempts retryable and never overwrites an
    // older installation, which may contain app-created files. Only the receipt is replaced.
    let version = tempfile::Builder::new().prefix("version-").tempdir_in(root.join("versions")).map_err(|e| e.to_string())?;
    record.directory = version.path().file_name().and_then(|n| n.to_str()).ok_or("Invalid installation directory.")?.to_string();
    fs::rename(files, version.path().join("app")).map_err(|e| format!("Couldn't keep the new package ({e})"))?;
    let bytes = serde_json::to_vec(&record).map_err(|e| e.to_string())?;
    let mut tmp = tempfile::NamedTempFile::new_in(root).map_err(|e| e.to_string())?;
    tmp.write_all(&bytes).and_then(|_| tmp.as_file().sync_all()).map_err(|e| e.to_string())?;
    tmp.persist(root.join("installed.json")).map_err(|e| e.to_string())?;
    let _ = version.keep();
    Ok(())
}

fn launch(root: &Path, source: &CommunitySource) -> Result<(), String> {
    let record = receipt(root, source).ok_or("This community app is not installed.")?;
    let version = root.join("versions").join(record.directory).join("app");
    let executable = version.join(record.executable);
    let home = root.join("home");
    for name in ["config", "data", "cache"] {
        fs::create_dir_all(home.join(name)).map_err(|e| e.to_string())?;
    }
    crate::process::command(&executable)
        .current_dir(executable.parent().unwrap_or(&version))
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("APPDATA", home.join("config"))
        .env("LOCALAPPDATA", home.join("data"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Couldn't open the community app ({e})"))
}

fn hex_digest(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

fn checksum(sums: &str, name: &str) -> Result<String, String> {
    let matches: Vec<_> = artcraft_launcher_core::release::parse_sha256sums(sums).into_iter().filter(|(_, file)| file == name).collect();
    match matches.as_slice() {
        [(hash, _)] if hex_digest(hash) => Ok(hash.clone()),
        _ => Err("The package needs one unambiguous SHA256SUMS.txt entry.".into()),
    }
}

fn relative_path(name: &str) -> Result<PathBuf, String> {
    if name.len() > 1024
        || name.split('/').any(|part| !artcraft_launcher_core::release::valid_file_name(part) || reserved_name(part) || part.ends_with(['.', ' ']))
    {
        return Err("The archive contains an unsafe path.".into());
    }
    Ok(name.split('/').collect())
}

fn reserved_name(part: &str) -> bool {
    let stem = part.split('.').next().unwrap_or_default().to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"))
        })
}

fn copy_entry(input: &mut impl Read, output: &mut impl Write, limit: u64, stop: &Stop) -> Result<(), String> {
    let mut input = input.take(limit.saturating_add(1));
    let mut buffer = [0u8; 64 * 1024];
    let mut written = 0;
    loop {
        stop.check()?;
        let n = input.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        written += n as u64;
        if written > limit {
            return Err("The archive entry has an unexpected length.".into());
        }
        output.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
    }
    if written != limit {
        return Err("The archive entry has an unexpected length.".into());
    }
    Ok(())
}

fn unpack_zip(file: &Path, destination: &Path, package: &str, stop: &Stop) -> Result<String, String> {
    let mut archive = zip::ZipArchive::new(fs::File::open(file).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    if archive.len() > MAX_ENTRIES {
        return Err("The archive contains too many files.".into());
    }
    let mut total = 0u64;
    let mut executables = Vec::new();
    for index in 0..archive.len() {
        stop.check()?;
        let mut entry = archive.by_index(index).map_err(|e| e.to_string())?;
        if entry.is_symlink() {
            return Err("Archive links are not supported.".into());
        }
        let name = entry.name().map_err(|e| e.to_string())?.trim_end_matches('/').to_string();
        let path = relative_path(&name)?;
        let target = destination.join(&path);
        if entry.is_dir() {
            fs::create_dir_all(&target).map_err(|e| e.to_string())?;
            continue;
        }
        total = total.checked_add(entry.size()).filter(|n| *n <= MAX_EXPANDED).ok_or("The expanded package is too large.")?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut out = fs::OpenOptions::new().write(true).create_new(true).open(&target).map_err(|e| e.to_string())?;
        let limit = entry.size();
        copy_entry(&mut entry, &mut out, limit, stop)?;
        if path.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(&format!("{package}.exe"))) {
            executables.push(name);
        }
    }
    match executables.as_slice() {
        // Preserve portable markers: Windows Known Folder APIs can ignore APPDATA overrides.
        // The publisher must keep its settings beside the portable app or honor the environment.
        [executable] => Ok(executable.clone()),
        _ => Err("The portable package must contain exactly one matching app executable.".into()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn source_paths_do_not_collide_with_official_apps_or_other_forks() {
        let paths = Paths::under(Path::new("test"));
        let a = CommunitySource::new("one/pdfcraft", "PDF", "pdfcraft").unwrap();
        let b = CommunitySource::new("two/pdfcraft", "PDF", "pdfcraft").unwrap();
        assert_ne!(root(&paths, &a), root(&paths, &b));
        assert!(!root(&paths, &a).starts_with(paths.apps()));
        assert_eq!(root(&paths, &a), root(&paths, &CommunitySource::new("ONE/PDFCRAFT", "Other", "pdfcraft").unwrap()));
    }

    #[test]
    fn checksums_and_archive_paths_fail_closed() {
        let hash = "a".repeat(64);
        assert_eq!(checksum(&format!("{hash}  app.zip"), "app.zip").unwrap(), hash);
        assert!(checksum("", "app.zip").is_err());
        assert!(checksum(&format!("{hash}  app.zip\n{hash}  app.zip"), "app.zip").is_err());
        for name in
            ["../outside", "/outside", "C:/outside", "a\\b", "a/../b", "a/./b", "a/file:stream", "a/", "file. ", "NUL", "a/COM1.txt", "aux.dll"]
        {
            assert!(relative_path(name).is_err(), "{name}");
        }
    }

    fn archive(dir: &Path, entries: &[(&str, &[u8])]) -> PathBuf {
        let file = dir.join("test.zip");
        let mut zip = zip::ZipWriter::new(fs::File::create(&file).unwrap());
        for (name, bytes) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
        file
    }

    #[test]
    fn portable_archive_is_confined_and_has_an_unambiguous_executable() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("files");
        fs::create_dir(&dest).unwrap();
        let file = archive(dir.path(), &[("bundle/pdfcraft.exe", b"synthetic"), ("bundle/portable.txt", b"")]);
        assert_eq!(unpack_zip(&file, &dest, "pdfcraft", &Stop::default()).unwrap(), "bundle/pdfcraft.exe");
        assert!(dest.join("bundle/portable.txt").exists());
        let file = archive(dir.path(), &[("../escaped", b"no")]);
        assert!(unpack_zip(&file, &dest, "pdfcraft", &Stop::default()).is_err());
        assert!(!dir.path().join("escaped").exists());
    }

    #[test]
    fn extraction_limits_cancellation_and_ambiguous_executables_fail_closed() {
        let stop = Stop::default();
        let mut out = Vec::new();
        assert!(copy_entry(&mut &b"too long"[..], &mut out, 3, &stop).is_err());
        assert!(out.is_empty());
        assert!(copy_entry(&mut &b"short"[..], &mut out, 9, &stop).is_err());
        out.clear();
        stop.cancel();
        assert!(copy_entry(&mut &b"data"[..], &mut out, 4, &stop).is_err());
        assert!(out.is_empty());
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("files");
        fs::create_dir(&dest).unwrap();
        let file = archive(dir.path(), &[("one/pdfcraft.exe", b"one"), ("two/pdfcraft.exe", b"two")]);
        assert!(unpack_zip(&file, &dest, "pdfcraft", &Stop::default()).is_err());
        assert!(!root(&Paths::under(dir.path()), &CommunitySource::new("example/pdfcraft", "PDF", "pdfcraft").unwrap()).exists());
    }

    #[test]
    fn publishing_updates_preserves_old_files_and_failed_attempts_are_retryable() {
        let dir = tempfile::tempdir().unwrap();
        let source = CommunitySource::new("example/pdfcraft", "PDF", "pdfcraft").unwrap();
        let root = root(&Paths::under(dir.path()), &source);
        fs::create_dir_all(root.join("versions")).unwrap();
        let make = |version: &str| {
            let files = tempfile::tempdir_in(dir.path()).unwrap();
            fs::write(files.path().join("pdfcraft.exe"), version).unwrap();
            let r = Receipt {
                repo: source.key(),
                package: source.package.clone(),
                version: version.into(),
                directory: String::new(),
                executable: "pdfcraft.exe".into(),
            };
            (files, r)
        };
        let (files, r) = make("1.0.0");
        publish(&root, files.path(), r).unwrap();
        let old = receipt(&root, &source).unwrap();
        let old_file = root.join("versions").join(old.directory).join("app/pdfcraft.exe");
        let (files, r) = make("2.0.0");
        publish(&root, files.path(), r).unwrap();
        assert_eq!(receipt(&root, &source).unwrap().version, "2.0.0");
        assert_eq!(fs::read_to_string(old_file).unwrap(), "1.0.0");
        // Force receipt publication to fail. Only the new staging tree is cleaned up.
        fs::remove_file(root.join("installed.json")).unwrap();
        fs::create_dir(root.join("installed.json")).unwrap();
        let (files, r) = make("3.0.0");
        assert!(publish(&root, files.path(), r).is_err());
        assert_eq!(fs::read_dir(root.join("versions")).unwrap().count(), 2);
        fs::remove_dir(root.join("installed.json")).unwrap();
        let (files, r) = make("3.0.0");
        publish(&root, files.path(), r).unwrap();
        assert_eq!(receipt(&root, &source).unwrap().version, "3.0.0");
    }
}
