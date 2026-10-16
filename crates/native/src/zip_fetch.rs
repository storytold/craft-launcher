//! Portable-zip deltas: fetch only the entries that changed.
//!
//! A portable `.zip` carries everything a delta needs inside itself: the Central Directory at
//! the end lists every entry (name, CRC32, sizes, offsets), and entries are compressed
//! independently. Entries matching the retained previous installer by name, CRC32,
//! compressed size and method are copied byte-exact from it; only the rest (plus every tiny
//! local header) travels over HTTP range requests. Compressed bytes are copied opaquely and
//! never decompressed. Like every other fetch here, the assembly is SHA-256-verified against
//! the release's published checksum exactly like a full download (#90), and any failure
//! means the caller falls back to that full download.
//!
//! Retention (keeping one previous installer per portable install) lives here too: it is the
//! launcher's own decision in its own download folder, needing nothing published.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::http::{Http, Stop};
use crate::paths::Paths;
use artcraft_launcher_core::zip_cd;

/// How much of the file's tail is fetched to find the End-of-Central-Directory record.
const TAIL_MAX: u64 = 128 << 10;
/// Largest single local read (old installer chunks are small; the cap is only a backstop).
const MAX_READ: u64 = 1 << 30;
/// Extra bytes fetched past a local header's declared name, covering its extra field: the
/// response is trimmed to the header's true length, so this only ever wastes a kilobyte.
const HEADER_SLACK: u64 = 1024;

pub struct ZipJob<'a> {
    pub http: &'a Http,
    /// The new asset's download URL.
    pub base_url: &'a str,
    /// The new asset's size (0 when unknown: no tail math is possible).
    pub file_size: u64,
    /// The retained previous installer to copy unchanged entries from.
    pub old_path: &'a Path,
    pub tmp_path: &'a Path,
    pub expected_sha256: &'a str,
    pub stop: &'a Stop,
    pub progress: &'a mut dyn FnMut(u64, u64),
}

/// Three attempts, reporting the first error: a flaky chunk is retried, a deterministic
/// refusal (range ignored, file gone) keeps its meaning.
fn retrying(stop: &Stop, mut work: impl FnMut() -> Result<Vec<u8>, String>) -> Result<Vec<u8>, String> {
    let mut first_err: Option<String> = None;
    for _ in 0..3 {
        match work() {
            Ok(b) => return Ok(b),
            Err(err) => {
                stop.check()?;
                if first_err.is_none() {
                    first_err = Some(err);
                }
            }
        }
    }
    match first_err {
        Some(err) => Err(err),
        None => Err("delta fetch failed".into()),
    }
}

fn read_at(path: &Path, off: u64, len: u64) -> Result<Vec<u8>, String> {
    if len > MAX_READ {
        return Err("range too large".into());
    }
    let mut file = File::open(path).map_err(|e| format!("couldn't read {} ({e})", path.display()))?;
    file.seek(SeekFrom::Start(off)).map_err(|e| format!("couldn't read {} ({e})", path.display()))?;
    let mut buf = Vec::new();
    file.take(len).read_to_end(&mut buf).map_err(|e| format!("couldn't read {} ({e})", path.display()))?;
    if buf.len() as u64 != len {
        return Err(format!("short read of {}", path.display()));
    }
    Ok(buf)
}

/// A zip's validated Central Directory, read from disk: every entry is name-safe (no
/// traversal, absolute paths, streams, reserved names, duplicates, symlinks or bad
/// methods) before anything is unpacked, so a hostile or broken zip fails before it can
/// write outside its folder.
pub(crate) fn read_local_cd(path: &Path) -> Result<Vec<zip_cd::Entry>, String> {
    let len = std::fs::metadata(path).map_err(|e| format!("couldn't read {} ({e})", path.display()))?.len();
    let tail = read_at(path, len.saturating_sub(TAIL_MAX), len.min(TAIL_MAX))?;
    let eocd = zip_cd::find_eocd(&tail, len)?;
    let cd_bytes = read_at(path, eocd.cd_offset, eocd.cd_size)?;
    let entries = zip_cd::parse_cd(&cd_bytes, eocd.count)?;
    zip_cd::validate_entries(&entries)?;
    Ok(entries)
}

