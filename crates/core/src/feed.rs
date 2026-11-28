//! The release feed from the signed website catalog:
//! `GET https://getartcraft.com/launcher-catalog.toml` and `.sig`.
//!
//! The website build statically generates each app's stable, prerelease, and nightly release
//! metadata into `launcher-catalog.toml` and signs it with the pinned Ed25519 key, so the
//! launcher doesn't have to poll GitHub (which allows 60 unauthenticated requests an hour).
//! GitHub stays the fallback: for apps or channels the catalog doesn't list, entries older than
//! [`MAX_AGE`], or when the catalog doesn't answer or fails signature verification.
//!
//! A developer can still serve a JSON feed and builds from their own machine instead (see
//! [`Feed::parse`] and [`Feed::with_mirror`]): then links under the mirror are accepted too, and
//! entries never go stale.

use std::collections::BTreeMap;

use ed25519_dalek::VerifyingKey;
use serde::Deserialize;
use serde::de::{DeserializeOwned, Deserializer};

use crate::catalog::{CraftApp, GITHUB_OWNER};
use crate::hosted_catalog::{CATALOG_URL, MAX_CATALOG_BYTES, SIGNATURE_URL, check_signature, pinned_key};
use crate::release::{self, Asset, Release, truncate};
use crate::time::parse_rfc3339;
use crate::version;

/// Where the signed release catalog lives.
pub const FEED_URL: &str = CATALOG_URL;
/// Detached Ed25519 signature over [`FEED_URL`].
pub const FEED_SIGNATURE_URL: &str = SIGNATURE_URL;

/// An app's entry is trusted for this long after storyteller-web fetched it.
pub const MAX_AGE: i64 = 24 * 3600;

/// The feed, read leniently.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Feed {
    /// By lowercase app name.
    pub apps: BTreeMap<String, FeedApp>,
    /// Catalog TOML explicitly describes release channels; legacy JSON only describes stable.
    pub channel_aware: bool,
    /// When the least recently fetched app was fetched (Unix seconds).
    pub last_fetched: Option<i64>,
    pub app_renames: Vec<AppRename>,
    /// Repositories that moved, both under [`GITHUB_OWNER`]: a rename anywhere else is dropped.
    pub repo_renames: Vec<RepoRename>,
    /// A local mirror's address (ending in `/`): downloads under it are accepted alongside GitHub's.
    /// Never read from the feed itself; the caller sets it with [`Feed::with_mirror`].
    pub mirror: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeedApp {
    pub repo: Option<String>,
    /// Present when the repo has a published stable release. Meaningless unless `fetched_at` is set.
    pub release: Option<FeedRelease>,
    /// Present when the repo has a published non-nightly prerelease (beta/RC).
    pub prerelease: Option<FeedRelease>,
    /// Present when the repo has a published nightly build.
    pub nightly: Option<FeedRelease>,
    /// When the catalog or feed last fetched this app (Unix seconds).
    pub fetched_at: Option<i64>,
}

/// A release as the feed describes it, before it's checked against the app's repo.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeedRelease {
    pub tag: String,
    pub name: String,
    pub published_at: Option<i64>,
    /// Actual release reconciliation time, independent of the website rebuild time.
    pub reconciled_at: Option<i64>,
    pub html_url: String,
    pub prerelease: bool,
    pub notes: String,
    pub assets: Vec<(String, u64, String, Option<String>)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppRename {
    pub old_name: String,
    pub old_name_lower: String,
    pub new_name: String,
    pub new_name_lower: String,
    pub description: String,
    pub renamed_at: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoRename {
    pub old_repo: String,
    pub new_repo: String,
    pub renamed_at: Option<i64>,
}

/// What the feed says about one app, once checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeedAnswer {
    /// Fresh: the app's latest release, or `None` when it has none.
    Fresh(Option<Release>),
    /// Not listed, never fetched, or older than [`MAX_AGE`]: ask GitHub.
    AskGitHub,
}

/// The most text kept from one rename's description.
const MAX_TEXT: usize = 4000;

impl Feed {
    /// Verifies the detached Ed25519 signature over `launcher-catalog.toml` using the pinned
    /// public key and extracts its static release channels.
    pub fn parse_signed_catalog(catalog_bytes: &[u8], signature_bytes: &[u8]) -> Result<Self, String> {
        Self::parse_signed_catalog_with_key(catalog_bytes, signature_bytes, &pinned_key()?)
    }

    /// Like [`Self::parse_signed_catalog`], using an explicit verifying key (tests).
    pub fn parse_signed_catalog_with_key(catalog_bytes: &[u8], signature_bytes: &[u8], key: &VerifyingKey) -> Result<Self, String> {
        check_signature(catalog_bytes, signature_bytes, MAX_CATALOG_BYTES, key)?;
        let text = std::str::from_utf8(catalog_bytes).map_err(|_| "catalog is not UTF-8")?;
        Self::parse_catalog_toml(text)
    }

