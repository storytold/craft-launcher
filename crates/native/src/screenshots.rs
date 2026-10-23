//! The pictures on an app's details page. They are not part of the launcher: opening a page
//! fetches them once and keeps them in the cache folder, so the next time (and any time offline)
//! they come from disk.

use std::path::Path;

use artcraft_launcher_core::details::{AppDetails, MAX_SCREENSHOT_BYTES, looks_like_jpeg, screenshot_within_budget};

use crate::http::Http;
use crate::paths::{Paths, write_atomic};

/// The picture saved at `saved`, if there is one and it's still a JPEG of a sane size. A damaged or
/// swapped file is ignored, and the picture is fetched again over it.
fn saved_copy(saved: &Path) -> Option<Vec<u8>> {
    if std::fs::metadata(saved).ok()?.len() > MAX_SCREENSHOT_BYTES as u64 {
        return None;
    }
    std::fs::read(saved).ok().filter(|bytes| looks_like_jpeg(bytes) && screenshot_within_budget(bytes))
}

/// Why a picture couldn't be had.
#[derive(Debug, PartialEq, Eq)]
enum Failure {
    /// This picture only: it isn't published, or what came back isn't a picture.
    Picture(String),
    /// The network or the server: the other pictures would fail the same way.
    Network(String),
}

/// Downloads the picture at `url` and saves it at `saved`. A saved file that can't be written is
/// only a missed cache: the picture is still shown.
fn download(http: &Http, url: &str, saved: &Path) -> Result<Vec<u8>, Failure> {
    let bytes = match http.get_bytes(url, MAX_SCREENSHOT_BYTES) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Err(Failure::Picture("this picture isn't published".into())),
        Err(why) => return Err(Failure::Network(why)),
    };
    // A captive portal's login page or a proxy's error page arrives as a 200: don't keep it.
    if !looks_like_jpeg(&bytes) {
        return Err(Failure::Picture("the answer isn't a JPEG picture".into()));
    }
    // A small JPEG can declare enormous dimensions; the image loader allocates from the header
    // before decoding, so refuse anything outside the pixel budget before it is shown or saved.
    if !screenshot_within_budget(&bytes) {
        return Err(Failure::Picture("the picture's dimensions are too large".into()));
    }
    if let Err(e) = write_atomic(saved, &bytes) {
        log::warn!("couldn't keep {}: {e}", saved.display());
    }
    Ok(bytes)
}

