//! Where release information comes from: our release API first, then GitHub.
//!
//! [`ApiSource`] reads one app's release history from `GET /v1/craft_apps/{app}/releases`
//! ([`AppReleases`]: every published release with its files and checksums, nightlies included),
//! keeps it in memory for a while, and answers the version list, a pinned version and the Nightly
//! track from it. The latest stable release and beta of every app come from the release feed
//! itself (`backend::latest_for`), and from [`ReleaseSource::latest_fallback`] when the feed has
//! no answer.
//!
//! When our API can't answer (it can't be reached, times out, answers an error, sends something
//! unreadable, or has nothing for the app), the same history is read from GitHub instead, in this
//! order:
//!
//! 1. GitHub's REST API (`/repos/{repo}/releases`, one page, no token), within a small hourly
//!    allowance of our own and never while GitHub has asked to be left alone (403 or 429).
//! 2. github.com's `releases.atom`, which costs no API quota, with each release built from its
//!    `SHA256SUMS.txt`: only the newest releases, so the history is incomplete.
//! 3. The last history any of them gave in this session, however old, marked as stale so Choose
//!    version can say the list may be out of date.
//!
//! Every download is still checked against the release's checksums before it is run, whichever
//! source named it. A local mirror never falls back to GitHub.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use artcraft_launcher_core::feed::{self, AppReleases, MAX_AGE};
use artcraft_launcher_core::protocol::{LatestRelease, VersionTrack};
use artcraft_launcher_core::release::{self, Release, ReleaseQuery};
use artcraft_launcher_core::version;

use crate::http::{FeedError, GitHubApiError, Http};

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

    /// Choose version's list, and whether it is a saved one that no source could confirm just
    /// now (stale), so the dialog says it may be out of date and offers to try again.
    fn picker_versions(&self, query: &ReleaseQuery, max_age: i64) -> Result<(Vec<Release>, bool), String> {
        self.versions_within(query, max_age).map(|list| (list, false))
    }

    /// The latest release (the higher of the stable release and beta with `prereleases`) from the
    /// app's history, for when the release feed has no answer for the app. `None` when the
    /// history couldn't be read either. Nightlies are never part of it.
    fn latest_fallback(&self, _query: &ReleaseQuery, _prereleases: bool) -> Option<LatestRelease> {
        None
    }

    /// The release `track` follows (the chosen nightly or exact version) in a history no older than
    /// `max_age` seconds, with no fallback to another. Not found in an incomplete history is not
    /// "none": it may be older than the history reaches.
    fn chosen_within(&self, query: &ReleaseQuery, track: &VersionTrack, max_age: i64) -> LatestRelease {
        match self.history_within(query, max_age) {
            Ok(history) => match release::choose_version_for(&history.releases, track, query.separate_nightly) {
                Some(found) => LatestRelease::Found(found),
                None if history.complete => LatestRelease::None,
                None => LatestRelease::Failed(BEYOND_HISTORY.into()),
            },
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
const BEYOND_HISTORY: &str = "This version isn't among the releases that could be checked for this app.";
/// Shown when our release API and both GitHub routes failed and nothing was saved.
const NOWHERE: &str = "Release information couldn't be checked: neither ArtCraft's release service nor GitHub answered. Try again later.";

/// How long our release API, or github.com, is left alone after it couldn't be reached or answered
/// a server error, so an outage costs each check one timeout, not one per app.
const QUIET_FOR: i64 = 60;
/// The most requests to GitHub's REST API in an hour. Without a token GitHub allows 60 an hour for
/// a whole network address, which other apps and machines share.
const GITHUB_PER_HOUR: u32 = 20;
/// GitHub's REST API answers at most this many releases on one page.
const GITHUB_PAGE: usize = 100;
/// The most releases built from github.com's release feed, each from its own `SHA256SUMS.txt`.
const PAGE_RELEASES: usize = 10;
/// Releases whose `SHA256SUMS.txt` is asked for at once.
const SUMS_AT_ONCE: usize = 4;

/// Which source an answer came from, for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Served {
    /// Our release API, just now.
    Api,
    /// A history read less than the asked age ago.
    Saved,
    /// GitHub's REST API.
    GitHubApi,
    /// github.com's release feed and checksums.
    GitHubPages,
    /// The last history read, older than asked for, because no source answered.
    Stale,
}

impl Served {
    fn label(self) -> &'static str {
        match self {
            Served::Api => "ArtCraft's release service",
            Served::Saved => "the saved history",
            Served::GitHubApi => "GitHub's API",
            Served::GitHubPages => "github.com's release feed",
            Served::Stale => "the last saved history (stale)",
        }
    }
}

