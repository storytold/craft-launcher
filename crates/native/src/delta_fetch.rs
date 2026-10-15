//! Delta updates: fetch only the blocks that changed (zsync, over HTTP range requests).
//!
//! A `.zsync` control file, published next to every Linux AppImage, lists a weak rolling
//! checksum and a truncated MD4 per block of the new file. Blocks the installed copy already
//! holds are copied from disk; only the rest travels the network, in HTTP range requests.
//!
//! MD4 here is a content matcher for zsync compatibility, never the security check: the
//! assembled file is verified against the release's published SHA-256 exactly like a full
//! download (#90). Any failure anywhere means the caller falls back to that full download.
//!
//! Nothing here overwrites anything: the new file is assembled into a temporary path, and only
//! the caller moves it into place after verification. A weak hit is always confirmed by its
//! MD4, so `Hash-Lengths`' middle field (zsync's consecutive-match policy) is deliberately
//! unused: safer, for a negligible CPU cost.

use std::collections::HashMap;
use std::path::Path;

use md4::{Digest, Md4};

use crate::http::{Http, Stop};

/// Largest installed file a delta is attempted from (it is read into memory once).
const MAX_OLD_FILE: u64 = 1 << 30;
/// Missing bytes are fetched in chunks of at most this (one HTTP range request each).
const RANGE_CHUNK: u64 = 8 << 20;
/// Sanity cap on the control file's block count.
const MAX_BLOCKS: u64 = 8 << 20;

/// A parsed `.zsync` control file.
#[derive(Clone, Debug)]
pub struct Control {
    block_size: u64,
    file_len: u64,
    weak_len: usize,
    strong_len: usize,
    /// `weak_len + strong_len` bytes per target block, in order.
    sums: Vec<u8>,
}

impl Control {
    fn block_count(&self) -> u64 {
        self.file_len.div_ceil(self.block_size)
    }

    fn block_len(&self, idx: u64) -> u64 {
        (self.file_len - idx * self.block_size).min(self.block_size)
    }

    fn entry_len(&self) -> usize {
        self.weak_len + self.strong_len
    }
}

/// Reads a `.zsync` control file: text header lines, a blank line, then exactly
/// `blocks × (weak + strong)` checksum bytes. Anything else is refused, so a corrupt or
/// hostile control file can only fall back to a full download, never to a wrong update.
/// `Filename`, `MTime`, `URL` and `SHA-1` lines are ignored on purpose: the caller owns the
/// authoritative download URL, and SHA-256 (not SHA-1) vouches for the result.
pub fn parse_zsync(data: &[u8]) -> Result<Control, String> {
    let split = data.windows(2).position(|w| w == b"\n\n").ok_or("not a zsync control file")?;
    let header = std::str::from_utf8(data.get(..split).ok_or("not a zsync control file")?).map_err(|_| "unreadable zsync header")?;
    let mut block_size = None;
    let mut file_len = None;
    let mut weak_len = None;
    let mut strong_len = None;
    for line in header.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        match key.trim() {
            "Blocksize" => block_size = Some(value.trim().parse::<u64>().map_err(|_| "bad zsync Blocksize")?),
            "Length" => file_len = Some(value.trim().parse::<u64>().map_err(|_| "bad zsync Length")?),
            "Hash-Lengths" => {
                let mut parts = value.split(',');
                let weak = parts.next().ok_or("bad zsync Hash-Lengths")?.trim().parse::<usize>().map_err(|_| "bad zsync Hash-Lengths")?;
                let _policy = parts.next().ok_or("bad zsync Hash-Lengths")?.trim().parse::<usize>().map_err(|_| "bad zsync Hash-Lengths")?;
                let strong = parts.next().ok_or("bad zsync Hash-Lengths")?.trim().parse::<usize>().map_err(|_| "bad zsync Hash-Lengths")?;
                if parts.next().is_some() {
                    return Err("bad zsync Hash-Lengths".into());
                }
                weak_len = Some(weak);
                strong_len = Some(strong);
            }
            _ => {}
        }
    }
    let (Some(block_size), Some(file_len), Some(weak_len), Some(strong_len)) = (block_size, file_len, weak_len, strong_len) else {
        return Err("incomplete zsync header".into());
    };
    if block_size == 0 || block_size > (1 << 20) || file_len == 0 || file_len > (1 << 34) {
        return Err("unusable zsync sizes".into());
    }
    if weak_len == 0 || weak_len > 4 || strong_len == 0 || strong_len > 16 {
        return Err("unusable zsync Hash-Lengths".into());
    }
    let blocks = file_len.div_ceil(block_size);
    if blocks > MAX_BLOCKS {
        return Err("unusable zsync sizes".into());
    }
    let sums = data.get(split + 2..).ok_or("not a zsync control file")?.to_vec();
    if sums.len() as u64 != blocks * (weak_len as u64 + strong_len as u64) {
        return Err("zsync checksums don't match its header".into());
    }
    Ok(Control { block_size, file_len, weak_len, strong_len, sums })
}

