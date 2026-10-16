//! GitHub releases: reading the API's answer and `SHA256SUMS.txt`.
//!
//! Everything here is untrusted input. Sizes are bounded by the caller, download links must point
//! at the app's own release downloads, and checksums must look like checksums.

use serde::{Deserialize, Serialize};

use crate::version::{self, Version};

/// One downloadable file of a release.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    /// Bytes; 0 when unknown.
    pub size: u64,
    /// `https://github.com/<repo>/releases/download/<tag>/<name>`.
    pub url: String,
    /// Lowercase hex SHA-256, when GitHub or `SHA256SUMS.txt` told us.
    pub sha256: Option<String>,
}

/// A published release of one app.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub tag: String,
    pub version: Option<Version>,
    /// `PhotoCraft v0.5.0`
    pub name: String,
    /// Unix seconds; `None` when only the tag is known.
    pub published_at: Option<i64>,
    pub html_url: String,
    /// Release notes (Markdown), capped.
    pub notes: String,
    pub prerelease: bool,
    pub assets: Vec<Asset>,
}

impl Release {
    pub fn version_label(&self) -> String {
        self.version.as_ref().map_or_else(|| self.tag.clone(), ToString::to_string)
    }

    /// Fills in checksums from a `SHA256SUMS.txt` for assets that don't have one.
    pub fn apply_sums(&mut self, sums: &[(String, String)]) {
        for asset in &mut self.assets {
            if asset.sha256.is_none() {
                asset.sha256 = sums.iter().find(|(_, n)| *n == asset.name).map(|(h, _)| h.clone());
            }
        }
    }

    pub fn sums_asset(&self) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == "SHA256SUMS.txt")
    }
}

const MAX_NOTES: usize = 64 * 1024;

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Whether `url` is safe to hand to the system browser from untrusted text (release notes):
/// `https` only, a host, no user-info, no spaces or control characters.
pub fn safe_web_link(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    url.len() <= 2000 && !authority.is_empty() && !authority.contains(['@', '\\']) && !url.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// The prefix every download of `repo` must start with.
pub fn download_prefix(repo: &str) -> String {
    format!("https://github.com/{repo}/releases/download/")
}

/// Reads `GET /repos/<repo>/releases/latest` (or one element of `/releases`).
pub fn parse_github_release(json: &str, repo: &str) -> Result<Release, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("unreadable answer from GitHub ({e})"))?;
    release_from_value(&v, repo)
}

/// Reads `GET /repos/<repo>/releases` and returns the newest stable (or any, with
/// `allow_prerelease`) published release.
pub fn parse_github_release_list(json: &str, repo: &str, allow_prerelease: bool) -> Result<Option<Release>, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("unreadable answer from GitHub ({e})"))?;
    let list = v.as_array().ok_or("unexpected answer from GitHub")?;
    let mut best: Option<Release> = None;
    for item in list {
        if item["draft"].as_bool().unwrap_or(false) {
            continue;
        }
        let Ok(r) = release_from_value(item, repo) else { continue };
        let Some(ver) = r.version.as_ref() else { continue };
        if !allow_prerelease && (r.prerelease || !version::is_stable(ver)) {
            continue;
        }
        let newer = best.as_ref().and_then(|b| b.version.as_ref()).is_none_or(|b| version::is_newer(ver, b));
        if newer {
            best = Some(r);
        }
    }
    Ok(best)
}

