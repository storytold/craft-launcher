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
/// `https` only, a host, no user-info, no spaces or control characters, and no shell metacharacters.
///
/// The system opener is not always a direct `open`: the pinned `webbrowser` crate's WSL fallback
/// passes the URL to `cmd.exe /c start` while escaping only `^` and `&`, so a character like `|`
/// in an otherwise valid URL becomes a command separator (CWE-78). Because a legitimate
/// `https://` web link never needs these characters unescaped, we refuse any URL that contains a
/// shell/cmd metacharacter rather than rely on the opener to neutralise it.
pub fn safe_web_link(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    url.len() <= 2000
        && !authority.is_empty()
        && !authority.contains(['@', '\\'])
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
        && !url.chars().any(is_shell_meta)
}

/// Characters a command interpreter (POSIX shells or `cmd.exe`) may treat specially. A real
/// `https` URL never needs any of them unescaped in a browser-opened link. `%` matters because
/// `cmd.exe` expands `%NAME%` references, and an expanded value could itself carry metacharacters.
pub fn is_shell_meta(c: char) -> bool {
    matches!(c, '|' | '&' | ';' | '<' | '>' | '(' | ')' | '$' | '`' | '"' | '\'' | '^' | '!' | '*' | '%' | '{' | '}' | '[' | ']')
}

/// Whether `url` is a valid release page URL strictly for `repo` on GitHub:
/// starts with `https://github.com/<repo>/releases`, contains no dot-segments (`.` or `..`),
/// no percent-encoded path separators (`%2f`, `%5c`), no query/fragment/backslash, and
/// satisfies [`safe_web_link`] (no shell metacharacters or control bytes, CWE-78).
pub fn safe_release_html_url(repo: &str, url: &str) -> Option<String> {
    if !safe_web_link(url) {
        return None;
    }
    let releases = format!("https://github.com/{repo}/releases");
    if url == releases {
        return Some(url.to_string());
    }
    let rest = url.strip_prefix(&releases)?;
    let path = rest.strip_prefix('/')?;
    if path.is_empty() || path.contains(['?', '#', '\\']) {
        return None;
    }
    for seg in path.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." || seg.contains("%2f") || seg.contains("%2F") || seg.contains("%5c") || seg.contains("%5C") {
            return None;
        }
    }
    Some(url.to_string())
}

/// The prefix every download of `repo` must start with.
pub fn download_prefix(repo: &str) -> String {
    format!("https://github.com/{repo}/releases/download/")
}

/// Whether `url` is a download that truly belongs to `repo`.
///
/// A raw `starts_with(prefix)` check is not enough: a value such as
/// `https://github.com/storytold/craft-launcher/releases/download/../../../ATTACKER/REPO/releases/download/v1/x`
/// shares the prefix textually, yet its interpreted path escapes the repository. The link is
/// accepted only when the text after the prefix is exactly `<tag>/<name>` — two non-empty path
/// segments with no traversal (`.`/`..`), no additional slashes, no query or fragment, and no
/// backslash or percent-encoded separators that a client could later re-interpret.
pub fn download_url_under(url: &str, prefix: &str) -> bool {
    let Some(rest) = url.strip_prefix(prefix) else {
        return false;
    };
    download_suffix_ok(rest)
}

/// Whether `url` sits under a developer mirror `base` (ending in `/`) without escaping it.
pub fn download_url_under_mirror(url: &str, base: &str) -> bool {
    let Some(rest) = url.strip_prefix(base) else {
        return false;
    };
    // A mirror serves `<tag>/<name>` just like GitHub, but we only require that it can't climb out
    // of its own folder; its layout is otherwise the developer's business.
    !rest.is_empty() && !url.contains(['?', '#', '\\']) && !contains_traversal(rest) && !contains_encoded_separator(rest)
}

