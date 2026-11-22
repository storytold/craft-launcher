//! The signed notices file from getartcraft.com: fetched at launch and at most every
//! [`REFRESH`] after, cached on disk as fetched and verified again on every read.
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

#[derive(Serialize, Deserialize)]
struct Saved {
    notices: String,
    signature: String,
}

/// The current notices: fetched when `http` is given and a refresh is due, else the verified
/// cache. `None` when neither has a valid signed file, so nothing is shown.
pub fn load(http: Option<&Http>, paths: &Paths, now: i64) -> Option<Notices> {
    let cache = cache_path(paths);
    // Unit tests never reach the website: a slow answer would push the backend tests past
    // their deadlines.
    if let Some(http) = http.filter(|_| !cfg!(test) && due(Instant::now())) {
        match fetch(http, now) {
            Ok((notices, saved)) => {
                if let Ok(bytes) = serde_json::to_vec(&saved)
                    && let Err(why) = write_atomic(&cache, &bytes)
                {
                    log::warn!("could not cache signed notices: {why}");
                }
                return Some(notices);
            }
            Err(why) => log::warn!("signed notices unavailable: {why}; using the verified cache"),
        }
    }
    saved(&cache, now)
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

fn fetch(http: &Http, now: i64) -> Result<(Notices, Saved), String> {
    let notices = http.get_catalog_bytes(NOTICES_URL, MAX_NOTICES_BYTES)?.ok_or("no notices file is published")?;
    let signature = http.get_catalog_bytes(NOTICES_SIGNATURE_URL, MAX_SIGNATURE_BYTES)?.ok_or("notices signature is not published")?;
    let verified = Notices::verify(&notices, &signature, now)?;
    let saved = Saved {
        notices: String::from_utf8(notices).map_err(|_| "notices file is not UTF-8")?,
        signature: String::from_utf8(signature).map_err(|_| "signature is not UTF-8")?,
    };
    Ok((verified, saved))
}

fn cache_path(paths: &Paths) -> PathBuf {
    paths.feed_cache().with_file_name("launcher-announcements.json")
}

fn saved(path: &Path, now: i64) -> Option<Notices> {
    // JSON escaping can expand a signed byte to six.
    let max = (MAX_NOTICES_BYTES * 6 + MAX_SIGNATURE_BYTES + 128) as u64;
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > max {
        return None;
    }
    let saved: Saved = serde_json::from_reader(std::io::Read::take(file, max)).ok()?;
    Notices::verify(saved.notices.as_bytes(), saved.signature.as_bytes(), now).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tampered_or_unsigned_cache_shows_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("launcher-announcements.json");
        assert!(saved(&path, 0).is_none());
        let fake = Saved { notices: "[[announcements]]\nid = \"x\"\ntitle = \"Not ours\"\n".into(), signature: "AAAA".into() };
        std::fs::write(&path, serde_json::to_vec(&fake).unwrap()).unwrap();
        assert!(saved(&path, 0).is_none());
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
