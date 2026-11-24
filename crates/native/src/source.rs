//! Where release information comes from.
//!
//! [`GitHubSource`] asks the GitHub REST API (one request per app, revalidated with ETags) and,
//! whenever that produces no answer, falls back to github.com's release pages: the
//! `releases/latest` redirect names the tag and `SHA256SUMS.txt` lists every file with its
//! checksum, which costs no API quota at all. A later [`ReleaseSource`] will ask the ArtCraft
//! catalog API for every app in one request.
//!
//! The fallback is not only for the API's quota. `api.github.com` is a different host from
//! `github.com`, and on a managed machine it is often the one that doesn't answer: a firewall, a
//! filtering DNS resolver or an HTTPS-inspecting proxy blocks the API (no connection at all, or a
//! 403/407 with none of GitHub's rate-limit headers) while the release pages load normally. Taking
//! that route only when GitHub itself said "out of quota" left every app the user hadn't installed
//! showing "Offline" on such a machine, which is what `resolve` now avoids.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use artcraft_launcher_core::protocol::{LatestRelease, Source};
use artcraft_launcher_core::release::{self, Release, ReleaseQuery};
use serde::{Deserialize, Serialize};

use crate::http::{Fetched, Http};

/// One answer from a source.
#[derive(Clone, Debug)]
pub struct Answer {
    pub latest: LatestRelease,
    pub source: Source,
}

/// Looks up latest releases. Called from worker threads.
pub trait ReleaseSource: Send + Sync {
    fn latest(&self, query: &ReleaseQuery, prereleases: bool) -> Answer;
    /// `repo`'s published releases, newest first, for choosing a version.
    fn versions(&self, _query: &ReleaseQuery) -> Result<Vec<Release>, String> {
        Err("version list isn't available here".into())
    }
    /// The release `track` follows: the latest stable one, or the chosen nightly or exact version
    /// from [`versions`](Self::versions), with no fallback to another.
    fn chosen(&self, query: &ReleaseQuery, track: &artcraft_launcher_core::protocol::VersionTrack) -> Answer {
        if *track == artcraft_launcher_core::protocol::VersionTrack::Stable {
            return self.latest(query, false);
        }
        let latest = match self.versions(query) {
            Ok(list) => release::choose_version_for(&list, track, query.separate_nightly).map_or(LatestRelease::None, LatestRelease::Found),
            Err(why) => LatestRelease::Failed(why),
        };
        Answer { latest, source: Source::GitHubApi }
    }
    /// The last answer saved for the query's repo, for an instant first paint.
    fn cached(&self, query: &ReleaseQuery, prereleases: bool) -> Option<LatestRelease>;
    /// The tag of `repo`'s newest stable release, by a route that costs no API quota (GitHub's
    /// `releases/latest` redirect), or `None` when it can't be told. Quick: a check pressed by the
    /// user asks this for every app to see whether the feed has fallen behind.
    fn newest_tag(&self, repo: &str) -> Option<String> {
        let _ = repo;
        None
    }
    /// API refused until this time (Unix seconds).
    fn rate_limited_until(&self) -> Option<i64>;
    /// Persist anything worth keeping.
    fn save(&self);
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Entry {
    etag: Option<String>,
    /// `None`: the repo has no releases.
    release: Option<Release>,
    prereleases: bool,
    fetched_at: i64,
}

#[derive(Default, Serialize, Deserialize)]
struct CacheFile {
    entries: BTreeMap<String, Entry>,
}

/// Once github.com itself hasn't answered, the release-pages route is left alone for this long.
/// Without it a machine with no network at all would take that route for every app in the catalog
/// in turn, each waiting on the same unreachable host, before reporting what the first try already
/// settled. (A user-pressed check still probes github.com once per app through
/// [`ReleaseSource::newest_tag`], which answers a different question and has its own quick timeout.)
const PAGES_RETRY_AFTER: i64 = 60;

/// Releases per page of the ordinary pre-release lookup, as on main.
const LIST_PAGE: usize = 30;
/// Releases per page of the version list.
const VERSIONS_PAGE: usize = 100;
/// The most pages read past a page full of a separately channelled app's nightlies. Those apps read
/// [`VERSIONS_PAGE`] releases a page in both the lookup and the version list, so both reach as far.
const ORDINARY_PAGES: usize = 5;

/// The number of entries GitHub returned on one list page, drafts included.
fn page_len(text: &str) -> Result<usize, String> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("unreadable answer from GitHub ({e})"))?;
    value.as_array().map(Vec::len).ok_or_else(|| "unexpected answer from GitHub".into())
}

pub struct GitHubSource {
    http: Http,
    cache: Mutex<CacheFile>,
    versions_cache: Mutex<BTreeMap<String, (i64, Vec<Release>)>>,
    cache_path: std::path::PathBuf,
    limited_until: Mutex<Option<i64>>,
    /// github.com couldn't be reached; don't ask it again until this time (Unix seconds).
    pages_quiet_until: Mutex<Option<i64>>,
}