/// `<tag>/<name>`: exactly two non-empty segments, no traversal, no re-interpretable separators.
fn download_suffix_ok(rest: &str) -> bool {
    if rest.contains(['?', '#', '\\']) || contains_encoded_separator(rest) {
        return false;
    }
    let mut parts = rest.split('/');
    let (Some(tag), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    valid_tag(tag) && valid_file_name(name)
}

/// Any `.` or `..` path segment that could climb out of the repository.
fn contains_traversal(rest: &str) -> bool {
    rest.split('/').any(|seg| seg == "." || seg == "..")
}

/// Percent-encoded slash or backslash (`%2f`, `%5c`): a client could decode these back into
/// separators after the check, so a confined URL must not contain them.
fn contains_encoded_separator(rest: &str) -> bool {
    let bytes = rest.as_bytes();
    bytes.windows(3).any(|w| w[0] == b'%' && matches!(&w[1..].to_ascii_lowercase()[..], b"2f" | b"5c"))
}

/// Reads `GET /repos/<repo>/releases/latest` (or one element of `/releases`).
///
/// # Errors
///
/// Returns an error if the JSON is malformed or does not describe a usable release.
pub fn parse_github_release(json: &str, repo: &str) -> Result<Release, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("unreadable answer from GitHub ({e})"))?;
    release_from_value(&v, repo)
}

/// Published releases offered for manual selection, in GitHub's order.
/// Drafts and unparseable tags cannot be installed.
///
/// # Errors
///
/// Returns an error if the JSON is malformed or its top level is not an array.
pub fn parse_github_versions(json: &str, repo: &str) -> Result<Vec<Release>, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("unreadable answer from GitHub ({e})"))?;
    let list = v.as_array().ok_or("unexpected answer from GitHub")?;
    Ok(list
        .iter()
        .filter(|item| !item["draft"].as_bool().unwrap_or(false))
        .filter_map(|item| release_from_value(item, repo).ok())
        .filter(|r| r.version.is_some())
        .take(100)
        .collect())
}

/// A published, explicitly marked nightly build, not merely a beta or release candidate.
pub fn is_nightly(release: &Release) -> bool {
    release.prerelease && release.version.as_ref().is_some_and(|v| v.pre.as_str().split('.').any(|part| part.eq_ignore_ascii_case("nightly")))
}

/// Canonical parser-level Nightly eligibility, not descriptor or target validation.
///
/// Only a strict SemVer tag, optionally prefixed with lowercase `v`, qualifies. The parsed
/// version must agree with the release, including build metadata, and its first prerelease
/// component must be exactly `nightly`. Short canonical tags remain candidates for the native
/// layer to validate against their descriptors.
pub fn canonical_nightly_version(release: &Release) -> Option<Version> {
    if !release.prerelease {
        return None;
    }
    let tag = release.tag.strip_prefix('v').unwrap_or(&release.tag);
    let parsed = Version::parse(tag).ok()?;
    if parsed.to_string() != tag || release.version.as_ref() != Some(&parsed) || parsed.pre.as_str().split('.').next() != Some("nightly") {
        return None;
    }
    Some(parsed)
}

/// Whether a release has a published checksum or checksum-list asset.
pub fn has_published_checksum(r: &Release) -> bool {
    r.sums_asset().is_some() || r.assets.iter().any(|a| a.sha256.is_some())
}

/// Selects a published release. A nightly must be canonical and have a published checksum;
/// a pin never falls back. Nightlies use SemVer precedence, ignoring build metadata. Equal
/// precedence prefers the lexically smallest tag, never publication time or a separate run ordinal.
pub fn choose_version(releases: &[Release], track: &crate::protocol::VersionTrack) -> Option<Release> {
    choose_version_with_policy(releases, track, NightlySelection::Canonical)
}

/// The separate channel has canonical build ordering; ordinary legacy Nightly tracks retain
/// main's publication-time ordering. Eligibility such as checksums/targets remains caller-owned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NightlySelection {
    Canonical,
    Published,
}

/// Best candidates first, without cloning release bodies or fetching evidence.
/// Stable sorting then reversing also preserves the old last-input winner on exact ties.
/// Native discovery must reject malformed candidate identities before calling this helper and
/// must not silently prefilter missing checksum evidence; ordinary selection requires checksums.
pub fn ranked_nightly_candidates<'a>(releases: impl IntoIterator<Item = &'a Release>, policy: NightlySelection) -> std::vec::IntoIter<&'a Release> {
    let mut ordered = match policy {
        NightlySelection::Canonical => {
            let mut candidates: Vec<_> =
                releases.into_iter().filter_map(|release| canonical_nightly_version(release).map(|version| (version, release))).collect();
            candidates.sort_by(|(av, a), (bv, b)| version::compare(av, bv).then_with(|| b.tag.cmp(&a.tag)));
            candidates.into_iter().map(|(_, release)| release).collect::<Vec<_>>()
        }
        NightlySelection::Published => {
            let mut candidates: Vec<_> = releases.into_iter().filter(|release| is_nightly(release)).collect();
            candidates.sort_by_key(|release| release.published_at);
            candidates
        }
    };
    ordered.reverse();
    ordered.into_iter()
}