    /// Reads release metadata from an already-verified `launcher-catalog.toml` document.
    pub fn parse_catalog_toml(toml_text: &str) -> Result<Self, String> {
        let doc: CatalogDocument = toml::from_str(toml_text).map_err(|e| format!("invalid catalog TOML: {e}"))?;
        let generated_at = doc.generated_at.as_deref().and_then(parse_rfc3339);
        let mut apps = BTreeMap::new();
        if let Some(launcher) = doc.launcher {
            let id = launcher.id.unwrap_or_else(|| "artcraft-launcher".into()).to_lowercase();
            if let Some(entry) = launcher.releases.and_then(|r| r.into_feed_app(launcher.repo, generated_at)) {
                apps.insert(id, entry);
            }
        }
        for app in doc.apps {
            let Some(id) = app.id.filter(|s| !s.is_empty()) else { continue };
            if let Some(entry) = app.releases.and_then(|r| r.into_feed_app(app.repo, generated_at)) {
                apps.insert(id.to_lowercase(), entry);
            }
        }
        Ok(Self { apps, channel_aware: true, last_fetched: generated_at, app_renames: Vec::new(), repo_renames: Vec::new(), mirror: None })
    }

    /// Reads the legacy/local-mirror JSON feed. `None` only when the answer isn't JSON (or isn't an object) at all.
    pub fn parse(json: &str) -> Option<Self> {
        let raw: RawResponse = serde_json::from_str(json).ok()?;
        let feed = Feed {
            apps: raw.release_info.apps.into_iter().map(|(k, v)| (k.to_lowercase(), v.into_app())).collect(),
            channel_aware: false,
            last_fetched: raw.release_info.last_fetched.as_deref().and_then(parse_rfc3339),
            app_renames: raw.app_renames.into_iter().filter_map(RawAppRename::into_rename).collect(),
            repo_renames: raw.repo_renames.into_iter().filter_map(RawRepoRename::into_rename).collect(),
            mirror: None,
        };
        Some(feed)
    }

    /// Trusts downloads under `base` (which must end in `/`) and keeps every entry fresh: a feed
    /// served by `cargo xtask local serve` on the developer's own machine.
    #[must_use]
    pub fn with_mirror(mut self, base: &str) -> Self {
        self.mirror = base.ends_with('/').then(|| base.to_string());
        self
    }

    /// The repo to ask GitHub about for `app`, following the feed's repo renames. Always under
    /// [`GITHUB_OWNER`]: the feed can move an app within ArtCraft's account, never out of it.
    pub fn repo_for(&self, app: &CraftApp) -> String {
        let mut repo = app.repo.to_string();
        // Follow a chain of moves (a → b → c), without looping forever.
        for _ in 0..8 {
            match self.repo_renames.iter().find(|r| r.old_repo.eq_ignore_ascii_case(&repo)) {
                Some(r) if !r.new_repo.eq_ignore_ascii_case(&repo) => repo = r.new_repo.clone(),
                _ => break,
            }
        }
        repo
    }

    /// What the feed says about `app` at `now`.
    pub fn answer(&self, app: &CraftApp, now: i64) -> FeedAnswer {
        let Some(entry) = self.apps.get(app.id) else { return FeedAnswer::AskGitHub };
        let Some(fetched_at) = entry.fetched_at else { return FeedAnswer::AskGitHub };
        // A timestamp from the future (clock skew) counts as just now; one too old doesn't count.
        // A local mirror is as fresh as its last sync.
        if self.mirror.is_none() && now.saturating_sub(fetched_at) > MAX_AGE {
            return FeedAnswer::AskGitHub;
        }
        // The feed's repo must be the app's, one the feed says it moved to, or the app's former repo
        // (downloads are then checked against that one).
        let mut repo = self.repo_for(app);
        if let Some(feed_repo) = &entry.repo
            && !feed_repo.eq_ignore_ascii_case(&repo)
        {
            match app.former_repo.filter(|former| feed_repo.eq_ignore_ascii_case(former)) {
                Some(former) => repo = former.to_string(),
                None => return FeedAnswer::AskGitHub,
            }
        }
        match &entry.release {
            // The app's other name may have the release: GitHub is asked about both.
            None if app.former_repo.is_some() => FeedAnswer::AskGitHub,
            None => FeedAnswer::Fresh(None),
            Some(r) => match r.to_release(&repo, self.mirror.as_deref()) {
                Some(release) => FeedAnswer::Fresh(Some(release)),
                None => FeedAnswer::AskGitHub,
            },
        }
    }

    /// What the feed says about `app` when pre-releases are enabled: the higher version between
    /// `stable` and `prerelease`, or `AskGitHub` if neither is fresh/valid.
    pub fn answer_prerelease(&self, app: &CraftApp, now: i64) -> FeedAnswer {
        let Some((entry, repo)) = self.fresh_entry(app, now) else { return FeedAnswer::AskGitHub };
        let stable = entry.release.as_ref().and_then(|r| r.to_release(&repo, self.mirror.as_deref()));
        let pre = entry.prerelease.as_ref().and_then(|r| r.to_release(&repo, self.mirror.as_deref()));
        match (stable, pre) {
            (Some(s), Some(p)) => {
                let pick_pre = match (&p.version, &s.version) {
                    (Some(pv), Some(sv)) => version::is_newer(pv, sv),
                    _ => false,
                };
                FeedAnswer::Fresh(Some(if pick_pre { p } else { s }))
            }
            (Some(s), None) => FeedAnswer::Fresh(Some(s)),
            (None, Some(p)) => FeedAnswer::Fresh(Some(p)),
            (None, None) => FeedAnswer::AskGitHub,
        }
    }