/// The network as the release sources use it: [`Http`] in the launcher, a fake in tests.
pub(crate) trait Net: Send + Sync {
    /// Our release API's history for one app.
    fn history(&self, url: &str) -> Result<String, FeedError>;
    /// GitHub's REST API.
    fn github_api(&self, url: &str) -> Result<String, GitHubApiError>;
    /// A small text file on github.com (`Ok(None)` for a 404).
    fn text(&self, url: &str) -> Result<Option<String>, String>;
}

impl Net for Http {
    fn history(&self, url: &str) -> Result<String, FeedError> {
        self.get_history(url)
    }
    fn github_api(&self, url: &str) -> Result<String, GitHubApiError> {
        self.get_github_api(url)
    }
    fn text(&self, url: &str) -> Result<Option<String>, String> {
        self.get_text(url)
    }
}

/// When each route may be asked again, and how much of GitHub's allowance this hour has used.
#[derive(Default)]
struct Gate {
    api_quiet_until: Option<i64>,
    github_limited_until: Option<i64>,
    pages_quiet_until: Option<i64>,
    /// The start of the current hour of GitHub API requests (Unix seconds), and how many it has had.
    github_hour: (i64, u32),
}

impl Gate {
    fn held(until: Option<i64>, now: i64) -> bool {
        until.is_some_and(|u| u > now)
    }

    /// Whether a GitHub API request may go out now; counts it when it may.
    fn spend_github(&mut self, now: i64) -> bool {
        if Self::held(self.github_limited_until, now) {
            return false;
        }
        if now.saturating_sub(self.github_hour.0) >= 3600 {
            self.github_hour = (now, 0);
        }
        if self.github_hour.1 >= GITHUB_PER_HOUR {
            return false;
        }
        self.github_hour.1 += 1;
        true
    }
}

/// Our release API, next to the release feed at `feed_url`, with GitHub behind it.
pub struct ApiSource {
    net: Box<dyn Net>,
    feed_url: String,
    /// Whether GitHub is asked when our API can't answer (not for a local mirror).
    github: bool,
    /// Each app's history (by app id and repo), when it was read (Unix seconds), and from where.
    saved: Mutex<HashMap<(String, String), (i64, AppReleases, Served)>>,
    gate: Mutex<Gate>,
}

impl ApiSource {
    pub fn new(http: Http, feed_url: String) -> Self {
        Self::with_net(Box::new(http), feed_url)
    }

    pub(crate) fn with_net(net: Box<dyn Net>, feed_url: String) -> Self {
        Self { net, feed_url, github: true, saved: Mutex::new(HashMap::new()), gate: Mutex::new(Gate::default()) }
    }

    /// The same source without the GitHub fallback: a local mirror's releases come only from it.
    pub fn without_github(self) -> Self {
        Self { github: false, ..self }
    }