fn release_from_value(v: &serde_json::Value, repo: &str) -> Result<Release, String> {
    if let Some(msg) = v["message"].as_str() {
        return Err(if msg == "Not Found" { "no releases yet".into() } else { format!("GitHub says: {}", truncate(msg, 200)) });
    }
    let tag = v["tag_name"].as_str().filter(|t| !t.is_empty() && t.len() <= 64).ok_or("no release found")?.to_string();
    let releases = format!("https://github.com/{repo}/releases");
    let html_url = v["html_url"]
        .as_str()
        .filter(|u| u.strip_prefix(&releases).is_some_and(|rest| rest.starts_with('/') && !rest.contains(['?', '#', '\\'])))
        .map_or_else(|| releases.clone(), str::to_string);
    let prefix = download_prefix(repo);
    let assets = v["assets"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|a| {
                    let name = a["name"].as_str().filter(|n| valid_file_name(n))?.to_string();
                    let url = a["browser_download_url"].as_str().filter(|u| u.starts_with(&prefix) && !u.contains(['?', '#', '\\']))?.to_string();
                    let sha256 = a["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).filter(|h| is_hex64(h)).map(str::to_ascii_lowercase);
                    Some(Asset { name, size: a["size"].as_u64().unwrap_or(0), url, sha256 })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Release {
        version: version::parse(&tag),
        name: v["name"].as_str().map(|n| truncate(n, 120)).filter(|n| !n.is_empty()).unwrap_or_else(|| tag.clone()),
        tag,
        published_at: v["published_at"].as_str().and_then(crate::time::parse_rfc3339),
        html_url,
        notes: v["body"].as_str().map(|b| truncate(b, MAX_NOTES)).unwrap_or_default(),
        prerelease: v["prerelease"].as_bool().unwrap_or(false),
        assets,
    })
}

/// A release known only from its tag and `SHA256SUMS.txt` (the quota-free route: the
/// `releases/latest` redirect names the tag, and the sums list every file).
pub fn release_from_sums(repo: &str, tag: &str, sums: &[(String, String)]) -> Release {
    let prefix = download_prefix(repo);
    let mut assets: Vec<Asset> = sums
        .iter()
        .map(|(hash, name)| Asset { name: name.clone(), size: 0, url: format!("{prefix}{tag}/{name}"), sha256: Some(hash.clone()) })
        .collect();
    assets.push(Asset { name: "SHA256SUMS.txt".into(), size: 0, url: format!("{prefix}{tag}/SHA256SUMS.txt"), sha256: None });
    Release {
        tag: tag.to_string(),
        version: version::parse(tag),
        name: tag.to_string(),
        published_at: None,
        html_url: format!("https://github.com/{repo}/releases/tag/{tag}"),
        notes: String::new(),
        prerelease: false,
        assets,
    }
}

/// The tag in a `https://github.com/<repo>/releases/tag/<tag>` redirect target.
pub fn tag_from_redirect(location: &str, repo: &str) -> Option<String> {
    let rest = location.strip_prefix(&format!("https://github.com/{repo}/releases/tag/"))?;
    (!rest.is_empty() && rest.len() <= 64 && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))).then(|| rest.to_string())
}

/// Reads `sha256sum` output: `<hex>  <name>` (or `<hex> *<name>`) per line.
pub fn parse_sha256sums(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (hash, rest) = line.trim().split_once(char::is_whitespace)?;
            let name = rest.trim_start().trim_start_matches('*');
            (is_hex64(hash) && valid_file_name(name)).then(|| (hash.to_ascii_lowercase(), name.to_string()))
        })
        .collect()
}