    /// What the feed says about `app`'s nightly build at `now`. Returns `AskGitHub` when no fresh
    /// nightly with valid checksums is listed.
    pub fn answer_nightly(&self, app: &CraftApp, now: i64) -> FeedAnswer {
        let Some((entry, repo)) = self.fresh_entry(app, now) else { return FeedAnswer::AskGitHub };
        let Some(nightly) = entry.nightly.as_ref().and_then(|r| r.to_release(&repo, self.mirror.as_deref())) else {
            return FeedAnswer::AskGitHub;
        };
        if release::is_nightly(&nightly) && release::has_published_checksum(&nightly) {
            FeedAnswer::Fresh(Some(nightly))
        } else {
            FeedAnswer::AskGitHub
        }
    }

    fn fresh_entry(&self, app: &CraftApp, now: i64) -> Option<(&FeedApp, String)> {
        let entry = self.apps.get(app.id)?;
        let fetched_at = entry.fetched_at?;
        if self.mirror.is_none() && now.saturating_sub(fetched_at) > MAX_AGE {
            return None;
        }
        let mut repo = self.repo_for(app);
        if let Some(feed_repo) = &entry.repo
            && !feed_repo.eq_ignore_ascii_case(&repo)
        {
            let former = app.former_repo.filter(|former| feed_repo.eq_ignore_ascii_case(former))?;
            repo = former.to_string();
        }
        Some((entry, repo))
    }
}

impl FeedRelease {
    /// The release with only the downloads that belong to `repo` (or sit under `mirror`). `None`
    /// when it doesn't hold up (no tag, a tag with odd characters, or not a version).
    fn to_release(&self, repo: &str, mirror: Option<&str>) -> Option<Release> {
        let version = version::parse(&self.tag).filter(|_| release::valid_tag(&self.tag))?;
        let allowed = |name: &str, url: &str| {
            (release::download_url_under(url, &release::download_prefix(repo))
                && url == format!("{}{}/{}", release::download_prefix(repo), self.tag, name))
                || mirror.is_some_and(|m| release::download_url_under_mirror(url, m))
        };
        let releases_page = format!("https://github.com/{repo}/releases/");
        let assets = self
            .assets
            .iter()
            .filter(|(name, _, url, _)| release::valid_file_name(name) && allowed(name, url))
            .map(|(name, size, url, sha)| Asset {
                name: name.clone(),
                size: *size,
                url: url.clone(),
                sha256: sha.clone().filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())).map(|h| h.to_ascii_lowercase()),
            })
            .collect();
        Some(Release {
            version: Some(version),
            name: if self.name.is_empty() { self.tag.clone() } else { truncate(&self.name, 120) },
            tag: self.tag.clone(),
            published_at: self.published_at,
            html_url: release::safe_release_html_url(repo, &self.html_url)
                .or_else(|| {
                    let fallback = format!("{releases_page}tag/{}", self.tag);
                    release::safe_release_html_url(repo, &fallback)
                })
                .unwrap_or_else(|| format!("https://github.com/{repo}/releases")),
            notes: truncate(&self.notes, 64 * 1024),
            prerelease: self.prerelease,
            assets,
        })
    }
}

// ---- signed catalog TOML format ---------------------------------------------------------------

#[derive(Deserialize, Default)]
struct CatalogDocument {
    #[serde(default)]
    generated_at: Option<String>,
    #[serde(default)]
    launcher: Option<CatalogLauncher>,
    #[serde(default)]
    apps: Vec<CatalogApp>,
}

#[derive(Deserialize, Default)]
struct CatalogLauncher {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    releases: Option<CatalogReleases>,
}

#[derive(Deserialize, Default)]
struct CatalogApp {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    releases: Option<CatalogReleases>,
}

#[derive(Deserialize, Default)]
struct CatalogReleases {
    #[serde(default)]
    stable: Option<CatalogRelease>,
    #[serde(default)]
    prerelease: Option<CatalogRelease>,
    #[serde(default)]
    nightly: Option<CatalogRelease>,
}

#[derive(Deserialize, Default)]
struct CatalogRelease {
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    reconciled_at: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    notes_markdown: Option<String>,
    #[serde(default)]
    assets: Vec<CatalogAsset>,
}

#[derive(Deserialize, Default)]
struct CatalogAsset {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    size_bytes: u64,
    #[serde(default)]
    download_url: Option<String>,
    #[serde(default)]
    sha256: Option<String>,
}

impl CatalogRelease {
    fn into_feed_release(self) -> Option<FeedRelease> {
        let tag = self.tag.filter(|t| !t.is_empty() && t.len() <= 64)?;
        Some(FeedRelease {
            tag,
            name: self.name.unwrap_or_default(),
            published_at: self.published_at.as_deref().and_then(parse_rfc3339),
            reconciled_at: self.reconciled_at.as_deref().and_then(parse_rfc3339),
            html_url: self.html_url.unwrap_or_default(),
            prerelease: self.prerelease,
            notes: self.notes_markdown.unwrap_or_default(),
            assets: self.assets.into_iter().filter_map(|a| Some((a.name?, a.size_bytes, a.download_url?, a.sha256))).take(300).collect(),
        })
    }
}