/// Every picture of `details`, lead first, handed to `emit` one by one as it is had. Saved copies
/// need no network. Once the network or the server has failed, the pictures still to fetch fail at
/// once with the same reason instead of each waiting for its own timeout; saved ones are still
/// served. A picture that is merely missing doesn't hold the others back.
pub fn fetch_all(http: Option<&Http>, paths: &Paths, details: &AppDetails, mut emit: impl FnMut(&'static str, Result<Vec<u8>, String>)) {
    let mut failed: Option<String> = None;
    for shot in details.shots {
        let saved = paths.screenshot(details.id, shot.key);
        if let Some(bytes) = saved_copy(&saved) {
            emit(shot.key, Ok(bytes));
            continue;
        }
        if let Some(why) = &failed {
            emit(shot.key, Err(why.clone()));
            continue;
        }
        let result = match (http, details.screenshot_url(shot.key)) {
            (Some(http), Some(url)) => download(http, &url, &saved),
            (None, _) => Err(Failure::Network("HTTPS isn't available".into())),
            (_, None) => Err(Failure::Picture("this page has no such picture".into())),
        };
        let result = result.map_err(|failure| {
            let why = match failure {
                Failure::Network(why) => {
                    failed = Some(why.clone());
                    why
                }
                Failure::Picture(why) => why,
            };
            log::warn!("picture {}/{} unavailable: {why}", details.id, shot.key);
            why
        });
        emit(shot.key, result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::tests::{Reply, serve};

    /// A body that reads as a valid, in-budget JPEG (SOI · APP0 · SOF0 declaring 1200×750), padded
    /// to a realistic size. Its address on the test server is returned alongside where used.
    fn jpeg() -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xD8]; // SOI
        bytes.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00]); // APP0, length 4
        bytes.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]); // SOF0, length 17, precision 8
        bytes.extend_from_slice(&750u16.to_be_bytes()); // height
        bytes.extend_from_slice(&1200u16.to_be_bytes()); // width
        bytes.extend((0..5_000u32).map(|i| (i % 251) as u8));
        bytes
    }

    /// A tiny JPEG whose SOF0 declares a pathological 65535×65535, to exercise the pixel budget.
    fn huge_jpeg() -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xD8];
        bytes.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        bytes.extend_from_slice(&65535u16.to_be_bytes());
        bytes.extend_from_slice(&65535u16.to_be_bytes());
        bytes.extend_from_slice(&[0x03, 0x01, 0x11, 0x00]);
        bytes
    }

    #[test]
    fn a_downloaded_picture_is_saved_where_it_will_be_read_from() {
        let dir = tempfile::tempdir().unwrap();
        let saved = dir.path().join("x").join("pic.jpg");
        let http = Http::new().unwrap();
        let (url, _) = serve(jpeg(), vec![Reply::Full]);
        assert_eq!(download(&http, &url, &saved).unwrap(), jpeg());
        assert_eq!(saved_copy(&saved), Some(jpeg()));
    }

    #[test]
    fn what_is_not_a_picture_is_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let saved = dir.path().join("pic.jpg");
        let http = Http::new().unwrap();
        let (url, _) = serve(b"<html>Sign in to the Wi-Fi</html>".to_vec(), vec![Reply::Full]);
        assert_eq!(download(&http, &url, &saved), Err(Failure::Picture("the answer isn't a JPEG picture".into())));
        assert!(!saved.exists());
        let (url, _) = serve(Vec::new(), vec![Reply::Missing]);
        assert_eq!(download(&http, &url, &saved), Err(Failure::Picture("this picture isn't published".into())));
        assert!(!saved.exists());
        // A server that can't be reached is the network's failure, not this picture's.
        let gone = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        assert!(matches!(download(&http, &format!("http://{gone}/pic.jpg"), &saved), Err(Failure::Network(_))));
    }

    #[test]
    fn a_picture_declaring_enormous_dimensions_is_refused_and_not_saved() {
        let dir = tempfile::tempdir().unwrap();
        let saved = dir.path().join("pic.jpg");
        let http = Http::new().unwrap();
        let (url, _) = serve(huge_jpeg(), vec![Reply::Full]);
        assert_eq!(download(&http, &url, &saved), Err(Failure::Picture("the picture's dimensions are too large".into())));
        assert!(!saved.exists());
        // Even if one is already on disk (e.g. from an older build), it is ignored.
        std::fs::create_dir_all(saved.parent().unwrap()).unwrap();
        std::fs::write(&saved, huge_jpeg()).unwrap();
        assert_eq!(saved_copy(&saved), None, "an oversized saved copy is not trusted");
    }

    #[test]
    fn a_damaged_saved_copy_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let saved = dir.path().join("pic.jpg");
        assert_eq!(saved_copy(&saved), None, "nothing saved");
        std::fs::write(&saved, b"<html>oops</html>").unwrap();
        assert_eq!(saved_copy(&saved), None, "not a JPEG");
        std::fs::write(&saved, vec![0xFF; MAX_SCREENSHOT_BYTES + 1]).unwrap();
        assert_eq!(saved_copy(&saved), None, "too big");
        std::fs::write(&saved, jpeg()).unwrap();
        assert_eq!(saved_copy(&saved), Some(jpeg()));
    }

    #[test]
    fn saved_pictures_need_no_network_and_a_failure_ends_the_run() {
        let paths = Paths::under(tempfile::tempdir().unwrap().path());
        let page = AppDetails::by_id("photocraft").unwrap();
        // The second picture is saved already; the first and the others need the network, which is not there.
        let second = paths.screenshot(page.id, page.shots[1].key);
        write_atomic(&second, &jpeg()).unwrap();
        let mut seen = Vec::new();
        fetch_all(None, &paths, page, |key, result| seen.push((key, result)));
        let keys: Vec<_> = seen.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, page.shots.iter().map(|s| s.key).collect::<Vec<_>>(), "every picture is answered, in order");
        assert!(seen[0].1.is_err() && seen[2].1.is_err() && seen[3].1.is_err());
        assert_eq!(seen[1].1.as_ref().unwrap(), &jpeg());
    }
}
