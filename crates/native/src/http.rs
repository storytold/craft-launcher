//! HTTPS requests: small JSON/text answers, redirect probes, and verified downloads.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};

use artcraft_launcher_core::protocol::{CANCELLED, PAUSED};
use std::time::Duration;

use sha2::{Digest, Sha256};

const USER_AGENT: &str = concat!("ArtCraftLauncher/", env!("CARGO_PKG_VERSION"));
/// Largest JSON or text answer we read.
const MAX_SMALL: u64 = 4 << 20;

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
    token: Option<String>,
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
        // An optional token raises GitHub's limit from 60 to 5000 requests an hour. Only ever sent
        // to api.github.com.
        let token = std::env::var("ARTCRAFT_LAUNCHER_GITHUB_TOKEN").ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        Ok(Self { small, feed, no_redirect, download, token })
    }

    /// GET a GitHub API resource, revalidating with `etag` when given.
    pub fn get_api(&self, url: &str, etag: Option<&str>) -> Result<Fetched, String> {
        let mut req = self.small.get(url).header("Accept", "application/vnd.github+json").header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(tag) = etag {
            req = req.header("If-None-Match", tag);
        }
        if let Some(token) = &self.token
            && url.starts_with("https://api.github.com/")
        {
            req = req.header("Authorization", &format!("Bearer {token}"));
        }
        let mut resp = req.call().map_err(|e| format!("couldn't reach GitHub ({e})"))?;
        let header = |name: &str| resp.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
        let status = resp.status().as_u16();
        match status {
            200 => {
                let etag = header("etag");
                let text =
                    resp.body_mut().with_config().limit(MAX_SMALL).read_to_string().map_err(|e| format!("unreadable answer from GitHub ({e})"))?;
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
        let mut resp =
            self.feed.get(url).header("Accept", "application/json").call().map_err(|e| failed(format!("couldn't reach {} ({e})", host(url))))?;
        // Seconds only; an HTTP date falls back to our own backoff.
        let retry_after = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|r| r.trim().parse::<i64>().ok());
        match resp.status().as_u16() {
            200 => resp.body_mut().with_config().limit(MAX_SMALL).read_to_string().map_err(|e| failed(format!("unreadable answer ({e})"))),
            s => Err(FeedError { why: format!("{} answered HTTP {s}", host(url)), throttled: s == 429, retry_after }),
        }
    }

    /// GET a small text file (following redirects).
    pub fn get_text(&self, url: &str) -> Result<Option<String>, String> {
        let mut resp = self.small.get(url).call().map_err(|e| format!("couldn't reach {} ({e})", host(url)))?;
        match resp.status().as_u16() {
            200 => resp.body_mut().with_config().limit(MAX_SMALL).read_to_string().map(Some).map_err(|e| format!("unreadable answer ({e})")),
            404 => Ok(None),
            s => Err(format!("{} answered HTTP {s}", host(url))),
        }
    }

    /// GET a small binary file (following redirects). `Ok(None)` for a 404. A file of more than
    /// `max` bytes is refused: by its announced length before a byte is read, and by a hard limit
    /// on the body when the length wasn't announced or was wrong.
    pub fn get_bytes(&self, url: &str, max: usize) -> Result<Option<Vec<u8>>, String> {
        let mut resp = self.small.get(url).call().map_err(|e| format!("couldn't reach {} ({e})", host(url)))?;
        match resp.status().as_u16() {
            200 => {
                let announced = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
                let too_big = || format!("{} sent more than {max} bytes", host(url));
                if announced.is_some_and(|n| n > max as u64) {
                    return Err(too_big());
                }
                // ureq errors once the limit is reached, so ask for one byte more than we accept.
                let body = resp.body_mut().with_config().limit(max as u64 + 1).read_to_vec().map_err(|e| format!("unreadable answer ({e})"))?;
                if body.len() > max { Err(too_big()) } else { Ok(Some(body)) }
            }
            404 => Ok(None),
            s => Err(format!("{} answered HTTP {s}", host(url))),
        }
    }

    /// Where `url` redirects to, without following it.
    pub fn redirect_target(&self, url: &str) -> Result<Option<String>, String> {
        let resp = self.no_redirect.head(url).call().map_err(|e| format!("couldn't reach {} ({e})", host(url)))?;
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
                std::fs::rename(&part, dest).map_err(|e| format!("couldn't save the download ({e})"))?;
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
        let mut req = self.download.get(url).header("Accept-Encoding", "identity");
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
        // Open the part file at the right place: a 200 restarts it, a 206 continues it.
        let mut file = match (status, start) {
            (206, s) if s == have => std::fs::OpenOptions::new().append(true).open(part),
            _ => std::fs::File::create(part),
        }
        .map_err(|e| AttemptError::Fatal(format!("couldn't save the download ({e})")))?;
        let mut done = if status == 206 && start == have { have } else { 0 };
        // We check the length and checksum ourselves; the reader's limit is only a backstop
        // against an endless body. (ureq errors once the limit is *reached*, so leave headroom.)
        let remaining = if total > 0 { total.saturating_sub(done) } else { MAX_DOWNLOAD };
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
            let chunk = buf.get(..n).ok_or_else(|| AttemptError::Fatal("read past the buffer".into()))?;
            file.write_all(chunk).map_err(|e| AttemptError::Fatal(format!("couldn't save the download ({e}). Is the disk full?")))?;
            done = done.saturating_add(n as u64);
            if total > 0 && done > total {
                return Err(retry("the server sent more than the file's size".into(), false));
            }
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

/// Asks a running download to pause (keeping its partial file) or cancel (deleting it).
#[derive(Debug, Default)]
pub struct Stop(AtomicU8);

impl Stop {
    const RUN: u8 = 0;
    const PAUSE: u8 = 1;
    const CANCEL: u8 = 2;

    pub fn pause(&self) {
        let _ = self.0.compare_exchange(Self::RUN, Self::PAUSE, Ordering::SeqCst, Ordering::SeqCst);
    }

    pub fn cancel(&self) {
        self.0.store(Self::CANCEL, Ordering::SeqCst);
    }

    /// `Err(PAUSED | CANCELLED)` once asked to stop.
    pub fn check(&self) -> Result<(), String> {
        match self.0.load(Ordering::SeqCst) {
            Self::PAUSE => Err(PAUSED.into()),
            Self::CANCEL => Err(CANCELLED.into()),
            _ => Ok(()),
        }
    }
}

/// Whether a file on disk is the published one: its SHA-256 matches.
fn matches_published(path: &Path, sha256: &str) -> bool {
    sha256_file(path).is_ok_and(|h| h.eq_ignore_ascii_case(sha256))
}

/// How much of a download is already on disk (a paused or interrupted one).
pub fn partial_bytes(dest: &Path, size: u64) -> Option<u64> {
    std::fs::metadata(part_path(dest, size)).ok().map(|m| m.len()).filter(|n| *n > 0)
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
fn part_path(dest: &Path, size: u64) -> std::path::PathBuf {
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

fn host(url: &str) -> &str {
    url.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or(url)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const DAYS_30: Duration = Duration::from_secs(30 * 86_400);
    const DAYS_7: Duration = Duration::from_secs(7 * 86_400);

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
}
