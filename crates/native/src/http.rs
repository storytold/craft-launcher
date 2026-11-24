//! HTTPS requests: small JSON/text answers, redirect probes, and verified downloads.

use std::io::{Read, Write};
use std::path::Path;

use artcraft_launcher_core::protocol::{CANCELLED, PAUSED};
use std::time::Duration;

use sha2::{Digest, Sha256};

const USER_AGENT: &str = concat!("ArtCraftLauncher/", env!("CARGO_PKG_VERSION"));
/// Largest JSON or text answer we read.
const MAX_SMALL: u64 = 4 << 20;
/// Largest GitHub API answer we read: 100 releases of an app with 49 assets are about 10 MB.
const MAX_API: u64 = 16 << 20;

/// What a GET of a small resource produced.
#[derive(Debug, PartialEq, Eq)]
pub enum Fetched {
    Body {
        text: String,
        etag: Option<String>,
    },
    /// `If-None-Match` matched: the saved copy is current.
    NotModified,
    NotFound,
    /// GitHub's API refused: out of quota until `reset` (Unix seconds), if it said.
    RateLimited {
        reset: Option<i64>,
    },
}

#[derive(Clone)]
pub struct Http {
    small: ureq::Agent,
    /// ArtCraft's release feed: a short timeout, so a slow server never holds up a check.
    feed: ureq::Agent,
    no_redirect: ureq::Agent,
    download: ureq::Agent,
    direct: ureq::Agent,
    direct_download: ureq::Agent,
    token: Option<String>,
    nightly_mirror: Option<NightlyMirror>,
}

/// An indeterminate Nightly lookup; a server-supplied backoff applies to every resource kind.
#[derive(Debug)]
pub(crate) struct NightlyHttpError {
    pub why: String,
    pub retry_at: Option<i64>,
}

/// Why ArtCraft's release feed couldn't be read.
#[derive(Debug)]
pub struct FeedError {
    pub why: String,
    /// The server answered HTTP 429 Too Many Requests.
    pub throttled: bool,
    /// The server's `Retry-After`, in seconds.
    pub retry_after: Option<i64>,
}

/// `http://<loopback host>:<port>` and an optional path: the host must be exactly 127.0.0.1,
/// localhost or [::1] (no `user@` in front of another host), with a numeric port.
pub fn is_loopback(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else { return false };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let Some((host, port)) = authority.rsplit_once(':') else { return false };
    matches!(host, "127.0.0.1" | "localhost" | "[::1]") && !port.is_empty() && port.parse::<u16>().is_ok()
}

/// Only a debug build explicitly enabling `local-mirror` constructs this transport. Logical
/// GitHub URLs stay intact for descriptor checks; physical requests go to this one-app fixture.
#[derive(Clone)]
struct NightlyMirror {
    base: String,
    agent: ureq::Agent,
}

impl NightlyMirror {
    fn new(base: &str) -> Result<Self, String> {
        let base = base.trim();
        if !is_loopback(base) || base.contains(['?', '#', '%', '\\']) || base.split('/').any(|part| matches!(part, "." | "..")) {
            return Err("The nightly test mirror must be an explicit loopback HTTP base.".into());
        }
        let agent = ureq::Agent::config_builder()
            .proxy(None)
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_recv_response(Some(Duration::from_secs(20)))
            .timeout_recv_body(Some(ATTEMPT_BODY_TIME))
            .max_redirects(0)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .user_agent(USER_AGENT)
            .build()
            .new_agent();
        Ok(Self { base: format!("{}/", base.trim_end_matches('/')), agent })
    }

    fn request_url(&self, logical: &str) -> Result<String, String> {
        if logical == "https://api.github.com/repos/storytold/photocraft/releases?per_page=100" {
            return Ok(format!("{}nightly/photocraft/releases.json", self.base));
        }
        for page in 2..=5 {
            if logical == format!("https://api.github.com/repos/storytold/photocraft/releases?per_page=100&page={page}") {
                return Ok(format!("{}nightly/photocraft/releases-page-{page}.json", self.base));
            }
        }
        let suffix = logical
            .strip_prefix("https://github.com/storytold/photocraft/releases/download/")
            .ok_or("The nightly test transport refuses requests outside the fixed PhotoCraft release.")?;
        let parts: Vec<_> = suffix.split('/').collect();
        if parts.len() != 2
            || parts.iter().any(|part| {
                part.is_empty()
                    || matches!(*part, "." | "..")
                    || !part.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
            })
        {
            return Err("The nightly test transport refuses an unsafe tag or asset path.".into());
        }
        Ok(format!("{}nightly/photocraft/assets/{suffix}", self.base))
    }
}