/// What to ask GitHub about one app's releases. `repo` may follow the feed's repo renames, so
/// whether the app's nightlies have their own channel is decided by the app, never by `repo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseQuery {
    pub repo: String,
    pub separate_nightly: bool,
    /// The app's [`former_repo`](crate::CraftApp::former_repo), asked when `repo` has no release.
    pub former_repo: Option<String>,
    /// Who is waiting for the answer, when the GitHub budget runs short.
    pub priority: Priority,
}

/// Who a GitHub request is for, most urgent first. When the GitHub budget runs short, the less
/// urgent ones wait so something the user just asked for still gets an answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// Something the user just did: Choose version, an install, a check they pressed.
    #[default]
    User,
    /// A background check of an app that is installed.
    Installed,
    /// A background check of an app that isn't installed.
    NotInstalled,
    /// A background check of an app following the nightly.
    NightlyTrack,
    /// Finding new builds for the Developer Builds page.
    Discovery,
}

impl ReleaseQuery {
    pub fn for_app(app: &crate::CraftApp, repo: impl Into<String>) -> Self {
        Self {
            repo: repo.into(),
            separate_nightly: app.has_separate_nightly_channel(),
            former_repo: app.former_repo.map(str::to_string),
            priority: Priority::User,
        }
    }

    /// The same question, asked on behalf of `priority`.
    pub fn with_priority(self, priority: Priority) -> Self {
        Self { priority, ..self }
    }

    /// The same question about the former repo, when there is one. Its answer's downloads must be
    /// under the former repo, as for any repo asked.
    pub fn former(&self) -> Option<Self> {
        self.former_repo.clone().map(|repo| Self { repo, separate_nightly: self.separate_nightly, former_repo: None, priority: self.priority })
    }
}

/// A version whose prerelease names a Nightly build, whatever the release's prerelease flag says.
fn nightly_version(version: &Version) -> bool {
    version.pre.as_str().split('.').any(|part| part.eq_ignore_ascii_case("nightly"))
}

/// A Nightly of an app with its own channel: by its flag or by its tag alone, so a build published
/// without the prerelease flag still stays out of the ordinary channel.
pub fn separate_channel_nightly(release: &Release) -> bool {
    is_nightly(release) || release.version.as_ref().is_some_and(nightly_version)
}

/// A Nightly-follow choice or a parsed exact Nightly pin, including main-supported legacy tags.
pub fn is_nightly_track(track: &crate::protocol::VersionTrack) -> bool {
    use crate::protocol::VersionTrack;
    match track {
        VersionTrack::Nightly => true,
        VersionTrack::Pinned(tag) => version::parse(tag).is_some_and(|v| nightly_version(&v)),
        VersionTrack::Stable => false,
    }
}

/// Ordinary settings for separately channelled apps cannot redirect their stable updater.
pub fn ordinary_track_allowed(app: &crate::CraftApp, track: &crate::protocol::VersionTrack) -> bool {
    !app.has_separate_nightly_channel() || !is_nightly_track(track)
}

/// The most release pages an ordinary lookup reads past one that holds no ordinary release, so a
/// page full of Nightlies cannot hide the versions behind it. Shared by every client that makes the
/// same promise: the desktop's API and version-list lookups, and the browser's pre-release lookup.
pub const ORDINARY_PAGES: usize = 5;

/// Ordinary release and cache paths never offer a Nightly, for any app: a pre-release means a
/// beta or release candidate. Nightlies have their own channel, or the explicit Nightly track.
pub fn ordinary_release_allowed(release: &Release) -> bool {
    !separate_channel_nightly(release)
}

/// Ordinary source callers keep main's Nightly selection for apps without their own channel.
pub fn choose_version_for(releases: &[Release], track: &crate::protocol::VersionTrack, separate_nightly: bool) -> Option<Release> {
    let policy = if separate_nightly { NightlySelection::Canonical } else { NightlySelection::Published };
    choose_version_with_policy(releases, track, policy)
}

