//! Portable-zip deltas end to end, without the real network: a local HTTP server plays
//! the release host serving a synthetic new zip, the retained old zip sits on disk, and
//! `fetch_zip_delta` must rebuild the new bytes while asking only for the directory and the
//! changed entries. Stored (uncompressed) entries keep the fixture builder free of any
//! compression library; the parser under test never decompresses.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use artcraft_launcher_core::zip_cd;
use artcraft_launcher_native::http::{Http, Stop};
use artcraft_launcher_native::zip_fetch::{self, ZipJob};
use sha2::{Digest as _, Sha256};

/// Builds a minimal stored-method zip: local headers + central directory + end record.
fn build_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cd = Vec::new();
    let mut off: u32 = 0;
    for (name, data) in files {
        let crc = zip_cd::crc32(data);
        let len = data.len() as u32;
        let name_len = name.len() as u16;
        out.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04]);
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&name_len.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        cd.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
        cd.extend_from_slice(&20u16.to_le_bytes());
        cd.extend_from_slice(&20u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&crc.to_le_bytes());
        cd.extend_from_slice(&len.to_le_bytes());
        cd.extend_from_slice(&len.to_le_bytes());
        cd.extend_from_slice(&name_len.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&[0u8; 4]);
        cd.extend_from_slice(&off.to_le_bytes());
        cd.extend_from_slice(name.as_bytes());
        off += 30 + name.len() as u32 + len;
    }
    let cd_off = off;
    out.extend_from_slice(&cd);
    out.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0]);
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// Serves `body`: exact byte ranges with 206s (`ranged`), or always the whole body with a 200
/// (`!ranged`, proving a range-ignoring server fails safe). Records every `Range` it saw.
fn serve(body: Vec<u8>, ranged: bool, expect: usize) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/wordcraft-0.6.2-windows-x64-portable.zip", listener.local_addr().unwrap());
    let hits = Arc::new(Mutex::new(Vec::new()));
    let seen = hits.clone();
    std::thread::spawn(move || {
        for _ in 0..expect {
            let Ok((mut stream, _)) = listener.accept() else { return };
            let mut range: Option<(u64, u64)> = None;
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                    let mut parts = v.trim().split('-');
                    if let (Some(Ok(s)), Some(Ok(e))) = (parts.next().map(str::parse), parts.next().map(str::parse)) {
                        range = Some((s, e));
                    }
                }
            }
            if ranged
                && let Some((s, e)) = range
                && let Some(slice) = body.get(s as usize..=e as usize)
            {
                seen.lock().unwrap().push(format!("{s}-{e}"));
                let _ = write!(
                    stream,
                    "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {s}-{}/{}\r\nConnection: close\r\n\r\n",
                    slice.len(),
                    e,
                    body.len()
                )
                .and_then(|()| stream.write_all(slice));
            } else {
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())
                    .and_then(|()| stream.write_all(&body));
            }
        }
    });
    (url, hits)
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn old_zip() -> Vec<u8> {
    // Bulk stays identical so the tail request cannot cover it: the delta must skip it.
    build_zip(&[("app/a.txt", &[1u8; 400000]), ("app/b.txt", &[2u8; 200]), ("app/c.txt", &[3u8; 400000])])
}

fn new_zip() -> Vec<u8> {
    build_zip(&[("app/a.txt", &[1u8; 400000]), ("app/b.txt", &[9u8; 200]), ("app/c.txt", &[3u8; 400000]), ("app/d.txt", &[4u8; 300])])
}

fn travelled(hits: &Arc<Mutex<Vec<String>>>) -> u64 {
    hits.lock().unwrap().iter().map(|h| h.split_once('-').map_or(0, |(s, e)| e.parse::<u64>().unwrap_or(0) - s.parse::<u64>().unwrap_or(0) + 1)).sum()
}

