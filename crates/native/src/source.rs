//! Where release information comes from: our release API only.
//!
//! [`ApiSource`] reads one app's release history from `GET /v1/craft_apps/{app}/releases`
//! ([`AppReleases`]: every published release with its files and checksums, nightlies included),
//! keeps it in memory for a while, and answers the version list, a pinned version and the Nightly
//! track from it. The latest stable release and beta of every app come from the release feed
//! itself (`backend::latest_for`). There is no other route: when our API can't be reached, the
//! launcher says it couldn't check.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use artcraft_launcher_core::feed::{self, AppReleases};
use artcraft_launcher_core::protocol::{LatestRelease, VersionTrack};
use artcraft_launcher_core::release::{self, Release, ReleaseQuery};

use crate::http::Http;

/// How long a fetched history stands in for asking again, for background checks.
pub(crate) const VERSIONS_FRESH: i64 = 60 * 60;
/// How long it stands in when the user opens Choose version, so a new release shows up soon.
pub(crate) const CHOOSER_FRESH: i64 = 15 * 60;

/// Looks up an app's releases. Called from worker threads.
pub trait ReleaseSource: Send + Sync {
    /// `query`'s app's release history, newest first, with a saved copy standing in when it is
    /// less than `max_age` seconds old (0: ask now).
    fn history_within(&self, query: &ReleaseQuery, max_age: i64) -> Result<AppReleases, String>;

    /// The app's published releases, newest first, for choosing a version.
    fn versions_within(&self, query: &ReleaseQuery, max_age: i64) -> Result<Vec<Release>, String> {
        self.history_within(query, max_age).map(|history| history.releases)
    }

    /// [`versions_within`](Self::versions_within) with a list no older than [`VERSIONS_FRESH`].
    fn versions(&self, query: &ReleaseQuery) -> Result<Vec<Release>, String> {
        self.versions_within(query, VERSIONS_FRESH)
    }

    /// The release `track` follows (the chosen nightly or exact version) in a history no older than
    /// `max_age` seconds, with no fallback to another.
    fn chosen_within(&self, query: &ReleaseQuery, track: &VersionTrack, max_age: i64) -> LatestRelease {
        match self.versions_within(query, max_age) {
            Ok(list) => release::choose_version_for(&list, track, query.separate_nightly).map_or(LatestRelease::None, LatestRelease::Found),
            Err(why) => LatestRelease::Failed(why),
        }
    }

    /// [`chosen_within`](Self::chosen_within) with a history no older than [`VERSIONS_FRESH`].
    fn chosen(&self, query: &ReleaseQuery, track: &VersionTrack) -> LatestRelease {
        self.chosen_within(query, track, VERSIONS_FRESH)
    }
}

/// Why an app's releases couldn't be read, as the launcher shows it.
pub(crate) const UNREACHABLE: &str = "ArtCraft's release service couldn't be reached. Try again later.";
const NOT_LISTED: &str = "ArtCraft's release service has no releases for this app.";

/// Our release API, next to the release feed at `feed_url`.
pub struct ApiSource {
    http: Http,
    feed_url: String,
    /// Each app's history (by app id and repo) and when it was fetched (Unix seconds).
    saved: Mutex<HashMap<(String, String), (i64, AppReleases)>>,
}

impl ApiSource {
    pub fn new(http: Http, feed_url: String) -> Self {
        Self { http, feed_url, saved: Mutex::new(HashMap::new()) }
    }
}

impl ReleaseSource for ApiSource {
    fn history_within(&self, query: &ReleaseQuery, max_age: i64) -> Result<AppReleases, String> {
        let key = (query.app.clone(), query.repo.clone());
        let now = crate::now();
        if let Some((at, history)) = self.saved.lock().unwrap_or_else(PoisonError::into_inner).get(&key)
            && max_age > 0
            && (0..max_age).contains(&now.saturating_sub(*at))
        {
            return Ok(history.clone());
        }
        let url = feed::app_releases_url(&self.feed_url, &query.app).ok_or(NOT_LISTED)?;
        let json = self.http.get_feed(&url).map_err(|e| {
            log::warn!("release history of {}: {}", query.app, e.why);
            UNREACHABLE.to_string()
        })?;
        let history = AppReleases::parse(&json, &query.repo, query.former_repo.as_deref(), now).ok_or(NOT_LISTED)?;
        self.saved.lock().unwrap_or_else(PoisonError::into_inner).insert(key, (now, history.clone()));
        Ok(history)
    }
}

/// `query`'s app's release history, asked of our API next to `feed_url` now, with no saved copy:
/// for the Developer Builds copy, which keeps no [`ApiSource`].
pub(crate) fn fetch_history(http: &Http, feed_url: &str, query: &ReleaseQuery) -> Result<AppReleases, String> {
    ApiSource::new(http.clone(), feed_url.to_string()).history_within(query, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source answering from `releases`.
    struct Fixed(Vec<Release>);
    impl ReleaseSource for Fixed {
        fn history_within(&self, _query: &ReleaseQuery, _max_age: i64) -> Result<AppReleases, String> {
            Ok(AppReleases { complete: true, releases: self.0.clone() })
        }
    }

    fn release(tag: &str) -> Release {
        Release { tag: tag.into(), version: artcraft_launcher_core::version::parse(tag), prerelease: tag.contains('-'), ..Default::default() }
    }

    fn query() -> ReleaseQuery {
        ReleaseQuery::for_app(artcraft_launcher_core::CraftApp::by_id("filmcraft").unwrap(), "storytold/filmcraft")
    }

    #[test]
    fn a_pinned_version_is_chosen_from_the_history_with_no_fallback() {
        let source = Fixed(vec![release("v0.6.0"), release("v0.5.0")]);
        let pinned = |tag: &str| source.chosen(&query(), &VersionTrack::Pinned(tag.into()));
        assert!(matches!(pinned("v0.5.0"), LatestRelease::Found(r) if r.tag == "v0.5.0"));
        assert_eq!(pinned("v0.4.0"), LatestRelease::None, "not published: no other version stands in");
    }

    #[test]
    fn a_feed_elsewhere_has_no_histories_and_is_never_asked_about_one() {
        let source = ApiSource::new(Http::new().unwrap(), "http://127.0.0.1:9/release_info.json".into());
        assert_eq!(source.history_within(&query(), 0).unwrap_err(), NOT_LISTED, "a local mirror keeps no histories");
    }
}
