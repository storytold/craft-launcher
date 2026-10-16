//! ZIP central-directory deltas for Windows portable updates.
//!
//! A portable `.zip` carries everything a delta needs inside itself: the Central Directory at
//! the end lists every entry (name, CRC32, sizes, offsets), and entries are compressed
//! independently. Entries matching the retained previous installer by name, CRC32,
//! compressed size and method are copied byte-exact from it; only the rest is range-fetched.
//! Everything here is pure: fetching and assembly live in `native`, which SHA-256-verifies
//! the result exactly like a full download (#90), so a wrong guess below can only fall back,
//! never install wrong bytes. Compressed bytes are copied opaquely and never decompressed.

use std::collections::HashMap;

/// One Central Directory entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub crc: u32,
    pub comp_size: u64,
    pub uncomp_size: u64,
    pub local_offset: u64,
    pub method: u16,
    /// DOS/external file attributes (symlink detection lives in the Unix mode bits).
    pub external_attrs: u32,
}

/// Compression methods a delta copies opaquely (stored, deflated).
pub fn supported_method(method: u16) -> bool {
    method == 0 || method == 8
}

/// CRC32 (zlib polynomial `0xedb88320`), vendored: the table is built at compile time, and the
/// standard check value guards it.
pub fn crc32(bytes: &[u8]) -> u32 {
    const fn build_table() -> [u32; 256] {
        let mut table = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut c = i as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 == 1 { 0xedb88320 ^ (c >> 1) } else { c >> 1 };
                k += 1;
            }
            table[i] = c;
            i += 1;
        }
        table
    }
    const TABLE: [u32; 256] = build_table();
    let mut crc = 0xffff_ffffu32;
    for b in bytes {
        crc = TABLE[((crc ^ u32::from(*b)) & 0xff) as usize] ^ (crc >> 8);
    }
    crc ^ 0xffff_ffff
}

/// Where the Central Directory is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Eocd {
    pub count: u64,
    pub cd_size: u64,
    pub cd_offset: u64,
}

fn le16(h: &[u8], off: usize) -> Option<u16> {
    h.get(off..off + 2).and_then(|b| b.try_into().ok()).map(u16::from_le_bytes)
}

fn le32(h: &[u8], off: usize) -> Option<u32> {
    h.get(off..off + 4).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes)
}

/// Finds the End-of-Central-Directory record in `tail` (a suffix of the file) given the file's
/// total size. Candidates are tried from the end backwards; the first fully valid one wins.
/// Anything off fails safe towards a full download.
pub fn find_eocd(tail: &[u8], file_size: u64) -> Result<Eocd, String> {
    if tail.len() as u64 > file_size {
        return Err("tail longer than the file".into());
    }
    let end = tail.len();
    let mut pos = end.saturating_sub(22);
    loop {
        let header = tail.get(pos..pos + 22).ok_or("no end-of-central-directory found")?;
        if header[0..4] == [0x50, 0x4b, 0x05, 0x06] {
            let comment = le16(header, 20).ok_or("no end-of-central-directory found")?;
            if pos + 22 + comment as usize == end && le16(header, 4) == Some(0) && le16(header, 6) == Some(0) {
                let count = le16(header, 10).ok_or("no end-of-central-directory found")? as u64;
                let cd_size = le32(header, 12).ok_or("no end-of-central-directory found")? as u64;
                let cd_offset = le32(header, 16).ok_or("no end-of-central-directory found")? as u64;
                if count >= 1 && cd_size >= 46 * count && cd_size <= (64 << 20) && cd_offset.checked_add(cd_size).is_some_and(|e| e <= file_size) {
                    return Ok(Eocd { count, cd_size, cd_offset });
                }
            }
        }
        if pos == 0 {
            break;
        }
        pos -= 1;
    }
    Err("no end-of-central-directory found".into())
}

