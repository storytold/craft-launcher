//! Our release API, from storyteller-web: `GET https://api.storyteller.ai/v1/craft_apps/release_info`
//! (the release feed: every app's latest stable release, beta and nightly) and
//! `GET https://api.storyteller.ai/v1/craft_apps/{app}/releases` (one app's whole release history,
//! [`AppReleases`]).
//!
//! storyteller-web polls GitHub for every Craft App on a schedule and serves the result. It is
//! the launcher's only source of release information: an app the feed doesn't list, or hasn't
//! fetched within [`MAX_AGE`], is [`FeedAnswer::Unknown`], and the launcher says it couldn't check.
//! Downloads still come from each app's GitHub releases.
//!
//! The feed is input like any other: every field is optional, unknown fields are ignored, a field
//! of the wrong type reads as missing, one bad entry never spoils the others, and download links
//! must still point at the app's own GitHub releases.
//!
//! A developer can serve the feed and the builds from their own machine instead (see
//! [`Feed::with_mirror`]): then links under the mirror are accepted too, and entries never go stale.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde::de::{DeserializeOwned, Deserializer};

use crate::catalog::{CraftApp, GITHUB_OWNER};
use crate::release::{self, Asset, Release, truncate};
use crate::time::parse_rfc3339;
use crate::version;

/// Where the feed lives.
pub const FEED_URL: &str = "https://api.storyteller.ai/v1/craft_apps/release_info";

/// An app's entry is trusted for this long after storyteller-web fetched it.
pub const MAX_AGE: i64 = 24 * 3600;

/// The feed, read leniently.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Feed {
    /// By lowercase app name.
    pub apps: BTreeMap<String, FeedApp>,
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
    /// Present when the repo has a published release. Meaningless unless `fetched_at` is set.
    pub release: Option<FeedRelease>,
    /// The entry sent a `latest_release` that can't be read: unlike `null`, that says nothing about
    /// whether the app has a release.
    pub release_unreadable: bool,
    /// When storyteller-web last fetched this app (Unix seconds).
    pub fetched_at: Option<i64>,
    /// Whether the entry carries a usable pre-release channel: the `releases` version list, and a
    /// `latest_prerelease` that is absent, `null` (no beta) or readable. A server from before those
    /// existed sends neither, and its silence says nothing about betas; a beta that can't be read
    /// says nothing either.
    pub channels: bool,
    /// The newest beta or release candidate. Meaningless unless `channels` is set.
    pub prerelease: Option<FeedRelease>,
    /// The tags of the recent ordinary releases (stable ones and betas), newest first, from the
    /// feed's version list. Meaningless unless `channels` is set.
    pub versions: Vec<String>,
}

/// A release as the feed describes it, before it's checked against the app's repo.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeedRelease {
    pub tag: String,
    pub name: String,
    pub published_at: Option<i64>,
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
    /// Not listed, never fetched, older than [`MAX_AGE`], or not holding up: unknown.
    Unknown,
}

/// The most text kept from one rename's description.
const MAX_TEXT: usize = 4000;

