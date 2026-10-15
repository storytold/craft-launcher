//! Delta updates, end to end without touching the real network: a local HTTP server plays
//! the release host, an old copy sits on disk, and `fetch_delta` must rebuild the new bytes
//! while asking only for the missing ranges. The decision chain (`update_method` +
//! `zsync_asset_for`) is covered here too, because `install::download` follows exactly it.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use artcraft_launcher_core::delta::{self, UpdateMethod};
use artcraft_launcher_core::platform::{Os, PackageKind};
use artcraft_launcher_core::release::{Asset, Release};
use artcraft_launcher_native::delta_fetch::{self, DeltaJob};
use artcraft_launcher_native::http::{Http, Stop};
use md4::{Digest, Md4};
use sha2::Sha256;

const BLOCK: usize = 512;
const APPIMAGE: &str = "someapp-0.2.0-linux-x86_64.AppImage";

fn pattern(len: usize) -> Vec<u8> {
    (0..len as u32).map(|i| (i.wrapping_mul(2654435761).wrapping_add(i >> 3) % 251) as u8).collect()
}

/// A `.zsync` control file for `new_bytes`, serialized exactly as zsyncmake writes it.
fn zsync_for(new_bytes: &[u8]) -> Vec<u8> {
    let n = new_bytes.len().div_ceil(BLOCK);
    let mut out = format!("zsync: 0.6.2\nBlocksize: {BLOCK}\nLength: {}\nHash-Lengths: 2,2,5\n\n", new_bytes.len()).into_bytes();
    for i in 0..n {
        let block = &new_bytes[i * BLOCK..((i + 1) * BLOCK).min(new_bytes.len())];
        out.extend_from_slice(&delta_fetch::weak_sum(block).to_be_bytes()[..2]);
        out.extend_from_slice(&Md4::digest(block).as_slice()[..5]);
    }
    out
}

fn release_with(appimage_url: &str, zsync_url: &str) -> Release {
    Release {
        tag: "v0.2.0".into(),
        assets: vec![
            Asset { name: APPIMAGE.into(), size: 0, url: appimage_url.into(), sha256: None },
            Asset { name: format!("{APPIMAGE}.zsync"), size: 0, url: zsync_url.into(), sha256: None },
        ],
        ..Release::default()
    }
}

/// Serves `body`: exact byte ranges with 206s (`ranged`), or always the whole body with a 200
/// (`full`, for proving a range-ignoring server fails safe). Records every `Range` it saw.
fn serve(body: Vec<u8>, ranged: bool, expect: usize) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/someapp-0.2.0-linux-x86_64.AppImage", listener.local_addr().unwrap());
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
    use sha2::Digest as _;
    format!("{:x}", Sha256::digest(bytes))
}

/// Old copy on disk, new bytes on the "server": the update must reuse most blocks locally.
fn fixtures() -> (Vec<u8>, Vec<u8>) {
    let old = pattern(10 * BLOCK);
    let mut new = old.clone();
    new[3 * BLOCK + 7] ^= 0xff;
    new[7 * BLOCK] ^= 0x01;
    new.extend_from_slice(&[0x77u8; 100]); // a changed tail, partial block included
    (old, new)
}

#[test]
fn decision_chain_offers_a_delta_for_a_listed_zsync() {
    let release = release_with("http://127.0.0.1:9/x", "http://127.0.0.1:9/x.zsync");
    let zsync = release.zsync_asset_for(APPIMAGE).unwrap();
    assert_eq!(zsync.name, format!("{APPIMAGE}.zsync"));
    assert_eq!(delta::update_method(Os::Linux, PackageKind::AppImage, true, true), UpdateMethod::DeltaZsync);
    assert_eq!(release.zsync_asset_for("other.AppImage"), None);
}