/// Reads exactly `count` Central Directory entries; the bytes must end where the last entry
/// ends. Entry names must be UTF-8; anything else fails safe towards a full download.
pub fn parse_cd(bytes: &[u8], expected: u64) -> Result<Vec<Entry>, String> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off < bytes.len() {
        let h = bytes.get(off..off + 46).ok_or("short central directory entry")?;
        if h[0..4] != [0x50, 0x4b, 0x01, 0x02] {
            return Err("bad central directory entry".into());
        }
        let name_len = le16(h, 28).ok_or("short central directory entry")? as usize;
        let extra_len = le16(h, 30).ok_or("short central directory entry")? as usize;
        let comment_len = le16(h, 32).ok_or("short central directory entry")? as usize;
        let end = off
            .checked_add(46)
            .and_then(|e| e.checked_add(name_len))
            .and_then(|e| e.checked_add(extra_len))
            .and_then(|e| e.checked_add(comment_len))
            .ok_or("bad central directory entry")?;
        let name = bytes.get(off + 46..off + 46 + name_len).ok_or("short central directory entry")?;
        let name = std::str::from_utf8(name).map_err(|_| "non-UTF-8 entry name")?.to_string();
        out.push(Entry {
            name,
            crc: le32(h, 16).ok_or("short central directory entry")?,
            comp_size: le32(h, 20).ok_or("short central directory entry")? as u64,
            uncomp_size: le32(h, 24).ok_or("short central directory entry")? as u64,
            local_offset: le32(h, 42).ok_or("short central directory entry")? as u64,
            method: le16(h, 10).ok_or("short central directory entry")?,
            external_attrs: le32(h, 38).ok_or("short central directory entry")?,
        });
        off = end;
    }
    if out.len() as u64 != expected {
        return Err("central directory count mismatch".into());
    }
    Ok(out)
}

/// A local file header's layout: variable lengths plus the declared method, CRC32 and
/// compressed size, to check against the Central Directory entry before trusting a range.
/// `has_descriptor` (bit 3) means the CRC/size fields are zero here and live after the data
/// instead, so they must not be compared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalHeader {
    pub name_len: usize,
    pub extra_len: usize,
    pub method: u16,
    pub crc: u32,
    pub comp_size: u64,
    pub has_descriptor: bool,
}

/// Reads a local file header's first 30 bytes.
pub fn parse_local_header(bytes: &[u8]) -> Result<LocalHeader, String> {
    let h = bytes.get(..30).ok_or("short local header")?;
    if h[0..4] != [0x50, 0x4b, 0x03, 0x04] {
        return Err("bad local header".into());
    }
    let flags = le16(h, 6).ok_or("short local header")?;
    Ok(LocalHeader {
        name_len: le16(h, 26).ok_or("short local header")? as usize,
        extra_len: le16(h, 28).ok_or("short local header")? as usize,
        method: le16(h, 8).ok_or("short local header")?,
        crc: le32(h, 14).ok_or("short local header")?,
        comp_size: le32(h, 18).ok_or("short local header")? as u64,
        has_descriptor: flags & 0x08 != 0,
    })
}

/// Safety caps for an installable archive: absurd counts or sizes fail fast instead of
/// exhausting memory or disk.
pub const MAX_ZIP_FILES: usize = 30_000;
pub const MAX_ZIP_BYTES: u64 = 8 << 30;

/// Rejects archives that must never touch disk: traversal/absolute paths, Windows reserved
/// names, alternate streams, symlinks, unsupported methods, duplicates, and absurd sizes.
/// Callers decide the meaning: the delta path falls back to a full download, the install
/// path refuses with a clear error (a full download of the same bytes would fail alike).
pub fn validate_entries(entries: &[Entry]) -> Result<(), String> {
    if entries.len() > MAX_ZIP_FILES {
        return Err("too many files in the archive".into());
    }
    let mut total: u64 = 0;
    let mut seen = std::collections::HashSet::new();
    for e in entries {
        if !supported_method(e.method) {
            return Err("unsupported compression method".into());
        }
        if !seen.insert(e.name.as_str()) {
            return Err("duplicate entry name".into());
        }
        safe_name(&e.name)?;
        if e.external_attrs >> 16 & 0o170000 == 0o120000 {
            return Err("archive symlink rejected".into());
        }
        total = total.checked_add(e.uncomp_size).ok_or("archive size overflow")?;
        if total > MAX_ZIP_BYTES {
            return Err("archive too large".into());
        }
    }
    Ok(())
}