impl Feed {
    /// Reads the feed. `None` only when the answer isn't JSON (or isn't an object) at all.
    pub fn parse(json: &str) -> Option<Self> {
        let raw: RawResponse = serde_json::from_str(json).ok()?;
        let feed = Feed {
            apps: raw.release_info.apps.into_iter().map(|(k, v)| (k.to_lowercase(), v.into_app())).collect(),
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

    /// The repo `app`'s downloads come from, following the feed's repo renames. Always under
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
        let Some((entry, repo)) = self.checked(app, now).filter(|(entry, _)| !entry.release_unreadable) else {
            return FeedAnswer::Unknown;
        };
        match &entry.release {
            None => FeedAnswer::Fresh(None),
            Some(r) => match r.to_release(&repo, self.mirror.as_deref()) {
                Some(release) => FeedAnswer::Fresh(Some(release)),
                None => FeedAnswer::Unknown,
            },
        }
    }

    /// What the feed says about `app` at `now` with pre-releases on: the higher of its stable
    /// release and its newest beta or RC, the higher version of the two. Unknown whenever the
    /// entry can't settle that: no stable answer (see [`Feed::answer`]), an entry
    /// without the pre-release channel (an older server), a beta that doesn't hold up, or a
    /// version list naming a higher ordinary version than the two the feed sent in full.
    pub fn answer_with_prereleases(&self, app: &CraftApp, now: i64) -> FeedAnswer {
        let FeedAnswer::Fresh(stable) = self.answer(app, now) else { return FeedAnswer::Unknown };
        let Some((entry, repo)) = self.checked(app, now).filter(|(entry, _)| entry.channels) else {
            return FeedAnswer::Unknown;
        };
        let beta = match &entry.prerelease {
            None => None,
            Some(r) => match r.to_release(&repo, self.mirror.as_deref()).filter(release::ordinary_release_allowed) {
                Some(beta) => Some(beta),
                None => return FeedAnswer::Unknown,
            },
        };
        let best = [stable, beta].into_iter().flatten().reduce(|a, b| if newer_release(&b, &a) { b } else { a });
        let best_version = best.as_ref().and_then(|r| r.version.as_ref());
        let listed_higher = entry
            .versions
            .iter()
            .filter_map(|tag| version::parse(tag).filter(|v| release::valid_tag(tag) && !release::nightly_version(v)))
            .any(|listed| best_version.is_none_or(|best| version::is_newer(&listed, best)));
        if listed_higher { FeedAnswer::Unknown } else { FeedAnswer::Fresh(best) }
    }

    /// `app`'s entry and the repo its downloads must belong to, when the entry is fresh and its
    /// repo is one the app may use.
    fn checked(&self, app: &CraftApp, now: i64) -> Option<(&FeedApp, String)> {
        let entry = self.apps.get(app.id)?;
        let fetched_at = entry.fetched_at?;
        // A timestamp from the future (clock skew) counts as just now; one too old doesn't count.
        // A local mirror is as fresh as its last sync.
        if self.mirror.is_none() && now.saturating_sub(fetched_at) > MAX_AGE {
            return None;
        }
        // The feed's repo must be the app's, one the feed says it moved to, or the app's former repo
        // (downloads are then checked against that one).
        let mut repo = self.repo_for(app);
        if let Some(feed_repo) = &entry.repo
            && !feed_repo.eq_ignore_ascii_case(&repo)
        {
            repo = app.former_repo.filter(|former| feed_repo.eq_ignore_ascii_case(former))?.to_string();
        }
        Some((entry, repo))
    }
}

/// Whether `a` is a higher version than `b`; a release without a
/// version never is.
fn newer_release(a: &Release, b: &Release) -> bool {
    match (a.version.as_ref(), b.version.as_ref()) {
        (Some(a), Some(b)) => version::is_newer(a, b),
        (Some(_), None) => true,
        (None, _) => false,
    }
}

/// One app's release history, from `GET /v1/craft_apps/{app}/releases`, checked like the feed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppReleases {
    /// Whether `releases` is every release the app has; when not, older ones exist.
    pub complete: bool,
    /// Newest first, nightlies included: every release that holds up for the app's repo.
    pub releases: Vec<Release>,
    /// When storyteller-web fetched it (Unix seconds): a saved copy older than [`MAX_AGE`] is as
    /// unknown as a stale answer.
    pub fetched_at: Option<i64>,
}

/// The most releases read from one app's history (storyteller-web sends at most 1000).
const MAX_HISTORY: usize = 1000;

/// The address of `app`'s release history, next to the feed at `feed_url`
/// (`…/craft_apps/release_info` → `…/craft_apps/{app}/releases`). `None` for a feed elsewhere
/// (a local mirror), which has no histories, and for an id that isn't a plain name.
pub fn app_releases_url(feed_url: &str, app: &str) -> Option<String> {
    let base = feed_url.strip_suffix("/release_info")?;
    let plain = !app.is_empty() && app.len() <= 64 && app.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    plain.then(|| format!("{base}/{app}/releases"))
}