impl GitHubSource {
    pub fn new(http: Http, cache_path: std::path::PathBuf) -> Self {
        let cache = std::fs::read(&cache_path).ok().and_then(|b| serde_json::from_slice::<CacheFile>(&b).ok()).unwrap_or_default();
        Self {
            http,
            cache: Mutex::new(cache),
            versions_cache: Mutex::new(BTreeMap::new()),
            cache_path,
            limited_until: Mutex::new(None),
            pages_quiet_until: Mutex::new(None),
        }
    }

    fn entry(&self, query: &ReleaseQuery, prereleases: bool) -> Option<Entry> {
        let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        cache
            .entries
            .get(&query.repo)
            .filter(|entry| {
                entry.prereleases == prereleases
                    && entry.release.as_ref().is_none_or(|release| release::ordinary_release_allowed(query.separate_nightly, release))
            })
            .cloned()
    }

    fn store(&self, repo: &str, entry: Entry) {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner).entries.insert(repo.to_string(), entry);
    }

    fn limited(&self) -> bool {
        let until = *self.limited_until.lock().unwrap_or_else(PoisonError::into_inner);
        until.is_some_and(|u| u > crate::now())
    }

    fn ask_api(&self, query: &ReleaseQuery, prereleases: bool) -> Result<Option<Answer>, String> {
        self.ask_api_with(query, prereleases, |url, etag| self.http.get_api(url, etag))
    }

    fn ask_api_with(
        &self,
        query: &ReleaseQuery,
        prereleases: bool,
        mut fetch: impl FnMut(&str, Option<&str>) -> Result<Fetched, String>,
    ) -> Result<Option<Answer>, String> {
        let (repo, separate) = (query.repo.as_str(), query.separate_nightly);
        let per_page = if separate { VERSIONS_PAGE } else { LIST_PAGE };
        let url = if prereleases {
            format!("https://api.github.com/repos/{repo}/releases?per_page={per_page}")
        } else {
            format!("https://api.github.com/repos/{repo}/releases/latest")
        };
        let saved = self.entry(query, prereleases);
        let found =
            |release: Option<Release>| Answer { latest: release.map_or(LatestRelease::None, LatestRelease::Found), source: Source::GitHubApi };
        match fetch(&url, saved.as_ref().and_then(|e| e.etag.as_deref()))? {
            Fetched::Body { text, etag } => {
                let mut multi_page = false;
                let release = if prereleases {
                    let mut release = release::parse_github_release_list(&text, repo, true, separate)?;
                    // Nightlies of a separately channelled app are not ordinary releases, so a page
                    // full of them proves nothing: look further, and report an incomplete answer
                    // rather than caching "no release" when the bounded search ends early.
                    if release.is_none() && separate {
                        // Page 1's ETag does not cover later pages: a 304 on it could replay an
                        // answer whose page has since changed, so a multi-page answer keeps none.
                        multi_page = true;
                        let mut full = page_len(&text)? >= per_page;
                        let mut page = 2;
                        while release.is_none() && full {
                            if page > ORDINARY_PAGES {
                                return Err(format!(
                                    "Nightly builds fill the first {ORDINARY_PAGES} release pages; the newest ordinary release is unknown and nothing was cached."
                                ));
                            }
                            match fetch(&format!("{url}&page={page}"), None)? {
                                Fetched::Body { text, .. } => {
                                    release = release::parse_github_release_list(&text, repo, true, separate)?;
                                    full = page_len(&text)? >= per_page;
                                }
                                Fetched::RateLimited { reset } => {
                                    *self.limited_until.lock().unwrap_or_else(PoisonError::into_inner) = Some(retry_at(reset, crate::now()));
                                    return Ok(None);
                                }
                                Fetched::NotModified | Fetched::NotFound => {
                                    return Err("GitHub's release list changed while it was read; the newest ordinary release is unknown.".into());
                                }
                            }
                            page += 1;
                        }
                    }
                    release
                } else {
                    Some(release::parse_github_release(&text, repo)?)
                };
                if release.as_ref().is_some_and(|release| !release::ordinary_release_allowed(separate, release)) {
                    return Err("This Nightly belongs to the separate Pre-Release channel, not the ordinary release cache.".into());
                }
                let etag = if multi_page { None } else { etag };
                self.store(repo, Entry { etag, release: release.clone(), prereleases, fetched_at: crate::now() });
                Ok(Some(found(release)))
            }
            Fetched::NotModified => match saved {
                Some(e) => {
                    self.store(repo, Entry { fetched_at: crate::now(), ..e.clone() });
                    Ok(Some(found(e.release)))
                }
                None => Err("GitHub said nothing changed, but there's no saved answer".into()),
            },
            Fetched::NotFound => {
                self.store(repo, Entry { etag: None, release: None, prereleases, fetched_at: crate::now() });
                Ok(Some(found(None)))
            }
            Fetched::RateLimited { reset } => {
                *self.limited_until.lock().unwrap_or_else(PoisonError::into_inner) = Some(retry_at(reset, crate::now()));
                Ok(None)
            }
        }
    }

    /// The version list. For an app whose nightlies have their own channel, pages full of them are
    /// read past (bounded) so the ordinary versions the picker offers cannot be crowded out.
    fn versions_with(
        &self,
        query: &ReleaseQuery,
        mut fetch: impl FnMut(&str, Option<&str>) -> Result<Fetched, String>,
    ) -> Result<Vec<Release>, String> {
        let repo = query.repo.as_str();
        let base = format!("https://api.github.com/repos/{repo}/releases?per_page={VERSIONS_PAGE}");
        let mut list = Vec::new();
        let mut page = 1;
        loop {
            let url = if page == 1 { base.clone() } else { format!("{base}&page={page}") };
            let full = match fetch(&url, None)? {
                Fetched::Body { text, .. } => {
                    list.extend(release::parse_github_versions(&text, repo)?);
                    page_len(&text)? >= VERSIONS_PAGE
                }
                Fetched::NotFound if page == 1 => false,
                Fetched::RateLimited { reset } => {
                    *self.limited_until.lock().unwrap_or_else(PoisonError::into_inner) = Some(retry_at(reset, crate::now()));
                    return Err("GitHub's request limit was reached. Try again later.".into());
                }
                Fetched::NotFound | Fetched::NotModified => return Err("GitHub returned no version list. Try again.".into()),
            };
            let crowded = query.separate_nightly && !list.iter().any(|r| release::ordinary_release_allowed(true, r));
            if !full || !crowded {
                return Ok(list);
            }
            if page >= ORDINARY_PAGES {
                return Err(format!("Nightly builds fill the first {ORDINARY_PAGES} release pages; ordinary versions could not be listed."));
            }
            page += 1;
        }
    }

    /// Whether github.com has just shown it can't be reached, so there is no point asking it again
    /// for the next app in the same sweep.
    fn pages_quiet(&self) -> bool {
        let until = *self.pages_quiet_until.lock().unwrap_or_else(PoisonError::into_inner);
        until.is_some_and(|u| u > crate::now())
    }

    /// Passes a release-pages error through, noting it when it means github.com never answered.
    fn note_pages_error(&self, why: String) -> String {
        if crate::http::is_unreachable(&why) {
            *self.pages_quiet_until.lock().unwrap_or_else(PoisonError::into_inner) = Some(crate::now().saturating_add(PAGES_RETRY_AFTER));
        }
        why
    }

    /// The quota-free route through github.com (stable releases only).
    fn ask_pages(&self, query: &ReleaseQuery) -> Result<Answer, String> {
        let repo = query.repo.as_str();
        if self.pages_quiet() {
            return Err("github.com couldn't be reached a moment ago".into());
        }
        let location = self.http.redirect_target(&format!("https://github.com/{repo}/releases/latest")).map_err(|e| self.note_pages_error(e))?;
        let no_release = || Ok(Answer { latest: LatestRelease::None, source: Source::GitHubPages });
        // A repo that isn't there answers 404, which `redirect_target` reports as no target at all.
        let Some(target) = location.as_deref() else { return no_release() };
        // Nothing published yet: github.com sends us to the repo's own release list instead.
        if is_release_list(target, repo) {
            return no_release();
        }
        let Some(tag) = release::tag_from_redirect(target, repo) else {
            // Somewhere that isn't github.com's: a captive portal's sign-in page, a filtering
            // proxy's notice. Reading that as "no release yet" would turn a blocked network into a
            // confident "Coming soon" on every card, so let the saved answer stand in instead.
            return Err("the latest-release redirect went somewhere that isn't github.com's".into());
        };
        if let Some(e) = self.entry(query, false)
            && let Some(r) = e.release.as_ref().filter(|r| r.tag == tag)
        {
            return Ok(Answer { latest: LatestRelease::Found(r.clone()), source: Source::GitHubPages });
        }
        let sums_url = format!("{}{tag}/SHA256SUMS.txt", release::download_prefix(repo));
        // Not `note_pages_error`: this one redirects to GitHub's asset CDN, so its failure says
        // nothing about whether github.com itself can be reached for the next app.
        let sums = self.http.get_text(&sums_url)?.map(|t| release::parse_sha256sums(&t)).unwrap_or_default();
        let r = release::release_from_sums(repo, &tag, &sums);
        if !release::ordinary_release_allowed(query.separate_nightly, &r) {
            return Err("The latest release is a Nightly of the separate Pre-Release channel, not an ordinary release.".into());
        }
        self.store(repo, Entry { etag: None, release: Some(r.clone()), prereleases: false, fetched_at: crate::now() });
        Ok(Answer { latest: LatestRelease::Found(r), source: Source::GitHubPages })
    }
}