#[test]
fn delta_rebuilds_the_new_file_from_missing_ranges_only() {
    let (old, new) = fixtures();
    let control = delta_fetch::parse_zsync(&zsync_for(&new)).unwrap();
    let plan = delta_fetch::plan(&old, &control).unwrap();
    let missing = delta_fetch::missing_ranges(&plan, &control);
    assert!(!missing.is_empty(), "something must have changed");
    let (url, hits) = serve(new.clone(), true, missing.len());
    let dir = tempfile::tempdir().unwrap();
    let old_path = dir.path().join("someapp.AppImage");
    let tmp_path = dir.path().join("someapp.AppImage.delta.tmp");
    std::fs::write(&old_path, &old).unwrap();
    let http = Http::new().unwrap();
    let mut progress = Vec::new();
    delta_fetch::fetch_delta(DeltaJob {
        http: &http,
        target_url: &url,
        control: &control,
        old_path: &old_path,
        tmp_path: &tmp_path,
        expected_sha256: &sha256_hex(&new),
        stop: &Stop::default(),
        progress: &mut |done, total| progress.push((done, total)),
    })
    .unwrap();
    assert_eq!(std::fs::read(&tmp_path).unwrap(), new);
    assert_eq!(progress.last(), Some(&(new.len() as u64, new.len() as u64)));
    // Only the missing bytes travelled: 2 changed blocks plus the new tail.
    let travelled: u64 = hits
        .lock()
        .unwrap()
        .iter()
        .map(|h| h.split_once('-').map_or(0, |(s, e)| e.parse::<u64>().unwrap_or(0) - s.parse::<u64>().unwrap_or(0) + 1))
        .sum();
    assert!(travelled < new.len() as u64 / 2, "{travelled} of {} bytes", new.len());
    assert_eq!(hits.lock().unwrap().len(), missing.len());
}

#[test]
fn delta_refuses_a_wrong_checksum_and_leaves_no_file() {
    let (old, new) = fixtures();
    let control = delta_fetch::parse_zsync(&zsync_for(&new)).unwrap();
    let plan = delta_fetch::plan(&old, &control).unwrap();
    let missing = delta_fetch::missing_ranges(&plan, &control);
    let (url, _) = serve(new.clone(), true, missing.len());
    let dir = tempfile::tempdir().unwrap();
    let old_path = dir.path().join("someapp.AppImage");
    let tmp_path = dir.path().join("someapp.AppImage.delta.tmp");
    std::fs::write(&old_path, &old).unwrap();
    let err = delta_fetch::fetch_delta(DeltaJob {
        http: &Http::new().unwrap(),
        target_url: &url,
        control: &control,
        old_path: &old_path,
        tmp_path: &tmp_path,
        expected_sha256: &"0".repeat(64),
        stop: &Stop::default(),
        progress: &mut |_, _| {},
    })
    .unwrap_err();
    assert!(err.contains("checksum"), "{err}");
    assert!(!tmp_path.exists());
}

#[test]
fn delta_fails_safe_when_the_server_ignores_ranges() {
    let (old, new) = fixtures();
    let control = delta_fetch::parse_zsync(&zsync_for(&new)).unwrap();
    // Three attempts per range before fetch_delta gives up and reports the first refusal.
    let (url, _) = serve(new, false, 3);
    let dir = tempfile::tempdir().unwrap();
    let old_path = dir.path().join("someapp.AppImage");
    let tmp_path = dir.path().join("someapp.AppImage.delta.tmp");
    std::fs::write(&old_path, &old).unwrap();
    let err = delta_fetch::fetch_delta(DeltaJob {
        http: &Http::new().unwrap(),
        target_url: &url,
        control: &control,
        old_path: &old_path,
        tmp_path: &tmp_path,
        expected_sha256: &"0".repeat(64),
        stop: &Stop::default(),
        progress: &mut |_, _| {},
    })
    .unwrap_err();
    assert!(err.contains("ignored"), "{err}");
    assert!(!tmp_path.exists());
}