impl CatalogReleases {
    fn into_feed_app(self, repo: Option<String>, fetched_at: Option<i64>) -> Option<FeedApp> {
        let release = self.stable.and_then(CatalogRelease::into_feed_release);
        let prerelease = self.prerelease.and_then(CatalogRelease::into_feed_release);
        let nightly = self.nightly.and_then(CatalogRelease::into_feed_release);
        if release.is_none() && prerelease.is_none() && nightly.is_none() {
            return None;
        }
        Some(FeedApp {
            repo: repo.filter(|r| r.split('/').count() == 2 && r.split('/').all(|part| !part.is_empty() && part != "." && part != "..")),
            release,
            prerelease,
            nightly,
            fetched_at,
        })
    }
}

// ---- lenient wire format ----------------------------------------------------------------------

/// Reads `T`, or its default when the value is missing, `null` or the wrong shape.
fn lenient<'de, D: Deserializer<'de>, T: DeserializeOwned + Default>(d: D) -> Result<T, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(T::deserialize(value).unwrap_or_default())
}

/// A list where entries of the wrong shape are dropped rather than spoiling the rest.
fn lenient_vec<'de, D: Deserializer<'de>, T: DeserializeOwned>(d: D) -> Result<Vec<T>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    let serde_json::Value::Array(items) = value else {
        return Ok(Vec::new());
    };
    Ok(items.into_iter().filter_map(|v| T::deserialize(v).ok()).collect())
}

/// A map where entries of the wrong shape are dropped.
fn lenient_map<'de, D: Deserializer<'de>, T: DeserializeOwned>(d: D) -> Result<BTreeMap<String, T>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    let serde_json::Value::Object(entries) = value else {
        return Ok(BTreeMap::new());
    };
    Ok(entries.into_iter().filter_map(|(k, v)| T::deserialize(v).ok().map(|t| (k, t))).collect())
}

#[derive(Deserialize, Default)]
struct RawResponse {
    #[serde(default, deserialize_with = "lenient")]
    release_info: RawReleaseInfo,
    #[serde(default, deserialize_with = "lenient_vec")]
    app_renames: Vec<RawAppRename>,
    #[serde(default, deserialize_with = "lenient_vec")]
    repo_renames: Vec<RawRepoRename>,
}