/// GitHub's quota refills within the hour, so a later reset is a bad answer.
const MAX_LIMIT_WAIT: i64 = 60 * 60;

/// When to ask the API again after a refusal.
fn retry_at(reset: Option<i64>, now: i64) -> i64 {
    reset.unwrap_or(now + 15 * 60).min(now + MAX_LIMIT_WAIT)
}

/// Whether `location` is github.com's own release list for `repo`, which is where it sends a
/// request for the latest release of a repo that has published none.
fn is_release_list(location: &str, repo: &str) -> bool {
    let list = format!("https://github.com/{repo}/releases");
    location == list || location.strip_suffix('/') == Some(list.as_str())
}

/// Which route one lookup takes, with the three ways of asking handed in so the policy can be
/// tested without a network.
///
/// The REST API goes first: it is the only route that knows pre-releases, and an ETag makes a
/// repeat check nearly free. When it produces no answer — out of quota, unreachable, refused, or
/// an answer that couldn't be read — github.com's release pages are asked instead, because they
/// cost no quota and sit on a host that may well be reachable when the API's isn't (see the module
/// docs). Pre-releases aren't published there, so with pre-releases turned on that route is
/// skipped. Only when neither answered does the lookup settle for the last saved answer, and
/// report the failure when there isn't one.
fn resolve(
    repo: &str,
    prereleases: bool,
    limited: bool,
    ask_api: impl FnOnce() -> Result<Option<Answer>, String>,
    ask_pages: impl FnOnce() -> Result<Answer, String>,
    cached: impl FnOnce() -> Option<LatestRelease>,
) -> Answer {
    let api_error = if limited {
        None
    } else {
        match ask_api() {
            Ok(Some(answer)) => return answer,
            Ok(None) => None,
            Err(e) => Some(e),
        }
    };
    if !prereleases {
        if let Some(e) = &api_error {
            log::info!("{repo}: the GitHub API didn't answer ({e}); trying the release pages");
        }
        match ask_pages() {
            Ok(answer) => return answer,
            Err(e) => log::warn!("{repo}: release pages: {e}"),
        }
    }
    let why = api_error.unwrap_or_else(|| "GitHub's request limit was reached".into());
    match cached() {
        Some(latest) => Answer { latest, source: Source::Cache },
        None => Answer { latest: LatestRelease::Failed(why), source: Source::Cache },
    }
}

