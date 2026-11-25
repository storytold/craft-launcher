//! The signed notices file from getartcraft.com: fetched at launch and at most every
//! [`REFRESH`] after. The latest verified copy is kept in memory and on disk, and verified again,
//! with its dates read afresh, every time it is served.
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use artcraft_launcher_core::hosted_catalog::MAX_SIGNATURE_BYTES;
use artcraft_launcher_core::notices::{MAX_NOTICES_BYTES, NOTICES_SIGNATURE_URL, NOTICES_URL, Notices};
use serde::{Deserialize, Serialize};

use crate::http::Http;
use crate::paths::{Paths, write_atomic};

/// Fewer requests tell the website less: a launcher left in the tray asks four times a day.
const REFRESH: Duration = Duration::from_secs(6 * 3600);

static LAST_FETCH: Mutex<Option<Instant>> = Mutex::new(None);
/// The newest file this process accepted. Served before the disk copy, so a cache write that
/// failed can never bring back notices a newer file withdrew.
static LATEST: Mutex<Option<Saved>> = Mutex::new(None);

#[derive(Clone, Serialize, Deserialize)]
struct Saved {
    notices: String,
    signature: String,
}

/// The current notices: fetched when `http` is given and a refresh is due, else the latest
/// verified copy. `None` when there is no valid signed file, so nothing is shown.
pub fn load(http: Option<&Http>, paths: &Paths, now: i64) -> Option<Notices> {
    // Unit tests never reach the website: a slow answer would push the backend tests past
    // their deadlines.
    let fetched = http
        .filter(|_| !cfg!(test) && due(Instant::now()))
        .and_then(|http| fetch(http).map_err(|why| log::warn!("signed notices unavailable: {why}; using the verified copy")).ok());
    serve(&LATEST, fetched, &cache_path(paths), now, verify)
}

fn verify(saved: &Saved, now: i64) -> Option<Notices> {
    Notices::verify(saved.notices.as_bytes(), saved.signature.as_bytes(), now).ok()
}

/// Accepts a verified `fetched` file as the latest (and caches it), then serves the latest:
/// from memory, or from disk when this process has none yet.
fn serve(
    latest: &Mutex<Option<Saved>>,
    fetched: Option<Saved>,
    cache: &Path,
    now: i64,
    verify: impl Fn(&Saved, i64) -> Option<Notices>,
) -> Option<Notices> {
    let mut latest = latest.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(saved) = fetched.filter(|s| verify(s, now).is_some()) {
        if let Err(why) = serde_json::to_vec(&saved).map_err(|e| e.to_string()).and_then(|bytes| write_atomic(cache, &bytes)) {
            log::warn!("could not cache signed notices: {why}");
        }
        *latest = Some(saved);
    }
    if latest.is_none() {
        *latest = read_cache(cache);
    }
    latest.as_ref().and_then(|s| verify(s, now))
}

/// Claims the next fetch when none has run for [`REFRESH`]. A failed fetch also waits.
fn due(now: Instant) -> bool {
    let mut last = LAST_FETCH.lock().unwrap_or_else(PoisonError::into_inner);
    if last.is_some_and(|t| now.saturating_duration_since(t) < REFRESH) {
        return false;
    }
    *last = Some(now);
    true
}

fn fetch(http: &Http) -> Result<Saved, String> {
    let notices = http.get_catalog_bytes(NOTICES_URL, MAX_NOTICES_BYTES)?.ok_or("no notices file is published")?;
    let signature = http.get_catalog_bytes(NOTICES_SIGNATURE_URL, MAX_SIGNATURE_BYTES)?.ok_or("notices signature is not published")?;
    Ok(Saved {
        notices: String::from_utf8(notices).map_err(|_| "notices file is not UTF-8")?,
        signature: String::from_utf8(signature).map_err(|_| "signature is not UTF-8")?,
    })
}

fn cache_path(paths: &Paths) -> PathBuf {
    paths.feed_cache().with_file_name("launcher-announcements.json")
}

fn read_cache(path: &Path) -> Option<Saved> {
    // JSON escaping can expand a signed byte to six.
    let max = (MAX_NOTICES_BYTES * 6 + MAX_SIGNATURE_BYTES + 128) as u64;
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > max {
        return None;
    }
    serde_json::from_reader(std::io::Read::take(file, max)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use artcraft_launcher_core::notices::Notice;

    /// Stands in for the signature check, which needs the website's private key: a file
    /// "signed" `ok` verifies, and its text becomes one notice's id.
    fn fake_verify(saved: &Saved, _now: i64) -> Option<Notices> {
        (saved.signature == "ok")
            .then(|| Notices { announcements: vec![Notice { id: saved.notices.clone(), ..Notice::default() }], alerts: Vec::new() })
    }

    fn file(text: &str) -> Saved {
        Saved { notices: text.into(), signature: "ok".into() }
    }

    fn shown(n: Option<Notices>) -> Option<String> {
        n.and_then(|n| n.announcements.first().map(|a| a.id.clone()))
    }

    #[test]
    fn a_tampered_or_unsigned_cache_shows_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launcher-announcements.json");
        assert!(serve(&Mutex::new(None), None, &path, 0, verify).is_none());
        let fake = Saved { notices: "[[announcements]]\nid = \"x\"\ntitle = \"Not ours\"\n".into(), signature: "AAAA".into() };
        std::fs::write(&path, serde_json::to_vec(&fake).unwrap()).unwrap();
        assert!(serve(&Mutex::new(None), None, &path, 0, verify).is_none());
    }

    #[test]
    fn a_newer_file_replaces_the_cache_and_survives_a_failed_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launcher-announcements.json");
        std::fs::write(&path, serde_json::to_vec(&file("old")).unwrap()).unwrap();
        // An existing cache is served, and a successful fetch replaces it on disk too.
        assert_eq!(shown(serve(&Mutex::new(None), None, &path, 0, fake_verify)).as_deref(), Some("old"));
        assert_eq!(shown(serve(&Mutex::new(None), Some(file("new")), &path, 0, fake_verify)).as_deref(), Some("new"));
        assert_eq!(shown(serve(&Mutex::new(None), None, &path, 0, fake_verify)).as_deref(), Some("new"));
        // The cache can no longer be written: a later fetch still wins over the disk copy, also
        // on the throttled load after it.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let latest = Mutex::new(None);
        assert_eq!(shown(serve(&latest, Some(file("newest")), &path, 0, fake_verify)).as_deref(), Some("newest"));
        assert_eq!(shown(serve(&latest, None, &path, 0, fake_verify)).as_deref(), Some("newest"));
        // A fetched file that fails verification changes nothing.
        let bad = Saved { notices: "forged".into(), signature: "no".into() };
        assert_eq!(shown(serve(&latest, Some(bad), &path, 0, fake_verify)).as_deref(), Some("newest"));
    }

    #[test]
    fn refreshes_wait_six_hours() {
        let start = Instant::now();
        *LAST_FETCH.lock().unwrap() = None;
        assert!(due(start));
        assert!(!due(start + Duration::from_secs(60)));
        assert!(due(start + REFRESH));
    }
}
