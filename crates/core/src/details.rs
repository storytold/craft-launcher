//! Hand-written product pages: what each app is, in the launcher's own words.
//!
//! Bundled text works offline and on the web; a verified hosted snapshot may replace it. What changes with every release (version, date, size, notes) comes
//! from the GitHub release instead. An app without an entry here simply has no details page.
//!
//! The pictures are not compiled in: they are fetched when a page is opened (see
//! [`AppDetails::screenshot_url`]), so the repository and the binary stay small.

/// One feature: a short name and a sentence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Feature {
    pub title: &'static str,
    pub text: &'static str,
}

/// A screenshot: `key` names the file to fetch (`<app>/<key>.jpg` in the pictures repository).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shot {
    pub key: &'static str,
    pub caption: &'static str,
}

const fn shot(key: &'static str, caption: &'static str) -> Shot {
    Shot { key, caption }
}

/// The editorial half of an app's page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppDetails {
    /// Matches [`crate::CraftApp::id`].
    pub id: &'static str,
    /// One line under the name.
    pub headline: &'static str,
    /// One or two paragraphs, separated by a blank line.
    pub about: &'static str,
    pub features: &'static [Feature],
    /// The first is the lead picture. All are 8:5.
    pub shots: &'static [Shot],
    /// Where it runs, for display (`"macOS, Windows, Linux, web"`).
    pub platforms: &'static str,
}

const fn feature(title: &'static str, text: &'static str) -> Feature {
    Feature { title, text }
}

/// Where the pictures live: one 1200 px wide, 8:5 JPEG per [`Shot`], at `<app>/<key>.jpg`.
///
/// Pinned to a tag (`SCREENSHOTS_REV`) so that what a launcher fetches never changes under it, and
/// so that a saved copy never goes stale. To change a picture, publish a new tag and bump the
/// revision: older launchers keep working from the tag they know.
pub const SCREENSHOTS_BASE: &str = "https://raw.githubusercontent.com/storytold/craft-launcher-assets";

/// The tag of the pictures repository the launcher reads. Also part of the saved copies' path.
pub const SCREENSHOTS_REV: &str = "v1";

/// The largest picture the launcher accepts. The real ones are 60 to 150 KB.
pub const MAX_SCREENSHOT_BYTES: usize = 2 << 20;

/// The largest decoded picture the launcher will hand to the image loader, in pixels.
///
/// The real screenshots are 1200 px wide and 8:5 (≈0.9 MP). A byte cap alone does not bound the
/// decoded size: a tiny JPEG can declare 65,535 × 65,535, and the loader allocates
/// width × height × channels *before* decoding, turning a 2 MiB response into a multi-gigabyte
/// allocation (CWE-789). 8192 × 8192 (67 MP) is far above any real screenshot yet refuses the
/// pathological headers.
pub const MAX_SCREENSHOT_PIXELS: u64 = 8192 * 8192;

/// True when `bytes` start like a JPEG file. A captive portal's login page, a proxy's error page or
/// a truncated answer is refused before it is saved or decoded.
pub fn looks_like_jpeg(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0xD8, 0xFF])
}