impl AppReleases {
    /// Reads an app's history (answer of [`app_releases_url`]). Its repo must be `repo` or the
    /// app's `former_repo`, as in [`Feed::answer`], and every release goes through the same download
    /// checks. `None` when it isn't such an answer, or was never fetched or is older than
    /// [`MAX_AGE`] at `now`, or has no release list: the history is then unknown.
    pub fn parse(json: &str, repo: &str, former_repo: Option<&str>, now: i64) -> Option<Self> {
        let raw: RawAppReleases = serde_json::from_str(json).ok()?;
        let fetched_at = raw.app_last_fetched.as_deref().and_then(parse_rfc3339)?;
        if now.saturating_sub(fetched_at) > MAX_AGE {
            return None;
        }
        let served = raw.repo?;
        let repo = if served.eq_ignore_ascii_case(repo) { repo } else { former_repo.filter(|former| served.eq_ignore_ascii_case(former))? };
        // No list at all is no answer, never a complete empty history.
        let serde_json::Value::Array(items) = raw.releases? else { return None };
        let releases = items
            .into_iter()
            .take(MAX_HISTORY)
            .filter_map(|item| RawRelease::deserialize(item).ok())
            .filter_map(RawRelease::into_release)
            .filter_map(|r| r.to_release(repo, None))
            .collect();
        Some(Self { complete: raw.complete, releases, fetched_at: Some(fetched_at) })
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
    /// Kept raw: a release that is there but can't be read is not the same as none.
    #[serde(default)]
    latest_release: Option<serde_json::Value>,
    #[serde(default, deserialize_with = "lenient")]
    app_last_fetched: Option<String>,
    /// Kept raw: a beta that is there but can't be read is not the same as none.
    #[serde(default)]
    latest_prerelease: Option<serde_json::Value>,
    /// Kept raw: whether it is a list at all says whether the server knows the channels.
    #[serde(default)]
    releases: Option<serde_json::Value>,
}

/// A release field as sent: absent or `null` is no release, `Err` one that is there but can't be read.
fn read_release(value: Option<serde_json::Value>) -> Result<Option<FeedRelease>, ()> {
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => RawRelease::deserialize(value).ok().and_then(RawRelease::into_release).map(Some).ok_or(()),
    }
}

/// The most entries read from an app's version list (storyteller-web sends at most 30).
const MAX_VERSIONS: usize = 100;

impl RawApp {
    fn into_app(self) -> FeedApp {
        let versions = match &self.releases {
            Some(serde_json::Value::Array(items)) => Some(
                items
                    .iter()
                    .filter_map(|item| item.get("tag")?.as_str().filter(|t| !t.is_empty() && t.len() <= 64).map(str::to_string))
                    .take(MAX_VERSIONS)
                    .collect(),
            ),
            _ => None,
        };
        let prerelease = read_release(self.latest_prerelease);
        let release = read_release(self.latest_release);
        FeedApp {
            repo: self.repo.filter(|r| r.split('/').count() == 2 && r.split('/').all(|part| !part.is_empty() && part != "." && part != "..")),
            release_unreadable: release.is_err(),
            release: release.unwrap_or_default(),
            fetched_at: self.app_last_fetched.as_deref().and_then(parse_rfc3339),
            channels: versions.is_some() && prerelease.is_ok(),
            prerelease: prerelease.unwrap_or_default(),
            versions: versions.unwrap_or_default(),
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
            html_url: self.html_url.unwrap_or_default(),
            prerelease: self.prerelease,
            notes: self.notes_markdown.unwrap_or_default(),
            assets: self.assets.into_iter().filter_map(|a| Some((a.name?, a.size_bytes, a.download_url?, a.sha256))).take(300).collect(),
        })
    }
}

#[derive(Deserialize, Default)]
struct RawAppReleases {
    #[serde(default, deserialize_with = "lenient")]
    repo: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    app_last_fetched: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    complete: bool,
    /// Kept raw: it must be a list, so a missing one isn't read as an empty history.
    #[serde(default)]
    releases: Option<serde_json::Value>,
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