/// Asks `query`'s repo, and its former repo when that has no release: `UCSLabs/mailcraft` doesn't
/// exist until the owner's rename, and afterwards the former name only redirects
/// (storytold/craft-launcher#1593). An error is never retried under the other name.
fn with_former<T>(query: &ReleaseQuery, ask: impl Fn(&ReleaseQuery) -> T, no_release: impl Fn(&T) -> bool) -> T {
    let answer = ask(query);
    match query.former() {
        Some(former) if no_release(&answer) => ask(&former),
        _ => answer,
    }
}

impl GitHubSource {
    fn versions_in(&self, query: &ReleaseQuery) -> Result<Vec<Release>, String> {
        if let Some((at, list)) = self.versions_cache.lock().unwrap_or_else(PoisonError::into_inner).get(&query.repo)
            && crate::now().saturating_sub(*at) < 15 * 60
        {
            return Ok(list.clone());
        }
        if self.limited() {
            return Err("GitHub's request limit was reached. Try again later.".into());
        }
        let list = self.versions_with(query, |url, etag| self.http.get_api(url, etag))?;
        self.versions_cache.lock().unwrap_or_else(PoisonError::into_inner).insert(query.repo.clone(), (crate::now(), list.clone()));
        Ok(list)
    }

    fn latest_in(&self, query: &ReleaseQuery, prereleases: bool) -> Answer {
        resolve(
            &query.repo,
            prereleases,
            self.limited(),
            || self.ask_api(query, prereleases),
            || self.ask_pages(query),
            // Offline, a release saved under the other name still stands in.
            || self.cached(query, prereleases),
        )
    }

    fn cached_in(&self, query: &ReleaseQuery, prereleases: bool) -> Option<LatestRelease> {
        self.entry(query, prereleases).map(|e| e.release.map_or(LatestRelease::None, LatestRelease::Found))
    }
}

impl ReleaseSource for GitHubSource {
    fn versions(&self, query: &ReleaseQuery) -> Result<Vec<Release>, String> {
        with_former(query, |q| self.versions_in(q), |list| list.as_ref().is_ok_and(Vec::is_empty))
    }

    fn latest(&self, query: &ReleaseQuery, prereleases: bool) -> Answer {
        with_former(query, |q| self.latest_in(q, prereleases), |a| a.latest == LatestRelease::None)
    }

    fn cached(&self, query: &ReleaseQuery, prereleases: bool) -> Option<LatestRelease> {
        with_former(query, |q| self.cached_in(q, prereleases), |c| matches!(c, None | Some(LatestRelease::None)))
    }

    fn newest_tag(&self, repo: &str) -> Option<String> {
        match self.http.redirect_target(&format!("https://github.com/{repo}/releases/latest")) {
            Ok(location) => location.as_deref().and_then(|l| release::tag_from_redirect(l, repo)),
            Err(e) => {
                log::debug!("{repo}: couldn't read the latest-release redirect: {e}");
                None
            }
        }
    }

    fn rate_limited_until(&self) -> Option<i64> {
        let until = *self.limited_until.lock().unwrap_or_else(PoisonError::into_inner);
        until.filter(|u| *u > crate::now())
    }

