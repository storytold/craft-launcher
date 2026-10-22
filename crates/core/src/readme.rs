//! What a third-party app's README may make the launcher fetch or open.
//!
//! The README is written by someone ArtCraft doesn't vouch for. Showing it must not turn into
//! "visit any address the author likes": a picture is only fetched from GitHub's own hosts over
//! HTTPS (so reading a README never tells a third-party server who is reading it), a link opens
//! in the browser only as a web or mail address, and nothing here is ever run.

/// The most bytes of one README picture that are kept.
pub const MAX_README_IMAGE_BYTES: usize = 2 * 1024 * 1024;

/// The longest address looked at; anything longer is not a real picture or link.
const MAX_URL_LEN: usize = 2048;

/// The picture formats the launcher can draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Jpeg,
}

/// The kind of picture `bytes` starts like. Only the first bytes are looked at: this refuses a
/// login page or an error page that arrived as a 200, and formats the launcher can't draw (SVG,
/// which could name other files; GIF and WebP, which it has no decoder for).
pub fn sniff(bytes: &[u8]) -> Option<ImageKind> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(ImageKind::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(ImageKind::Jpeg)
    } else {
        None
    }
}

/// Whether `host` is one of GitHub's own: `github.com` (user attachments, raw links) or a
/// subdomain of `githubusercontent.com` (raw files, user images, the image proxy).
fn github_host(host: &str) -> bool {
    host == "github.com" || host == "githubusercontent.com" || host.ends_with(".githubusercontent.com")
}

/// The host of an `https://` address with nothing odd about it (no login part, no port, no
/// spaces or control characters), lower-cased.
fn https_host(url: &str) -> Option<String> {
    if url.len() > MAX_URL_LEN || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let rest = url.get(..8).filter(|p| p.eq_ignore_ascii_case("https://")).and_then(|_| url.get(8..))?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = rest.get(..end)?.to_ascii_lowercase();
    if host.is_empty() || host.contains(['@', ':']) || !host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-')) {
        return None;
    }
    Some(host)
}

/// Whether `url` is an address a README picture may be fetched from.
pub fn image_allowed(url: &str) -> bool {
    https_host(url).is_some_and(|h| github_host(&h))
}

/// A path inside the repository, as a URL path: no `..`, no scheme, no backslashes, no spaces.
fn repo_path(src: &str) -> Option<String> {
    let src = src.split('#').next().unwrap_or("");
    let src = src.trim_start_matches("./").trim_start_matches('/');
    if src.is_empty()
        || src.len() > MAX_URL_LEN
        || src.contains("://")
        || src.contains('\\')
        || src.chars().any(|c| c.is_whitespace() || c.is_control())
        || src.split(['/', '?']).any(|part| part == "..")
    {
        return None;
    }
    Some(src.to_string())
}

/// Where the picture a README names at `src` is fetched from, or `None` when it must not be.
/// A full address needs to be on one of GitHub's hosts; a path (`docs/shot.png`, `/logo.png`) is
/// a file of `repo` (`owner/name`) on its default branch.
pub fn image_url(repo: &str, src: &str) -> Option<String> {
    let src = src.trim();
    if src.contains("://") || src.starts_with("//") {
        return image_allowed(src).then(|| src.to_string());
    }
    // `data:`, `javascript:` and the like are not paths.
    if src.split(['/', '?']).next().is_some_and(|first| first.contains(':')) {
        return None;
    }
    let (owner, name) = repo.split_once('/')?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some(format!("https://raw.githubusercontent.com/{repo}/HEAD/{}", repo_path(src)?))
}