#[test]
fn delta_rebuilds_the_new_zip_from_changed_entries_only() {
    let old = old_zip();
    let new = new_zip();
    // Requests: tail + directory + one header per new entry (4) + changed data (2) + tail copy (1).
    let (url, hits) = serve(new.clone(), true, 9);
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().join("new.zip.delta.tmp");
    let old_path = dir.path().join("old.zip");
    std::fs::write(&old_path, &old).unwrap();
    let mut progress = Vec::new();
    zip_fetch::fetch_zip_delta(ZipJob {
        http: &Http::new().unwrap(),
        base_url: &url,
        file_size: new.len() as u64,
        old_path: &old_path,
        tmp_path: &tmp,
        expected_sha256: &sha256_hex(&new),
        stop: &Stop::default(),
        progress: &mut |done, total| progress.push((done, total)),
    })
    .unwrap();
    assert_eq!(std::fs::read(&tmp).unwrap(), new);
    assert_eq!(progress.last(), Some(&(new.len() as u64, new.len() as u64)));
    // Two entries' data plus small headers travelled: far less than the whole zip.
    assert!(travelled(&hits) < new.len() as u64 / 2, "{} of {} bytes", travelled(&hits), new.len());
    assert_eq!(hits.lock().unwrap().len(), 9);
}

#[test]
fn delta_refuses_a_wrong_checksum_and_leaves_no_file() {
    let old = old_zip();
    let new = new_zip();
    let (url, _) = serve(new.clone(), true, 9);
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().join("new.zip.delta.tmp");
    let old_path = dir.path().join("old.zip");
    std::fs::write(&old_path, &old).unwrap();
    let err = zip_fetch::fetch_zip_delta(ZipJob {
        http: &Http::new().unwrap(),
        base_url: &url,
        file_size: new.len() as u64,
        old_path: &old_path,
        tmp_path: &tmp,
        expected_sha256: &"0".repeat(64),
        stop: &Stop::default(),
        progress: &mut |_, _| {},
    })
    .unwrap_err();
    assert!(err.contains("checksum"), "{err}");
    assert!(!tmp.exists());
}

#[test]
fn delta_fails_safe_when_the_server_ignores_ranges() {
    let old = old_zip();
    let new = new_zip();
    // Three attempts at the tail before giving up with the first refusal.
    let (full_url, _) = serve(new.clone(), false, 3);
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().join("new.zip.delta.tmp");
    let old_path = dir.path().join("old.zip");
    std::fs::write(&old_path, &old).unwrap();
    let err = zip_fetch::fetch_zip_delta(ZipJob {
        http: &Http::new().unwrap(),
        base_url: &full_url,
        file_size: new.len() as u64,
        old_path: &old_path,
        tmp_path: &tmp,
        expected_sha256: &sha256_hex(&new),
        stop: &Stop::default(),
        progress: &mut |_, _| {},
    })
    .unwrap_err();
    assert!(err.contains("ignored"), "{err}");
    assert!(!tmp.exists());
}

#[test]
fn nothing_reusable_skips_the_delta() {
    let new = new_zip();
    // Tail + directory, then the plan finds no shared entry and stops before entry fetches.
    let (url, hits) = serve(new.clone(), true, 2);
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().join("new.zip.delta.tmp");
    let old_path = dir.path().join("old.zip");
    std::fs::write(&old_path, build_zip(&[("other/z.txt", &[7u8; 60])])).unwrap();
    let err = zip_fetch::fetch_zip_delta(ZipJob {
        http: &Http::new().unwrap(),
        base_url: &url,
        file_size: new.len() as u64,
        old_path: &old_path,
        tmp_path: &tmp,
        expected_sha256: &sha256_hex(&new),
        stop: &Stop::default(),
        progress: &mut |_, _| {},
    })
    .unwrap_err();
    assert!(err.contains("nothing reusable"), "{err}");
    assert_eq!(hits.lock().unwrap().len(), 2);
    assert!(!tmp.exists());
}