/// Where `old` keeps the previous installer for deltas: a subfolder of the downloads folder.
/// `clean_downloads` only touches files directly inside downloads, so this folder is left alone.
pub fn kept_dir(paths: &Paths, app_id: &str) -> PathBuf {
    paths.downloads().join(".kept").join(app_id)
}

/// The retained installer, when exactly one is kept. Anything else (none, several, a
/// directory) fails safe towards a full download.
pub fn find_kept(paths: &Paths, app_id: &str) -> Option<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(kept_dir(paths, app_id)).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
    (files.len() == 1).then(|| files.pop()).flatten()
}

/// Deletes the retained installer for a complete removal: the whole `.kept/<app>`
/// folder, which is launcher-owned cache (never user files). A missing folder is
/// already gone.
pub(crate) fn remove_kept(paths: &Paths, app_id: &str) -> Result<(), String> {
    let dir = kept_dir(paths, app_id);
    if !dir.exists() {
        return Ok(());
    }
    std::fs::remove_dir_all(&dir).map_err(|e| format!("couldn't remove the saved installer for {app_id} ({e})"))?;
    Ok(())
}

/// Keeps `file` as the next delta base, pruning anything previously kept. Best-effort: a
/// retention failure must never fail the install that just succeeded.
pub fn retain_installer(paths: &Paths, app_id: &str, file: &Path) {
    let dir = kept_dir(paths, app_id);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let name = match file.file_name() {
        Some(n) => n.to_os_string(),
        None => return,
    };
    if std::fs::rename(file, dir.join(&name)).is_err() {
        return;
    }
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            if e.path().file_name() != Some(name.as_os_str()) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// Delta-updates the retained installer to `tmp_path`: the new directory is range-fetched,
/// unchanged entries are copied from the retained file, the rest is range-fetched, and the
/// assembly (local blocks in order, directory tail verbatim) is SHA-256-verified. Progress
/// hears `(done, total)` bytes. Any error means the caller falls back to a full download,
/// except pause and cancel, which propagate unchanged.
pub fn fetch_zip_delta(job: ZipJob<'_>) -> Result<(), String> {
    let ZipJob { http, base_url, file_size, old_path, tmp_path, expected_sha256, stop, progress } = job;
    stop.check()?;
    if file_size == 0 {
        return Err("unknown file size".into());
    }
    // The new directory: tail, then the directory itself.
    let tail_len = file_size.min(TAIL_MAX);
    let tail = retrying(stop, || http.get_range(base_url, file_size - tail_len, file_size - 1))?;
    let eocd = zip_cd::find_eocd(&tail, file_size)?;
    let cd_bytes = retrying(stop, || http.get_range(base_url, eocd.cd_offset, eocd.cd_offset + eocd.cd_size - 1))?;
    let new_cd = zip_cd::parse_cd(&cd_bytes, eocd.count)?;
    // Fail fast before fetching entry data: a hostile directory never becomes ranges.
    zip_cd::validate_entries(&new_cd)?;
    // The old directory, entirely from disk.
    let old_len = std::fs::metadata(old_path).map_err(|e| format!("couldn't read {} ({e})", old_path.display()))?.len();
    let old_tail = read_at(old_path, old_len.saturating_sub(TAIL_MAX), old_len.min(TAIL_MAX))?;
    let old_eocd = zip_cd::find_eocd(&old_tail, old_len)?;
    let old_cd_bytes = read_at(old_path, old_eocd.cd_offset, old_eocd.cd_size)?;
    let old_cd = zip_cd::parse_cd(&old_cd_bytes, old_eocd.count)?;
    let plan = zip_cd::plan(&old_cd, &new_cd)?;
    if !plan.iter().any(|p| matches!(p, zip_cd::Need::Copy { .. })) {
        return Err("nothing reusable; a full download is cheaper".into());
    }
    let mut out = File::create(tmp_path).map_err(|e| format!("couldn't save the download ({e})"))?;
    let mut written: u64 = 0;
    progress(0, file_size);
    for (entry, need) in new_cd.iter().zip(plan.iter()) {
        stop.check()?;
        // Every entry's local header travels (it is tiny): its true length trims the slack,
        // and its declared values must match the directory (unless a data descriptor holds
        // the real CRC/size after the data instead).
        let name_len = entry.name.len() as u64;
        let head_end = (entry.local_offset + 30 + name_len + HEADER_SLACK).min(file_size.saturating_sub(1));
        let head = retrying(stop, || http.get_range(base_url, entry.local_offset, head_end))?;
        let lh = zip_cd::parse_local_header(&head)?;
        if lh.name_len as u64 != name_len || lh.method != entry.method {
            return Err("local header doesn't match the directory".into());
        }
        if !lh.has_descriptor && (lh.crc != entry.crc || lh.comp_size != entry.comp_size) {
            return Err("local header doesn't match the directory".into());
        }
        let hlen = 30u64 + lh.name_len as u64 + lh.extra_len as u64;
        let header = head.get(..hlen as usize).ok_or("local header longer than fetched")?;
        let data = match need {
            zip_cd::Need::Copy { old_index } => {
                let old_entry = old_cd.get(*old_index).ok_or("plan doesn't match the old directory")?;
                let old_head = read_at(old_path, old_entry.local_offset, 30)?;
                let old_lh = zip_cd::parse_local_header(&old_head)?;
                read_at(old_path, old_entry.local_offset + 30 + old_lh.name_len as u64 + old_lh.extra_len as u64, entry.comp_size)?
            }
            zip_cd::Need::Fetch => {
                if entry.comp_size == 0 {
                    Vec::new()
                } else {
                    let data_start = entry.local_offset + hlen;
                    retrying(stop, || http.get_range(base_url, data_start, data_start + entry.comp_size - 1))?
                }
            }
        };
        out.write_all(header).map_err(|e| format!("couldn't save the download ({e})"))?;
        out.write_all(&data).map_err(|e| format!("couldn't save the download ({e})"))?;
        written += hlen + data.len() as u64;
        progress(written, file_size);
    }
    // The directory tail verbatim: central directory plus end record.
    let cd_tail = retrying(stop, || http.get_range(base_url, eocd.cd_offset, file_size - 1))?;
    out.write_all(&cd_tail).map_err(|e| format!("couldn't save the download ({e})"))?;
    written += cd_tail.len() as u64;
    out.sync_all().map_err(|e| format!("couldn't save the download ({e})"))?;
    drop(out);
    progress(written, file_size);
    if written != file_size {
        let _ = std::fs::remove_file(tmp_path);
        return Err("assembled size doesn't match".into());
    }
    stop.check()?;
    if !crate::http::sha256_file(tmp_path).is_ok_and(|h| h.eq_ignore_ascii_case(expected_sha256)) {
        let _ = std::fs::remove_file(tmp_path);
        return Err("the delta update doesn't match its published checksum, so it wasn't installed".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kept_files_round_trip_and_prune() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        assert_eq!(find_kept(&paths, "wordcraft"), None);
        let src = dir.path().join("dl").join("wordcraft-0.6.1.zip");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, b"v1").unwrap();
        retain_installer(&paths, "wordcraft", &src);
        assert!(!src.exists(), "retained, not copied");
        assert_eq!(find_kept(&paths, "wordcraft").map(|p| std::fs::read(&p).unwrap()), Some(b"v1".to_vec()));
        // A newer keep replaces the older one: at most one base is ever stored.
        let src2 = dir.path().join("dl").join("wordcraft-0.6.2.zip");
        std::fs::write(&src2, b"v2").unwrap();
        retain_installer(&paths, "wordcraft", &src2);
        assert_eq!(find_kept(&paths, "wordcraft").map(|p| std::fs::read(&p).unwrap()), Some(b"v2".to_vec()));
        // A stray second file disables the delta base rather than confusing it.
        std::fs::write(kept_dir(&paths, "wordcraft").join("stray.zip"), b"x").unwrap();
        assert_eq!(find_kept(&paths, "wordcraft"), None);
    }

    #[test]
    fn kept_bases_go_away_completely() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let src = dir.path().join("dl").join("wordcraft-0.6.1.zip");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, b"v1").unwrap();
        retain_installer(&paths, "wordcraft", &src);
        assert!(find_kept(&paths, "wordcraft").is_some());
        remove_kept(&paths, "wordcraft").unwrap();
        assert_eq!(find_kept(&paths, "wordcraft"), None);
        assert!(!kept_dir(&paths, "wordcraft").exists());
        // Missing is already gone.
        remove_kept(&paths, "wordcraft").unwrap();
    }

    #[test]
    fn short_reads_fail_safe() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f.bin");
        std::fs::write(&f, b"12345").unwrap();
        assert_eq!(read_at(&f, 1, 3).unwrap(), b"234");
        assert!(read_at(&f, 4, 3).is_err(), "past the end");
        assert!(read_at(&f, 0, MAX_READ + 1).is_err(), "capped");
        assert!(read_at(&dir.path().join("missing"), 0, 1).is_err());
    }

    /// A minimal stored-entry zip, built by hand: local file, central directory, end record.
    fn stored_zip(name: &str, data: &[u8]) -> Vec<u8> {
        fn u16le(v: u16, out: &mut Vec<u8>) {
            out.extend_from_slice(&v.to_le_bytes());
        }
        fn u32le(v: u32, out: &mut Vec<u8>) {
            out.extend_from_slice(&v.to_le_bytes());
        }
        let mut zip = vec![];
        zip.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04]);
        u16le(20, &mut zip);
        u16le(0, &mut zip);
        u16le(0, &mut zip);
        u16le(0, &mut zip);
        u16le(0, &mut zip);
        u32le(0, &mut zip);
        u32le(data.len() as u32, &mut zip);
        u32le(data.len() as u32, &mut zip);
        u16le(name.len() as u16, &mut zip);
        u16le(0, &mut zip);
        zip.extend_from_slice(name.as_bytes());
        let local_len = zip.len() as u64;
        zip.extend_from_slice(data);
        let cd_off = zip.len() as u64;
        zip.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
        u16le(20, &mut zip);
        u16le(20, &mut zip);
        u16le(0, &mut zip);
        u16le(0, &mut zip);
        u16le(0, &mut zip);
        u16le(0, &mut zip);
        u32le(0, &mut zip);
        u32le(data.len() as u32, &mut zip);
        u32le(data.len() as u32, &mut zip);
        u16le(name.len() as u16, &mut zip);
        for _ in 0..4 {
            u16le(0, &mut zip);
        }
        u32le(0, &mut zip);
        u32le(0, &mut zip);
        zip.extend_from_slice(name.as_bytes());
        let cd_len = zip.len() as u64 - cd_off;
        zip.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06]);
        for _ in 0..2 {
            u16le(0, &mut zip);
        }
        u16le(1, &mut zip);
        u16le(1, &mut zip);
        u32le(cd_len as u32, &mut zip);
        u32le(cd_off as u32, &mut zip);
        u16le(0, &mut zip);
        assert_eq!(local_len, 30 + name.len() as u64, "local header math");
        zip
    }

    #[test]
    fn local_directories_validate_before_unpack() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.zip");
        std::fs::write(&good, stored_zip("app/file.txt", b"hi")).unwrap();
        let entries = read_local_cd(&good).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "app/file.txt");
        // A traversal entry fails before anything is unpacked, not after.
        let evil = dir.path().join("evil.zip");
        std::fs::write(&evil, stored_zip("../evil.txt", b"hi")).unwrap();
        assert!(read_local_cd(&evil).is_err(), "traversal");
        assert!(read_local_cd(&dir.path().join("missing.zip")).is_err());
    }
}