/// Reads the pixel dimensions `(width, height)` declared in a JPEG's frame header, without
/// decoding any pixels. `None` when the bytes aren't a JPEG we can read a frame size from.
///
/// It walks the marker segments to the Start-Of-Frame (SOFn) marker, whose payload begins with
/// `precision (1) · height (2, big-endian) · width (2)`.
pub fn jpeg_dimensions(bytes: &[u8]) -> Option<(u16, u16)> {
    if !looks_like_jpeg(bytes) {
        return None;
    }
    let mut i = 2; // past the SOI (FF D8).
    let mut dims = None;
    while i + 1 < bytes.len() {
        // Markers are FF, with any number of FF fill bytes before the type byte.
        if bytes[i] != 0xFF {
            break;
        }
        let mut j = i;
        while j < bytes.len() && bytes[j] == 0xFF {
            j += 1;
        }
        let marker = *bytes.get(j)?;
        i = j + 1;
        match marker {
            // Standalone markers with no length: RSTn, SOI, EOI, TEM.
            0x01 | 0xD0..=0xD9 => continue,
            // SOS (Start of Scan): marker headers end, entropy-coded scan data begins.
            0xDA => break,
            _ => {}
        }
        let len = u16::from_be_bytes([*bytes.get(i)?, *bytes.get(i + 1)?]) as usize;
        if len < 2 {
            return None;
        }
        // Only SOF0 (0xC0: baseline), SOF1 (0xC1: extended sequential), and SOF2 (0xC2: progressive)
        // are supported by standard image rendering decoders.
        if matches!(marker, 0xC0..=0xC2) {
            if dims.is_some() {
                // Ambiguous / multiple frame markers: reject to prevent frame smuggling (CWE-789).
                return None;
            }
            let h = u16::from_be_bytes([*bytes.get(i + 3)?, *bytes.get(i + 4)?]);
            let w = u16::from_be_bytes([*bytes.get(i + 5)?, *bytes.get(i + 6)?]);
            dims = Some((w, h));
        } else if matches!(marker, 0xC3 | 0xC5..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            // Unsupported frame marker (e.g. lossless 0xC3): the rendering decoder will skip it or fail,
            // so accepting its dimensions would let a subsequent large frame bypass the budget (CWE-789).
            return None;
        }
        i += len;
    }
    dims
}

/// Whether `bytes` is a JPEG whose declared dimensions are within [`MAX_SCREENSHOT_PIXELS`], so it
/// is safe to hand to an image loader that allocates from the header before decoding.
pub fn screenshot_within_budget(bytes: &[u8]) -> bool {
    match jpeg_dimensions(bytes) {
        Some((w, h)) => w != 0 && h != 0 && u64::from(w) * u64::from(h) <= MAX_SCREENSHOT_PIXELS,
        // A JPEG we can't read a frame size from is refused rather than trusted.
        None => false,
    }
}

include!(concat!(env!("OUT_DIR"), "/details.rs"));

impl AppDetails {
    /// Where the picture `key` of this app is published, or `None` if the page has no such picture.
    /// The only way to turn a request into an address: callers never build one from their own text.
    pub fn screenshot_url(&self, key: &str) -> Option<String> {
        self.shots.iter().find(|s| s.key == key).map(|s| format!("{SCREENSHOTS_BASE}/{SCREENSHOTS_REV}/{}/{}.jpg", self.id, s.key))
    }