#[derive(Deserialize, Default)]
struct RawReleaseInfo {
    #[serde(default, deserialize_with = "lenient_map")]
    apps: BTreeMap<String, RawApp>,
    #[serde(default, deserialize_with = "lenient")]
    last_fetched: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawApp {
    #[serde(default, deserialize_with = "lenient")]
    repo: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    latest_release: Option<RawRelease>,
    #[serde(default, deserialize_with = "lenient")]
    app_last_fetched: Option<String>,
}

impl RawApp {
    fn into_app(self) -> FeedApp {
        FeedApp {
            repo: self.repo.filter(|r| r.split('/').count() == 2 && r.split('/').all(|part| !part.is_empty() && part != "." && part != "..")),
            release: self.latest_release.and_then(RawRelease::into_release),
            prerelease: None,
            nightly: None,
            fetched_at: self.app_last_fetched.as_deref().and_then(parse_rfc3339),
        }
    }
}

#[derive(Deserialize, Default)]
struct RawRelease {
    #[serde(default, deserialize_with = "lenient")]
    tag: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    name: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    published_at: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    html_url: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    prerelease: bool,
    #[serde(default, deserialize_with = "lenient")]
    notes_markdown: Option<String>,
    #[serde(default, deserialize_with = "lenient_vec")]
    assets: Vec<RawAsset>,
}

impl RawRelease {
    fn into_release(self) -> Option<FeedRelease> {
        let tag = self.tag.filter(|t| !t.is_empty() && t.len() <= 64)?;
        Some(FeedRelease {
            tag,
            name: self.name.unwrap_or_default(),
            published_at: self.published_at.as_deref().and_then(parse_rfc3339),
            reconciled_at: None,
            html_url: self.html_url.unwrap_or_default(),
            prerelease: self.prerelease,
            notes: self.notes_markdown.unwrap_or_default(),
            assets: self.assets.into_iter().filter_map(|a| Some((a.name?, a.size_bytes, a.download_url?, a.sha256))).take(300).collect(),
        })
    }
}

#[derive(Deserialize, Default)]
struct RawAsset {
    #[serde(default, deserialize_with = "lenient")]
    name: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    size_bytes: u64,
    #[serde(default, deserialize_with = "lenient")]
    download_url: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    sha256: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawAppRename {
    #[serde(default, deserialize_with = "lenient")]
    old_name: String,
    #[serde(default, deserialize_with = "lenient")]
    old_name_lower: String,
    #[serde(default, deserialize_with = "lenient")]
    new_name: String,
    #[serde(default, deserialize_with = "lenient")]
    new_name_lower: String,
    #[serde(default, deserialize_with = "lenient")]
    description: String,
    #[serde(default, deserialize_with = "lenient")]
    renamed_at: Option<String>,
}

impl RawAppRename {
    fn into_rename(self) -> Option<AppRename> {
        (!self.old_name_lower.is_empty() && !self.new_name_lower.is_empty()).then(|| AppRename {
            old_name: if self.old_name.is_empty() { self.old_name_lower.clone() } else { truncate(&self.old_name, 80) },
            new_name: if self.new_name.is_empty() { self.new_name_lower.clone() } else { truncate(&self.new_name, 80) },
            old_name_lower: self.old_name_lower.to_lowercase(),
            new_name_lower: self.new_name_lower.to_lowercase(),
            description: truncate(&self.description, MAX_TEXT),
            renamed_at: self.renamed_at.as_deref().and_then(parse_rfc3339),
        })
    }
}

#[derive(Deserialize, Default)]
struct RawRepoRename {
    #[serde(default, deserialize_with = "lenient")]
    old_name: String,
    #[serde(default, deserialize_with = "lenient")]
    new_name: String,
    #[serde(default, deserialize_with = "lenient")]
    renamed_at: Option<String>,
}

impl RawRepoRename {
    /// `None` unless both names are `owner/name` under [`GITHUB_OWNER`]. A feed that could send
    /// an app's downloads to someone else's repository would make its checksums worthless (#90).
    fn into_rename(self) -> Option<RepoRename> {
        let ok = |r: &str| {
            let mut parts = r.split('/');
            matches!((parts.next(), parts.next(), parts.next()), (Some(a), Some(b), None) if a.eq_ignore_ascii_case(GITHUB_OWNER) && !b.is_empty() && b != "." && b != ".."
          && r.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_./".contains(&c)))
        };
        (ok(&self.old_name) && ok(&self.new_name)).then(|| RepoRename {
            old_repo: self.old_name,
            new_repo: self.new_name,
            renamed_at: self.renamed_at.as_deref().and_then(parse_rfc3339),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{CraftApp, LAUNCHER};

    const NOW: i64 = 1_791_500_000; // 2026-10-09T…
    const HASH: &str = "8a5edab282632443219e051e4ade2d1d5bbc671c781051bf1437897cbdfea0f1";

    /// What storyteller-web serves (its `GetCraftAppsReleaseInfoResponse`), abridged.
    fn sample() -> String {
        format!(
            r#"{{
        "success": true,
        "release_info": {{
          "apps": {{
            "photocraft": {{
              "name": "PhotoCraft", "repo": "storytold/photocraft",
              "latest_release": {{
                "tag": "v0.6.0", "version": "0.6.0", "name": "PhotoCraft v0.6.0",
                "published_at": "2026-10-09T10:00:00Z",
                "html_url": "https://github.com/storytold/photocraft/releases/tag/v0.6.0",
                "prerelease": false, "notes_markdown": "Notes",
                "assets": [
                  {{ "name": "photocraft-0.6.0-macos-universal.dmg", "size_bytes": 123,
                     "download_url": "https://github.com/storytold/photocraft/releases/download/v0.6.0/photocraft-0.6.0-macos-universal.dmg",
                     "sha256": "{HASH}", "platform": {{ "os": "macos", "arch": "universal", "package_type": "dmg" }} }},
                  {{ "name": "evil.dmg", "size_bytes": 1, "download_url": "https://evil.example/evil.dmg", "sha256": null }}
                ]
              }},
              "app_last_fetched": "2026-10-09T13:00:00Z"
            }},
            "soundcraft": {{ "name": "SoundCraft", "repo": "storytold/soundcraft", "latest_release": null,
                             "app_last_fetched": "2026-10-09T13:00:00Z" }},
            "filmcraft": {{ "name": "FilmCraft", "repo": "storytold/filmcraft", "latest_release": null, "app_last_fetched": null }},
            "vectorcraft": {{ "repo": "storytold/vectorcraft", "app_last_fetched": "2026-10-01T00:00:00Z",
                              "latest_release": {{ "tag": "v0.7.0" }} }},
            "brandnewcraft": {{ "repo": "storytold/brandnewcraft", "app_last_fetched": "2026-10-09T13:00:00Z" }}
          }},
          "last_fetched": "2026-10-09T13:00:00Z",
          "some_future_field": {{ "whatever": [1, 2, 3] }}
        }},
        "app_renames": [],
        "repo_renames": [],
        "important_notices": {{ "announcements": [], "alerts": [] }}
      }}"#
        )
    }

    fn app(id: &str) -> &'static CraftApp {
        CraftApp::by_id(id).unwrap()
    }

    #[test]
    fn uses_fresh_entries_and_falls_back_for_everything_else() {
        let feed = Feed::parse(&sample()).unwrap();
        // Fresh, with a release: only the download that belongs to the repo survives.
        let FeedAnswer::Fresh(Some(r)) = feed.answer(app("photocraft"), NOW) else { panic!() };
        assert_eq!(r.version_label(), "0.6.0");
        assert_eq!(r.assets.len(), 1);
        assert_eq!(r.assets[0].sha256.as_deref(), Some(HASH));
        assert_eq!(r.published_at, parse_rfc3339("2026-10-09T10:00:00Z"));
        // Fresh, and no release yet.
        assert_eq!(feed.answer(app("soundcraft"), NOW), FeedAnswer::Fresh(None));
        // Never fetched, more than a day old, or not listed: ask GitHub.
        assert_eq!(feed.answer(app("filmcraft"), NOW), FeedAnswer::AskGitHub);
        assert_eq!(feed.answer(app("vectorcraft"), NOW), FeedAnswer::AskGitHub);
        assert_eq!(feed.answer(app("wordcraft"), NOW), FeedAnswer::AskGitHub);
        assert_eq!(feed.answer(&LAUNCHER, NOW), FeedAnswer::AskGitHub);
        // An app the launcher doesn't know is simply carried along.
        assert!(feed.apps.contains_key("brandnewcraft"));
    }

    #[test]
    fn an_app_with_a_former_repo_is_matched_under_either_name() {
        // MailCraft moved from `UCS-Care-Group` to `UCSLabs` (#1593): a feed still naming the former
        // repo is used, with downloads checked against that repo, and "no release" under one name
        // sends the launcher to GitHub, where the other name is asked too.
        let mut mailcraft = *app("mailcraft");
        mailcraft.repo = "UCSLabs/mailcraft";
        mailcraft.former_repo = Some("UCS-Care-Group/mailcraft");
        let mailcraft = &mailcraft;
        let former = "UCS-Care-Group/mailcraft";
        let entry = |repo: &str, release: &str| {
            format!(
                r#"{{"release_info": {{"apps": {{"mailcraft": {{"repo": "{repo}", "app_last_fetched": "2026-10-09T13:00:00Z", "latest_release": {release}}}}}}}}}"#
            )
        };
        let asset = |repo: &str| {
            format!(
                r#"{{"name": "mailcraft-0.15.0-x.msi", "size_bytes": 1, "download_url": "https://github.com/{repo}/releases/download/v0.15.0/mailcraft-0.15.0-x.msi", "sha256": "{HASH}"}}"#
            )
        };
        let release = format!(r#"{{"tag": "v0.15.0", "assets": [{}, {}]}}"#, asset(former), asset(mailcraft.repo));
        let FeedAnswer::Fresh(Some(r)) = Feed::parse(&entry(former, &release)).unwrap().answer(mailcraft, NOW) else { panic!() };
        assert_eq!(r.assets.len(), 1);
        assert!(r.assets[0].url.starts_with(&release::download_prefix(former)));
        for repo in [mailcraft.repo, former] {
            assert_eq!(Feed::parse(&entry(repo, "null")).unwrap().answer(mailcraft, NOW), FeedAnswer::AskGitHub, "{repo}");
        }
        assert_eq!(Feed::parse(&entry("someone/mailcraft", &release)).unwrap().answer(mailcraft, NOW), FeedAnswer::AskGitHub);
    }

    #[test]
    fn survives_anything_the_server_might_send() {
        for junk in ["", "null", "[]", "42", "<html>502 Bad Gateway</html>", "{\"success\":tru"] {
            let feed = Feed::parse(junk);
            if let Some(feed) = feed {
                assert!(feed.apps.is_empty(), "{junk}");
            }
        }
        // Every field the wrong type: nothing panics, nothing is trusted.
        let weird = r#"{"release_info": {"apps": {"photocraft": {"repo": 7, "latest_release": "yes",
        "app_last_fetched": ["no"]}, "pdfcraft": 5}, "last_fetched": {}},
        "app_renames": "none", "repo_renames": [1, {"old_name": "a/b"}],
        "important_notices": {"announcements": [null, {"title": 3}], "alerts": "x"}}"#;
        let feed = Feed::parse(weird).unwrap();
        assert_eq!(feed.answer(app("photocraft"), NOW), FeedAnswer::AskGitHub);
        assert!(feed.repo_renames.is_empty());
        // An empty object is a feed that knows nothing.
        assert_eq!(Feed::parse("{}").unwrap(), Feed::default());
    }