/// The rsync weak rolling checksum: `s1 = Σ bytes`, `s2 = Σ s1`, both mod 2^16.
pub fn weak_sum(block: &[u8]) -> u32 {
    let mut s1: u32 = 0;
    let mut s2: u32 = 0;
    for b in block {
        s1 = (s1 + u32::from(*b)) & 0xffff;
        s2 = (s2 + s1) & 0xffff;
    }
    (s2 << 16) | s1
}

/// First `len` bytes of a weak checksum, big-endian: how zsync stores (and matches) them.
fn weak_key(weak: u32, len: usize) -> u32 {
    weak.to_be_bytes().get(..len).map_or(0, |b| b.iter().fold(0u32, |k, x| (k << 8) | u32::from(*x)))
}

fn stored_weak(control: &Control, idx: u64) -> u32 {
    let off = idx as usize * control.entry_len();
    control.sums.get(off..off + control.weak_len).map_or(0, |b| b.iter().fold(0u32, |k, x| (k << 8) | u32::from(*x)))
}

/// Whether `window` is target block `idx`: weak match first (cheap), MD4 prefix always.
fn strong_matches(window: &[u8], control: &Control, idx: u64) -> bool {
    let off = idx as usize * control.entry_len() + control.weak_len;
    let Some(stored) = control.sums.get(off..off + control.strong_len) else { return false };
    Md4::digest(window).as_slice().get(..control.strong_len).is_some_and(|sum| sum == stored)
}

/// Where each target block comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockPlan {
    /// Copy `block_len` bytes from this offset of the installed copy.
    Reuse { old_offset: u64 },
    /// Fetch from the network.
    Fetch,
}

/// Matches every target block against the installed copy: exact position first (the common
/// case: most blocks neither move nor change), then a rolling scan for blocks that moved.
/// A block is reused only after its MD4 prefix verifies; anything uncertain stays [`BlockPlan::Fetch`].
pub fn plan(old: &[u8], control: &Control) -> Result<Vec<BlockPlan>, String> {
    let n = control.block_count();
    if n == 0 || n > MAX_BLOCKS {
        return Err("unusable block count".into());
    }
    let bs = control.block_size as usize;
    let mut plan = vec![BlockPlan::Fetch; n as usize];
    // Exact positions first.
    for (i, slot) in plan.iter_mut().enumerate() {
        let i = i as u64;
        let len = control.block_len(i) as usize;
        let off = i as usize * bs;
        if let Some(window) = old.get(off..off + len)
            && weak_key(weak_sum(window), control.weak_len) == stored_weak(control, i)
            && strong_matches(window, control, i)
        {
            *slot = BlockPlan::Reuse { old_offset: off as u64 };
        }
    }
    // Rolling scan for full-size blocks that moved (insertions/deletions shift the rest).
    let mut wanted: HashMap<u32, Vec<u64>> = HashMap::new();
    for (i, slot) in plan.iter().enumerate() {
        if *slot == BlockPlan::Fetch && control.block_len(i as u64) == control.block_size {
            wanted.entry(stored_weak(control, i as u64)).or_default().push(i as u64);
        }
    }
    if !wanted.is_empty()
        && let Some(first) = old.get(..bs)
    {
        let n32 = bs as u32;
        let (mut s1, mut s2) = window_sums(first);
        let mut pos = 0usize;
        loop {
            if let Some(idxs) = wanted.get(&weak_key((s2 << 16) | s1, control.weak_len)).cloned() {
                for idx in idxs {
                    if plan.get(idx as usize) == Some(&BlockPlan::Fetch)
                        && let Some(window) = old.get(pos..pos + bs)
                        && strong_matches(window, control, idx)
                    {
                        plan[idx as usize] = BlockPlan::Reuse { old_offset: pos as u64 };
                        wanted.retain(|_, v| {
                            v.retain(|b| *b != idx);
                            !v.is_empty()
                        });
                        break;
                    }
                }
            }
            if wanted.is_empty() {
                break;
            }
            let Some(&inn) = old.get(pos + bs) else { break };
            let out = old[pos];
            s1 = (s1.wrapping_sub(u32::from(out)).wrapping_add(u32::from(inn))) & 0xffff;
            s2 = (s2.wrapping_sub(n32.wrapping_mul(u32::from(out))).wrapping_add(s1)) & 0xffff;
            pos += 1;
        }
    }
    Ok(plan)
}