    /// A feed listing PhotoCraft's stable `v0.6.0` and, when given, its channels: the beta
    /// `beta` (or none) and the version list `versions`.
    fn with_channels(beta: Option<&str>, versions: Option<&[&str]>) -> Feed {
        let release = |tag: &str, pre: bool| {
            format!(
                r#"{{"tag": "{tag}", "prerelease": {pre}, "assets": [{{"name": "a.dmg", "size_bytes": 1, "sha256": "{HASH}",
                    "download_url": "https://github.com/storytold/photocraft/releases/download/{tag}/a.dmg"}}]}}"#
            )
        };
        let beta = beta.map_or("null".to_string(), |tag| release(tag, true));
        let versions = versions.map_or(String::new(), |tags| {
            let list: Vec<String> = tags.iter().map(|t| format!(r#"{{"tag": "{t}", "prerelease": {}}}"#, t.contains('-'))).collect();
            format!(r#", "releases": [{}]"#, list.join(","))
        });
        Feed::parse(&format!(
            r#"{{"release_info": {{"apps": {{"photocraft": {{"repo": "storytold/photocraft", "app_last_fetched": "2026-10-09T13:00:00Z",
                "latest_release": {}, "latest_prerelease": {beta}{versions}}}}}}}}}"#,
            release("v0.6.0", false)
        ))
        .unwrap()
    }

    fn tag(answer: FeedAnswer) -> Option<String> {
        match answer {
            FeedAnswer::Fresh(found) => Some(found.map_or("none".into(), |r| r.tag)),
            FeedAnswer::Unknown => None,
        }
    }

    #[test]
    fn with_pre_releases_the_feed_names_the_higher_of_its_stable_release_and_its_beta() {
        let photo = app("photocraft");
        let newer_beta = with_channels(Some("v0.7.0-rc.1"), Some(&["v0.7.0-rc.1", "v0.6.0"]));
        assert_eq!(tag(newer_beta.answer_with_prereleases(photo, NOW)).as_deref(), Some("v0.7.0-rc.1"));
        assert_eq!(tag(newer_beta.answer(photo, NOW)).as_deref(), Some("v0.6.0"), "pre-releases off: stable only");
        // A beta published after a stable release with higher numbers doesn't replace it.
        let older_beta = with_channels(Some("v0.5.1-rc.1"), Some(&["v0.5.1-rc.1", "v0.6.0"]));
        assert_eq!(tag(older_beta.answer_with_prereleases(photo, NOW)).as_deref(), Some("v0.6.0"));
        let no_beta = with_channels(None, Some(&["v0.6.0"]));
        assert_eq!(tag(no_beta.answer_with_prereleases(photo, NOW)).as_deref(), Some("v0.6.0"));
    }

    #[test]
    fn with_pre_releases_the_answer_is_unknown_whenever_the_feed_cant_settle_it() {
        let photo = app("photocraft");
        // A server from before the channels: its silence says nothing about betas.
        let old_server = with_channels(None, None);
        assert_eq!(old_server.answer_with_prereleases(photo, NOW), FeedAnswer::Unknown);
        assert_eq!(tag(old_server.answer(photo, NOW)).as_deref(), Some("v0.6.0"));
        // The version list names a higher version than the two sent in full.
        let behind = with_channels(Some("v0.7.0-rc.1"), Some(&["v0.7.0", "v0.7.0-rc.1", "v0.6.0"]));
        assert_eq!(behind.answer_with_prereleases(photo, NOW), FeedAnswer::Unknown);
        // A "beta" that is really a nightly, whose tag isn't a version, or that has no tag: it is
        // there, so it isn't "no beta".
        for beta in ["v0.7.0-nightly.20261126.1.1", "latest", ""] {
            let feed = with_channels(Some(beta), Some(&["v0.6.0"]));
            assert_eq!(feed.answer_with_prereleases(photo, NOW), FeedAnswer::Unknown, "{beta}");
        }
        // Stale entries are unknown with pre-releases on too.
        let fresh = with_channels(Some("v0.7.0-rc.1"), Some(&["v0.7.0-rc.1"]));
        assert_eq!(fresh.answer_with_prereleases(photo, NOW + 2 * MAX_AGE), FeedAnswer::Unknown);
    }

    #[test]
    fn a_nightly_in_the_version_list_does_not_make_the_answer_unknown() {
        let feed = with_channels(Some("v0.7.0-rc.1"), Some(&["v0.8.0-nightly.1", "v0.7.0-rc.1", "v0.6.0"]));
        assert_eq!(tag(feed.answer_with_prereleases(app("photocraft"), NOW)).as_deref(), Some("v0.7.0-rc.1"));
    }

    /// A stable release that is there but can't be read says nothing; `null` says "none yet".
    #[test]
    fn an_unreadable_stable_release_is_unknown_not_none() {
        let entry = |release: &str| {
            Feed::parse(&format!(
                r#"{{"release_info": {{"apps": {{"photocraft": {{"repo": "storytold/photocraft", "app_last_fetched": "2026-10-09T13:00:00Z", "latest_release": {release}}}}}}}}}"#
            ))
            .unwrap()
        };
        let photo = app("photocraft");
        for unreadable in [r#"{"tag": ""}"#, r#"{"name": "x"}"#, "5", r#""v1.0.0""#] {
            assert_eq!(entry(unreadable).answer(photo, NOW), FeedAnswer::Unknown, "{unreadable}");
        }
        assert_eq!(entry("null").answer(photo, NOW), FeedAnswer::Fresh(None));
    }

    #[test]
    fn reads_the_channels_leniently() {
        let feed = Feed::parse(
            r#"{"release_info": {"apps": {
                "photocraft": {"latest_prerelease": null, "releases": [{"tag": "v1.0.0"}, {"tag": 7}, "x", {"tag": ""}]},
                "soundcraft": {"releases": "nope"},
                "filmcraft": {"releases": null},
                "wordcraft": {"latest_prerelease": 5, "releases": []}
            }}}"#,
        )
        .unwrap();
        let photo = &feed.apps["photocraft"];
        assert!(photo.channels && photo.prerelease.is_none(), "null: no beta");
        assert_eq!(photo.versions, ["v1.0.0"]);
        assert!(!feed.apps["soundcraft"].channels && !feed.apps["filmcraft"].channels);
        assert!(!feed.apps["wordcraft"].channels, "a beta of the wrong shape isn't \"no beta\"");
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
        // Never fetched, more than a day old, or not listed: unknown.
        assert_eq!(feed.answer(app("filmcraft"), NOW), FeedAnswer::Unknown);
        assert_eq!(feed.answer(app("vectorcraft"), NOW), FeedAnswer::Unknown);
        assert_eq!(feed.answer(app("wordcraft"), NOW), FeedAnswer::Unknown);
        assert_eq!(feed.answer(&LAUNCHER, NOW), FeedAnswer::Unknown);
        // An app the launcher doesn't know is simply carried along.
        assert!(feed.apps.contains_key("brandnewcraft"));
    }

    #[test]
    fn an_app_with_a_former_repo_is_matched_under_either_name() {
        // MailCraft moved from `UCS-Care-Group` to `UCSLabs` (#1593): a feed still naming the former
        // repo is used, with downloads checked against that repo.
        let mailcraft = app("mailcraft");
        let former = mailcraft.former_repo.unwrap();
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
            assert_eq!(Feed::parse(&entry(repo, "null")).unwrap().answer(mailcraft, NOW), FeedAnswer::Fresh(None), "{repo}");
        }
        assert_eq!(Feed::parse(&entry("someone/mailcraft", &release)).unwrap().answer(mailcraft, NOW), FeedAnswer::Unknown);
    }

    /// What `GET /v1/craft_apps/photocraft/releases` serves, with `releases` and its fetch time.
    fn history(repo: &str, fetched: &str, releases: &str) -> String {
        format!(
            r#"{{"success": true, "app_key": "photocraft", "repo": "{repo}", "app_last_fetched": {fetched}, "complete": true, "releases": [{releases}]}}"#
        )
    }

    #[test]
    fn reads_an_apps_history_with_the_feeds_checks() {
        let photo = app("photocraft");
        let release = |tag: &str, repo: &str| {
            format!(
                r#"{{"tag": "{tag}", "prerelease": {}, "assets": [{{"name": "a.dmg", "size_bytes": 1, "sha256": "{HASH}",
                    "download_url": "https://github.com/{repo}/releases/download/{tag}/a.dmg"}}]}}"#,
                tag.contains('-')
            )
        };
        let releases = [
            release("v0.7.0-nightly.20261126.1.1", photo.repo),
            release("v0.6.0", photo.repo),
            release("v0.5.0", "evil/repo"),
            r#"{"tag": ""}"#.into(),
        ];
        let json = history(photo.repo, r#""2026-10-09T13:00:00Z""#, &releases.join(","));
        let read = AppReleases::parse(&json, photo.repo, None, NOW).unwrap();
        assert!(read.complete);
        assert_eq!(read.fetched_at, parse_rfc3339("2026-10-09T13:00:00Z"));
        let tags: Vec<&str> = read.releases.iter().map(|r| r.tag.as_str()).collect();
        assert_eq!(tags, ["v0.7.0-nightly.20261126.1.1", "v0.6.0", "v0.5.0"], "newest first, nightlies kept, a tagless one dropped");
        assert_eq!(read.releases[1].assets.len(), 1);
        assert!(read.releases[2].assets.is_empty(), "a download from another repo is dropped");
        // Another repo's history, one never fetched, a stale one, and junk are unknown.
        assert!(AppReleases::parse(&history("someone/photocraft", r#""2026-10-09T13:00:00Z""#, ""), photo.repo, None, NOW).is_none());
        assert!(AppReleases::parse(&history(photo.repo, "null", ""), photo.repo, None, NOW).is_none());
        assert!(AppReleases::parse(&json, photo.repo, None, NOW + 2 * MAX_AGE).is_none());
        // A "complete" history that sends no list at all is no answer.
        let no_list = json.replace(r#", "releases": ["#, r#", "missing": ["#);
        assert!(AppReleases::parse(&no_list, photo.repo, None, NOW).is_none());
        for junk in ["", "[]", "<html>", r#"{"repo": 5}"#] {
            assert!(AppReleases::parse(junk, photo.repo, None, NOW).is_none(), "{junk}");
        }
    }

    #[test]
    fn an_apps_history_lives_next_to_the_feed() {
        assert_eq!(app_releases_url(FEED_URL, "photocraft").as_deref(), Some("https://api.storyteller.ai/v1/craft_apps/photocraft/releases"));
        assert_eq!(app_releases_url("http://127.0.0.1:8787/release_info.json", "photocraft"), None, "a local mirror has none");
        for odd in ["", "../x", "a/b", "Photo", "a?b"] {
            assert_eq!(app_releases_url(FEED_URL, odd), None, "{odd}");
        }
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
        assert_eq!(feed.answer(app("photocraft"), NOW), FeedAnswer::Unknown);
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
        // The same entry without the rename: the repo doesn't match, so the answer is unknown.
        let unannounced = moved.replace(r#"{"old_name": "Storytold/photocraft", "new_name": "storytold/photocraft-app", "renamed_at": null}"#, "");
        let feed = Feed::parse(&unannounced).unwrap();
        assert_eq!(feed.answer(app("photocraft"), NOW), FeedAnswer::Unknown);
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
        assert_eq!(feed.answer(app("photocraft"), NOW), FeedAnswer::Unknown);
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
    fn a_feed_release_with_an_odd_tag_is_unknown() {
        for odd in ["v0.6.0~beta-\\u202e", "v0.6.0~beta-a/b"] {
            let feed = sample().replace("\"tag\": \"v0.6.0\"", &format!("\"tag\": \"{odd}\""));
            assert_eq!(Feed::parse(&feed).unwrap().answer(app("photocraft"), NOW), FeedAnswer::Unknown, "{odd:?}");
        }
    }
}