    #[test]
    fn follows_repo_renames_but_never_trusts_a_repo_it_was_not_told_about() {
        let moved = sample()
            .replace(
                "\"repo_renames\": []",
                r#""repo_renames": [{"old_name": "Storytold/photocraft", "new_name": "storytold/photocraft-app", "renamed_at": null}]"#,
            )
            .replace(
                "\"name\": \"PhotoCraft\", \"repo\": \"storytold/photocraft\"",
                "\"name\": \"PhotoCraft\", \"repo\": \"storytold/photocraft-app\"",
            )
            .replace("github.com/storytold/photocraft/releases", "github.com/storytold/photocraft-app/releases");
        let feed = Feed::parse(&moved).unwrap();
        assert_eq!(feed.repo_for(app("photocraft")), "storytold/photocraft-app");
        let FeedAnswer::Fresh(Some(r)) = feed.answer(app("photocraft"), NOW) else { panic!() };
        assert!(r.assets[0].url.starts_with("https://github.com/storytold/photocraft-app/releases/download/"));
        // The same entry without the rename: the repo doesn't match, so GitHub decides.
        let unannounced = moved.replace(r#"{"old_name": "Storytold/photocraft", "new_name": "storytold/photocraft-app", "renamed_at": null}"#, "");
        let feed = Feed::parse(&unannounced).unwrap();
        assert_eq!(feed.answer(app("photocraft"), NOW), FeedAnswer::AskGitHub);
    }

    #[test]
    fn a_rename_out_of_storytold_is_dropped() {
        // Even a feed that vouches for the new repository, with matching links and checksums,
        // can't send an app's downloads to another account (#90).
        let moved = sample()
            .replace(
                "\"repo_renames\": []",
                r#""repo_renames": [{"old_name": "storytold/photocraft", "new_name": "art/photocraft", "renamed_at": null},
                                  {"old_name": "art/photocraft", "new_name": "storytold/photocraft", "renamed_at": null},
                                  {"old_name": "storytold/photocraft", "new_name": "storytold.evil.example/photocraft", "renamed_at": null}]"#,
            )
            .replace("\"name\": \"PhotoCraft\", \"repo\": \"storytold/photocraft\"", "\"name\": \"PhotoCraft\", \"repo\": \"art/photocraft\"")
            .replace("github.com/storytold/photocraft/releases", "github.com/art/photocraft/releases");
        let feed = Feed::parse(&moved).unwrap();
        assert!(feed.repo_renames.is_empty(), "{:?}", feed.repo_renames);
        assert_eq!(feed.repo_for(app("photocraft")), "storytold/photocraft");
        assert_eq!(feed.answer(app("photocraft"), NOW), FeedAnswer::AskGitHub);
    }