    pub fn by_id(id: &str) -> Option<&'static AppDetails> {
        crate::hosted_catalog::details(id).or_else(|| DETAILS.iter().find(|d| d.id == id))
    }

    pub fn paragraphs(&self) -> impl Iterator<Item = &'static str> {
        self.about.split("\n\n").map(str::trim).filter(|p| !p.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CraftApp, SUITE};

    #[test]
    fn every_page_belongs_to_a_suite_app_once() {
        let mut ids: Vec<_> = DETAILS.iter().map(|d| d.id).collect();
        for id in &ids {
            assert!(CraftApp::by_id(id).is_some(), "{id}");
        }
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), DETAILS.len());
    }

    #[test]
    fn every_app_has_a_page() {
        for a in &SUITE {
            assert!(AppDetails::by_id(a.id).is_some(), "{}", a.id);
        }
    }

    #[test]
    fn picture_addresses_come_only_from_the_pages_own_pictures() {
        let page = AppDetails::by_id("photocraft").unwrap();
        assert_eq!(
            page.screenshot_url("demo").as_deref(),
            Some("https://raw.githubusercontent.com/storytold/craft-launcher-assets/v1/photocraft/demo.jpg")
        );
        for key in ["", "nope", "../vectorcraft/neon", "demo/../masks", "demo.jpg", "/etc/passwd", "demo?x=1", "demo#"] {
            assert_eq!(page.screenshot_url(key), None, "{key:?}");
        }
        // Every picture of every page has an address of the one shape, with nothing to escape.
        for d in &DETAILS {
            for s in d.shots {
                let url = d.screenshot_url(s.key).unwrap();
                let path = url.strip_prefix(&format!("{SCREENSHOTS_BASE}/{SCREENSHOTS_REV}/")).unwrap();
                assert_eq!(path, format!("{}/{}.jpg", d.id, s.key));
                assert!(path.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '/' | '-' | '.')), "{path}");
            }
        }
    }

    #[test]
    fn only_jpeg_files_are_accepted() {
        assert!(looks_like_jpeg(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10]));
        assert!(!looks_like_jpeg(b""));
        assert!(!looks_like_jpeg(&[0xFF, 0xD8]));
        assert!(!looks_like_jpeg(b"<!DOCTYPE html>"));
        // A PNG's signature.
        assert!(!looks_like_jpeg(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]));
    }

    /// A minimal JPEG: SOI, an APP0 segment, then a baseline SOF0 declaring `w × h`.
    fn jpeg_of(w: u16, h: u16) -> Vec<u8> {
        let mut b = vec![0xFF, 0xD8]; // SOI
        b.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00]); // APP0, length 4, two payload bytes
        b.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]); // SOF0, length 17, precision 8
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&[0x03]); // component count (payload need not be complete for parsing)
        b
    }

    #[test]
    fn reads_jpeg_frame_dimensions() {
        assert_eq!(jpeg_dimensions(&jpeg_of(1200, 750)), Some((1200, 750)));
        assert_eq!(jpeg_dimensions(&jpeg_of(65535, 65535)), Some((65535, 65535)));
        // Not a JPEG, or no frame header present.
        assert_eq!(jpeg_dimensions(b"<html>"), None);
        assert_eq!(jpeg_dimensions(&[0xFF, 0xD8]), None);

        // Unsupported C3 frame marker is rejected (CWE-789).
        let mut c3 = vec![0xFF, 0xD8, 0xFF, 0xC3, 0x00, 0x11, 0x08];
        c3.extend_from_slice(&100u16.to_be_bytes());
        c3.extend_from_slice(&100u16.to_be_bytes());
        c3.push(0x03);
        assert_eq!(jpeg_dimensions(&c3), None);

        // Multiple / conflicting SOF markers are rejected.
        let mut multi = vec![0xFF, 0xD8];
        multi.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
        multi.extend_from_slice(&100u16.to_be_bytes());
        multi.extend_from_slice(&100u16.to_be_bytes());
        multi.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]);
        multi.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
        multi.extend_from_slice(&9000u16.to_be_bytes());
        multi.extend_from_slice(&9000u16.to_be_bytes());
        multi.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]);
        assert_eq!(jpeg_dimensions(&multi), None);
    }

    #[test]
    fn oversized_screenshots_are_refused_before_decoding() {
        assert!(screenshot_within_budget(&jpeg_of(1200, 750)));
        assert!(screenshot_within_budget(&jpeg_of(8192, 8192)));
        // The decompression-bomb header: a tiny file declaring billions of pixels.
        assert!(!screenshot_within_budget(&jpeg_of(65535, 65535)));
        assert!(!screenshot_within_budget(&jpeg_of(8193, 8192)));
        assert!(!screenshot_within_budget(&jpeg_of(0, 100)));
        // A non-JPEG (or a JPEG we can't size) is refused rather than trusted.
        assert!(!screenshot_within_budget(b"<html>"));
    }

    #[test]
    fn pages_are_filled_in() {
        for d in &DETAILS {
            assert!(!d.headline.is_empty() && !d.features.is_empty() && d.paragraphs().next().is_some(), "{}", d.id);
        }
    }
}