    fn save(&self) {
        let json = {
            let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            serde_json::to_vec(&*cache)
        };
        match json {
            Ok(bytes) => {
                if let Err(e) = crate::paths::write_atomic(&self.cache_path, &bytes) {
                    log::warn!("release cache: {e}");
                }
            }
            Err(e) => log::warn!("release cache: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// PhotoCraft's own repo has the separate channel; any other repo in these tests does not.
    fn q(repo: &str) -> ReleaseQuery {
        ReleaseQuery { repo: repo.into(), separate_nightly: repo == REPO, former_repo: None }
    }

    const REPO: &str = "storytold/photocraft";
    /// What a blocked `api.github.com` leaves behind, and what github.com then says in turn.
    const NO_API: &str = "couldn't reach GitHub (io: connection refused)";
    const NO_PAGES: &str = "couldn't reach github.com (io: connection refused)";

    fn answer(tag: &str, source: Source) -> Answer {
        Answer { latest: LatestRelease::Found(Release { tag: tag.to_string(), ..Release::default() }), source }
    }

    fn tag_of(a: &Answer) -> Option<&str> {
        match &a.latest {
            LatestRelease::Found(r) => Some(r.tag.as_str()),
            _ => None,
        }
    }

    /// Records whether the release-pages route was taken, and answers `reply` when it was.
    fn pages(asked: &Cell<bool>, reply: Result<Answer, String>) -> impl FnOnce() -> Result<Answer, String> {
        move || {
            asked.set(true);
            reply
        }
    }

    #[test]
    fn an_api_answer_ends_the_lookup() {
        let asked = Cell::new(false);
        let a = resolve(REPO, false, false, || Ok(Some(answer("v0.6.0", Source::GitHubApi))), pages(&asked, Err("unused".into())), || None);
        assert_eq!((tag_of(&a), a.source), (Some("v0.6.0"), Source::GitHubApi));
        assert!(!asked.get(), "github.com is left alone when the API answered");
    }

    #[test]
    fn an_api_that_cannot_be_reached_still_reaches_the_release_pages() {
        // The bug this guards (#875): a machine where api.github.com is blocked but github.com is
        // not showed every app the user hadn't installed as "Offline", because the quota-free
        // route was taken only when GitHub itself said "out of quota".
        for api_error in [
            NO_API,
            "GitHub refused the request (HTTP 403)",
            "GitHub answered HTTP 407",
            "GitHub answered HTTP 503",
            "GitHub said nothing changed, but there's no saved answer",
        ] {
            let asked = Cell::new(false);
            let a = resolve(REPO, false, false, || Err(api_error.into()), pages(&asked, Ok(answer("v0.6.0", Source::GitHubPages))), || None);
            assert!(asked.get(), "{api_error}");
            assert_eq!((tag_of(&a), a.source), (Some("v0.6.0"), Source::GitHubPages), "{api_error}");
        }
    }

    #[test]
    fn an_exhausted_quota_takes_the_same_route() {
        // Already known to be out of quota, so the API isn't asked at all.
        let (asked, api_asked) = (Cell::new(false), Cell::new(false));
        let api = || {
            api_asked.set(true);
            Ok(None)
        };
        let a = resolve(REPO, false, true, api, pages(&asked, Ok(answer("v0.6.0", Source::GitHubPages))), || None);
        assert_eq!(a.source, Source::GitHubPages);
        assert!(asked.get() && !api_asked.get(), "an API known to be out of quota is left alone");
        // Told so by this very request.
        let asked = Cell::new(false);
        let a = resolve(REPO, false, false, || Ok(None), pages(&asked, Ok(answer("v0.6.0", Source::GitHubPages))), || None);
        assert_eq!(a.source, Source::GitHubPages);
        assert!(asked.get());
    }

    #[test]
    fn with_prereleases_on_the_release_pages_cannot_answer() {
        // They only ever name the latest stable release, so there is nothing to ask them for.
        let asked = Cell::new(false);
        let saved = || Some(LatestRelease::Found(Release { tag: "v0.6.0-rc.1".into(), ..Release::default() }));
        let a = resolve(REPO, true, false, || Err(NO_API.into()), pages(&asked, Err("unused".into())), saved);
        assert!(!asked.get());
        assert_eq!((tag_of(&a), a.source), (Some("v0.6.0-rc.1"), Source::Cache));
    }

    #[test]
    fn when_neither_route_answers_the_saved_answer_stands_in() {
        let asked = Cell::new(false);
        let saved = || Some(LatestRelease::Found(Release { tag: "v0.5.0".into(), ..Release::default() }));
        let a = resolve(REPO, false, false, || Err(NO_API.into()), pages(&asked, Err(NO_PAGES.into())), saved);
        assert!(asked.get());
        assert_eq!((tag_of(&a), a.source), (Some("v0.5.0"), Source::Cache));
    }

    #[test]
    fn with_nothing_saved_the_api_reason_is_what_the_user_is_told() {
        let asked = Cell::new(false);
        let a = resolve(REPO, false, false, || Err(NO_API.into()), pages(&asked, Err(NO_PAGES.into())), || None);
        assert!(asked.get());
        assert_eq!(a.latest, LatestRelease::Failed(NO_API.into()));
        // Out of quota, with github.com no help either: say that instead.
        let a = resolve(REPO, false, true, || Ok(None), || Err(NO_PAGES.into()), || None);
        assert_eq!(a.latest, LatestRelease::Failed("GitHub's request limit was reached".into()));
    }

    #[test]
    fn only_github_coms_own_answer_is_read_as_no_release_yet() {
        // github.com sends a repo without releases to its release list. A sign-in page from a
        // captive portal or a filtering proxy must not be read the same way: the saved answer is
        // better than a confident "Coming soon" on every card.
        assert!(is_release_list("https://github.com/storytold/photocraft/releases", REPO));
        assert!(is_release_list("https://github.com/storytold/photocraft/releases/", REPO));
        for elsewhere in [
            "https://github.com/storytold/photocraft/releases/tag/v0.6.0",
            "https://github.com/storytold/photocraft/releases/latest",
            "https://github.com/storytold/photocraft",
            "https://github.com/someone-else/photocraft/releases",
            "http://portal.example/login?continue=github.com",
        ] {
            assert!(!is_release_list(elsewhere, REPO), "{elsewhere}");
        }
    }

    #[test]
    fn github_com_is_not_probed_again_for_every_app_once_it_is_silent() {
        let dir = tempfile::tempdir().unwrap();
        let source = GitHubSource::new(Http::new().unwrap(), dir.path().join("releases.json"));
        assert!(!source.pages_quiet());
        // An HTTP status is an answer: github.com is still worth asking for the next app.
        assert_eq!(source.note_pages_error("github.com answered HTTP 500".into()), "github.com answered HTTP 500");
        assert!(!source.pages_quiet());
        assert_eq!(source.note_pages_error(NO_PAGES.into()), NO_PAGES);
        assert!(source.pages_quiet());
        let err = source.ask_pages(&q(REPO)).unwrap_err();
        assert!(err.contains("couldn't be reached a moment ago"), "{err}");
    }

    #[test]
    fn a_refusal_waits_for_the_reset_github_names() {
        assert_eq!(retry_at(Some(1_000 + 600), 1_000), 1_600);
        assert_eq!(retry_at(None, 1_000), 1_000 + 15 * 60);
    }

    #[test]
    fn a_reset_past_the_quota_window_cannot_stop_the_api_for_good() {
        assert_eq!(retry_at(Some(i64::MAX), 1_000), 1_000 + 60 * 60);
        assert_eq!(retry_at(Some(99_999_999_999), 1_000), 1_000 + 60 * 60);
    }
    fn shared_selection_saved_source(repo: &str, tag: &str) -> (tempfile::TempDir, GitHubSource, Release) {
        let temp = tempfile::tempdir().unwrap();
        let release = release::parse_github_release(&serde_json::json!({"tag_name":tag,"prerelease":true,"assets":[]}).to_string(), repo).unwrap();
        let cache = CacheFile {
            entries: [(repo.into(), Entry { etag: Some("old-etag".into()), release: Some(release.clone()), prereleases: true, fetched_at: 7 })]
                .into(),
        };
        let path = temp.path().join("releases.json");
        std::fs::write(&path, serde_json::to_vec(&cache).unwrap()).unwrap();
        let source = GitHubSource::new(Http::new().unwrap(), path);
        (temp, source, release)
    }
    #[test]
    fn shared_selection_saved_first_party_nightly_never_replays_offline_or_rate_limited() {
        let (_temp, source, _) = shared_selection_saved_source(REPO, "v1.0.0-dev.nightly.1");
        assert!(source.cached(&q(REPO), true).is_none(), "invalid cache is unavailable, not proof of no ordinary releases");
        for limited in [false, true] {
            let result = resolve(
                REPO,
                true,
                limited,
                || Err(NO_API.into()),
                || panic!("prerelease lookups never use latest release pages"),
                || source.cached(&q(REPO), true),
            );
            assert!(matches!(result.latest, LatestRelease::Failed(_)));
        }
    }
    #[test]
    fn shared_selection_stale_nightly_etag_cannot_keep_shadowing_a_beta() {
        let (temp, source, _) = shared_selection_saved_source(REPO, "v1.0.0-dev.nightly.1");
        let original = std::fs::read(temp.path().join("releases.json")).unwrap();
        let mut sent_etag = Some("not called".to_string());
        let replay = source.ask_api_with(&q(REPO), true, |_, etag| {
            sent_etag = etag.map(str::to_owned);
            Ok(Fetched::NotModified)
        });
        assert!(replay.is_err(), "an invalid cached Nightly cannot become a successful unchanged ordinary answer");
        assert_eq!(sent_etag, None, "force a real body instead of perpetually accepting the stale ETag");
        assert_eq!(std::fs::read(temp.path().join("releases.json")).unwrap(), original, "admission must not delete or rewrite the old on-disk cache");
        let body = r#"[{"tag_name":"v1.0.0-dev.nightly.1","prerelease":true},{"tag_name":"v0.9.0-beta.1","prerelease":true}]"#;
        let answer = source
            .ask_api_with(&q(REPO), true, |_, etag| {
                assert!(etag.is_none());
                Ok(Fetched::Body { text: body.into(), etag: Some("current-etag".into()) })
            })
            .unwrap()
            .unwrap();
        assert_eq!(tag_of(&answer), Some("v0.9.0-beta.1"));
        let unchanged = source
            .ask_api_with(&q(REPO), true, |_, etag| {
                assert_eq!(etag, Some("current-etag"));
                Ok(Fetched::NotModified)
            })
            .unwrap()
            .unwrap();
        assert_eq!(tag_of(&unchanged), Some("v0.9.0-beta.1"));
    }
    #[test]
    fn shared_selection_legacy_nightly_cache_and_etag_replay_remain_main_compatible() {
        for id in ["solidcraft", "filmcraft"] {
            let repo = artcraft_launcher_core::CraftApp::by_id(id).unwrap().repo;
            let (_temp, source, release) = shared_selection_saved_source(repo, "v1.0.0-dev.nightly.1");
            assert_eq!(source.cached(&q(repo), true), Some(LatestRelease::Found(release.clone())));
            let answer = source
                .ask_api_with(&q(repo), true, |_, etag| {
                    assert_eq!(etag, Some("old-etag"));
                    Ok(Fetched::NotModified)
                })
                .unwrap()
                .unwrap();
            assert_eq!(answer.latest, LatestRelease::Found(release));
        }
    }
    fn crowded_page(repo: &str, count: usize, offset: usize) -> String {
        let items: Vec<_> = (0..count)
            .map(|i| serde_json::json!({"tag_name": format!("v1.0.0-nightly.20261120.{}.1.gabcdefabcdef", 1000 - offset - i), "prerelease": true, "assets": []}))
            .collect();
        let _ = repo;
        serde_json::to_string(&items).unwrap()
    }
    fn fresh_source() -> (tempfile::TempDir, GitHubSource) {
        let temp = tempfile::tempdir().unwrap();
        let source = GitHubSource::new(Http::new().unwrap(), temp.path().join("releases.json"));
        (temp, source)
    }
    #[test]
    fn a_first_page_of_separate_channel_nightlies_does_not_hide_the_newest_beta() {
        let (_temp, source) = fresh_source();
        let mut asked = Vec::new();
        let answer = source
            .ask_api_with(&q(REPO), true, |url, _| {
                asked.push(url.to_owned());
                Ok(Fetched::Body {
                    text: if url.ends_with("&page=2") {
                        r#"[{"tag_name":"v0.9.0-beta.2","prerelease":true}]"#.into()
                    } else {
                        crowded_page(REPO, VERSIONS_PAGE, 0)
                    },
                    etag: None,
                })
            })
            .unwrap()
            .unwrap();
        assert_eq!(tag_of(&answer), Some("v0.9.0-beta.2"));
        assert_eq!(asked.len(), 2);
    }
    #[test]
    fn an_exhausted_bounded_search_is_incomplete_and_never_cached_as_no_release() {
        let (_temp, source) = fresh_source();
        let mut asked = 0;
        let result = source.ask_api_with(&q(REPO), true, |_, _| {
            asked += 1;
            Ok(Fetched::Body { text: crowded_page(REPO, VERSIONS_PAGE, asked * VERSIONS_PAGE), etag: Some("etag".into()) })
        });
        assert!(result.is_err());
        assert_eq!(asked, ORDINARY_PAGES);
        assert!(source.cached(&q(REPO), true).is_none(), "absence was not established, so nothing is cached");
    }
    #[test]
    fn crowded_separate_channel_version_lists_still_reach_ordinary_versions() {
        let (_temp, source) = fresh_source();
        let mut asked = 0;
        let list = source
            .versions_with(&q(REPO), |url, _| {
                asked += 1;
                Ok(Fetched::Body {
                    text: if url.ends_with("&page=2") {
                        r#"[{"tag_name":"v0.9.0","prerelease":false}]"#.into()
                    } else {
                        crowded_page(REPO, VERSIONS_PAGE, 0)
                    },
                    etag: None,
                })
            })
            .unwrap();
        assert!(list.iter().any(|r| r.tag == "v0.9.0"));
        assert_eq!(asked, 2);
    }
    #[test]
    fn third_party_lookups_keep_mains_single_page_with_its_nightlies() {
        let repo = artcraft_launcher_core::CraftApp::by_id("solidcraft").unwrap().repo;
        let (_temp, source) = fresh_source();
        let mut asked = 0;
        let answer = source
            .ask_api_with(&q(repo), true, |_, _| {
                asked += 1;
                Ok(Fetched::Body { text: crowded_page(repo, LIST_PAGE, 0), etag: None })
            })
            .unwrap()
            .unwrap();
        assert!(tag_of(&answer).is_some_and(|tag| tag.contains("nightly")));
        let list = source
            .versions_with(&q(repo), |_, _| {
                asked += 1;
                Ok(Fetched::Body { text: crowded_page(repo, VERSIONS_PAGE, 0), etag: None })
            })
            .unwrap();
        assert_eq!(list.len(), VERSIONS_PAGE);
        assert_eq!(asked, 2, "one request each, as on main");
    }
    #[test]
    fn a_multi_page_answer_is_revalidated_when_only_a_later_page_changes() {
        let (_temp, source) = fresh_source();
        let page2 = std::cell::RefCell::new(r#"[{"tag_name":"v0.9.0-beta.2","prerelease":true}]"#.to_string());
        let mut ask = |url: &str, etag: Option<&str>| -> Result<Fetched, String> {
            if url.ends_with("&page=2") {
                return Ok(Fetched::Body { text: page2.borrow().clone(), etag: Some("page-2".into()) });
            }
            // Page 1 itself never changes, so any ETag sent for it would get a 304.
            Ok(if etag.is_some() {
                Fetched::NotModified
            } else {
                Fetched::Body { text: crowded_page(REPO, VERSIONS_PAGE, 0), etag: Some("page-1".into()) }
            })
        };
        let first = source.ask_api_with(&q(REPO), true, &mut ask).unwrap().unwrap();
        assert_eq!(tag_of(&first), Some("v0.9.0-beta.2"));
        *page2.borrow_mut() = r#"[{"tag_name":"v0.9.0-beta.3","prerelease":true}]"#.into();
        let second = source.ask_api_with(&q(REPO), true, &mut ask).unwrap().unwrap();
        assert_eq!(tag_of(&second), Some("v0.9.0-beta.3"), "page 1's 304 must not replay an answer read from a changed later page");
    }

    #[test]
    fn a_feed_repo_rename_keeps_photocraft_nightlies_out_of_the_ordinary_lookup_and_cache() {
        // benandez's case: the unsigned feed renames storytold/photocraft; the policy follows the app.
        let app = artcraft_launcher_core::CraftApp::by_id("photocraft").unwrap();
        let feed = artcraft_launcher_core::feed::Feed::parse(
            r#"{"repo_renames":[{"old_name":"storytold/photocraft","new_name":"storytold/photocraft-studio"}]}"#,
        )
        .unwrap();
        let renamed = ReleaseQuery::for_app(app, feed.repo_for(app));
        assert_eq!(renamed.repo, "storytold/photocraft-studio");
        let (_temp, source, _) = shared_selection_saved_source(&renamed.repo, "v1.0.0-dev.nightly.1");
        assert!(source.cached(&renamed, true).is_none(), "a saved nightly under the renamed repo is not replayed");
        let mut sent_etag = Some("not asked".to_string());
        let body = r#"[{"tag_name":"v2.0.0-nightly.20261123.1.1.gabcdefabcdef","prerelease":true},{"tag_name":"v1.0.0-beta.1","prerelease":true}]"#;
        let answer = source
            .ask_api_with(&renamed, true, |_, etag| {
                sent_etag = etag.map(str::to_owned);
                Ok(Fetched::Body { text: body.into(), etag: None })
            })
            .unwrap()
            .unwrap();
        assert_eq!(sent_etag, None, "the saved nightly's ETag is not revalidated");
        assert_eq!(tag_of(&answer), Some("v1.0.0-beta.1"));
    }

    #[test]
    fn an_app_with_a_former_repo_is_found_under_either_name() {
        // MailCraft moved from `UCS-Care-Group` to `UCSLabs` (#1593). Before the rename the new name
        // is a 404, after it the new name answers: either way the release is found once, and its
        // downloads belong to the repo that answered.
        let app = artcraft_launcher_core::CraftApp::by_id("mailcraft").unwrap();
        let query = ReleaseQuery::for_app(app, app.repo);
        let former = app.former_repo.unwrap();
        let (_temp, source) = fresh_source();
        for live in [former, app.repo] {
            let asked = std::cell::RefCell::new(Vec::new());
            let answer = with_former(
                &query,
                |q| {
                    asked.borrow_mut().push(q.repo.clone());
                    let body = |repo: &str| {
                        let url = format!("{}v0.15.0/mailcraft-0.15.0-x.msi", release::download_prefix(repo));
                        serde_json::json!({"tag_name": "v0.15.0", "assets": [{"name": "mailcraft-0.15.0-x.msi", "size": 1, "browser_download_url": url}]}).to_string()
                    };
                    source
                        .ask_api_with(q, false, |_, _| {
                            Ok(if q.repo == live { Fetched::Body { text: body(live), etag: None } } else { Fetched::NotFound })
                        })
                        .unwrap()
                        .unwrap()
                },
                |a| a.latest == LatestRelease::None,
            );
            let LatestRelease::Found(r) = answer.latest else { panic!("{live}: {:?}", answer.latest) };
            assert!(r.assets.iter().all(|a| a.url.starts_with(&release::download_prefix(live))) && !r.assets.is_empty(), "{live}");
            assert_eq!(asked.borrow().first(), Some(&app.repo.to_string()), "the current name is asked first");
            assert_eq!(asked.borrow().len(), if live == app.repo { 1 } else { 2 }, "{live}");
        }
    }

    #[test]
    fn a_release_saved_under_the_former_name_still_stands_in() {
        // Checked before the rename, opened offline after it: nothing is saved under the new name.
        let app = artcraft_launcher_core::CraftApp::by_id("mailcraft").unwrap();
        let (_temp, source, release) = shared_selection_saved_source(app.former_repo.unwrap(), "v0.15.0");
        assert_eq!(source.cached(&ReleaseQuery::for_app(app, app.repo), true), Some(LatestRelease::Found(release)));
    }

    #[test]
    fn a_failed_lookup_is_not_retried_under_the_former_name() {
        // A rate limit or network error says nothing about which name holds the release.
        let app = artcraft_launcher_core::CraftApp::by_id("mailcraft").unwrap();
        let asked = Cell::new(0);
        let answer = with_former(
            &ReleaseQuery::for_app(app, app.repo),
            |_| {
                asked.set(asked.get() + 1);
                Answer { latest: LatestRelease::Failed(NO_API.into()), source: Source::GitHubApi }
            },
            |a| a.latest == LatestRelease::None,
        );
        assert!(matches!(answer.latest, LatestRelease::Failed(_)));
        assert_eq!(asked.get(), 1);
    }
}