impl Http {
    pub fn new() -> Result<Self, String> {
        // The OS checks each certificate chain itself: Windows fetches roots it hasn't stored yet
        // and trusts what antivirus HTTPS scanning installs. Reading the store ourselves missed
        // both ("invalid peer certificate: UnknownIssuer").
        let tls = || ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
        // Chosen once: the proxy environment variables, else the OS proxy setting.
        let proxy = crate::proxy::pick();
        let small = ureq::Agent::config_builder()
            .tls_config(tls())
            .proxy(proxy.clone())
            .timeout_global(Some(Duration::from_secs(20)))
            .http_status_as_error(false)
            .user_agent(USER_AGENT)
            .build()
            .new_agent();
        let feed = ureq::Agent::config_builder()
            .tls_config(tls())
            .proxy(proxy.clone())
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_global(Some(Duration::from_secs(10)))
            .http_status_as_error(false)
            .user_agent(USER_AGENT)
            .build()
            .new_agent();
        let no_redirect = ureq::Agent::config_builder()
            .tls_config(tls())
            .proxy(proxy.clone())
            .timeout_global(Some(Duration::from_secs(20)))
            .http_status_as_error(false)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .user_agent(USER_AGENT)
            .build()
            .new_agent();
        let download = ureq::Agent::config_builder()
            .tls_config(tls())
            .proxy(proxy.clone())
            .timeout_connect(Some(Duration::from_secs(20)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            // ureq has no idle timeout: cap each attempt instead, and resume where it stopped.
            .timeout_recv_body(Some(ATTEMPT_BODY_TIME))
            .http_status_as_error(false)
            .user_agent(USER_AGENT)
            .build()
            .new_agent();
        let direct = ureq::Agent::config_builder()
            .proxy(None)
            .tls_config(tls())
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_global(Some(Duration::from_secs(20)))
            .http_status_as_error(false)
            .user_agent(USER_AGENT)
            .build()
            .new_agent();
        let direct_download = ureq::Agent::config_builder()
            .proxy(None)
            .tls_config(tls())
            .timeout_connect(Some(Duration::from_secs(20)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            .timeout_recv_body(Some(ATTEMPT_BODY_TIME))
            .http_status_as_error(false)
            .user_agent(USER_AGENT)
            .build()
            .new_agent();
        // An optional token raises GitHub's limit from 60 to 5000 requests an hour. Only ever sent
        // to api.github.com.
        let token = std::env::var("ARTCRAFT_LAUNCHER_GITHUB_TOKEN").ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        Ok(Self { small, feed, no_redirect, download, direct, direct_download, token, nightly_mirror: None })
    }

    /// Production/default builds ignore the staging environment, including malformed values.
    /// This is used only by the immutable Nightly backend, never by stable requests.
    pub(crate) fn with_nightly_test_transport(mut self) -> Result<Self, String> {
        if cfg!(all(feature = "local-mirror", debug_assertions))
            && let Ok(base) = std::env::var("ARTCRAFT_LAUNCHER_DOWNLOAD_BASE")
        {
            self.nightly_mirror = Some(NightlyMirror::new(&base)?);
        }
        Ok(self)
    }

    fn request<'a>(&'a self, logical: &'a str, default: &'a ureq::Agent) -> Result<(std::borrow::Cow<'a, str>, &'a ureq::Agent), String> {
        match &self.nightly_mirror {
            Some(mirror) => Ok((std::borrow::Cow::Owned(mirror.request_url(logical)?), &mirror.agent)),
            None => Ok((std::borrow::Cow::Borrowed(logical), default)),
        }
    }

    /// GET a GitHub API resource, revalidating with `etag` when given.
    pub fn get_api(&self, url: &str, etag: Option<&str>) -> Result<Fetched, String> {
        // A request carrying the token must not follow a redirect: a cross-origin `Location`
        // could otherwise forward the bearer token. Requests without one keep the old behaviour.
        let agent = if self.token.is_some() && url.starts_with("https://api.github.com/") { &self.no_redirect } else { &self.small };
        let (physical, agent) = self.request(url, agent)?;
        let mut req = agent.get(physical.as_ref()).header("Accept", "application/vnd.github+json").header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(tag) = etag {
            req = req.header("If-None-Match", tag);
        }
        if let Some(token) = &self.token
            && physical == url
            && url.starts_with("https://api.github.com/")
        {
            req = req.header("Authorization", &format!("Bearer {token}"));
        }
        let mut resp = req.call().map_err(|e| format!("{UNREACHABLE} GitHub ({e})"))?;
        let header = |name: &str| resp.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
        let status = resp.status().as_u16();
        match status {
            200 => {
                let etag = header("etag");
                let text = read_text_capped(&mut resp, MAX_API).map_err(|e| format!("unreadable answer from GitHub ({e})"))?;
                Ok(Fetched::Body { text, etag })
            }
            304 => Ok(Fetched::NotModified),
            404 => Ok(Fetched::NotFound),
            403 | 429 => {
                let remaining = header("x-ratelimit-remaining");
                let reset = header("x-ratelimit-reset").and_then(|r| r.trim().parse::<i64>().ok());
                let retry_after = header("retry-after").and_then(|r| r.trim().parse::<i64>().ok());
                match refused_until(status, remaining.as_deref(), reset, retry_after, crate::now()) {
                    Some(reset) => Ok(Fetched::RateLimited { reset }),
                    None => Err(format!("GitHub refused the request (HTTP {status})")),
                }
            }
            s => Err(format!("GitHub answered HTTP {s}")),
        }
    }

    /// ArtCraft's release feed, or why it couldn't be read. Never takes more than ~10 seconds.
    pub fn get_feed(&self, url: &str) -> Result<String, FeedError> {
        let failed = |why: String| FeedError { why, throttled: false, retry_after: None };
        let agent = if is_loopback(url) { &self.direct } else { &self.feed };
        let mut resp =
            agent.get(url).header("Accept", "application/json").call().map_err(|e| failed(format!("{UNREACHABLE} {} ({e})", host(url))))?;
        // Seconds only; an HTTP date falls back to our own backoff.
        let retry_after = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|r| r.trim().parse::<i64>().ok());
        match resp.status().as_u16() {
            200 => read_text_capped(&mut resp, MAX_SMALL).map_err(|e| failed(format!("unreadable answer ({e})"))),
            s => Err(FeedError { why: format!("{} answered HTTP {s}", host(url)), throttled: s == 429, retry_after }),
        }
    }

    /// GET a small text file (following redirects).
    pub fn get_text(&self, url: &str) -> Result<Option<String>, String> {
        let agent = if is_loopback(url) { &self.direct } else { &self.small };
        let (physical, agent) = self.request(url, agent)?;
        let mut resp = agent.get(physical.as_ref()).call().map_err(|e| format!("{UNREACHABLE} {} ({e})", host(url)))?;
        match resp.status().as_u16() {
            200 => read_text_capped(&mut resp, MAX_SMALL).map(Some).map_err(|e| format!("unreadable answer ({e})")),
            404 => Ok(None),
            s => Err(format!("{} answered HTTP {s}", host(url))),
        }
    }

    /// GET a small binary file (following redirects). `Ok(None)` for a 404. A file of more than
    /// `max` bytes is refused: by its announced length before a byte is read, and by a hard limit
    /// on the body when the length wasn't announced or was wrong.
    pub fn get_bytes(&self, url: &str, max: usize) -> Result<Option<Vec<u8>>, String> {
        let agent = if is_loopback(url) { &self.direct } else { &self.small };
        let (physical, agent) = self.request(url, agent)?;
        Self::get_bytes_with(agent, physical.as_ref(), max)
    }

    /// Nightly-only transport: one bounded decoded body and one elapsed-time budget, including
    /// redirects and body reads. Callers own logical URLs and the overall discovery budget;
    /// this method owns transport failure/backoff, not authoritative absence (except HTTP 404).
    /// Stable's helpers deliberately keep their existing policies.
    pub(crate) fn nightly_get(&self, url: &str, max: usize, timeout: Duration, _lookup_now: i64) -> Result<Option<Vec<u8>>, NightlyHttpError> {
        // The lookup timestamp predates admission/queueing and network I/O. It cannot anchor
        // a server's relative wait. Sample the wall clock only after receiving its response.
        self.nightly_get_with_receipt_clock(url, max, timeout, crate::now)
    }

    fn nightly_get_with_receipt_clock(
        &self,
        url: &str,
        max: usize,
        timeout: Duration,
        receipt_now: impl Fn() -> i64,
    ) -> Result<Option<Vec<u8>>, NightlyHttpError> {
        let started = std::time::Instant::now();
        let failed = |why: String| NightlyHttpError { why, retry_at: None };
        let api = url.starts_with("https://api.github.com/");
        let default = if is_loopback(url) { &self.direct } else { &self.small };
        let (physical, agent) = self.request(url, default).map_err(failed)?;
        let mut current = physical.to_string();
        for redirects in 0..=3 {
            // ureq derives Basic auth from URL userinfo, so reject it on every hop, too.
            nightly_safe_url(&current).map_err(failed)?;
            let remaining =
                timeout.checked_sub(started.elapsed()).filter(|d| !d.is_zero()).ok_or_else(|| failed("Nightly request timed out".into()))?;
            let mut req = agent.get(&current).config().timeout_global(Some(remaining)).max_redirects(0).build();
            if api {
                req = req.header("Accept", "application/vnd.github+json").header("X-GitHub-Api-Version", "2022-11-28");
                if physical == url
                    && current == url
                    && let Some(token) = &self.token
                {
                    req = req.header("Authorization", &format!("Bearer {token}"));
                }
            }
            let mut resp = req.call().map_err(|e| failed(format!("{UNREACHABLE} Nightly resource ({e})")))?;
            let header = |name: &str| resp.headers().get(name).and_then(|v| v.to_str().ok());
            match resp.status().as_u16() {
                200 => {
                    let announced = header("content-length").and_then(|v| v.parse::<u64>().ok());
                    if announced.is_some_and(|n| n > max as u64) {
                        return Err(failed(format!("Nightly resource sent more than {max} bytes")));
                    }
                    let bytes = read_bytes_capped(&mut resp, max).map_err(|e| failed(format!("unreadable Nightly answer ({e})")))?;
                    if started.elapsed() >= timeout {
                        return Err(failed("Nightly request timed out".into()));
                    }
                    return Ok(Some(bytes));
                }
                404 => return Ok(None),
                status @ (403 | 429) => {
                    let now = receipt_now();
                    let retry = header("retry-after").and_then(|v| nightly_retry_after(v, now));
                    let reset = header("x-ratelimit-reset").and_then(|v| v.trim().parse::<i64>().ok());
                    let empty = header("x-ratelimit-remaining").map(str::trim) == Some("0");
                    let retry_at = if empty {
                        reset.max(retry).or(Some(now.saturating_add(60)))
                    } else {
                        retry.or_else(|| (status == 429).then(|| now.saturating_add(60)))
                    };
                    return Err(NightlyHttpError { why: format!("Nightly resource answered HTTP {status}"), retry_at });
                }
                status @ (301 | 302 | 303 | 307 | 308) => {
                    // API redirects are refused even without a token. The fixed debug mirror
                    // also cannot widen its allowlist through a physical Location response.
                    if api || self.nightly_mirror.is_some() || redirects == 3 {
                        return Err(failed(format!("Nightly resource refused redirect (HTTP {status})")));
                    }
                    let target = header("location").ok_or_else(|| failed("Nightly redirect has no Location".into()))?;
                    let next = nightly_redirect(&current, target).map_err(failed)?;
                    // Production assets stay on TLS; a test fixture may redirect only to loopback HTTP.
                    if !next.starts_with("https://") && !(is_loopback(&current) && is_loopback(&next)) {
                        return Err(failed("Nightly redirect must retain HTTPS".into()));
                    }
                    current = next;
                }
                status => return Err(failed(format!("Nightly resource answered HTTP {status}"))),
            }
        }
        Err(failed("Nightly redirect budget exhausted".into()))
    }

    /// Catalog resources remain on the pinned HTTPS website; redirect responses are refused.
    pub fn get_catalog_bytes(&self, url: &str, max: usize) -> Result<Option<Vec<u8>>, String> {
        if !url.starts_with("https://getartcraft.com/") {
            return Err("catalog resources must use the pinned HTTPS website".into());
        }
        Self::get_bytes_with(&self.no_redirect, url, max)
    }

    fn get_bytes_with(agent: &ureq::Agent, url: &str, max: usize) -> Result<Option<Vec<u8>>, String> {
        let mut resp = agent.get(url).call().map_err(|e| format!("{UNREACHABLE} {} ({e})", host(url)))?;
        match resp.status().as_u16() {
            200 => {
                let announced = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
                let too_big = || format!("{} sent more than {max} bytes", host(url));
                if announced.is_some_and(|n| n > max as u64) {
                    return Err(too_big());
                }
                let body = read_bytes_capped(&mut resp, max)
                    .map_err(|e| if e.contains("larger than") { too_big() } else { format!("unreadable answer ({e})") })?;
                Ok(Some(body))
            }
            404 => Ok(None),
            s => Err(format!("{} answered HTTP {s}", host(url))),
        }
    }

    /// Where `url` redirects to, without following it.
    pub fn redirect_target(&self, url: &str) -> Result<Option<String>, String> {
        let resp = self.no_redirect.head(url).call().map_err(|e| format!("{UNREACHABLE} {} ({e})", host(url)))?;
        match resp.status().as_u16() {
            300..=399 => Ok(resp.headers().get("location").and_then(|v| v.to_str().ok()).map(str::to_string)),
            404 => Ok(None),
            s => Err(format!("{} answered HTTP {s}", host(url))),
        }
    }

    /// Downloads `url` to `dest`, checking it against `sha256` when a checksum is published (and
    /// always against `size`, when known).
    ///
    /// Tolerant by design: an interrupted or stalled transfer resumes where it stopped (HTTP range
    /// requests) with backoff; a paused download, or one left by an earlier run, continues from
    /// its partial file; a file that fails its checksum is fetched once more from scratch before
    /// giving up. When the connection stays down, the download ends paused (`PAUSED`), never
    /// losing what it has. `stop` pauses or cancels it between reads.
    pub fn download(&self, url: &str, dest: &Path, size: u64, sha256: &str, stop: &Stop, progress: &mut dyn FnMut(Transfer)) -> Result<(), String> {
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("couldn't create {} ({e})", dir.display()))?;
        }
        // A finished copy from an earlier attempt that never got installed. Marking it as just
        // used, before checking it, keeps `clean_downloads` away from it while it's installed.
        if dest.is_file() && touch(dest) && matches_published(dest, sha256) {
            progress(Transfer::Bytes { done: size, total: size });
            return Ok(());
        }
        // The partial file is named after the release file and its size; if it still turns out
        // to belong to something else, the checksum catches it and the download starts over.
        let part = part_path(dest, size);
        let mut fresh_start_used = false;
        loop {
            if let Err(e) = self.fetch_resuming(url, &part, size, stop, progress) {
                if e == CANCELLED {
                    let _ = std::fs::remove_file(&part);
                } else if e != PAUSED {
                    log::warn!("download of {url} failed: {e}");
                }
                return Err(e);
            }
            if matches_published(&part, sha256) {
                if let Err(e) = rename_retrying(&part, dest, stop) {
                    if e == CANCELLED {
                        let _ = std::fs::remove_file(&part);
                    }
                    return Err(e);
                }
                return Ok(());
            }
            let _ = std::fs::remove_file(&part);
            if fresh_start_used {
                return Err("the download doesn't match its published checksum, so it wasn't installed".into());
            }
            // A resumed file can be spliced wrongly by a misbehaving proxy: try once from zero.
            log::warn!("checksum mismatch for {url}; downloading again from the start");
            fresh_start_used = true;
        }
    }