    #[test]
    fn feed_release_drops_an_asset_from_another_tag() {
        let wrong = sample().replace("releases/download/v0.6.0/photocraft", "releases/download/v9.0.0/photocraft");
        let FeedAnswer::Fresh(Some(r)) = Feed::parse(&wrong).unwrap().answer(app("photocraft"), NOW) else { panic!() };
        assert!(r.assets.is_empty());
    }

    #[test]
    fn a_feed_url_cannot_traverse_out_of_the_repository() {
        // The feed advertises a legitimate tag but points the asset at another repository by
        // climbing out with `../` after the approved prefix (CWE-180).
        let escaped = sample().replace(
            "https://github.com/storytold/photocraft/releases/download/v0.6.0/photocraft-0.6.0-macos-universal.dmg",
            "https://github.com/storytold/photocraft/releases/download/../../../../attacker/repo/releases/download/v99/photocraft-0.6.0-macos-universal.dmg",
        );
        let FeedAnswer::Fresh(Some(r)) = Feed::parse(&escaped).unwrap().answer(app("photocraft"), NOW) else { panic!() };
        assert!(r.assets.is_empty(), "the traversing download must be rejected");
    }

    #[test]
    fn a_local_mirror_is_trusted_only_when_asked_for_and_never_goes_stale() {
        const MIRROR: &str = "http://127.0.0.1:8787/";
        let local = sample().replace("https://github.com/storytold/photocraft/releases/download/v0.6.0/", "http://127.0.0.1:8787/photocraft/v0.6.0/");
        // Without the mirror, the local link is dropped like any other stranger's.
        let FeedAnswer::Fresh(Some(r)) = Feed::parse(&local).unwrap().answer(app("photocraft"), NOW) else { panic!() };
        assert!(r.assets.is_empty());
        let feed = Feed::parse(&local).unwrap().with_mirror(MIRROR);
        let FeedAnswer::Fresh(Some(r)) = feed.answer(app("photocraft"), NOW) else { panic!() };
        assert_eq!(r.assets.len(), 1, "the mirror's file is kept, evil.example's still isn't");
        assert_eq!(r.assets[0].url, "http://127.0.0.1:8787/photocraft/v0.6.0/photocraft-0.6.0-macos-universal.dmg");
        // A week-old sync still counts.
        assert!(matches!(feed.answer(app("photocraft"), NOW + 7 * 86_400), FeedAnswer::Fresh(Some(_))));
        // Escaping the mirror's folder isn't allowed, and a base without its trailing slash isn't a mirror.
        let escaping = local.replace("http://127.0.0.1:8787/photocraft/", "http://127.0.0.1:8787/../photocraft/");
        let FeedAnswer::Fresh(Some(r)) = Feed::parse(&escaping).unwrap().with_mirror(MIRROR).answer(app("photocraft"), NOW) else { panic!() };
        assert!(r.assets.is_empty());
        assert_eq!(Feed::default().with_mirror("http://127.0.0.1:8787").mirror, None);
    }

    /// Contract check against what storyteller-web really serves: point `CRAFT_APPS_FEED_SAMPLE`
    /// at a response it produced (its `live_round_fetches_real_releases` test writes one).
    #[test]
    #[ignore = "needs a sample written by storyteller-web"]
    fn reads_what_storyteller_web_serves() {
        let Ok(path) = std::env::var("CRAFT_APPS_FEED_SAMPLE") else { return };
        let json = std::fs::read_to_string(path).unwrap();
        let feed = Feed::parse(&json).unwrap();
        let now = feed.last_fetched.unwrap();
        for app in [app("photocraft"), &LAUNCHER] {
            let FeedAnswer::Fresh(Some(r)) = feed.answer(app, now) else { panic!("{} wasn't fresh", app.id) };
            assert!(r.version.is_some(), "{}", app.id);
            assert!(!r.assets.is_empty() && r.assets.iter().all(|a| a.sha256.is_some()), "{}", app.id);
            assert!(r.assets.iter().all(|a| a.url.starts_with(&format!("https://github.com/{}/releases/download/", app.repo))));
        }
    }

    #[test]
    fn a_feed_release_with_an_odd_tag_asks_github() {
        for odd in ["v0.6.0~beta-\\u202e", "v0.6.0~beta-a/b"] {
            let feed = sample().replace("\"tag\": \"v0.6.0\"", &format!("\"tag\": \"{odd}\""));
            assert_eq!(Feed::parse(&feed).unwrap().answer(app("photocraft"), NOW), FeedAnswer::AskGitHub, "{odd:?}");
        }
    }