    /// The history and where it came from: see the module's fallback order.
    pub(crate) fn lookup(&self, query: &ReleaseQuery, max_age: i64) -> Result<(AppReleases, Served), String> {
        let key = (query.app.clone(), query.repo.clone());
        let now = crate::now();
        // Kept while it is younger than `max_age` here and its source's own fetch is still fresh.
        if let Some((at, history, _)) = self.saved.lock().unwrap_or_else(PoisonError::into_inner).get(&key)
            && max_age > 0
            && (0..max_age).contains(&now.saturating_sub(*at))
            && history.fetched_at.is_some_and(|fetched| now.saturating_sub(fetched) <= MAX_AGE)
        {
            return Ok((history.clone(), Served::Saved));
        }
        // A feed elsewhere (a local mirror) keeps no histories, and nothing else is asked for it.
        let Some(url) = feed::app_releases_url(&self.feed_url, &query.app) else {
            return Err(NOT_LISTED.into());
        };
        let api = self.ask_api(&url, query, now);
        let answer = match api {
            Ok(history) => Ok((history, Served::Api)),
            Err(why) if self.github => {
                log::warn!("release history of {} from ArtCraft's release service: {why}; asking GitHub", query.app);
                self.ask_github(query, now).map_err(|github| {
                    log::warn!("release history of {} from GitHub: {github}", query.app);
                    NOWHERE.to_string()
                })
            }
            Err(why) => Err(why),
        };
        match answer {
            Ok((history, served)) => {
                log::info!("release history of {}: {} releases from {}", query.app, history.releases.len(), served.label());
                self.saved.lock().unwrap_or_else(PoisonError::into_inner).insert(key, (now, history.clone(), served));
                Ok((history, served))
            }
            Err(why) => match self.saved.lock().unwrap_or_else(PoisonError::into_inner).get(&key) {
                Some((at, history, _)) => {
                    log::info!("release history of {}: no source answered; showing the one read {}s ago", query.app, now.saturating_sub(*at));
                    Ok((history.clone(), Served::Stale))
                }
                None => Err(why),
            },
        }
    }

    /// Our release API's history for `query`'s app. Any failure is the caller's cue to ask
    /// GitHub: no answer, an error status, an unreadable or stale answer, or no releases at all.
    fn ask_api(&self, url: &str, query: &ReleaseQuery, now: i64) -> Result<AppReleases, String> {
        if Gate::held(self.gate.lock().unwrap_or_else(PoisonError::into_inner).api_quiet_until, now) {
            return Err(UNREACHABLE.into());
        }
        let json = self.net.history(url).map_err(|e| {
            if crate::http::is_unreachable(&e.why) || e.throttled || e.why.contains(" answered HTTP 5") {
                let until = e.retry_after.map_or(now + QUIET_FOR, |secs| now.saturating_add(secs.clamp(QUIET_FOR, 3600)));
                self.gate.lock().unwrap_or_else(PoisonError::into_inner).api_quiet_until = Some(until);
            }
            format!("{UNREACHABLE} ({})", e.why)
        })?;
        let history = AppReleases::parse(&json, &query.repo, query.former_repo.as_deref(), now).ok_or(NOT_LISTED)?;
        if history.releases.is_empty() {
            return Err(NOT_LISTED.into());
        }
        Ok(history)
    }

    /// The history from GitHub: its REST API, then github.com's release feed.
    fn ask_github(&self, query: &ReleaseQuery, now: i64) -> Result<(AppReleases, Served), String> {
        let repo = query.repo.as_str();
        let mut why = String::from("GitHub's API is holding requests back");
        if self.gate.lock().unwrap_or_else(PoisonError::into_inner).spend_github(now) {
            match self.net.github_api(&format!("https://api.github.com/repos/{repo}/releases?per_page={GITHUB_PAGE}")) {
                Ok(json) => match github_history(&json, repo, now) {
                    Ok(history) => return Ok((history, Served::GitHubApi)),
                    Err(e) => why = e,
                },
                Err(e) => {
                    if let Some(until) = e.limited_until {
                        self.gate.lock().unwrap_or_else(PoisonError::into_inner).github_limited_until = Some(until);
                    }
                    why = e.why;
                }
            }
        }
        log::info!("{repo}: GitHub's API gave no release list ({why}); reading github.com's release feed");
        if Gate::held(self.gate.lock().unwrap_or_else(PoisonError::into_inner).pages_quiet_until, now) {
            return Err(format!("{why}; github.com couldn't be reached a moment ago"));
        }
        let quiet = |e: String| {
            self.gate.lock().unwrap_or_else(PoisonError::into_inner).pages_quiet_until = Some(now + QUIET_FOR);
            e
        };
        let Some(atom) = self.net.text(&format!("https://github.com/{repo}/releases.atom")).map_err(quiet)? else {
            return Err(format!("{why}; github.com has no release feed for {repo}"));
        };
        let named = !release::tags_from_atom(&atom, repo).is_empty();
        let releases = releases_from_atom(repo, Some(&atom), |url| self.net.text(url)).map_err(quiet)?;
        if named && releases.is_empty() {
            return Err(format!("{why}; no release in github.com's feed for {repo} has checksums"));
        }
        // An empty feed is a repo with no releases yet; a short one is the newest releases only.
        Ok((AppReleases { complete: !named, releases, fetched_at: Some(now) }, Served::GitHubPages))
    }
}