fn choose_version_with_policy(releases: &[Release], track: &crate::protocol::VersionTrack, policy: NightlySelection) -> Option<Release> {
    use crate::protocol::VersionTrack;
    match track {
        VersionTrack::Stable => releases
            .iter()
            .filter(|r| !r.prerelease && r.version.as_ref().is_some_and(version::is_stable))
            .max_by(|a, b| a.version.cmp(&b.version))
            .cloned(),
        VersionTrack::Nightly => ranked_nightly_candidates(releases.iter().filter(|release| has_published_checksum(release)), policy).next().cloned(),
        VersionTrack::Pinned(tag) => releases.iter().find(|r| r.tag == *tag).cloned(),
    }
}

/// Reads `GET /repos/<repo>/releases` and returns the newest stable (or any, with
/// `allow_prerelease`) published release. Nightlies are never part of it.
///
/// # Errors
///
/// Returns an error if the JSON is malformed or its top level is not an array.
pub fn parse_github_release_list(json: &str, repo: &str, allow_prerelease: bool) -> Result<Option<Release>, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("unreadable answer from GitHub ({e})"))?;
    let list = v.as_array().ok_or("unexpected answer from GitHub")?;
    let mut best: Option<Release> = None;
    for item in list {
        if item["draft"].as_bool().unwrap_or(false) {
            continue;
        }
        let Ok(r) = release_from_value(item, repo) else {
            continue;
        };
        let Some(ver) = r.version.as_ref() else {
            continue;
        };
        if !ordinary_release_allowed(&r) || (!allow_prerelease && (r.prerelease || !version::is_stable(ver))) {
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
    let tag = v["tag_name"].as_str().filter(|t| valid_tag(t)).ok_or("no release found")?.to_string();
    let releases = format!("https://github.com/{repo}/releases");
    let html_url = v["html_url"].as_str().and_then(|u| safe_release_html_url(repo, u)).unwrap_or_else(|| releases.clone());
    let prefix = download_prefix(repo);
    let assets = v["assets"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|a| {
                    let name = a["name"].as_str().filter(|n| valid_file_name(n))?.to_string();
                    let url = a["browser_download_url"]
                        .as_str()
                        .filter(|u| download_url_under(u, &prefix) && *u == format!("{prefix}{tag}/{name}"))?
                        .to_string();
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
    valid_tag(rest).then(|| rest.to_string())
}

/// A release tag we'd put in a URL, a file or the UI: up to 64 of `[A-Za-z0-9._+-]`, not
/// starting with a dot, so no separators, control or bidi characters and no `.`/`..`.
pub fn valid_tag(tag: &str) -> bool {
    tag.len() <= 64 && !tag.starts_with('.') && plain_name(tag)
}

/// Non-empty and only `[A-Za-z0-9._+-]`, the characters every Craft release tag and asset uses.
fn plain_name(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
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

/// A bare file name we'd be willing to save: only `[A-Za-z0-9._+-]`, no leading or trailing dot,
/// and not a name Windows reserves for a device (`CON`, `nul.txt`, `COM1.msi`).
pub fn valid_file_name(name: &str) -> bool {
    name.len() <= 200 && !name.starts_with('.') && !name.ends_with('.') && plain_name(name) && !windows_device(name)
}

fn windows_device(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or_default().to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || matches!(stem.as_bytes(), [b'C', b'O', b'M', b'1'..=b'9'] | [b'L', b'P', b'T', b'1'..=b'9'])
}

/// `s` cut to at most `max` bytes (on a char boundary) with an ellipsis, for text from a
/// release or the feed shown in the UI.
pub(crate) fn truncate(s: &str, max: usize) -> String {
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
    fn generic_prerelease_updates_cannot_select_a_separate_nightly() {
        let json = r#"[
            {"tag_name":"v9.0.0-nightly.20261120","prerelease":true},
            {"tag_name":"v1.0.0-beta.1","prerelease":true},
            {"tag_name":"v0.9.0","prerelease":false}
        ]"#;
        let release = parse_github_release_list(json, "storytold/photocraft", true).unwrap().unwrap();
        assert!(!is_nightly(&release));
        assert_eq!(release.version, crate::version::parse("v1.0.0-beta.1"));
    }

    #[test]
    fn truncate_keeps_whole_characters() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("abcdef", 3), "abc…");
        // `é` is two bytes: cutting at 2 would split it, so the cut backs off to 1.
        assert_eq!(truncate("aé", 2), "a…");
    }

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
            // Shell/cmd metacharacters: the WSL opener fallback shells out, so these must be
            // refused even inside an otherwise valid https URL (CWE-78).
            "https://example.com/a|calc",
            "https://example.com/p?x=1&y=2",
            "https://example.com/`id`",
            "https://example.com/$(whoami)",
            "https://example.com/a;b",
            "https://example.com/a>b",
            "https://example.com/%COMSPEC%",
            "https://example.com/%PATH%",
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
    fn release_html_url_rejects_traversal_and_metacharacters() {
        let repo = "storytold/photocraft";
        assert_eq!(
            safe_release_html_url(repo, "https://github.com/storytold/photocraft/releases/tag/v1.0.0"),
            Some("https://github.com/storytold/photocraft/releases/tag/v1.0.0".to_string())
        );
        assert_eq!(
            safe_release_html_url(repo, "https://github.com/storytold/photocraft/releases"),
            Some("https://github.com/storytold/photocraft/releases".to_string())
        );
        for bad in [
            "https://github.com/storytold/photocraft/releases/../../attacker/repo/releases/tag/v1",
            "https://github.com/storytold/photocraft/releases/tag/v1/../../../attacker/repo",
            "https://github.com/storytold/photocraft/releases/tag/v1|calc.exe",
            "https://github.com/storytold/photocraft/releases/tag/v1%2f..%2f..",
            "https://github.com/storytold/photocraft/releases/tag/v1?x=1",
            "https://github.com/storytold/photocraft/releases/tag/v1#hash",
            "https://github.com/storytold/other/releases/tag/v1.0.0",
            "http://github.com/storytold/photocraft/releases/tag/v1",
        ] {
            assert!(safe_release_html_url(repo, bad).is_none(), "should reject: {bad}");
        }
    }

    #[test]
    fn selected_release_cannot_download_an_asset_from_a_different_tag() {
        let wrong_tag = "https://github.com/storytold/photocraft/releases/download/v9.0.0/photocraft-0.5.0-macos-universal.dmg";
        let r = parse_github_release(&api_json(wrong_tag), REPO).unwrap();
        assert!(r.assets.is_empty());
    }

    #[test]
    fn traversal_cannot_escape_the_repository() {
        // Raw-prefix matches but the interpreted path climbs into another repository (CWE-180).
        for url in [
            "https://github.com/storytold/photocraft/releases/download/../../../../attacker/repo/releases/download/v99/photocraft-99.0.0-linux-x86_64.AppImage",
            "https://github.com/storytold/photocraft/releases/download/v1/../../../attacker/repo/x.dmg",
            "https://github.com/storytold/photocraft/releases/download/./v1/x.dmg",
            "https://github.com/storytold/photocraft/releases/download/v1/sub/x.dmg",
            "https://github.com/storytold/photocraft/releases/download/v1%2f..%2f..%2fx.dmg",
            "https://github.com/storytold/photocraft/releases/download/v1/x%2fy.dmg",
        ] {
            let prefix = download_prefix(REPO);
            assert!(!download_url_under(url, &prefix), "{url}");
            let r = parse_github_release(&api_json(url), REPO).unwrap();
            assert!(r.assets.is_empty(), "{url}");
        }
        // A plain, confined download still passes.
        let prefix = download_prefix(REPO);
        assert!(download_url_under("https://github.com/storytold/photocraft/releases/download/v0.5.0/photocraft-0.5.0-macos-universal.dmg", &prefix));
    }

    #[test]
    fn version_choices_exclude_drafts_and_distinguish_nightly_from_betas() {
        let json = r#"[
            {"tag_name":"v0.8.0-nightly.20261028","prerelease":true,"draft":false,"assets":[]},
            {"tag_name":"v0.8.0-beta.1","prerelease":true,"draft":false,"assets":[]},
            {"tag_name":"v0.7.0","prerelease":false,"draft":false,"assets":[]},
            {"tag_name":"v0.9.0-nightly.20261029","prerelease":true,"draft":true,"assets":[]},
            {"tag_name":"not-a-version","prerelease":false,"draft":false,"assets":[]}]"#;
        let choices = parse_github_versions(json, REPO).unwrap();
        assert_eq!(choices.iter().map(|r| r.tag.as_str()).collect::<Vec<_>>(), ["v0.8.0-nightly.20261028", "v0.8.0-beta.1", "v0.7.0"]);
        assert!(is_nightly(&choices[0]));
        assert!(!is_nightly(&choices[1]));
        assert!(!is_nightly(&choices[2]));
    }

    fn nightly_release(tag: &str, published_at: Option<i64>) -> Release {
        Release {
            tag: tag.into(),
            version: version::parse(tag),
            prerelease: true,
            published_at,
            assets: vec![Asset { name: "app.zip".into(), sha256: Some(HASH.into()), ..Default::default() }],
            ..Default::default()
        }
    }

    #[test]
    fn nightly_selection_requires_canonical_matching_tags_but_not_descriptor_shape() {
        use crate::protocol::VersionTrack;
        for tag in ["1.2.3-nightly", "v1.2.3-nightly.20261120.runA", "v1.2.3-nightly+build.02"] {
            let release = nightly_release(tag, None);
            assert_eq!(canonical_nightly_version(&release), release.version, "{tag}");
            assert_eq!(choose_version(&[release], &VersionTrack::Nightly).unwrap().tag, tag);
        }
        for tag in [
            " 1.2.3-nightly",
            "1.2.3-nightly ",
            "V1.2.3-nightly",
            "artcraft-v1.2.3-nightly",
            "v1.2-nightly",
            "v01.2.3-nightly",
            "v1.2.3-Nightly",
            "v1.2.3-beta.nightly",
            "v1.2.3-nightlyish",
            "v1.2.3",
        ] {
            let release = nightly_release(tag, None);
            assert!(canonical_nightly_version(&release).is_none(), "{tag}");
            assert!(choose_version(&[release], &VersionTrack::Nightly).is_none(), "{tag}");
        }
        let canonical = nightly_release("v1.2.3-nightly+build.a", None);
        let mut unmarked = canonical.clone();
        unmarked.prerelease = false;
        let mut missing_version = canonical.clone();
        missing_version.version = None;
        let mut mismatched_version = canonical.clone();
        mismatched_version.version = version::parse("v1.2.4-nightly+build.a");
        let mut mismatched_build = canonical;
        mismatched_build.version = version::parse("v1.2.3-nightly+build.b");
        for release in [unmarked, missing_version, mismatched_version, mismatched_build] {
            assert!(canonical_nightly_version(&release).is_none());
            assert!(choose_version(&[release], &VersionTrack::Nightly).is_none());
        }
    }

    #[test]
    fn later_publication_of_an_older_nightly_cannot_replace_a_newer_version() {
        use crate::protocol::VersionTrack;
        let older = nightly_release("v1.2.3-nightly.20261120.runA", Some(200));
        let newer = nightly_release("v1.3.0-nightly.20261119.runB", Some(100));
        let old_policy = [older.clone(), newer.clone()];
        assert_eq!(
            old_policy.iter().max_by_key(|release| release.published_at).unwrap().tag,
            older.tag,
            "the old publication-time policy selects the counterexample's older SemVer"
        );
        for list in [[older.clone(), newer.clone()], [newer.clone(), older]] {
            assert_eq!(choose_version(&list, &VersionTrack::Nightly).unwrap().tag, newer.tag);
        }
    }

    #[test]
    fn rerun_dates_and_opaque_ids_are_compared_only_as_semver_components() {
        use crate::protocol::VersionTrack;
        for (older, newer) in [
            // A rerun retains its original tag date even when it is published later.
            ("v1.2.3-nightly.20261119.run999", "v1.2.3-nightly.20261120.run1"),
            // Opaque alphanumeric IDs follow SemVer lexical order, not extracted integers.
            ("v1.2.3-nightly.20261120.run10", "v1.2.3-nightly.20261120.run9"),
            // Numeric components still follow SemVer numeric order, not raw tag order.
            ("v1.2.3-nightly.20261120.9", "v1.2.3-nightly.20261120.10"),
        ] {
            let older = nightly_release(older, Some(200));
            let newer = nightly_release(newer, None);
            for list in [[older.clone(), newer.clone()], [newer.clone(), older]] {
                assert_eq!(choose_version(&list, &VersionTrack::Nightly).unwrap().tag, newer.tag);
            }
        }
    }

    #[test]
    fn equal_nightly_precedence_prefers_smallest_tag_without_metadata_or_publication_ordering() {
        use crate::protocol::VersionTrack;
        for tags in [["v1.2.3-nightly+z", "v1.2.3-nightly+a"], ["v1.2.3-nightly+a", "v1.2.3-nightly"], ["v1.2.3-nightly", "1.2.3-nightly+z"]] {
            let larger_tag = nightly_release(tags[0], Some(200));
            let smaller_tag = nightly_release(tags[1], Some(100));
            for list in [[larger_tag.clone(), smaller_tag.clone()], [smaller_tag.clone(), larger_tag]] {
                assert_eq!(choose_version(&list, &VersionTrack::Nightly).unwrap().tag, smaller_tag.tag);
            }
        }
        let older = nightly_release("v1.2.3-nightly.9+z", Some(200));
        let newer = nightly_release("v1.2.3-nightly.10+a", Some(100));
        assert_eq!(choose_version(&[older, newer.clone()], &VersionTrack::Nightly).unwrap().tag, newer.tag);
    }

    #[test]
    fn ordinary_version_list_keeps_its_order_limit_beta_filtering_and_exact_pins() {
        use crate::protocol::VersionTrack;
        let mut items: Vec<_> = (0..105).map(|i| serde_json::json!({"tag_name": format!("v0.1.{i}"), "prerelease": false})).collect();
        items[0] = serde_json::json!({"tag_name": "v99.0.0-nightly", "prerelease": true});
        items[1] = serde_json::json!({"tag_name": "v9.0.0-beta.1", "prerelease": true});
        let json = serde_json::to_string(&items).unwrap();
        let list = parse_github_versions(&json, REPO).unwrap();
        assert_eq!(list.len(), 100);
        assert_eq!(list[0].tag, "v99.0.0-nightly");
        assert_eq!(list[99].tag, "v0.1.99");
        assert_eq!(choose_version(&list, &VersionTrack::Stable).unwrap().tag, "v0.1.99");
        // Pinned releases retain exact matching, with neither checksum nor Nightly fallback.
        assert_eq!(choose_version(&list, &VersionTrack::Pinned("v99.0.0-nightly".into())).unwrap().tag, "v99.0.0-nightly");
        assert_eq!(choose_version(&list, &VersionTrack::Pinned("v0.1.99".into())).unwrap().tag, "v0.1.99");
        assert!(choose_version(&list, &VersionTrack::Pinned("v0.1.100".into())).is_none());
        assert!(choose_version(&list, &VersionTrack::Pinned("0.1.99".into())).is_none());
        assert_eq!(parse_github_release_list(&json, REPO, false).unwrap().unwrap().tag, "v0.1.104");
        assert_eq!(parse_github_release_list(&json, REPO, true).unwrap().unwrap().tag, "v9.0.0-beta.1");
    }

    #[test]
    fn a_nightly_without_published_checksums_is_unavailable() {
        let mut nightly = Release {
            tag: "v1.1.0-nightly.20261028".into(),
            version: crate::version::parse("v1.1.0-nightly.20261028"),
            prerelease: true,
            ..Default::default()
        };
        nightly.assets.push(Asset { name: "app.zip".into(), size: 3, url: "https://example.invalid/app.zip".into(), sha256: None });
        assert!(choose_version(&[nightly], &crate::protocol::VersionTrack::Nightly).is_none());
    }

    #[test]
    fn selection_follows_nightly_or_pins_an_older_release_without_beta_fallback() {
        use crate::protocol::VersionTrack;
        let json = r#"[
            {"tag_name":"v0.8.0-nightly.20261028","prerelease":true,"published_at":"2026-10-28T10:00:00Z"},
            {"tag_name":"v0.8.0-beta.1","prerelease":true,"published_at":"2026-10-28T12:00:00Z"},
            {"tag_name":"v0.8.0-nightly.20261027","prerelease":true,"published_at":"2026-10-27T10:00:00Z"},
            {"tag_name":"v0.7.0","prerelease":false},
            {"tag_name":"v0.6.0","prerelease":false}]"#;
        let mut list = parse_github_versions(json, REPO).unwrap();
        for r in list.iter_mut().filter(|r| is_nightly(r)) {
            r.assets.push(Asset { name: "app.zip".into(), size: 3, url: "https://example.invalid/app.zip".into(), sha256: Some(HASH.into()) });
        }
        assert_eq!(choose_version(&list, &VersionTrack::Stable).unwrap().tag, "v0.7.0");
        assert_eq!(choose_version(&list, &VersionTrack::Nightly).unwrap().tag, "v0.8.0-nightly.20261028");
        assert_eq!(choose_version(&list, &VersionTrack::Pinned("v0.6.0".into())).unwrap().tag, "v0.6.0");
        assert!(choose_version(&list, &VersionTrack::Pinned("v9.0.0".into())).is_none());
        assert!(choose_version(&list[3..], &VersionTrack::Nightly).is_none());
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
        for bad in [
            "",
            "..",
            "a/b",
            "a\\b",
            ".hidden",
            "c:x",
            "x\u{0}",
            "a b.dmg",
            "a%2e.msi",
            "a#b",
            "evil\u{202e}gpj.exe",
            "x.msi.",
            "CON",
            "nul.txt",
            "COM1.msi",
            "lpt9",
            "prn",
            "aux.log",
            "a".repeat(201).as_str(),
        ] {
            assert!(!valid_file_name(bad), "{bad:?}");
        }
        for good in [
            "photocraft-0.5.0-linux-x86_64.AppImage",
            "lightcraft_0.9.0_amd64.deb",
            "x-1.0+git.tar.gz",
            "console.zip",
            "COM10.txt",
            "COM0.msi",
            "lpt0",
        ] {
            assert!(valid_file_name(good), "{good:?}");
        }
    }

    #[test]
    fn tags_are_plain() {
        for good in ["v0.6.0", "v1.0.0-beta.2", "nightly-2026-10-29", "v1+build.7"] {
            assert!(valid_tag(good), "{good:?}");
        }
        for bad in ["", ".", "..", ".v1", "v1/x", "v1\\x", "v1\nx", "v1\u{0}x", "v1\u{202e}", "v 1", "v1?x", "v".repeat(65).as_str()] {
            assert!(!valid_tag(bad), "{bad:?}");
        }
        let api = r#"{"tag_name": "v1\u202egpj", "html_url": "", "assets": []}"#;
        assert!(parse_github_release(api, REPO).is_err());
        let prefix = download_prefix(REPO);
        assert!(download_url_under(&format!("{prefix}v1/a.dmg"), &prefix));
        assert!(!download_url_under(&format!("{prefix}v1\u{202e}/a.dmg"), &prefix), "an odd tag in a download link");
        assert!(!download_url_under(&format!("{prefix}v1/a\u{202e}.dmg"), &prefix), "an odd name in a download link");
        assert_eq!(tag_from_redirect(&format!("https://github.com/{REPO}/releases/tag/v1\u{202e}"), REPO), None);
    }
    /// A pre-release is a beta or release candidate, never a Nightly, for every app: one without
    /// its own Nightly channel (a community app) as much as one with it.
    #[test]
    fn ordinary_prerelease_lookup_never_offers_a_nightly_for_any_app() {
        // Authored representative metadata; not a claim about live SolidCraft releases.
        let json = r#"[{"tag_name":"v2.0.0-dev.nightly.1","prerelease":true},{"tag_name":"v1.0.0-beta.1","prerelease":true},{"tag_name":"v0.9.0","prerelease":false}]"#;
        for id in ["solidcraft", "filmcraft", "photocraft"] {
            let repo = crate::CraftApp::by_id(id).unwrap().repo;
            assert_eq!(parse_github_release_list(json, repo, true).unwrap().unwrap().tag, "v1.0.0-beta.1", "{id}");
            assert_eq!(parse_github_release_list(json, repo, false).unwrap().unwrap().tag, "v0.9.0", "{id}");
        }
    }
    #[test]
    fn a_feed_repo_rename_or_a_missing_prerelease_flag_cannot_bring_photocraft_nightlies_back() {
        let app = crate::CraftApp::by_id("photocraft").unwrap();
        let feed =
            crate::feed::Feed::parse(r#"{"repo_renames":[{"old_name":"storytold/photocraft","new_name":"storytold/photocraft-studio"}]}"#).unwrap();
        let query = ReleaseQuery::for_app(app, feed.repo_for(app));
        assert_eq!(query.repo, "storytold/photocraft-studio");
        let flagged =
            r#"[{"tag_name":"v2.0.0-nightly.20261123.1.1.gabcdefabcdef","prerelease":true},{"tag_name":"v1.0.0-beta.1","prerelease":true}]"#;
        assert_eq!(parse_github_release_list(flagged, &query.repo, true).unwrap().unwrap().tag, "v1.0.0-beta.1");
        let unflagged = r#"[{"tag_name":"v2.0.0-nightly.20261123.1.1.gabcdefabcdef","prerelease":false},{"tag_name":"v1.0.0","prerelease":false}]"#;
        assert_eq!(parse_github_release_list(unflagged, app.repo, true).unwrap().unwrap().tag, "v1.0.0");
        // Any other repo leaves nightlies out of ordinary lookups too.
        assert_eq!(parse_github_release_list(flagged, "storytold/photocraft-studio", true).unwrap().unwrap().tag, "v1.0.0-beta.1");
    }
}
