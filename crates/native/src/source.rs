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
use artcraft_launcher_core::release::{self, Release};
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
    fn latest(&self, repo: &str, prereleases: bool) -> Answer;
    /// The last answer saved for `repo`, for an instant first paint.
    fn cached(&self, repo: &str, prereleases: bool) -> Option<LatestRelease>;
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

pub struct GitHubSource {
    http: Http,
    cache: Mutex<CacheFile>,
    cache_path: std::path::PathBuf,
    limited_until: Mutex<Option<i64>>,
    /// github.com couldn't be reached; don't ask it again until this time (Unix seconds).
    pages_quiet_until: Mutex<Option<i64>>,
}

impl GitHubSource {
    pub fn new(http: Http, cache_path: std::path::PathBuf) -> Self {
        let cache = std::fs::read(&cache_path).ok().and_then(|b| serde_json::from_slice::<CacheFile>(&b).ok()).unwrap_or_default();
        Self { http, cache: Mutex::new(cache), cache_path, limited_until: Mutex::new(None), pages_quiet_until: Mutex::new(None) }
    }

    fn entry(&self, repo: &str, prereleases: bool) -> Option<Entry> {
        let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        cache.entries.get(repo).filter(|e| e.prereleases == prereleases).cloned()
    }

    fn store(&self, repo: &str, entry: Entry) {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner).entries.insert(repo.to_string(), entry);
    }

    fn limited(&self) -> bool {
        let until = *self.limited_until.lock().unwrap_or_else(PoisonError::into_inner);
        until.is_some_and(|u| u > crate::now())
    }

    fn ask_api(&self, repo: &str, prereleases: bool) -> Result<Option<Answer>, String> {
        let url = if prereleases {
            format!("https://api.github.com/repos/{repo}/releases?per_page=30")
        } else {
            format!("https://api.github.com/repos/{repo}/releases/latest")
        };
        let saved = self.entry(repo, prereleases);
        let found =
            |release: Option<Release>| Answer { latest: release.map_or(LatestRelease::None, LatestRelease::Found), source: Source::GitHubApi };
        match self.http.get_api(&url, saved.as_ref().and_then(|e| e.etag.as_deref()))? {
            Fetched::Body { text, etag } => {
                let release = if prereleases {
                    release::parse_github_release_list(&text, repo, true)?
                } else {
                    Some(release::parse_github_release(&text, repo)?)
                };
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
                *self.limited_until.lock().unwrap_or_else(PoisonError::into_inner) = Some(reset.unwrap_or_else(|| crate::now() + 15 * 60));
                Ok(None)
            }
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
    fn ask_pages(&self, repo: &str) -> Result<Answer, String> {
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
        if let Some(e) = self.entry(repo, false)
            && let Some(r) = e.release.as_ref().filter(|r| r.tag == tag)
        {
            return Ok(Answer { latest: LatestRelease::Found(r.clone()), source: Source::GitHubPages });
        }
        let sums_url = format!("{}{tag}/SHA256SUMS.txt", release::download_prefix(repo));
        // Not `note_pages_error`: this one redirects to GitHub's asset CDN, so its failure says
        // nothing about whether github.com itself can be reached for the next app.
        let sums = self.http.get_text(&sums_url)?.map(|t| release::parse_sha256sums(&t)).unwrap_or_default();
        let r = release::release_from_sums(repo, &tag, &sums);
        self.store(repo, Entry { etag: None, release: Some(r.clone()), prereleases: false, fetched_at: crate::now() });
        Ok(Answer { latest: LatestRelease::Found(r), source: Source::GitHubPages })
    }
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

impl ReleaseSource for GitHubSource {
    fn latest(&self, repo: &str, prereleases: bool) -> Answer {
        resolve(repo, prereleases, self.limited(), || self.ask_api(repo, prereleases), || self.ask_pages(repo), || self.cached(repo, prereleases))
    }

    fn cached(&self, repo: &str, prereleases: bool) -> Option<LatestRelease> {
        self.entry(repo, prereleases).map(|e| e.release.map_or(LatestRelease::None, LatestRelease::Found))
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
        let err = source.ask_pages(REPO).unwrap_err();
        assert!(err.contains("couldn't be reached a moment ago"), "{err}");
    }
}