/// The address a README link opens, or `None` when it shouldn't open anything: web and mail
/// addresses as they are, a path inside the repository as its page on GitHub, and nothing else
/// (page anchors, `javascript:`, `file:`, custom schemes).
pub fn link_url(repo: &str, href: &str) -> Option<String> {
    let href = href.trim();
    if href.len() > MAX_URL_LEN || href.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let lower = href.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:") {
        return Some(href.to_string());
    }
    if href.starts_with('#') || href.starts_with("//") || href.is_empty() {
        return None;
    }
    if href.split(['/', '?', '#']).next().is_some_and(|first| first.contains(':')) {
        return None;
    }
    let (owner, name) = repo.split_once('/')?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some(format!("https://github.com/{repo}/blob/HEAD/{}", repo_path(href)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_png_and_jpeg_bytes_are_pictures() {
        assert_eq!(sniff(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0]), Some(ImageKind::Png));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(ImageKind::Jpeg));
        assert_eq!(sniff(b"<svg xmlns='http://www.w3.org/2000/svg'/>"), None);
        assert_eq!(sniff(b"GIF89a"), None);
        assert_eq!(sniff(b"<html>Sign in to the Wi-Fi</html>"), None);
        assert_eq!(sniff(&[]), None);
    }

    #[test]
    fn pictures_come_only_from_githubs_own_hosts_over_https() {
        let repo = "octo/tool";
        for ok in [
            "https://raw.githubusercontent.com/octo/tool/main/a.png",
            "https://user-images.githubusercontent.com/1/a.png",
            "https://github.com/user-attachments/assets/abc",
            "HTTPS://GitHub.com/octo/tool/raw/main/a.png",
        ] {
            assert_eq!(image_url(repo, ok).as_deref(), Some(ok), "{ok}");
        }
        for bad in [
            "http://raw.githubusercontent.com/octo/tool/main/a.png",
            "https://img.shields.io/badge/a-b-green.svg",
            "https://evil.example/a.png",
            "https://github.com.evil.example/a.png",
            "https://evilgithubusercontent.com/a.png",
            "https://github.com@evil.example/a.png",
            "https://raw.githubusercontent.com:8443/a.png",
            "https://raw.githubusercontent.com/a b.png",
            "//raw.githubusercontent.com/a.png",
            "data:image/png;base64,AAAA",
            "javascript:alert(1)",
            "file:///etc/passwd",
        ] {
            assert_eq!(image_url(repo, bad), None, "{bad}");
        }
    }

    #[test]
    fn a_relative_picture_is_a_file_of_the_repository() {
        let url = |s: &str| image_url("octo/tool", s);
        assert_eq!(url("docs/shot.png").as_deref(), Some("https://raw.githubusercontent.com/octo/tool/HEAD/docs/shot.png"));
        assert_eq!(url("./logo.png").as_deref(), Some("https://raw.githubusercontent.com/octo/tool/HEAD/logo.png"));
        assert_eq!(url("/assets/a.jpg?raw=true").as_deref(), Some("https://raw.githubusercontent.com/octo/tool/HEAD/assets/a.jpg?raw=true"));
        assert_eq!(url("../../other/repo/a.png"), None);
        assert_eq!(url("a/../../b.png"), None);
        assert_eq!(url(""), None);
        assert_eq!(url("a\\b.png"), None);
        assert_eq!(image_url("not-a-repo", "a.png"), None);
    }

    #[test]
    fn links_open_only_as_web_or_mail_addresses_or_repository_pages() {
        let url = |s: &str| link_url("octo/tool", s);
        assert_eq!(url("https://example.com/a?b=1").as_deref(), Some("https://example.com/a?b=1"));
        assert_eq!(url("http://example.com").as_deref(), Some("http://example.com"));
        assert_eq!(url("mailto:a@b.c").as_deref(), Some("mailto:a@b.c"));
        assert_eq!(url("docs/INSTALL.md").as_deref(), Some("https://github.com/octo/tool/blob/HEAD/docs/INSTALL.md"));
        assert_eq!(url("#install"), None);
        assert_eq!(url("javascript:alert(1)"), None);
        assert_eq!(url("file:///etc/passwd"), None);
        assert_eq!(url("steam://run/1"), None);
        assert_eq!(url("//evil.example"), None);
        assert_eq!(url("a b"), None);
        assert_eq!(url(""), None);
    }
}