    #[test]
    fn catalog_channels_fall_back_independently_and_empty_records_are_ignored() {
        let photo = app("photocraft");
        let parsed = |channels: &str| {
            Feed::parse_catalog_toml(&format!(
                "generated_at = \"2026-10-09T13:00:00Z\"\n[[apps]]\nid = \"photocraft\"\nrepo = \"storytold/photocraft\"\n{channels}"
            ))
            .unwrap()
        };
        let stable = parsed("[apps.releases.stable]\ntag = \"v1.0.0\"\n");
        assert!(matches!(stable.answer_prerelease(photo, NOW), FeedAnswer::Fresh(Some(r)) if r.tag == "v1.0.0"));
        assert_eq!(stable.answer_nightly(photo, NOW), FeedAnswer::AskGitHub);
        let pre = parsed("[apps.releases.prerelease]\ntag = \"v1.1.0-rc.1\"\n");
        assert!(matches!(pre.answer_prerelease(photo, NOW), FeedAnswer::Fresh(Some(r)) if r.tag == "v1.1.0-rc.1"));
        let empty = parsed("[apps.releases]\n");
        assert!(empty.apps.is_empty());
        assert_eq!(empty.answer_prerelease(photo, NOW), FeedAnswer::AskGitHub);
        assert_eq!(pre.answer_prerelease(photo, NOW + MAX_AGE * 2), FeedAnswer::AskGitHub);
    }

    #[test]
    fn signed_catalog_toml_answers_stable_prerelease_nightly_and_launcher() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        use ed25519_dalek::{Signer as _, SigningKey};

        let toml = format!(
            r#"generated_at = "2026-10-09T13:00:00Z"

[launcher]
id = "artcraft-launcher"
repo = "storytold/craft-launcher"
[launcher.releases.stable]
tag = "v0.20.4"
version = "0.20.4"
html_url = "https://github.com/storytold/craft-launcher/releases/tag/v0.20.4"
prerelease = false
[[launcher.releases.stable.assets]]
name = "artcraft-launcher-0.20.4-macos-universal.dmg"
download_url = "https://github.com/storytold/craft-launcher/releases/download/v0.20.4/artcraft-launcher-0.20.4-macos-universal.dmg"
sha256 = "{HASH}"

[[apps]]
id = "photocraft"
repo = "storytold/photocraft"
[apps.releases.stable]
tag = "v0.23.1"
version = "0.23.1"
html_url = "https://github.com/storytold/photocraft/releases/tag/v0.23.1"
prerelease = false
[[apps.releases.stable.assets]]
name = "photocraft-0.23.1-macos-universal.dmg"
download_url = "https://github.com/storytold/photocraft/releases/download/v0.23.1/photocraft-0.23.1-macos-universal.dmg"
sha256 = "{HASH}"
[apps.releases.prerelease]
tag = "v0.24.0-rc.1"
version = "0.24.0-rc.1"
html_url = "https://github.com/storytold/photocraft/releases/tag/v0.24.0-rc.1"
prerelease = true
[apps.releases.nightly]
tag = "v0.24.0-nightly.20261009.1.1"
version = "0.24.0-nightly.20261009.1.1"
html_url = "https://github.com/storytold/photocraft/releases/tag/v0.24.0-nightly.20261009.1.1"
prerelease = true
[[apps.releases.nightly.assets]]
name = "photocraft-0.24.0-nightly.20261009.1.1-linux-x86_64.AppImage"
download_url = "https://github.com/storytold/photocraft/releases/download/v0.24.0-nightly.20261009.1.1/photocraft-0.24.0-nightly.20261009.1.1-linux-x86_64.AppImage"
sha256 = "{HASH}"
"#
        );
        let key = SigningKey::from_bytes(&[7; 32]);
        let sig = STANDARD.encode(key.sign(toml.as_bytes()).to_bytes());
        let feed = Feed::parse_signed_catalog_with_key(toml.as_bytes(), sig.as_bytes(), &key.verifying_key()).unwrap();

        let FeedAnswer::Fresh(Some(launcher)) = feed.answer(&LAUNCHER, NOW) else { panic!() };
        assert_eq!(launcher.tag, "v0.20.4");

        let FeedAnswer::Fresh(Some(stable)) = feed.answer(app("photocraft"), NOW) else { panic!() };
        assert_eq!(stable.tag, "v0.23.1");
        assert_eq!(stable.assets.len(), 1);

        let FeedAnswer::Fresh(Some(pre)) = feed.answer_prerelease(app("photocraft"), NOW) else { panic!() };
        assert_eq!(pre.tag, "v0.24.0-rc.1");

        let FeedAnswer::Fresh(Some(nightly)) = feed.answer_nightly(app("photocraft"), NOW) else { panic!() };
        assert_eq!(nightly.tag, "v0.24.0-nightly.20261009.1.1");

        // Tampering or wrong key is rejected before parsing.
        let wrong_key = SigningKey::from_bytes(&[9; 32]);
        assert!(Feed::parse_signed_catalog_with_key(toml.as_bytes(), sig.as_bytes(), &wrong_key.verifying_key()).is_err());
        assert!(Feed::parse_signed_catalog_with_key(format!("{toml}\n").as_bytes(), sig.as_bytes(), &key.verifying_key()).is_err());
    }
}