/// A history from GitHub's REST API release list. One page that isn't full is every release.
fn github_history(json: &str, repo: &str, now: i64) -> Result<AppReleases, String> {
    let sent =
        serde_json::from_str::<serde_json::Value>(json).ok().and_then(|v| v.as_array().map(Vec::len)).ok_or("unreadable release list from GitHub")?;
    let releases = release::parse_github_versions(json, repo)?;
    Ok(AppReleases { complete: sent < GITHUB_PAGE, releases, fetched_at: Some(now) })
}

/// The newest releases `atom` (github.com's `releases.atom` for `repo`, `None` when there is none)
/// names, each built from its `SHA256SUMS.txt` fetched with `get_text`. A release without one is
/// left out, since its downloads couldn't be checked.
fn releases_from_atom(
    repo: &str,
    atom: Option<&str>,
    get_text: impl Fn(&str) -> Result<Option<String>, String> + Sync,
) -> Result<Vec<Release>, String> {
    let tags: Vec<String> = atom.map(|atom| release::tags_from_atom(atom, repo)).unwrap_or_default().into_iter().take(PAGE_RELEASES).collect();
    let build = |tag: &String| -> Result<Option<Release>, String> {
        let Some(text) = get_text(&format!("{}{tag}/SHA256SUMS.txt", release::download_prefix(repo)))? else { return Ok(None) };
        let sums = release::parse_sha256sums(&text);
        Ok((!sums.is_empty()).then(|| release::release_from_sums(repo, tag, &sums)))
    };
    let mut built = Vec::with_capacity(tags.len());
    for chunk in tags.chunks(SUMS_AT_ONCE) {
        let results: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = chunk.iter().map(|tag| scope.spawn(|| build(tag))).collect();
            handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("a release's checksums couldn't be read".into()))).collect()
        });
        built.extend(results);
    }
    let mut first_error = None;
    let mut list = Vec::new();
    for result in built {
        match result {
            Ok(Some(r)) => list.push(r),
            Ok(None) => {}
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    match first_error {
        Some(e) if list.is_empty() => Err(e),
        _ => Ok(list),
    }
}

/// The newest release an ordinary update may offer: stable, or any beta with `prereleases`, and
/// never a nightly, for any app.
fn newest_ordinary(releases: &[Release], prereleases: bool) -> Option<Release> {
    releases
        .iter()
        .filter(|r| release::ordinary_release_allowed(r) && !release::is_nightly(r))
        .filter(|r| r.version.as_ref().is_some_and(|v| prereleases || (!r.prerelease && version::is_stable(v))))
        .max_by(|a, b| a.version.cmp(&b.version))
        .cloned()
}

impl ReleaseSource for ApiSource {
    fn history_within(&self, query: &ReleaseQuery, max_age: i64) -> Result<AppReleases, String> {
        self.lookup(query, max_age).map(|(history, _)| history)
    }

    fn picker_versions(&self, query: &ReleaseQuery, max_age: i64) -> Result<(Vec<Release>, bool), String> {
        self.lookup(query, max_age).map(|(history, served)| (history.releases, served == Served::Stale))
    }

    fn latest_fallback(&self, query: &ReleaseQuery, prereleases: bool) -> Option<LatestRelease> {
        let (history, served) = self.lookup(query, VERSIONS_FRESH).ok()?;
        match newest_ordinary(&history.releases, prereleases) {
            Some(found) => Some(LatestRelease::Found(found)),
            None if history.complete && served != Served::Stale => Some(LatestRelease::None),
            None => None,
        }
    }
}

/// `query`'s app's release history, asked of our API next to `feed_url`, with no saved copy, and
/// how many bytes it was: for Developer Builds discovery, which keeps no [`ApiSource`] and counts
/// them against its byte budget. A server's `Retry-After` (or a 429 without one), counted from when
/// the answer arrived, becomes the scheduler's backoff. Developer Builds don't fall back to GitHub:
/// their candidates are verified against their signed descriptors, and a lookup that can't answer
/// is retried by the scheduler.
pub(crate) fn fetch_history(http: &Http, feed_url: &str, query: &ReleaseQuery) -> Result<(AppReleases, usize), crate::http::NightlyHttpError> {
    let error = |why: &str, retry_at: Option<i64>| crate::http::NightlyHttpError { over_budget: false, why: why.into(), retry_at };
    let url = feed::app_releases_url(feed_url, &query.app).ok_or_else(|| error(NOT_LISTED, None))?;
    let json = http.get_history(&url).map_err(|e| {
        log::warn!("release history of {}: {}", query.app, e.why);
        let received = crate::now();
        let retry_at = e
            .retry_after
            .map(|secs| received.saturating_add(secs.clamp(0, 24 * 60 * 60)))
            .or_else(|| e.throttled.then(|| received.saturating_add(60)));
        error(UNREACHABLE, retry_at)
    })?;
    let history = AppReleases::parse(&json, &query.repo, query.former_repo.as_deref(), crate::now()).ok_or_else(|| error(NOT_LISTED, None))?;
    Ok((history, json.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source answering from `releases`, a complete history or not.
    struct Fixed(Vec<Release>, bool);
    impl ReleaseSource for Fixed {
        fn history_within(&self, _query: &ReleaseQuery, _max_age: i64) -> Result<AppReleases, String> {
            Ok(AppReleases { complete: self.1, releases: self.0.clone(), fetched_at: None })
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
        let source = Fixed(vec![release("v0.6.0"), release("v0.5.0")], true);
        let pinned = |tag: &str| source.chosen(&query(), &VersionTrack::Pinned(tag.into()));
        assert!(matches!(pinned("v0.5.0"), LatestRelease::Found(r) if r.tag == "v0.5.0"));
        assert_eq!(pinned("v0.4.0"), LatestRelease::None, "not published: no other version stands in");
        // In a history that doesn't reach back to it, a version isn't known to be gone.
        let partial = Fixed(vec![release("v0.6.0")], false);
        assert!(matches!(partial.chosen(&query(), &VersionTrack::Pinned("v0.5.0".into())), LatestRelease::Failed(_)));
    }

    /// A scripted network: each route answers from its own queue (the last answer repeats), and
    /// every request is recorded. Nothing reaches a real host.
    #[derive(Default)]
    struct FakeNet {
        api: Mutex<Vec<Result<String, FeedError>>>,
        github: Mutex<Vec<Result<String, GitHubApiError>>>,
        pages: Mutex<HashMap<String, Result<Option<String>, String>>>,
        asked: Mutex<Vec<String>>,
    }

    fn next<T: Clone>(queue: &Mutex<Vec<T>>) -> T {
        let mut q = queue.lock().unwrap();
        if q.len() > 1 { q.remove(0) } else { q[0].clone() }
    }

    impl Net for std::sync::Arc<FakeNet> {
        fn history(&self, url: &str) -> Result<String, FeedError> {
            self.asked.lock().unwrap().push(url.into());
            next(&self.api)
        }
        fn github_api(&self, url: &str) -> Result<String, GitHubApiError> {
            self.asked.lock().unwrap().push(url.into());
            next(&self.github)
        }
        fn text(&self, url: &str) -> Result<Option<String>, String> {
            self.asked.lock().unwrap().push(url.into());
            self.pages.lock().unwrap().get(url).cloned().unwrap_or(Ok(None))
        }
    }

    const FEED: &str = "https://api.storyteller.ai/v1/craft_apps/release_info";
    const REPO: &str = "storytold/filmcraft";

    fn api_error(why: &str) -> FeedError {
        FeedError { why: why.into(), throttled: false, retry_after: None }
    }

    /// Our API's history of `tags`, fetched at `fetched` (RFC 3339).
    fn api_history(tags: &[&str], fetched: &str) -> String {
        let releases: Vec<String> = tags
            .iter()
            .map(|t| {
                format!(
                    r#"{{"tag":"{t}","name":"{t}","prerelease":{},"assets":[{{"name":"filmcraft.zip","size_bytes":3,"download_url":"https://github.com/{REPO}/releases/download/{t}/filmcraft.zip","sha256":"{}"}}]}}"#,
                    t.contains('-'),
                    "a".repeat(64)
                )
            })
            .collect();
        format!(r#"{{"repo":"{REPO}","app_last_fetched":"{fetched}","complete":true,"releases":[{}]}}"#, releases.join(","))
    }

    /// GitHub's REST API release list of `tags`.
    fn github_list(tags: &[&str]) -> String {
        let items: Vec<String> = tags
            .iter()
            .map(|t| format!(r#"{{"tag_name":"{t}","prerelease":{},"draft":false,"assets":[{{"name":"filmcraft.zip","size":3,"browser_download_url":"https://github.com/{REPO}/releases/download/{t}/filmcraft.zip"}}]}}"#, t.contains('-')))
            .collect();
        format!("[{}]", items.join(","))
    }

    fn atom(tags: &[&str]) -> String {
        let entries: String = tags
            .iter()
            .map(|t| format!(r#"<entry><link rel="alternate" type="text/html" href="https://github.com/{REPO}/releases/tag/{t}"/></entry>"#))
            .collect();
        format!("<feed>{entries}</feed>")
    }

    fn sums_url(tag: &str) -> String {
        format!("https://github.com/{REPO}/releases/download/{tag}/SHA256SUMS.txt")
    }

    fn source(net: &std::sync::Arc<FakeNet>) -> ApiSource {
        ApiSource::with_net(Box::new(net.clone()), FEED.into())
    }

    fn tags(list: &[Release]) -> Vec<&str> {
        list.iter().map(|r| r.tag.as_str()).collect()
    }

    fn asked_github_api(net: &FakeNet) -> usize {
        net.asked.lock().unwrap().iter().filter(|u| u.starts_with("https://api.github.com/")).count()
    }

    fn asked_api(net: &FakeNet) -> usize {
        net.asked.lock().unwrap().iter().filter(|u| u.starts_with("https://api.storyteller.ai/")).count()
    }

    const FRESH: &str = "2999-01-01T00:00:00Z";

    #[test]
    fn our_api_answers_first_and_github_is_never_asked() {
        let net = std::sync::Arc::new(FakeNet { api: Mutex::new(vec![Ok(api_history(&["v0.6.0", "v0.5.0"], FRESH))]), ..Default::default() });
        let (history, served) = source(&net).lookup(&query(), 0).unwrap();
        assert_eq!((tags(&history.releases), served), (vec!["v0.6.0", "v0.5.0"], Served::Api));
        assert_eq!(asked_github_api(&net), 0);
    }

    /// Every way our API can fail to answer for an app sends the lookup to GitHub's API.
    #[test]
    fn any_failure_of_our_api_falls_back_to_githubs_api() {
        let failures: Vec<(&str, Result<String, FeedError>)> = vec![
            ("unreachable", Err(api_error("couldn't reach api.storyteller.ai (timed out)"))),
            ("server error", Err(api_error("api.storyteller.ai answered HTTP 503"))),
            ("not found for the app", Err(api_error("api.storyteller.ai answered HTTP 404"))),
            ("malformed JSON", Ok("{not json".into())),
            ("no release list", Ok(format!(r#"{{"repo":"{REPO}","app_last_fetched":"{FRESH}","complete":true}}"#))),
            ("an empty list", Ok(api_history(&[], FRESH))),
            ("another repo's history", Ok(api_history(&["v0.6.0"], FRESH).replace(REPO, "someone/else"))),
            ("a history the server stopped refreshing", Ok(api_history(&["v0.5.0"], "2000-01-01T00:00:00Z"))),
        ];
        for (what, failure) in failures {
            let net = std::sync::Arc::new(FakeNet {
                api: Mutex::new(vec![failure]),
                github: Mutex::new(vec![Ok(github_list(&["v0.7.0", "v0.6.0"]))]),
                ..Default::default()
            });
            let (history, served) = source(&net).lookup(&query(), 0).unwrap_or_else(|e| panic!("{what}: {e}"));
            assert_eq!((tags(&history.releases), served), (vec!["v0.7.0", "v0.6.0"], Served::GitHubApi), "{what}");
            assert!(history.complete, "{what}: a page that isn't full is every release");
        }
    }

    /// An outage costs one timeout, not one per app: our API is left alone for a minute.
    #[test]
    fn an_unreachable_api_is_not_asked_again_for_every_app() {
        let net = std::sync::Arc::new(FakeNet {
            api: Mutex::new(vec![Err(api_error("couldn't reach api.storyteller.ai (connection refused)"))]),
            github: Mutex::new(vec![Ok(github_list(&["v0.6.0"]))]),
            ..Default::default()
        });
        let source = source(&net);
        source.lookup(&query(), 0).unwrap();
        let word = ReleaseQuery::for_app(artcraft_launcher_core::CraftApp::by_id("wordcraft").unwrap(), "storytold/wordcraft");
        source.lookup(&word, 0).unwrap();
        assert_eq!(asked_api(&net), 1);
        assert_eq!(asked_github_api(&net), 2);
    }

    /// When GitHub's API refuses (rate limit), github.com's release feed answers, and the API is
    /// left alone until the time GitHub named.
    #[test]
    fn a_github_rate_limit_falls_back_to_the_release_feed_and_holds_the_api() {
        let net = std::sync::Arc::new(FakeNet {
            api: Mutex::new(vec![Err(api_error("api.storyteller.ai answered HTTP 502"))]),
            github: Mutex::new(vec![Err(GitHubApiError {
                why: "api.github.com answered HTTP 403".into(),
                limited_until: Some(crate::now() + 1800),
            })]),
            ..Default::default()
        });
        let sums = format!("{}  filmcraft.zip", "b".repeat(64));
        {
            let mut pages = net.pages.lock().unwrap();
            pages.insert(format!("https://github.com/{REPO}/releases.atom"), Ok(Some(atom(&["v0.8.0-rc.1", "v0.7.1", "v0.7.0"]))));
            pages.insert(sums_url("v0.8.0-rc.1"), Ok(Some(sums.clone())));
            pages.insert(sums_url("v0.7.0"), Ok(Some(sums)));
        }
        let source = source(&net);
        let (history, served) = source.lookup(&query(), 0).unwrap();
        assert_eq!(served, Served::GitHubPages);
        assert_eq!(tags(&history.releases), ["v0.8.0-rc.1", "v0.7.0"], "v0.7.1 has no checksums, so it is left out");
        assert!(history.releases[0].prerelease && !history.releases[1].prerelease);
        assert!(history.releases.iter().all(|r| r.assets.iter().any(|a| a.sha256.is_some())), "downloads carry their checksums");
        assert!(!history.complete, "the release feed names only the newest releases");
        source.lookup(&query(), 0).unwrap();
        assert_eq!(asked_github_api(&net), 1, "GitHub's API isn't asked again while it holds requests back");
    }

    /// Our own hourly allowance keeps GitHub's API for the requests that need it most.
    #[test]
    fn githubs_api_is_asked_at_most_the_hourly_allowance() {
        let net = std::sync::Arc::new(FakeNet {
            api: Mutex::new(vec![Err(api_error("api.storyteller.ai answered HTTP 500"))]),
            github: Mutex::new(vec![Ok(github_list(&["v0.6.0"]))]),
            ..Default::default()
        });
        net.pages.lock().unwrap().insert(format!("https://github.com/{REPO}/releases.atom"), Ok(Some(atom(&[]))));
        let source = source(&net);
        for _ in 0..GITHUB_PER_HOUR + 5 {
            // Each lookup asks again (max age 0), and our API stays quiet after its first failure.
            source.lookup(&query(), 0).unwrap();
        }
        assert_eq!(asked_github_api(&net), GITHUB_PER_HOUR as usize);
    }

    /// When nothing answers, the last history read stands in however old, marked stale; with
    /// nothing saved, the user is told that no source answered.
    #[test]
    fn with_every_source_down_the_last_history_stands_in_marked_stale() {
        let net = std::sync::Arc::new(FakeNet {
            api: Mutex::new(vec![Ok(api_history(&["v0.6.0"], FRESH)), Err(api_error("couldn't reach api.storyteller.ai (timed out)"))]),
            github: Mutex::new(vec![Err(GitHubApiError { why: "couldn't reach api.github.com".into(), limited_until: None })]),
            ..Default::default()
        });
        net.pages.lock().unwrap().insert(format!("https://github.com/{REPO}/releases.atom"), Err("couldn't reach github.com".into()));
        let source = source(&net);
        let (list, stale) = source.picker_versions(&query(), 0).unwrap();
        assert_eq!((tags(&list), stale), (vec!["v0.6.0"], false));
        let (list, stale) = source.picker_versions(&query(), 0).unwrap();
        assert_eq!((tags(&list), stale), (vec!["v0.6.0"], true));
        let nothing = ApiSource::with_net(Box::new(net.clone()), FEED.into());
        assert_eq!(nothing.picker_versions(&query(), 0).unwrap_err(), NOWHERE);
    }

    /// The latest release from a history: the newest stable one, a beta with pre-releases on, and
    /// never a nightly; an app GitHub lists no release for has none.
    #[test]
    fn the_latest_release_from_the_fallback_is_never_a_nightly() {
        let net = std::sync::Arc::new(FakeNet {
            api: Mutex::new(vec![Err(api_error("api.storyteller.ai answered HTTP 404"))]),
            github: Mutex::new(vec![Ok(github_list(&["v0.9.0-nightly.20261126.1", "v0.8.0-rc.1", "v0.7.0", "v0.6.0"]))]),
            ..Default::default()
        });
        let source = source(&net);
        let latest = |pre| match source.latest_fallback(&query(), pre) {
            Some(LatestRelease::Found(r)) => r.tag,
            other => panic!("{other:?}"),
        };
        assert_eq!(latest(false), "v0.7.0");
        assert_eq!(latest(true), "v0.8.0-rc.1");
        let empty = std::sync::Arc::new(FakeNet {
            api: Mutex::new(vec![Err(api_error("api.storyteller.ai answered HTTP 404"))]),
            github: Mutex::new(vec![Ok("[]".into())]),
            ..Default::default()
        });
        assert_eq!(self::source(&empty).latest_fallback(&query(), false), Some(LatestRelease::None));
        let down = std::sync::Arc::new(FakeNet {
            api: Mutex::new(vec![Err(api_error("couldn't reach api.storyteller.ai"))]),
            github: Mutex::new(vec![Err(GitHubApiError { why: "couldn't reach api.github.com".into(), limited_until: None })]),
            ..Default::default()
        });
        assert_eq!(self::source(&down).latest_fallback(&query(), false), None, "the feed's own reason is kept");
    }

    /// A local mirror's releases come only from it: GitHub is never asked.
    #[test]
    fn without_the_github_fallback_github_is_never_asked() {
        let net = std::sync::Arc::new(FakeNet {
            api: Mutex::new(vec![Err(api_error("couldn't reach api.storyteller.ai"))]),
            github: Mutex::new(vec![Ok(github_list(&["v0.6.0"]))]),
            ..Default::default()
        });
        assert!(source(&net).without_github().lookup(&query(), 0).is_err());
        assert!(net.asked.lock().unwrap().iter().all(|u| u.starts_with("https://api.storyteller.ai/")));
    }

    #[test]
    fn a_feed_elsewhere_has_no_histories_and_is_never_asked_about_one() {
        let source = ApiSource::new(Http::new().unwrap(), "http://127.0.0.1:9/release_info.json".into());
        assert_eq!(source.history_within(&query(), 0).unwrap_err(), NOT_LISTED, "a local mirror keeps no histories");
    }
}