    /// Fetches `url` into `part`, resuming from whatever `part` already holds. Retries while the
    /// transfer keeps making progress; after several attempts in a row get nowhere, it stops and
    /// reports `PAUSED` so the user can resume later.
    fn fetch_resuming(&self, url: &str, part: &Path, size: u64, stop: &Stop, progress: &mut dyn FnMut(Transfer)) -> Result<(), String> {
        let mut fruitless = 0u32;
        loop {
            stop.check()?;
            let have = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);
            if fruitless >= MAX_FRUITLESS_ATTEMPTS {
                log::warn!("{url}: no progress after {fruitless} attempts; pausing");
                return Err(PAUSED.into());
            }
            if fruitless > 0 {
                progress(Transfer::Reconnecting { done: have, total: size });
                // 1 s, 2 s, 4 s … 30 s, staying responsive to Pause and Cancel.
                let wait = Duration::from_secs((1u64 << (fruitless - 1).min(5)).min(30));
                let until = std::time::Instant::now() + wait;
                while std::time::Instant::now() < until {
                    stop.check()?;
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            if size > 0 && have == size {
                return Ok(());
            }
            if size > 0 && have > size {
                std::fs::remove_file(part).map_err(|e| format!("couldn't save the download ({e})"))?;
                continue;
            }
            match self.attempt(url, part, have, size, stop, progress) {
                Ok(()) => return Ok(()),
                Err(AttemptError::Stopped(e)) => return Err(e),
                Err(AttemptError::Fatal(e)) => return Err(e),
                Err(AttemptError::Retry { error, progressed }) => {
                    log::info!("{url}: {error}; retrying");
                    fruitless = if progressed { 1 } else { fruitless + 1 };
                }
            }
        }
    }

    /// One request: appends to `part` from byte `have`.
    fn attempt(&self, url: &str, part: &Path, have: u64, size: u64, stop: &Stop, progress: &mut dyn FnMut(Transfer)) -> Result<(), AttemptError> {
        let retry = |error: String, progressed: bool| AttemptError::Retry { error, progressed };
        // Binaries are already compressed; ask for the bytes exactly as published.
        let agent = if is_loopback(url) { &self.direct_download } else { &self.download };
        let (physical, agent) = self.request(url, agent).map_err(AttemptError::Fatal)?;
        let mut req = agent.get(physical.as_ref()).header("Accept-Encoding", "identity");
        if have > 0 {
            req = req.header("Range", &format!("bytes={have}-"));
        }
        let mut resp = match req.call() {
            Ok(r) => r,
            Err(e) => return Err(retry(format!("couldn't connect: {e}"), false)),
        };
        let status = resp.status().as_u16();
        let header = |name: &str| resp.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
        let (start, total) = match status {
            200 => (0, header("content-length").and_then(|v| v.parse::<u64>().ok()).filter(|n| *n > 0).unwrap_or(size)),
            206 => match header("content-range").as_deref().and_then(parse_content_range) {
                Some((start, Some(total))) => (start, total),
                Some((start, None)) => (start, size),
                None => return Err(retry("the server sent an unreadable partial response".into(), false)),
            },
            // The range is past the end: the part file is already complete (or wrong; the
            // checksum decides).
            416 if have > 0 => return Ok(()),
            404 | 410 => return Err(AttemptError::Fatal("the file is no longer on the release page".into())),
            401 | 403 => return Err(retry(format!("the server refused the download (HTTP {status})"), false)),
            s => return Err(retry(format!("HTTP {s}"), false)),
        };
        if size > 0 && total != size {
            return Err(AttemptError::Fatal(format!("the server offered {total} bytes, but the release lists {size}")));
        }
        if let Some(enc) = header("content-encoding")
            && !enc.eq_ignore_ascii_case("identity")
        {
            return Err(AttemptError::Fatal(format!("unexpected Content-Encoding '{enc}' on binary artifact download")));
        }
        let limit = if total > 0 { total } else { MAX_DOWNLOAD };
        let appends = status == 206 && start == have;
        if appends && have > limit {
            return Err(AttemptError::Fatal(format!("the download exceeds the permitted size of {limit} bytes")));
        }
        // Open the part file at the right place: a 200 restarts it, a 206 continues it.
        let mut file = match (status, start) {
            (206, s) if s == have => std::fs::OpenOptions::new().append(true).open(part),
            _ => std::fs::File::create(part),
        }
        .map_err(|e| AttemptError::Fatal(format!("couldn't save the download ({e})")))?;
        let mut done = if status == 206 && start == have { have } else { 0 };
        // We check the length and checksum ourselves; the reader's limit is only a backstop
        // against an endless body. (ureq errors once the limit is *reached*, so leave headroom.)
        let remaining = limit.saturating_sub(done);
        let mut reader = resp.body_mut().with_config().limit(remaining.saturating_add(1 << 20)).reader();
        let mut buf = vec![0u8; 256 * 1024];
        let at_start = done;
        progress(Transfer::Bytes { done, total });
        loop {
            stop.check().map_err(AttemptError::Stopped)?;
            let n = match reader.read(&mut buf) {
                Ok(n) => n,
                Err(e) => {
                    let _ = file.sync_all();
                    return Err(retry(format!("the connection dropped: {e}"), done > at_start));
                }
            };
            if n == 0 {
                break;
            }
            let next_done = done.saturating_add(n as u64);
            if next_done > limit {
                return Err(AttemptError::Fatal(format!("the server sent more than the permitted size of {limit} bytes")));
            }
            let chunk = buf.get(..n).ok_or_else(|| AttemptError::Fatal("read past the buffer".into()))?;
            file.write_all(chunk).map_err(|e| AttemptError::Fatal(format!("couldn't save the download ({e}). Is the disk full?")))?;
            done = next_done;
            progress(Transfer::Bytes { done, total });
        }
        file.sync_all().map_err(|e| AttemptError::Fatal(format!("couldn't save the download ({e})")))?;
        if total > 0 && done < total {
            return Err(retry(format!("the connection closed early ({done} of {total} bytes)"), done > at_start));
        }
        Ok(())
    }
}

/// Attempts in a row without any new bytes before pausing (about two minutes of backoff).
const MAX_FRUITLESS_ATTEMPTS: u32 = 8;
/// The longest one request may spend receiving; the next request resumes.
const ATTEMPT_BODY_TIME: Duration = Duration::from_secs(5 * 60);
/// Backstop when the server doesn't say how big the file is.
const MAX_DOWNLOAD: u64 = 16 << 30;

/// What a download reports as it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transfer {
    Bytes {
        done: u64,
        total: u64,
    },
    /// The connection dropped; waiting before trying again.
    Reconnecting {
        done: u64,
        total: u64,
    },
}

/// Asks a running download to pause (keeping its partial file) or cancel (deleting it). Shared with
/// `craft_update`, so a self-update sees the same pause and cancel.
pub use craft_update::Stop;

/// Whether a file on disk is the published one: its SHA-256 matches.
fn matches_published(path: &Path, sha256: &str) -> bool {
    sha256_file(path).is_ok_and(|h| h.eq_ignore_ascii_case(sha256))
}

/// How much of a download is already on disk (a paused or interrupted one).
pub fn partial_bytes(dest: &Path, size: u64) -> Option<u64> {
    std::fs::metadata(part_path(dest, size)).ok().map(|m| m.len()).filter(|n| *n > 0)
}

/// Renames `from` to `to`, retrying for a few seconds while Windows reports the file busy: an
/// antivirus scanner often holds a just-written installer open, and the rename then fails with
/// "Access denied" (os error 5) or a sharing violation (os error 32). `stop` pauses or cancels
/// it between tries.
fn rename_retrying(from: &Path, to: &Path, stop: &Stop) -> Result<(), String> {
    let mut wait = Duration::from_millis(100);
    loop {
        stop.check()?;
        match std::fs::rename(from, to) {
            Err(e) if cfg!(windows) && matches!(e.raw_os_error(), Some(5 | 32)) && wait <= Duration::from_secs(4) => {
                log::info!("{} is busy ({e}); retrying the rename", to.display());
                std::thread::sleep(wait);
                wait = wait.saturating_mul(2);
            }
            result => return result.map_err(|e| format!("couldn't save the download ({e})")),
        }
    }
}

/// Sets a file's modification time to now.
fn touch(path: &Path) -> bool {
    std::fs::File::options().write(true).open(path).and_then(|f| f.set_modified(std::time::SystemTime::now())).is_ok()
}

/// Whether a finished download is waiting at `dest`. Judged by size alone: `download` checks it
/// against the published checksum before anything uses it.
pub fn finished(dest: &Path, size: u64) -> bool {
    std::fs::metadata(dest).is_ok_and(|m| m.is_file() && m.len() == size)
}