fn window_sums(window: &[u8]) -> (u32, u32) {
    let weak = weak_sum(window);
    (weak & 0xffff, weak >> 16)
}

/// Missing bytes as `(start, end_exclusive)` ranges, coalesced and split into fetchable chunks.
pub fn missing_ranges(plan: &[BlockPlan], control: &Control) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let mut run: Option<(u64, u64)> = None;
    let flush = |run: &mut Option<(u64, u64)>, out: &mut Vec<(u64, u64)>| {
        if let Some((mut s, e)) = run.take() {
            while s < e {
                let c = (e - s).min(RANGE_CHUNK);
                out.push((s, s + c));
                s += c;
            }
        }
    };
    for (i, p) in plan.iter().enumerate() {
        if *p == BlockPlan::Fetch {
            let s = i as u64 * control.block_size;
            let e = s + control.block_len(i as u64);
            run = Some(match run {
                Some((rs, re)) if re == s => (rs, e),
                _ => (s, e),
            });
        } else {
            flush(&mut run, &mut out);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// Builds the new file: reused blocks from the installed copy, the rest from `chunks`
/// (`(start_offset, bytes)`, in order, exactly covering the [`BlockPlan::Fetch`] blocks).
/// Any inconsistency is an error, never a wrong file.
pub fn assemble(old: &[u8], control: &Control, plan: &[BlockPlan], chunks: &[(u64, Vec<u8>)]) -> Result<Vec<u8>, String> {
    if plan.len() as u64 != control.block_count() {
        return Err("plan doesn't match the control file".into());
    }
    let mut out = Vec::new();
    let mut ci = 0usize;
    for (i, p) in plan.iter().enumerate() {
        let blen = control.block_len(i as u64) as usize;
        match *p {
            BlockPlan::Reuse { old_offset } => {
                let Some(o) = usize::try_from(old_offset).ok() else { return Err("bad reuse offset".into()) };
                let end = o.checked_add(blen).ok_or("bad reuse offset")?;
                out.extend_from_slice(old.get(o..end).ok_or("reuse past the old file")?);
            }
            BlockPlan::Fetch => {
                let boff = i as u64 * control.block_size;
                loop {
                    let (cs, cb) = chunks.get(ci).ok_or("downloaded chunks don't cover a missing block")?;
                    let cend = cs.checked_add(cb.len() as u64).ok_or("bad chunk")?;
                    if boff >= *cs && boff + blen as u64 <= cend {
                        let from = (boff - cs) as usize;
                        out.extend_from_slice(cb.get(from..from + blen).ok_or("chunk too short")?);
                        if boff + blen as u64 == cend {
                            ci += 1;
                        }
                        break;
                    } else if cend <= boff {
                        ci += 1;
                    } else {
                        return Err("downloaded chunks don't cover a missing block".into());
                    }
                }
            }
        }
    }
    if out.len() as u64 != control.file_len {
        return Err("assembled size doesn't match".into());
    }
    Ok(out)
}

/// What a delta fetch needs, mirroring [`crate::install::Job`].
pub struct DeltaJob<'a> {
    pub http: &'a Http,
    pub target_url: &'a str,
    pub control: &'a Control,
    pub old_path: &'a Path,
    pub tmp_path: &'a Path,
    pub expected_sha256: &'a str,
    pub stop: &'a Stop,
    pub progress: &'a mut dyn FnMut(u64, u64),
}

/// Delta-updates `old_path` to `tmp_path`: plans from the installed copy, range-fetches only
/// the missing bytes, assembles the new file and checks it against `expected_sha256` (the
/// release's published checksum). `progress` hears `(done, total)` bytes. Any error, including
/// a checksum mismatch (which deletes the temporary file), means the caller falls back to a
/// full download; pause and cancel (`stop`) propagate unchanged.
pub fn fetch_delta(job: DeltaJob<'_>) -> Result<(), String> {
    let DeltaJob { http, target_url, control, old_path, tmp_path, expected_sha256, stop, progress } = job;
    if std::fs::metadata(old_path).map_err(|e| format!("couldn't read the installed copy ({e})"))?.len() > MAX_OLD_FILE {
        return Err("installed copy too large for a delta".into());
    }
    let old = std::fs::read(old_path).map_err(|e| format!("couldn't read the installed copy ({e})"))?;
    let plan = plan(&old, control)?;
    let mut reused: u64 = 0;
    for (i, p) in plan.iter().enumerate() {
        if matches!(p, BlockPlan::Reuse { .. }) {
            reused += control.block_len(i as u64);
        }
    }
    progress(reused, control.file_len);
    let mut chunks = Vec::new();
    let mut fetched: u64 = 0;
    for (s, e) in missing_ranges(&plan, control) {
        stop.check()?;
        // A flaky chunk is retried; the first error is the one reported, so a
        // deterministic refusal (a range-ignoring server, a gone file) keeps its meaning.
        let mut first_err: Option<String> = None;
        let mut fetched_bytes: Option<Vec<u8>> = None;
        for _ in 0..3 {
            match http.get_range(target_url, s, e - 1) {
                Ok(b) => {
                    fetched_bytes = Some(b);
                    break;
                }
                Err(err) => {
                    stop.check()?;
                    if first_err.is_none() {
                        log::warn!("delta range {s}-{} failed ({err}); retrying", e - 1);
                        first_err = Some(err);
                    }
                }
            }
        }
        let bytes = match fetched_bytes {
            Some(b) => b,
            None => match first_err {
                Some(err) => return Err(err),
                None => return Err("delta fetch failed".into()),
            },
        };
        fetched += bytes.len() as u64;
        progress(reused + fetched, control.file_len);
        chunks.push((s, bytes));
    }
    let new_bytes = assemble(&old, control, &plan, &chunks)?;
    stop.check()?;
    std::fs::write(tmp_path, &new_bytes).map_err(|e| format!("couldn't save the download ({e})"))?;
    if !crate::http::sha256_file(tmp_path).is_ok_and(|h| h.eq_ignore_ascii_case(expected_sha256)) {
        let _ = std::fs::remove_file(tmp_path);
        return Err("the delta update doesn't match its published checksum, so it wasn't installed".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a control file the way zsyncmake would: real weak + MD4 sums over `new_bytes`.
    fn control_for(new_bytes: &[u8], block_size: u64) -> Control {
        let weak_len = 2;
        let strong_len = 5;
        let n = new_bytes.len().div_ceil(block_size as usize);
        let mut sums = Vec::new();
        for i in 0..n {
            let block = &new_bytes[i * block_size as usize..((i + 1) * block_size as usize).min(new_bytes.len())];
            sums.extend_from_slice(&weak_sum(block).to_be_bytes()[..weak_len]);
            sums.extend_from_slice(&Md4::digest(block).as_slice()[..strong_len]);
        }
        Control { block_size, file_len: new_bytes.len() as u64, weak_len, strong_len, sums }
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len as u32).map(|i| (i.wrapping_mul(2654435761).wrapping_add(i >> 3) % 251) as u8).collect()
    }

    #[test]
    fn weak_sum_matches_the_rsync_definition() {
        // s1 = 97+98+99 = 294, s2 = 3·97+2·98+99 = 586.
        assert_eq!(weak_sum(b"abc"), 0x024a_0126);
        assert_eq!(weak_sum(b""), 0);
    }

    #[test]
    fn md4_is_correct() {
        // Reference vectors (RFC 1320, cross-checked with a second implementation).
        assert_eq!(hex(Md4::digest(b"").as_slice()), "31d6cfe0d16ae931b73c59d7e0c089c0");
        assert_eq!(hex(Md4::digest(b"abc").as_slice()), "a448017aaf21d8525fc10ae87aa6729d");
        assert_eq!(hex(Md4::digest(b"message digest").as_slice()), "d9130a8164549fe818874806e1c7014b");
        assert_eq!(
            hex(Md4::digest(b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789").as_slice()),
            "043f8582f241db351ce627e153e7f0e4"
        );
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The exact header text published with PhotoCraft v0.8.2's AppImage: the parser must read
    /// what zsyncmake really writes, field for field.
    const REAL_HEADER: &str = "zsync: 0.6.2\nFilename: photocraft-0.8.2-linux-x86_64.AppImage\nMTime: Mon, 12 Oct 2026 17:48:31 +0000\nBlocksize: 2048\nLength: 75782648\nHash-Lengths: 2,2,5\nURL: photocraft-0.8.2-linux-x86_64.AppImage\nSHA-1: 65807951f78c6c6f194ba86432a5cff44d870ede\n\n";

    #[test]
    fn parses_what_zsyncmake_publishes() {
        let blocks = 75782648u64.div_ceil(2048);
        assert_eq!(blocks, 37004);
        let mut file = REAL_HEADER.as_bytes().to_vec();
        file.extend(std::iter::repeat_n(0x5au8, blocks as usize * 7));
        let c = parse_zsync(&file).unwrap();
        assert_eq!((c.block_size, c.file_len, c.weak_len, c.strong_len), (2048, 75782648, 2, 5));
        assert_eq!(c.block_count(), 37004);
        assert_eq!(c.block_len(37003), 75782648 - 37003 * 2048);
        // Anything off fails safe towards a full download.
        assert!(parse_zsync(b"hello").is_err());
        assert!(parse_zsync(&file[..REAL_HEADER.len()]).is_err(), "checksums missing");
        assert!(parse_zsync(&file[..file.len() - 7]).is_err(), "checksums short");
        let mut long = file.clone();
        long.push(0x00);
        assert!(parse_zsync(&long).is_err(), "checksums long");
        let no_blank = REAL_HEADER.replace("\n\n", "\nX\n");
        let mut no_blank = no_blank.into_bytes();
        no_blank.push(0x00);
        assert!(parse_zsync(&no_blank).is_err(), "no blank line");
    }

    #[test]
    fn plan_reuses_unchanged_blocks_and_fetches_the_rest() {
        let mut new = pattern(8 * 32);
        let old = new.clone();
        // Mutate two blocks; the last block is partial-length (8·32 = 256 exactly, so extend).
        new.extend_from_slice(&[9u8; 10]);
        new[2 * 32 + 5] ^= 0xff;
        new[5 * 32] ^= 0x01;
        let control = control_for(&new, 32);
        let plan = plan(&old, &control).unwrap();
        assert_eq!(plan.len() as u64, control.block_count());
        // Blocks 2 and 5 changed, block 8 is new (the old file ends at block 7).
        for (i, p) in plan.iter().enumerate() {
            if i == 2 || i == 5 || i == 8 {
                assert_eq!(*p, BlockPlan::Fetch, "block {i}");
            } else {
                assert_eq!(*p, BlockPlan::Reuse { old_offset: i as u64 * 32 }, "block {i}");
            }
        }
        // Fetch the missing ranges from `new` itself and reassemble byte-identical output.
        let ranges = missing_ranges(&plan, &control);
        let chunks: Vec<(u64, Vec<u8>)> = ranges.iter().map(|(s, e)| (*s, new[*s as usize..*e as usize].to_vec())).collect();
        assert_eq!(assemble(&old, &control, &plan, &chunks).unwrap(), new);
    }

    #[test]
    fn rolling_scan_finds_blocks_an_insertion_shifted() {
        // One block inserted up front shifts every later block: the fast path misses them all,
        // the rolling scan must still find them, so only ~2 blocks travel the network.
        let old = pattern(8 * 32);
        let mut new = vec![0xabu8; 32];
        new.extend_from_slice(&old);
        let control = control_for(&new, 32);
        let plan = plan(&old, &control).unwrap();
        let fetched_blocks = plan.iter().filter(|p| **p == BlockPlan::Fetch).count();
        assert!(fetched_blocks <= 2, "{fetched_blocks} blocks fetched");
        assert_eq!(plan[0], BlockPlan::Fetch);
        let ranges = missing_ranges(&plan, &control);
        let chunks: Vec<(u64, Vec<u8>)> = ranges.iter().map(|(s, e)| (*s, new[*s as usize..*e as usize].to_vec())).collect();
        assert_eq!(assemble(&old, &control, &plan, &chunks).unwrap(), new);
    }

    #[test]
    fn assemble_rejects_inconsistent_input() {
        let new = pattern(64);
        let control = control_for(&new, 32);
        let plan = plan(&[0u8; 64], &control).unwrap();
        assert!(plan.iter().all(|p| *p == BlockPlan::Fetch));
        // Coverage errors fail here; wrong *content* is the SHA-256 check's job in fetch_delta.
        assert!(assemble(&[], &control, &plan, &[(0, new[..32].to_vec())]).is_err(), "half covered");
        assert!(assemble(&[], &control, &plan, &[]).is_err(), "nothing fetched");
        // Plan from another file.
        assert!(assemble(&[], &control, &[BlockPlan::Fetch], &[(0, new)]).is_err());
    }
}