/// A name that cannot escape its folder, collide with a device, or smuggle a stream.
fn safe_name(name: &str) -> Result<(), String> {
    let normalized = name.replace('\\', "/");
    if normalized.contains(':') || normalized.starts_with('/') {
        return Err("unsafe archive path".into());
    }
    let mut parts = 0;
    for part in normalized.split('/') {
        parts += 1;
        if part.is_empty() || part == "." || part == ".." {
            return Err("unsafe archive path".into());
        }
        if part.ends_with([' ', '.']) {
            return Err("reserved Windows path".into());
        }
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.ends_with(['1', '2', '3', '4', '5', '6', '7', '8', '9']))
        {
            return Err("reserved Windows path".into());
        }
    }
    if parts == 0 {
        return Err("unsafe archive path".into());
    }
    Ok(())
}

/// Where one new entry's bytes come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    /// Copy the compressed bytes of the retained installer's entry.
    Copy { old_index: usize },
    /// Range-fetch from the new file.
    Fetch,
}

/// Matches the new directory against the retained one: same name, CRC32, compressed size and
/// method means the compressed bytes are reused (the final SHA-256 still vouches for the
/// assembly). Duplicate names or an unsupported method fail safe towards a full download.
pub fn plan(old: &[Entry], new: &[Entry]) -> Result<Vec<Need>, String> {
    let mut by_name: HashMap<&str, usize> = HashMap::new();
    for (i, e) in old.iter().enumerate() {
        if by_name.insert(e.name.as_str(), i).is_some() {
            return Err("duplicate entry name".into());
        }
    }
    let mut seen = std::collections::HashSet::new();
    for e in new {
        if !seen.insert(e.name.as_str()) {
            return Err("duplicate entry name".into());
        }
    }
    new.iter()
        .map(|n| {
            if !supported_method(n.method) {
                return Err("unsupported compression method".into());
            }
            match by_name.get(n.name.as_str()) {
                Some(&oi) if old.get(oi).is_some_and(|o| o.method == n.method && o.crc == n.crc && o.comp_size == n.comp_size) => {
                    Ok(Need::Copy { old_index: oi })
                }
                _ => Ok(Need::Fetch),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xcbf43926);
        assert_eq!(crc32(b""), 0);
    }

    /// The exact 22 bytes closing WordCraft v0.6.1's published windows-x64 portable zip
    /// (95,828,700 bytes): the parser must read what real build tooling writes.
    const REAL_EOCD: [u8; 22] =
        [0x50, 0x4b, 0x05, 0x06, 0x00, 0x00, 0x00, 0x00, 0x0a, 0x00, 0x0a, 0x00, 0xf0, 0x03, 0x00, 0x00, 0xd6, 0x36, 0xb6, 0x05, 0x00, 0x00];

    #[test]
    fn finds_the_real_end_of_central_directory() {
        let eocd = find_eocd(&REAL_EOCD, 95828700).unwrap();
        assert_eq!(eocd, Eocd { count: 10, cd_size: 1008, cd_offset: 95827670 });
        assert!(find_eocd(b"hello", 95828700).is_err());
        assert!(find_eocd(&REAL_EOCD[..21], 95828700).is_err(), "truncated");
        assert!(find_eocd(&REAL_EOCD, 100).is_err(), "tail longer than the file");
    }

    /// The exact first Central Directory entry of that same zip.
    const REAL_ENTRY0: [u8; 97] = [
        0x50, 0x4b, 0x01, 0x02, 0x14, 0x00, 0x14, 0x00, 0x00, 0x00, 0x08, 0x00, 0xfa, 0x7c, 0x4c, 0x5d, 0xc8, 0xc7, 0xa5, 0x9b, 0xe8, 0x0d, 0x00,
        0x00, 0x6f, 0x28, 0x00, 0x00, 0x33, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x77, 0x6f, 0x72, 0x64, 0x63, 0x72, 0x61, 0x66, 0x74, 0x2d, 0x30, 0x2e, 0x36, 0x2e, 0x31, 0x2d, 0x77, 0x69, 0x6e, 0x64, 0x6f, 0x77, 0x73,
        0x2d, 0x78, 0x36, 0x34, 0x2d, 0x70, 0x6f, 0x72, 0x74, 0x61, 0x62, 0x6c, 0x65, 0x2f, 0x4c, 0x49, 0x43, 0x45, 0x4e, 0x53, 0x45, 0x2d, 0x41,
        0x50, 0x41, 0x43, 0x48, 0x45,
    ];

    #[test]
    fn parses_the_real_directory_entry() {
        let entries = parse_cd(&REAL_ENTRY0, 1).unwrap();
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.name, "wordcraft-0.6.1-windows-x64-portable/LICENSE-APACHE");
        assert_eq!((e.method, e.crc, e.comp_size, e.uncomp_size, e.local_offset), (8, 0x9ba5c7c8, 3560, 10351, 0));
        assert_eq!(e.external_attrs, 0);
        assert!(parse_cd(&REAL_ENTRY0, 2).is_err(), "count mismatch");
        assert!(parse_cd(&REAL_ENTRY0[..60], 1).is_err(), "truncated");
    }

    #[test]
    fn local_headers_parse_and_reject() {
        let mut h = [0u8; 30];
        h[0..4].copy_from_slice(&[0x50, 0x4b, 0x03, 0x04]);
        h[8..10].copy_from_slice(&8u16.to_le_bytes());
        h[14..18].copy_from_slice(&0xc7c85d4cu32.to_le_bytes());
        h[18..22].copy_from_slice(&3560u32.to_le_bytes());
        h[26..28].copy_from_slice(&51u16.to_le_bytes());
        let lh = parse_local_header(&h).unwrap();
        assert_eq!((lh.method, lh.crc, lh.comp_size, lh.name_len, lh.extra_len, lh.has_descriptor), (8, 0xc7c85d4c, 3560, 51, 0, false));
        h[6] = 0x08;
        assert!(parse_local_header(&h).unwrap().has_descriptor);
        assert!(parse_local_header(&h[..29]).is_err());
        assert!(parse_local_header(&[0u8; 30]).is_err());
    }

    fn entry(name: &str, crc: u32, comp: u64) -> Entry {
        Entry { name: name.into(), crc, comp_size: comp, uncomp_size: comp, local_offset: 0, method: 8, external_attrs: 0 }
    }

    #[test]
    fn plan_reuses_identical_entries_and_fetches_the_rest() {
        let old = vec![entry("a", 1, 10), entry("b", 2, 20), entry("c", 3, 30)];
        let new = vec![entry("a", 1, 10), entry("b", 9, 20), entry("d", 4, 40)];
        assert_eq!(plan(&old, &new).unwrap(), [Need::Copy { old_index: 0 }, Need::Fetch, Need::Fetch]);
        // Same content re-compressed to a different size is fetched, never trusted blindly.
        assert_eq!(plan(&old, &[entry("a", 1, 11)]).unwrap(), [Need::Fetch]);
        assert!(plan(&old, &[entry("x", 1, 10), entry("x", 1, 10)]).is_err(), "duplicate names");
        assert!(plan(&[entry("a", 1, 10)], &[Entry { method: 99, ..entry("a", 1, 10) }]).is_err(), "bad method");
    }

    #[test]
    fn validation_rejects_hostile_archives() {
        assert!(validate_entries(&[entry("a", 1, 10)]).is_ok());
        assert!(validate_entries(&[entry("../escape", 1, 10)]).is_err(), "traversal");
        assert!(validate_entries(&[entry("/abs", 1, 10)]).is_err(), "absolute");
        assert!(validate_entries(&[entry("C:\\win", 1, 10)]).is_err(), "drive letter");
        assert!(validate_entries(&[entry("a:stream", 1, 10)]).is_err(), "alternate stream");
        assert!(validate_entries(&[entry("CON", 1, 10)]).is_err(), "reserved name");
        assert!(validate_entries(&[entry("dir/COM1.txt", 1, 10)]).is_err(), "reserved name in folder");
        assert!(validate_entries(&[entry("x", 1, 10), entry("x", 1, 10)]).is_err(), "duplicate names");
        assert!(validate_entries(&[Entry { method: 99, ..entry("a", 1, 10) }]).is_err(), "bad method");
        assert!(validate_entries(&[Entry { external_attrs: 0o120000 << 16, ..entry("a", 1, 10) }]).is_err(), "symlink");
        // A real published entry passes validation.
        let real = parse_cd(&REAL_ENTRY0, 1).unwrap();
        assert!(validate_entries(&real).is_ok());
    }
}