/// Removes leftovers from the download folder: partial files in an old naming scheme, paused
/// downloads untouched for `partial_max_age`, and finished ones (kept after a failed install)
/// untouched for `finished_max_age` (they're for releases long since replaced, or given up on).
/// Files written within the last minute are always kept, so a running download is never touched.
pub fn clean_downloads(dir: &Path, partial_max_age: Duration, finished_max_age: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let age = meta.modified().ok().and_then(|m| now.duration_since(m).ok()).unwrap_or_default();
        if age < Duration::from_secs(60) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        // Current partial files are `<file>.<size>.part`.
        let current_part = name
            .strip_suffix(".part")
            .and_then(|stem| stem.rsplit_once('.'))
            .is_some_and(|(_, size)| !size.is_empty() && size.bytes().all(|b| b.is_ascii_digit()));
        let legacy_part = name.ends_with(".part") && !current_part;
        let max_age = if name.ends_with(".part") { partial_max_age } else { finished_max_age };
        if legacy_part || age > max_age {
            log::info!("removing stale download {}", path.display());
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Throws away a paused download, and a finished one kept for another try at installing.
pub fn discard(dest: &Path, size: u64) {
    let _ = std::fs::remove_file(part_path(dest, size));
    let _ = std::fs::remove_file(dest);
}

enum AttemptError {
    /// Paused or cancelled.
    Stopped(String),
    /// Won't get better by trying again.
    Fatal(String),
    Retry {
        error: String,
        progressed: bool,
    },
}

/// `<dest>.<size>.part`: where an unfinished download of `dest` is kept.
pub(crate) fn part_path(dest: &Path, size: u64) -> std::path::PathBuf {
    let tag = size.to_string();
    let mut name = dest.file_name().map(std::ffi::OsStr::to_os_string).unwrap_or_default();
    name.push(format!(".{tag}.part"));
    dest.with_file_name(name)
}

/// `bytes 100-199/1000` → (100, Some(1000)); `bytes 100-199/*` → (100, None).
fn parse_content_range(v: &str) -> Option<(u64, Option<u64>)> {
    let rest = v.trim().strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let start = range.split_once('-')?.0.trim().parse().ok()?;
    let total = if total.trim() == "*" { None } else { Some(total.trim().parse().ok()?) };
    Some((start, total))
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// SHA-256 of a file on disk.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("couldn't read {} ({e})", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|e| format!("couldn't read {} ({e})", path.display()))?;
    Ok(hex(&hasher.finalize()))
}

/// Whether a 403 or 429 from GitHub's API means "slow down", and until when (Unix seconds, when
/// it says). `None`: refused for some other reason.
///
/// GitHub sends `x-ratelimit-reset` with every answer: it's when the hourly quota refills, which
/// only matters once `x-ratelimit-remaining` is 0. A secondary limit says how long to wait in
/// `Retry-After` (seconds) and leaves the hourly quota alone; without either, GitHub's advice is
/// to wait a minute.
fn refused_until(status: u16, remaining: Option<&str>, reset: Option<i64>, retry_after: Option<i64>, now: i64) -> Option<Option<i64>> {
    let retry = retry_after.map(|secs| now.saturating_add(secs.max(0)));
    if remaining.map(str::trim) == Some("0") {
        // `None` sorts first: whichever of the two is known, and the later if both are.
        return Some(reset.max(retry));
    }
    if retry.is_some() {
        return Some(retry);
    }
    (status == 429).then_some(Some(now.saturating_add(60)))
}

/// Reject implicit credentials and non-HTTP transports before ureq sees a URL.
fn nightly_safe_url(url: &str) -> Result<(), String> {
    let uri = url.parse::<ureq::http::Uri>().map_err(|_| "Invalid Nightly resource URL")?;
    if !matches!(uri.scheme_str(), Some("https")) && !is_loopback(url) {
        return Err("Nightly resources must use HTTPS or an explicit loopback fixture".into());
    }
    if uri.authority().is_none_or(|a| a.as_str().contains('@')) {
        return Err("Nightly resource URLs must not contain credentials".into());
    }
    Ok(())
}

/// GitHub uses absolute asset redirects. Also support ordinary relative fixture Locations;
/// every resulting URL is validated before a request and never inherits an Authorization header.
fn nightly_redirect(current: &str, target: &str) -> Result<String, String> {
    if target.contains(['\\', '#']) || target.is_empty() {
        return Err("Invalid Nightly redirect URL".into());
    }
    let uri = current.parse::<ureq::http::Uri>().map_err(|_| "Invalid Nightly resource URL")?;
    let scheme = uri.scheme_str().ok_or("Invalid Nightly resource scheme")?;
    let authority = uri.authority().ok_or("Invalid Nightly resource authority")?;
    if target.starts_with("//") {
        return Ok(format!("{scheme}:{target}"));
    }
    if target.contains("://") {
        return Ok(target.to_string());
    }
    if target.starts_with('/') {
        return Ok(format!("{scheme}://{authority}{target}"));
    }
    if target.starts_with('?') {
        return Ok(format!("{scheme}://{authority}{}{target}", uri.path()));
    }
    let (parent, _) = uri.path().rsplit_once('/').unwrap_or(("", ""));
    Ok(format!("{scheme}://{authority}{parent}/{target}"))
}

/// Retry-After accepts seconds or the current HTTP-date format. Calendar conversion stays in
/// core's existing jiff-backed UTC parser; no new native dependency or calendar arithmetic.
fn nightly_retry_after(value: &str, now: i64) -> Option<i64> {
    let value = value.trim();
    if value.bytes().all(|b| b.is_ascii_digit())
        && let Ok(seconds) = value.parse::<i64>()
    {
        return Some(now.saturating_add(seconds));
    }
    let parts: Vec<_> = value.split_ascii_whitespace().collect();
    let [weekday, day, month, year, clock, "GMT"] = parts.as_slice() else { return None };
    if !matches!(*weekday, "Mon," | "Tue," | "Wed," | "Thu," | "Fri," | "Sat," | "Sun,")
        || day.len() != 2
        || year.len() != 4
        || clock.len() != 8
        || clock.as_bytes().get(2) != Some(&b':')
        || clock.as_bytes().get(5) != Some(&b':')
    {
        return None;
    }
    let month = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"].iter().position(|m| m == month)? + 1;
    let text = format!("{year}-{month:02}-{day}T{clock}Z");
    let seconds = artcraft_launcher_core::time::parse_rfc3339(&text)?;
    let (actual_year, actual_month, actual_day) = artcraft_launcher_core::time::civil_from_unix(seconds);
    if actual_year != year.parse::<i64>().ok()? || actual_month as usize != month || actual_day != day.parse::<u32>().ok()? {
        return None;
    }
    Some(seconds.max(now))
}

/// The host of `url` for error messages, or all of it when it has no scheme.
fn host(url: &str) -> &str {
    url.split_once("://").and_then(|(_, rest)| rest.split('/').next()).unwrap_or(url)
}

/// How every error in this module starts when the request didn't complete at all: no name
/// resolution, no route, a refused connection, a dropped handshake, nothing within the timeout.
/// Kept next to the errors it describes so the two can't drift apart. (`Http::attempt`'s
/// "couldn't connect" is deliberately outside it: that one never leaves the download loop.)
const UNREACHABLE: &str = "couldn't reach";

/// Whether one of this module's GETs ended without the host completing an answer, as opposed to
/// answering with something unwelcome (an HTTP status, a body that couldn't be read). A caller
/// with a second host to try uses this to tell "ask the other one" from "this network is going
/// nowhere". Erring towards `true` only costs that caller one skipped retry.
pub fn is_unreachable(error: &str) -> bool {
    error.starts_with(UNREACHABLE)
}

/// Reads a text body, rejecting one whose *decoded* size exceeds `max`.
///
/// ureq's own `limit` guards the *compressed* stream: it sits below the transparent gzip decoder,
/// so a few hundred wire bytes can still expand far past the cap. So we cap the fully decoded
/// reader with `take` instead, one byte past the cap, and refuse an oversized answer outright —
/// before it is parsed — instead of truncating it into something that merely fails to parse
/// (CWE-409). Invalid UTF-8 stays an error, as before.
fn read_text_capped(resp: &mut ureq::http::Response<ureq::Body>, max: u64) -> Result<String, String> {
    let mut reader = resp.body_mut().with_config().reader().take(max.saturating_add(1));
    let mut text = String::new();
    reader.read_to_string(&mut text).map_err(|e| e.to_string())?;
    if text.len() as u64 > max {
        return Err(format!("the answer is larger than {max} bytes"));
    }
    Ok(text)
}

/// Reads a binary body, rejecting one whose decoded size exceeds `max` (CWE-409).
fn read_bytes_capped(resp: &mut ureq::http::Response<ureq::Body>, max: usize) -> Result<Vec<u8>, String> {
    let mut reader = resp.body_mut().with_config().reader().take((max as u64).saturating_add(1));
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() > max {
        return Err(format!("the answer is larger than {max} bytes"));
    }
    Ok(bytes)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn nightly_test_mirror_rejects_raw_and_encoded_dot_base_paths() {
        assert!(NightlyMirror::new("http://127.0.0.1:1/fixtures").is_ok());
        for base in ["http://127.0.0.1:1/a/../", "http://127.0.0.1:1/a/./", "http://127.0.0.1:1/%2e%2e/", "http://127.0.0.1:1/a%2fb/"] {
            assert!(NightlyMirror::new(base).is_err(), "{base}");
        }
    }

    #[test]
    fn nightly_test_transport_cannot_change_the_logical_repository_or_escape_its_asset_path() {
        let mirror = NightlyMirror::new("http://127.0.0.1:9021/fixture").unwrap();
        assert_eq!(
            mirror.request_url("https://api.github.com/repos/storytold/photocraft/releases?per_page=100").unwrap(),
            "http://127.0.0.1:9021/fixture/nightly/photocraft/releases.json"
        );
        assert_eq!(
            mirror.request_url("https://github.com/storytold/photocraft/releases/download/v1.0.0-nightly.1/NIGHTLY.json").unwrap(),
            "http://127.0.0.1:9021/fixture/nightly/photocraft/assets/v1.0.0-nightly.1/NIGHTLY.json"
        );
        for logical in [
            "https://api.github.com/repos/storytold/filmcraft/releases?per_page=100",
            "https://github.com/storytold/photocraft/releases/download/../NIGHTLY.json",
            "https://github.com/storytold/photocraft/releases/download/v1/%2e%2e",
            "https://github.com/storytold/photocraft/releases/download/v1/a/b.zip",
            "https://github.com/storytold/photocraft/releases/download/v1/a.zip?redirect=1",
            "http://127.0.0.1:9021/direct.zip",
        ] {
            assert!(mirror.request_url(logical).is_err(), "{logical}");
        }
        for base in ["https://github.com/", "http://127.0.0.1.evil:9021/", "http://user@localhost:9021/", "http://localhost:9021/#fragment"] {
            assert!(NightlyMirror::new(base).is_err(), "{base}");
        }
    }

    #[test]
    fn nightly_mirror_api_requests_never_forward_a_github_token_or_follow_redirects() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let mut captured = String::new();
            for line in std::io::BufRead::lines(std::io::BufReader::new(&stream)) {
                let line = line.unwrap();
                if line.is_empty() {
                    break;
                }
                captured.push_str(&line);
                captured.push('\n');
            }
            send.send(captured).unwrap();
            stream
                .write_all(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/not-followed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
        });
        let mut http = Http::new().unwrap();
        http.token = Some("test-only-not-a-real-token".into());
        http.nightly_mirror = Some(NightlyMirror::new(&base).unwrap());
        let error = http.get_api("https://api.github.com/repos/storytold/photocraft/releases?per_page=100", None).unwrap_err();
        assert!(error.contains("HTTP 302"), "{error}");
        let request = receive.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(request.starts_with("GET /nightly/photocraft/releases.json "));
        assert!(!request.to_ascii_lowercase().contains("authorization:"));
        assert!(!request.contains("test-only-not-a-real-token"));
    }

    // These synthetic HTTP responses and payloads are authored here, not copied from a feed.
    // Each fixture accepts exactly its scripted requests; no external host is contacted.
    fn nightly_fixture(responses: Vec<(Duration, Vec<u8>)>) -> (String, std::sync::mpsc::Receiver<String>, std::thread::JoinHandle<()>) {
        nightly_fixture_before_response(responses, || {})
    }

    fn nightly_fixture_before_response(
        responses: Vec<(Duration, Vec<u8>)>,
        mut before_response: impl FnMut() + Send + 'static,
    ) -> (String, std::sync::mpsc::Receiver<String>, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (send, receive) = std::sync::mpsc::channel();
        listener.set_nonblocking(true).unwrap();
        let worker = std::thread::spawn(move || {
            for (delay, response) in responses {
                let until = std::time::Instant::now() + Duration::from_secs(3);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && std::time::Instant::now() < until => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(e) => panic!("Nightly fixture did not receive its scripted request: {e}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                let mut captured = String::new();
                for line in std::io::BufRead::lines(std::io::BufReader::new(&stream)) {
                    let line = line.unwrap();
                    if line.is_empty() {
                        break;
                    }
                    captured.push_str(&line);
                    captured.push('\n');
                }
                send.send(captured).unwrap();
                std::thread::sleep(delay);
                before_response();
                let _ = stream.write_all(&response);
            }
        });
        (base, receive, worker)
    }

    fn nightly_response(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
        let mut response = format!("HTTP/1.1 {status}\r\n{headers}Connection: close\r\n\r\n").into_bytes();
        response.extend_from_slice(body);
        response
    }

    #[test]
    fn nightly_fixture_accepts_delayed_request_bytes_after_connect() {
        let (base, seen, worker) = nightly_fixture(vec![(Duration::ZERO, nightly_response("200 OK", "Content-Length: 2\r\n", b"ok"))]);
        let mut client = std::net::TcpStream::connect(base.strip_prefix("http://").unwrap()).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        // Let the fixture accept before the first byte arrives. Windows accepts inherit
        // the listener's nonblocking mode unless the fixture explicitly clears it.
        std::thread::sleep(Duration::from_secs(1));
        client.write_all(b"GET /delayed HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).unwrap();
        assert!(response.ends_with(b"ok"));
        assert!(seen.recv_timeout(Duration::from_secs(2)).unwrap().starts_with("GET /delayed HTTP/1.1"));
        worker.join().unwrap();
    }

    #[test]
    fn nightly_get_distinguishes_missing_from_indeterminate_and_preserves_backoff() {
        let now = 1_800_000_000;
        for (status, headers, expected) in [
            ("403 Forbidden", "Retry-After: 90\r\nX-RateLimit-Remaining: 7\r\nX-RateLimit-Reset: 1800000900\r\n", Some(now + 90)),
            ("403 Forbidden", "Retry-After: 90\r\nX-RateLimit-Remaining: 0\r\nX-RateLimit-Reset: 1800000900\r\n", Some(now + 900)),
            ("403 Forbidden", "X-RateLimit-Remaining: 0\r\n", Some(now + 60)),
            ("429 Too Many Requests", "", Some(now + 60)),
            ("403 Forbidden", "Retry-After: invalid\r\n", None),
            ("500 Server Error", "Retry-After: 90\r\n", None),
        ] {
            let (base, seen, worker) = nightly_fixture(vec![(Duration::ZERO, nightly_response(status, headers, b""))]);
            let error = Http::new().unwrap().nightly_get_with_receipt_clock(&base, 8, Duration::from_secs(2), || now).unwrap_err();
            assert!(error.why.contains(status.split_whitespace().next().unwrap()), "{}", error.why);
            assert_eq!(error.retry_at, expected);
            assert!(seen.recv_timeout(Duration::from_secs(2)).unwrap().starts_with("GET / "));
            worker.join().unwrap();
        }
        let (base, seen, worker) = nightly_fixture(vec![(Duration::ZERO, nightly_response("404 Not Found", "", b""))]);
        assert!(Http::new().unwrap().nightly_get(&base, 8, Duration::from_secs(2), now).unwrap().is_none());
        seen.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn nightly_backoff_uses_final_response_receipt_after_queue_and_redirect_delays() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicI64, Ordering};

        let lookup_at = artcraft_launcher_core::time::parse_rfc3339("2026-11-21T00:00:00Z").unwrap();
        let queued_seconds = 90;
        let hop_seconds = 37;
        let received_at = lookup_at + queued_seconds + 2 * hop_seconds;
        for (status, headers, expected) in [
            ("403 Forbidden", "Retry-After: 120\r\n", received_at + 120),
            ("429 Too Many Requests", "Retry-After: 120\r\n", received_at + 120),
            ("429 Too Many Requests", "", received_at + 60),
            ("429 Too Many Requests", "Retry-After: invalid\r\n", received_at + 60),
            ("403 Forbidden", "X-RateLimit-Remaining: 0\r\nRetry-After: invalid\r\n", received_at + 60),
            ("429 Too Many Requests", "Retry-After: Sat, 21 Nov 2026 00:10:00 GMT\r\n", lookup_at + 600),
            ("403 Forbidden", "X-RateLimit-Remaining: 0\r\nX-RateLimit-Reset: 1\r\n", 1),
            ("403 Forbidden", "X-RateLimit-Remaining: 0\r\nX-RateLimit-Reset: 1\r\nRetry-After: 120\r\n", received_at + 120),
        ] {
            // The lookup baseline is already stale before transport starts. Advance the injected
            // wall clock only after each real request arrives and before releasing its response.
            // Exact assertions use that clock, never the scheduler's observed sleep duration.
            let clock = Arc::new(AtomicI64::new(lookup_at + queued_seconds));
            let server_clock = clock.clone();
            let (base, seen, worker) = nightly_fixture_before_response(
                vec![
                    (Duration::from_millis(10), nightly_response("302 Found", "Location: /limited\r\nContent-Length: 0\r\n", b"")),
                    (Duration::from_millis(10), nightly_response(status, headers, b"")),
                ],
                move || {
                    server_clock.fetch_add(hop_seconds, Ordering::SeqCst);
                },
            );
            let samples = std::cell::Cell::new(0);
            let error = Http::new()
                .unwrap()
                .nightly_get_with_receipt_clock(&base, 8, Duration::from_secs(2), || {
                    samples.set(samples.get() + 1);
                    clock.load(Ordering::SeqCst)
                })
                .unwrap_err();
            assert_eq!(error.retry_at, Some(expected), "{status}: {headers}");
            assert_eq!(samples.get(), 1, "sample only the throttling response, not admission or redirects");
            assert!(seen.recv_timeout(Duration::from_secs(2)).unwrap().starts_with("GET / "));
            assert!(seen.recv_timeout(Duration::from_secs(2)).unwrap().starts_with("GET /limited "));
            worker.join().unwrap();
        }
    }

    #[test]
    fn nightly_backoff_default_clock_ignores_stale_lookup_timestamp() {
        let (base, seen, worker) = nightly_fixture(vec![(Duration::from_millis(10), nightly_response("429 Too Many Requests", "", b""))]);
        let before = crate::now();
        let error = Http::new().unwrap().nightly_get(&base, 8, Duration::from_secs(2), 0).unwrap_err();
        let after = crate::now();
        let retry_at = error.retry_at.unwrap();
        assert!((before.saturating_add(60)..=after.saturating_add(60)).contains(&retry_at));
        seen.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn nightly_get_caps_announced_unannounced_and_decoded_bodies() {
        let mut compressed = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        compressed.write_all(&[b'A'; 4096]).unwrap();
        let gzip = compressed.finish().unwrap();
        for response in [
            nightly_response("200 OK", "Content-Length: 101\r\n", &[b'A'; 101]),
            nightly_response("200 OK", "", &[b'A'; 101]),
            nightly_response("200 OK", "Content-Encoding: gzip\r\n", &gzip),
        ] {
            let (base, seen, worker) = nightly_fixture(vec![(Duration::ZERO, response)]);
            let error = Http::new().unwrap().nightly_get(&base, 100, Duration::from_secs(2), 0).unwrap_err();
            assert!(error.why.contains("bytes"), "{}", error.why);
            assert!(error.retry_at.is_none());
            seen.recv_timeout(Duration::from_secs(2)).unwrap();
            worker.join().unwrap();
        }
        for body in [b"".as_slice(), b"abcd".as_slice()] {
            let (base, seen, worker) = nightly_fixture(vec![(Duration::ZERO, nightly_response("200 OK", "", body))]);
            assert_eq!(Http::new().unwrap().nightly_get(&base, body.len(), Duration::from_secs(2), 0).unwrap(), Some(body.to_vec()));
            seen.recv_timeout(Duration::from_secs(2)).unwrap();
            worker.join().unwrap();
        }
    }

    #[test]
    fn nightly_get_stops_redirects_after_three_and_never_attaches_asset_credentials() {
        let redirect = nightly_response("302 Found", "Location: /again\r\nContent-Length: 0\r\n", b"");
        let (base, seen, worker) = nightly_fixture(vec![(Duration::ZERO, redirect); 4]);
        let mut http = Http::new().unwrap();
        http.token = Some("test-only-not-a-real-token".into());
        let error = http.nightly_get(&base, 8, Duration::from_secs(2), 0).unwrap_err();
        assert!(error.why.contains("refused redirect"), "{}", error.why);
        for _ in 0..4 {
            let request = seen.recv_timeout(Duration::from_secs(2)).unwrap().to_ascii_lowercase();
            assert!(!request.contains("authorization:"));
            assert!(!request.contains("test-only-not-a-real-token"));
        }
        worker.join().unwrap();
    }

    #[test]
    fn nightly_mirror_api_redirects_are_refused_with_or_without_a_token() {
        for token in [None, Some("test-only-not-a-real-token".to_string())] {
            let (base, seen, worker) = nightly_fixture(vec![(Duration::ZERO, nightly_response("302 Found", "Location: /unexpected\r\n", b""))]);
            let mut http = Http::new().unwrap();
            http.token = token;
            http.nightly_mirror = Some(NightlyMirror::new(&base).unwrap());
            let error = http
                .nightly_get("https://api.github.com/repos/storytold/photocraft/releases?per_page=100", 8, Duration::from_secs(2), 0)
                .unwrap_err();
            assert!(error.why.contains("refused redirect"), "{}", error.why);
            let request = seen.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(request.starts_with("GET /nightly/photocraft/releases.json "));
            assert!(!request.to_ascii_lowercase().contains("authorization:"));
            worker.join().unwrap();
        }
    }

    #[test]
    fn nightly_mirror_pagination_stays_in_the_fixed_fixture_allowlist() {
        let mirror = NightlyMirror::new("http://127.0.0.1:9021/fixture").unwrap();
        for page in 2..=5 {
            assert_eq!(
                mirror.request_url(&format!("https://api.github.com/repos/storytold/photocraft/releases?per_page=100&page={page}")).unwrap(),
                format!("http://127.0.0.1:9021/fixture/nightly/photocraft/releases-page-{page}.json")
            );
        }
        for query in ["per_page=100&page=1", "per_page=100&page=6", "per_page=100&page=02", "page=2&per_page=100", "per_page=100&page=2&extra=1"] {
            assert!(mirror.request_url(&format!("https://api.github.com/repos/storytold/photocraft/releases?{query}")).is_err());
        }
    }

    #[test]
    fn nightly_get_redirects_can_succeed_but_userinfo_is_never_sent() {
        let (base, seen, worker) = nightly_fixture(vec![
            (Duration::ZERO, nightly_response("302 Found", "Location: /body\r\nContent-Length: 0\r\n", b"")),
            (Duration::ZERO, nightly_response("200 OK", "Content-Length: 2\r\n", b"ok")),
        ]);
        assert_eq!(Http::new().unwrap().nightly_get(&base, 8, Duration::from_secs(2), 0).unwrap(), Some(b"ok".to_vec()));
        seen.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(seen.recv_timeout(Duration::from_secs(2)).unwrap().starts_with("GET /body "));
        worker.join().unwrap();
        let (base, seen, worker) =
            nightly_fixture(vec![(Duration::ZERO, nightly_response("302 Found", "Location: http://user:secret@127.0.0.1:9/refused\r\n", b""))]);
        let error = Http::new().unwrap().nightly_get(&base, 8, Duration::from_secs(2), 0).unwrap_err();
        assert!(error.retry_at.is_none());
        seen.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
        assert!(nightly_safe_url("https://user:secret@github.com/file").is_err());
    }

    #[test]
    fn nightly_request_timeout_includes_redirect_elapsed_time() {
        // Each hop alone fits the budget and the two together exceed it. The fixture's sleeps and
        // accept polling run late on a loaded Mac (a 5 ms poll took up to 25 ms, an 80 ms sleep up
        // to 153 ms), so the first hop gets 400 ms of room. Sleeps never return early, so the two
        // hops always exceed the budget by at least 200 ms.
        let delay = Duration::from_millis(600);
        let (base, seen, worker) = nightly_fixture(vec![
            (delay, nightly_response("302 Found", "Location: /slow\r\nContent-Length: 0\r\n", b"")),
            (delay, nightly_response("200 OK", "Content-Length: 2\r\n", b"ok")),
        ]);
        let error = Http::new().unwrap().nightly_get(&base, 8, Duration::from_millis(1000), 0).unwrap_err();
        assert!(error.retry_at.is_none());
        seen.recv_timeout(Duration::from_secs(2)).unwrap();
        seen.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
        // A spent budget cannot send even the first request.
        assert!(Http::new().unwrap().nightly_get("http://127.0.0.1:9/unused", 8, Duration::ZERO, 0).unwrap_err().why.contains("timed out"));
    }

    #[test]
    fn nightly_request_timeout_also_bounds_a_stalled_response_body() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            for line in std::io::BufRead::lines(std::io::BufReader::new(&stream)) {
                if line.unwrap().is_empty() {
                    break;
                }
            }
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\no").unwrap();
            std::thread::sleep(Duration::from_millis(200));
            let _ = stream.write_all(b"k");
        });
        let error = Http::new().unwrap().nightly_get(&base, 8, Duration::from_millis(80), 0).unwrap_err();
        assert!(error.retry_at.is_none());
        assert!(error.why.contains("unreadable") || error.why.contains("couldn't reach"), "{}", error.why);
        worker.join().unwrap();
    }

    #[test]
    fn nightly_retry_after_handles_seconds_dates_and_invalid_calendar_values() {
        let now = artcraft_launcher_core::time::parse_rfc3339("2026-11-21T00:00:00Z").unwrap();
        assert_eq!(nightly_retry_after("120", now), Some(now + 120));
        assert_eq!(nightly_retry_after("Sat, 21 Nov 2026 00:02:00 GMT", now), Some(now + 120));
        assert_eq!(nightly_retry_after("Sat, 21 Nov 2026 00:00:00 GMT", now + 1), Some(now + 1));
        for value in ["-1", "tomorrow", "Sat, 31 Feb 2026 00:00:00 GMT", "Sat, 21 Nov 2026 25:00:00 GMT"] {
            assert!(nightly_retry_after(value, now).is_none(), "{value}");
        }
    }

    const DAYS_30: Duration = Duration::from_secs(30 * 86_400);
    const DAYS_7: Duration = Duration::from_secs(7 * 86_400);

    #[test]
    fn host_is_what_follows_the_scheme() {
        assert_eq!(host("https://api.github.com/repos/storytold/wordcraft"), "api.github.com");
        assert_eq!(host("http://127.0.0.1:8080"), "127.0.0.1:8080");
        assert_eq!(host("not a url"), "not a url");
    }

    #[test]
    fn a_host_that_never_answered_is_told_from_one_that_said_no() {
        // A caller with a second host to try leans on this: only the first kind is worth retrying
        // somewhere else.
        let gone = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let http = Http::new().unwrap();
        let refused = http.get_text(&format!("http://{gone}/file.txt")).unwrap_err();
        assert!(is_unreachable(&refused), "{refused}");
        assert!(is_unreachable(&http.get_feed(&format!("http://{gone}/feed.json")).unwrap_err().why));
        assert!(is_unreachable(&http.redirect_target(&format!("http://{gone}/latest")).unwrap_err()));
        // A refusal is an answer, and so is a body that can't be read.
        assert!(!is_unreachable("GitHub refused the request (HTTP 403)"));
        assert!(!is_unreachable(&format!("{} answered HTTP 503", host("https://github.com/storytold/photocraft/releases/latest"))));
        assert!(!is_unreachable("unreadable answer from GitHub (the answer is larger than 4194304 bytes)"));
    }

    #[test]
    fn cleaning_keeps_current_partial_files_and_removes_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(3600);
        let make = |name: &str| {
            let p = dir.path().join(name);
            std::fs::write(&p, b"x").unwrap();
            std::fs::File::options().write(true).open(&p).unwrap().set_modified(old).unwrap();
            p
        };
        let keep = make("photocraft-0.5.0-macos-universal.dmg.150000.part");
        let legacy = make("photocraft-0.5.0-macos-universal.part");
        let finished = make("photocraft-0.5.0-macos-universal.dmg");
        let fresh = dir.path().join("x.part");
        std::fs::write(&fresh, b"x").unwrap();
        clean_downloads(dir.path(), DAYS_30, DAYS_7);
        assert!(keep.exists() && fresh.exists() && finished.exists());
        assert!(!legacy.exists());
        // A finished file goes sooner than a paused one.
        clean_downloads(dir.path(), DAYS_30, Duration::from_secs(60));
        assert!(keep.exists() && !finished.exists());
        clean_downloads(dir.path(), Duration::from_secs(60), DAYS_7);
        assert!(!keep.exists());
    }

    #[test]
    fn a_kept_file_that_still_matches_is_not_fetched_again() {
        let (body, sha) = body();
        let len = body.len() as u64;
        let (url, requests) = serve(body.clone(), vec![Reply::Full, Reply::Full]);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("file.bin");
        let http = Http::new().unwrap();
        http.download(&url, &dest, len, &sha, &Stop::default(), &mut |_| {}).unwrap();
        assert!(finished(&dest, len) && !finished(&dest, 0));
        // Old enough to be cleaned up, were it not about to be used.
        let old = std::time::SystemTime::now() - Duration::from_secs(40 * 86_400);
        std::fs::File::options().write(true).open(&dest).unwrap().set_modified(old).unwrap();
        let mut last = (0, 0);
        let again = http.download(&url, &dest, len, &sha, &Stop::default(), &mut |t| {
            if let Transfer::Bytes { done, total } = t {
                last = (done, total);
            }
        });
        assert_eq!(again, Ok(()));
        assert_eq!(last, (len, len));
        assert_eq!(requests.lock().unwrap().len(), 1);
        // A cleanup while the installer runs leaves the reused file alone.
        clean_downloads(dir.path(), DAYS_30, DAYS_7);
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        discard(&dest, len);
        assert!(!finished(&dest, len));
    }

    #[test]
    fn a_kept_file_that_no_longer_matches_is_fetched_again() {
        let (body, sha) = body();
        let len = body.len() as u64;
        let (url, requests) = serve(body.clone(), vec![Reply::Full, Reply::Full]);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("file.bin");
        let http = Http::new().unwrap();
        http.download(&url, &dest, len, &sha, &Stop::default(), &mut |_| {}).unwrap();
        // The same size, so only the checksum can tell.
        std::fs::write(&dest, vec![b'x'; body.len()]).unwrap();
        http.download(&url, &dest, len, &sha, &Stop::default(), &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert_eq!(requests.lock().unwrap().len(), 2);
    }

    #[test]
    fn reads_content_ranges() {
        assert_eq!(parse_content_range("bytes 100-199/1000"), Some((100, Some(1000))));
        assert_eq!(parse_content_range("bytes 5-9/*"), Some((5, None)));
        assert_eq!(parse_content_range("items 1-2/3"), None);
        assert_eq!(parse_content_range("bytes x-9/10"), None);
    }

    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// How the fake server answers one request.
    #[derive(Clone, Copy, Debug)]
    pub(crate) enum Reply {
        /// The whole file.
        Full,
        /// Promise the whole file, send `n` bytes, hang up.
        DropAfter(usize),
        /// Honour `Range` with a 206.
        Ranged,
        /// The right length, wrong bytes.
        Corrupt,
        /// 404.
        Missing,
        /// The whole body, already a gzip stream, announced as `Content-Encoding: gzip`.
        Gzipped,
    }

    /// Serves `body` over plain HTTP following `script`, one reply per connection; records the
    /// `Range` header each request carried.
    pub(crate) fn serve(body: Vec<u8>, script: Vec<Reply>) -> (String, Arc<Mutex<Vec<Option<String>>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/file.bin", listener.local_addr().unwrap());
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let seen = ranges.clone();
        std::thread::spawn(move || {
            for reply in script {
                let Ok((mut stream, _)) = listener.accept() else { return };
                let mut range = None;
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        range = v.trim().trim_end_matches('-').parse::<usize>().ok();
                    }
                }
                seen.lock().unwrap().push(range.map(|r| r.to_string()));
                let len = body.len();
                let _ = match reply {
                    Reply::Full => {
                        write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n").and_then(|()| stream.write_all(&body))
                    }
                    Reply::DropAfter(n) => write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n")
                        .and_then(|()| stream.write_all(&body[..n])),
                    Reply::Corrupt => write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n")
                        .and_then(|()| stream.write_all(&vec![b'x'; len])),
                    Reply::Missing => write!(stream, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
                    Reply::Gzipped => {
                        write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nContent-Encoding: gzip\r\nConnection: close\r\n\r\n")
                            .and_then(|()| stream.write_all(&body))
                    }
                    Reply::Ranged => {
                        let from = range.unwrap_or(0);
                        write!(
                            stream,
                            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {from}-{}/{len}\r\nConnection: close\r\n\r\n",
                            len - from,
                            len - 1
                        )
                        .and_then(|()| stream.write_all(&body[from..]))
                    }
                };
            }
        });
        (url, ranges)
    }

    pub(crate) fn body() -> (Vec<u8>, String) {
        let body: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
        let sha = hex(&Sha256::digest(&body));
        (body, sha)
    }

    fn run(script: Vec<Reply>) -> (Result<Vec<u8>, String>, Vec<Option<String>>) {
        let (body, sha) = body();
        let (url, ranges) = serve(body.clone(), script);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("file.bin");
        let http = Http::new().unwrap();
        let mut last = (0, 0);
        let result = http.download(&url, &dest, body.len() as u64, &sha, &Stop::default(), &mut |t| {
            if let Transfer::Bytes { done, total } = t {
                last = (done, total);
            }
        });
        let result = result.map(|()| std::fs::read(&dest).unwrap());
        if result.is_ok() {
            assert_eq!(last, (body.len() as u64, body.len() as u64));
        }
        let seen = ranges.lock().unwrap().clone();
        (result, seen)
    }

    #[test]
    fn small_binary_files_are_read_whole_and_a_404_is_none() {
        let (body, _) = body();
        let http = Http::new().unwrap();
        let (url, _) = serve(body.clone(), vec![Reply::Full]);
        assert_eq!(http.get_bytes(&url, body.len()).unwrap(), Some(body.clone()), "a body of exactly the cap is fine");
        let (url, _) = serve(body, vec![Reply::Missing]);
        assert_eq!(http.get_bytes(&url, 1 << 20).unwrap(), None);
    }

    #[test]
    fn a_file_over_the_cap_is_refused() {
        let (body, _) = body();
        let http = Http::new().unwrap();
        let (url, _) = serve(body.clone(), vec![Reply::Full]);
        let err = http.get_bytes(&url, body.len() - 1).unwrap_err();
        assert!(err.contains("more than"), "{err}");
    }

    #[test]
    fn a_complete_body_of_exactly_the_published_size_succeeds() {
        // The bug this guards: a reader limit equal to the size failed on the final read.
        let (result, _) = run(vec![Reply::Full]);
        assert_eq!(result.unwrap(), body().0);
    }

    #[test]
    fn a_dropped_connection_resumes_where_it_stopped() {
        let (result, ranges) = run(vec![Reply::DropAfter(300_000), Reply::Ranged]);
        assert_eq!(result.unwrap(), body().0);
        assert_eq!(ranges, [None, Some("300000".to_string())]);
    }

    #[test]
    fn a_server_that_ignores_ranges_starts_over() {
        let (result, _) = run(vec![Reply::DropAfter(123_456), Reply::Full]);
        assert_eq!(result.unwrap(), body().0);
    }

    #[test]
    fn corrupt_data_is_fetched_again_and_never_kept() {
        let (result, _) = run(vec![Reply::Corrupt, Reply::Full]);
        assert_eq!(result.unwrap(), body().0);
        let (result, _) = run(vec![Reply::Corrupt, Reply::Corrupt]);
        assert!(result.unwrap_err().contains("checksum"));
    }

    #[test]
    fn pausing_keeps_the_partial_file_and_resuming_continues_it() {
        let (body, sha) = body();
        let len = body.len() as u64;
        let (url, ranges) = serve(body.clone(), vec![Reply::Full, Reply::Ranged]);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("file.bin");
        let http = Http::new().unwrap();
        let stop = Stop::default();
        // Pause as soon as the first bytes arrive.
        let err = http
            .download(&url, &dest, len, &sha, &stop, &mut |t| {
                if matches!(t, Transfer::Bytes { done, .. } if done > 0) {
                    stop.pause();
                }
            })
            .unwrap_err();
        assert_eq!(err, PAUSED);
        let kept = partial_bytes(&dest, len).unwrap();
        assert!(kept > 0 && kept < len, "{kept}");
        http.download(&url, &dest, len, &sha, &Stop::default(), &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert_eq!(ranges.lock().unwrap()[1], Some(kept.to_string()));
    }

    #[test]
    fn cancelling_throws_the_partial_file_away() {
        let (body, sha) = body();
        let len = body.len() as u64;
        let (url, _) = serve(body, vec![Reply::Full]);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("file.bin");
        let stop = Stop::default();
        let err = Http::new()
            .unwrap()
            .download(&url, &dest, len, &sha, &stop, &mut |t| {
                if matches!(t, Transfer::Bytes { done, .. } if done > 0) {
                    stop.cancel();
                }
            })
            .unwrap_err();
        assert_eq!(err, CANCELLED);
        assert_eq!(partial_bytes(&dest, len), None);
    }

    #[test]
    fn a_missing_file_fails_at_once() {
        let (result, ranges) = run(vec![Reply::Missing, Reply::Full]);
        assert!(result.unwrap_err().contains("no longer"));
        assert_eq!(ranges.len(), 1);
    }

    #[test]
    fn a_partial_file_from_an_earlier_run_is_continued() {
        let (body, sha) = body();
        let (url, ranges) = serve(body.clone(), vec![Reply::Ranged]);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("file.bin");
        std::fs::write(part_path(&dest, body.len() as u64), &body[..500_000]).unwrap();
        assert_eq!(partial_bytes(&dest, body.len() as u64), Some(500_000));
        Http::new().unwrap().download(&url, &dest, body.len() as u64, &sha, &Stop::default(), &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert_eq!(*ranges.lock().unwrap(), [Some("500000".to_string())]);
        assert!(!part_path(&dest, body.len() as u64).exists());
    }

    #[test]
    fn an_oversized_partial_file_is_restarted_from_zero() {
        let (body, sha) = body();
        let (url, ranges) = serve(body.clone(), vec![Reply::Full]);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("file.bin");
        let size = body.len() as u64;
        std::fs::write(part_path(&dest, size), vec![b'x'; body.len() + 1]).unwrap();
        Http::new().unwrap().download(&url, &dest, size, &sha, &Stop::default(), &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(dest).unwrap(), body);
        assert_eq!(*ranges.lock().unwrap(), [None]);
    }

    #[test]
    fn an_oversized_partial_file_that_cannot_be_removed_reports_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let part = part_path(&dir.path().join("file.bin"), 1);
        // A directory cannot be removed as a file on Unix. On Windows, hold a file open
        // without delete sharing: metadata remains readable, but removing it is refused.
        #[cfg(unix)]
        std::fs::create_dir(&part).unwrap();
        #[cfg(windows)]
        let _locked_partial = {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::write(&part, b"too large").unwrap();
            // FILE_SHARE_READ | FILE_SHARE_WRITE, without FILE_SHARE_DELETE.
            std::fs::File::options().read(true).share_mode(0x00000003).open(&part).unwrap()
        };
        assert!(std::fs::metadata(&part).unwrap().len() > 1);
        assert!(std::fs::remove_file(&part).is_err());
        let http = Http::new().unwrap();
        let stop = Stop::default();
        // Bound the regression: the old retry loop spins until Cancel, without a request.
        let (finished, done) = std::sync::mpsc::channel();
        let error = std::thread::scope(|scope| {
            let watchdog_stop = &stop;
            scope.spawn(move || {
                if done.recv_timeout(Duration::from_secs(10)).is_err() {
                    watchdog_stop.cancel();
                }
            });
            let result = http.fetch_resuming("http://127.0.0.1:9/file.bin", &part, 1, &stop, &mut |_| {});
            let _ = finished.send(());
            result.unwrap_err()
        });
        assert!(error.contains("couldn't save the download"), "{error}");
        assert!(part.exists(), "a blocked partial file must be left alone");
    }

    #[test]
    fn an_oversized_feed_answer_is_refused_before_parsing() {
        // More than MAX_SMALL (4 MiB) of decoded text: rejected outright, not truncated and parsed.
        let big = vec![b' '; (MAX_SMALL as usize) + 1024];
        let (url, _) = serve(big, vec![Reply::Full]);
        let http = Http::new().unwrap();
        let err = http.get_feed(&url).unwrap_err();
        assert!(err.why.contains("larger than"), "{}", err.why);
        // A small answer still comes back intact.
        let (url, _) = serve(b"{\"ok\":true}".to_vec(), vec![Reply::Full]);
        assert_eq!(http.get_feed(&url).unwrap(), "{\"ok\":true}");
    }

    #[test]
    fn a_long_release_list_is_read_up_to_the_api_cap() {
        // SolidCraft's list was 6.5 MB, past MAX_SMALL (#1021).
        let long = vec![b' '; (MAX_SMALL as usize) + 1024];
        let (url, _) = serve(long.clone(), vec![Reply::Full]);
        let http = Http::new().unwrap();
        assert!(matches!(http.get_api(&url, None), Ok(Fetched::Body { text, .. }) if text.len() == long.len()));
        // Past MAX_API it is refused outright, not truncated and parsed.
        let (url, _) = serve(vec![b' '; (MAX_API as usize) + 1024], vec![Reply::Full]);
        let err = http.get_api(&url, None).unwrap_err();
        assert!(err.contains("larger than"), "{err}");
    }

    #[test]
    fn a_gzip_bomb_answer_is_refused_before_parsing() {
        // A few hundred bytes on the wire that decode to more than MAX_SMALL. ureq's own limit
        // sits below the transparent gzip decoder, so only a cap on the decoded stream (CWE-409)
        // refuses this; a wire-byte limit would hand the parser truncated JSON.
        let big = vec![b'x'; (MAX_SMALL as usize) + 1024];
        let gz = gzip(&big);
        assert!(gz.len() * 4 < MAX_SMALL as usize, "no longer a bomb: {} compressed bytes", gz.len());
        let (url, _) = serve(gz, vec![Reply::Gzipped]);
        let http = Http::new().unwrap();
        let err = http.get_text(&url).unwrap_err();
        assert!(err.contains("larger than"), "{err}");
        // A small gzip answer still decodes intact.
        let (url, _) = serve(gzip(b"{\"ok\":true}"), vec![Reply::Gzipped]);
        assert_eq!(http.get_text(&url).unwrap().unwrap(), "{\"ok\":true}");
    }

    /// `data` as a real gzip stream (what `Reply::Gzipped` serves).
    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    /// Live: a real Craft App release asset (`cargo test -p artcraft-launcher-native -- --ignored`).
    #[test]
    #[ignore = "needs network access"]
    fn downloads_a_real_release_asset() {
        let http = Http::new().unwrap();
        let Fetched::Body { text, .. } = http.get_api("https://api.github.com/repos/storytold/wordcraft/releases/latest", None).unwrap() else {
            panic!("no release")
        };
        let release = artcraft_launcher_core::release::parse_github_release(&text, "storytold/wordcraft").unwrap();
        let sums_url = &release.sums_asset().unwrap().url;
        let sums = artcraft_launcher_core::release::parse_sha256sums(&http.get_text(sums_url).unwrap().unwrap());
        let asset = release.assets.iter().find(|a| a.name.ends_with("freebsd-x86_64.tar.gz")).unwrap();
        let sha = asset.sha256.clone().or_else(|| sums.iter().find(|(_, n)| *n == asset.name).map(|(h, _)| h.clone())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join(&asset.name);
        http.download(&asset.url, &dest, asset.size, &sha, &Stop::default(), &mut |_| {}).unwrap();
        assert_eq!(std::fs::metadata(&dest).unwrap().len(), asset.size);
    }

    /// Answers one request with `head` (status line and headers) and an empty body.
    fn answer_once(head: String) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/repos/storytold/photocraft/releases/latest", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
            }
            let _ = write!(stream, "{head}Content-Length: 0\r\nConnection: close\r\n\r\n");
        });
        url
    }

    #[test]
    fn a_secondary_limit_waits_for_retry_after_not_the_hourly_reset() {
        // GitHub's secondary limit: quota left, an hourly reset 50 minutes away, and "try again
        // in a minute". That's a minute, not 50 minutes of "the hourly limit was reached".
        let now = crate::now();
        let url = answer_once(format!(
            "HTTP/1.1 403 Forbidden\r\nx-ratelimit-remaining: 41\r\nx-ratelimit-reset: {}\r\nretry-after: 60\r\n",
            now + 50 * 60
        ));
        let Fetched::RateLimited { reset: Some(until) } = Http::new().unwrap().get_api(&url, None).unwrap() else { panic!("not rate limited") };
        assert!((now + 60..now + 120).contains(&until), "limited for {} s", until - now);
    }

    #[test]
    fn an_empty_hourly_quota_waits_for_the_reset() {
        let now = crate::now();
        let reset = now + 31 * 60;
        let url = answer_once(format!("HTTP/1.1 403 Forbidden\r\nx-ratelimit-remaining: 0\r\nx-ratelimit-reset: {reset}\r\n"));
        assert_eq!(Http::new().unwrap().get_api(&url, None).unwrap(), Fetched::RateLimited { reset: Some(reset) });
    }

    #[test]
    fn github_refusals_are_read_the_way_github_documents_them() {
        const NOW: i64 = 1_791_500_000;
        // Out of quota: until the reset, or Retry-After if that's later.
        assert_eq!(refused_until(403, Some("0"), Some(NOW + 1800), None, NOW), Some(Some(NOW + 1800)));
        assert_eq!(refused_until(403, Some("0"), Some(NOW + 30), Some(90), NOW), Some(Some(NOW + 90)));
        assert_eq!(refused_until(403, Some("0"), None, None, NOW), Some(None));
        // A secondary limit: Retry-After, never the hourly reset.
        assert_eq!(refused_until(429, Some("12"), Some(NOW + 1800), Some(30), NOW), Some(Some(NOW + 30)));
        // A 429 that doesn't say: a minute.
        assert_eq!(refused_until(429, Some("12"), Some(NOW + 1800), None, NOW), Some(Some(NOW + 60)));
        // A plain 403 (blocked, private, …) isn't a rate limit.
        assert_eq!(refused_until(403, Some("12"), Some(NOW + 1800), None, NOW), None);
        assert_eq!(refused_until(403, None, None, None, NOW), None);
    }

    #[test]
    fn partial_files_are_named_after_the_file_and_size() {
        assert_eq!(part_path(Path::new("/d/photocraft.dmg"), 9), Path::new("/d/photocraft.dmg.9.part"));
    }

    #[test]
    fn get_bytes_rejects_oversized_payload() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else { return };
            let mut line = String::new();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            while reader.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
                line.clear();
            }
            let body = vec![b'A'; 200];
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: 200\r\nConnection: close\r\n\r\n");
            let _ = stream.write_all(&body);
        });
        let http = Http::new().unwrap();
        let result = http.get_bytes(&format!("http://127.0.0.1:{port}/test.bin"), 100);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("more than 100 bytes"));
    }
}