/// A bare file name we'd be willing to save: no paths, no control characters.
pub fn valid_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && name != "."
        && name != ".."
        && !name.starts_with('.')
        && !name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*'))
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPO: &str = "storytold/photocraft";

    #[test]
    fn only_plain_https_links_pass() {
        assert!(safe_web_link("https://github.com/storytold/photocraft/releases/tag/v1"));
        for bad in [
            "http://example.com/",
            "javascript:alert(1)",
            "file:///C:/Windows/System32/calc.exe",
            "ms-msdt:/id",
            "https://",
            "https://user@evil.example/",
            "https://example.com/a b",
            "https://example.com/\nx",
            "https:///path",
        ] {
            assert!(!safe_web_link(bad), "{bad:?}");
        }
    }
    const HASH: &str = "8a5edab282632443219e051e4ade2d1d5bbc671c781051bf1437897cbdfea0f1";

    fn api_json(url: &str) -> String {
        format!(
            r#"{{"tag_name":"v0.5.0","name":"PhotoCraft v0.5.0","html_url":"https://github.com/storytold/photocraft/releases/tag/v0.5.0",
            "published_at":"2026-10-08T14:52:50Z","prerelease":false,"body":"Notes",
            "assets":[{{"name":"photocraft-0.5.0-macos-universal.dmg","size":123,"browser_download_url":"{url}","digest":"sha256:{HASH}"}}]}}"#
        )
    }

    #[test]
    fn reads_the_latest_release() {
        let r = parse_github_release(
            &api_json("https://github.com/storytold/photocraft/releases/download/v0.5.0/photocraft-0.5.0-macos-universal.dmg"),
            REPO,
        )
        .unwrap();
        assert_eq!(r.version, Some(Version::new(0, 5, 0)));
        assert_eq!(r.name, "PhotoCraft v0.5.0");
        assert_eq!(r.published_at, Some(1_791_471_170));
        assert_eq!(r.assets.len(), 1);
        assert_eq!(r.assets[0].sha256.as_deref(), Some(HASH));
        assert_eq!(r.assets[0].size, 123);
    }

    #[test]
    fn drops_downloads_that_point_elsewhere() {
        for url in ["https://evil.example/photocraft.dmg", "https://github.com/storytold/other/releases/download/v1/x.dmg", "javascript:alert(1)"] {
            let r = parse_github_release(&api_json(url), REPO).unwrap();
            assert!(r.assets.is_empty(), "{url}");
        }
        assert_eq!(parse_github_release(r#"{"message":"Not Found"}"#, REPO).unwrap_err(), "no releases yet");
        assert!(parse_github_release("<html>", REPO).is_err());
    }

    #[test]
    fn list_picks_the_newest_stable_release() {
        let json = r#"[
            {"tag_name":"v0.6.0-rc.1","prerelease":false,"draft":false,"assets":[]},
            {"tag_name":"v0.5.1","prerelease":true,"draft":false,"assets":[]},
            {"tag_name":"v0.7.0","draft":true,"assets":[]},
            {"tag_name":"v0.5.0","prerelease":false,"draft":false,"assets":[]},
            {"tag_name":"v0.4.0","prerelease":false,"draft":false,"assets":[]}]"#;
        assert_eq!(parse_github_release_list(json, REPO, false).unwrap().unwrap().tag, "v0.5.0");
        assert_eq!(parse_github_release_list(json, REPO, true).unwrap().unwrap().tag, "v0.6.0-rc.1");
        assert_eq!(parse_github_release_list("[]", REPO, false).unwrap(), None);
    }

    #[test]
    fn reads_checksums_and_redirects() {
        let sums = parse_sha256sums(&format!("{HASH}  photocraft-0.5.0-macos-universal.dmg\n{HASH} *b.zip\nnot a line\n{HASH}  ../escape\n"));
        assert_eq!(sums, [(HASH.to_string(), "photocraft-0.5.0-macos-universal.dmg".to_string()), (HASH.to_string(), "b.zip".to_string())]);
        assert_eq!(tag_from_redirect("https://github.com/storytold/photocraft/releases/tag/v0.5.0", REPO).as_deref(), Some("v0.5.0"));
        assert_eq!(tag_from_redirect("https://github.com/storytold/photocraft/releases", REPO), None);
        assert_eq!(tag_from_redirect("https://evil/storytold/photocraft/releases/tag/v1", REPO), None);
        let r = release_from_sums(REPO, "v0.5.0", &sums);
        assert_eq!(r.assets[0].url, "https://github.com/storytold/photocraft/releases/download/v0.5.0/photocraft-0.5.0-macos-universal.dmg");
        assert_eq!(r.version, Some(Version::new(0, 5, 0)));
    }

    #[test]
    fn file_names_cannot_escape() {
        for bad in ["", "..", "a/b", "a\\b", ".hidden", "c:x", "x\u{0}"] {
            assert!(!valid_file_name(bad), "{bad:?}");
        }
        assert!(valid_file_name("photocraft-0.5.0-linux-x86_64.AppImage"));
    }
}
