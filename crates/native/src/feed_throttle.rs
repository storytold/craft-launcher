//! [`FeedThrottle`]: how often the launcher may call ArtCraft's API (the release feed).
//!
//! Every launcher in the world polls the same server, so the limits are strict and favour the
//! server over freshness (the last saved answer always stands in):
//!
//! - **At most one request a minute** once the first load is over.
//! - **During the first load** (until the feed first answers, at most [`WARM_REQUESTS`]
//!   requests), up to ten a minute: front-loaded, with the gaps doubling after each failure
//!   (2 s, 4 s, 8 s, …), so a struggling server sees each client ease off quickly.
//! - **Failures back off**: after a failed request the next waits 2, 4, 8 … minutes (at most an
//!   hour), with jitter.
//! - **HTTP 429 throttles hard**: at least 15 minutes, doubling with each 429 in a row up to six
//!   hours, never sooner than the server's `Retry-After`, plus up to 50% jitter so a herd of
//!   clients doesn't come back in step. This survives restarts (saved to disk), so relaunching
//!   doesn't skip the wait.

use std::collections::VecDeque;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The window the per-minute limits count over (seconds).
const WINDOW: i64 = 60;
/// Requests per [`WINDOW`] once the first load is over.
const STEADY_PER_WINDOW: usize = 1;
/// Requests per [`WINDOW`] during the first load.
const WARM_PER_WINDOW: usize = 10;
/// The first load ends at the first good answer, or after this many requests.
const WARM_REQUESTS: u32 = 10;
/// Gaps between first-load requests after 0, 1, 2 … failures in a row (seconds).
const WARM_GAPS: [i64; 6] = [1, 2, 4, 8, 16, 32];
/// After a failure (once loaded), the wait before the next request: this, doubled per failure in
/// a row…
const FAILURE_BACKOFF: i64 = 2 * 60;
/// …up to this.
const MAX_FAILURE_BACKOFF: i64 = 60 * 60;
/// After an HTTP 429, the wait before the next request: this, doubled per 429 in a row…
const THROTTLE_BACKOFF: i64 = 15 * 60;
/// …up to this.
const MAX_THROTTLE_BACKOFF: i64 = 6 * 60 * 60;
/// A `Retry-After` beyond this is ignored as nonsense (we still wait our own backoff).
const MAX_RETRY_AFTER: i64 = 24 * 60 * 60;

/// What happened to one request, as far as pacing is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A readable answer.
    Answered,
    /// No answer, an error status or an unreadable body: the server may be struggling.
    /// `retry_after` is the server's `Retry-After` (seconds), if it sent one.
    Failed { retry_after: Option<i64> },
    /// HTTP 429 Too Many Requests.
    Throttled { retry_after: Option<i64> },
}

/// The part that outlives the process, so a relaunch keeps backing off.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    /// No request before this (Unix seconds).
    not_before: i64,
    /// Failed requests in a row.
    failures: u32,
    /// HTTP 429s in a row.
    throttles: u32,
}

/// The pacing state for ArtCraft's API. Times are Unix seconds, passed in so it can be tested.
#[derive(Debug)]
pub struct FeedThrottle {
    saved: Saved,
    /// When recent requests were sent (the last [`WINDOW`] seconds' worth).
    sent: VecDeque<i64>,
    /// Requests sent during the first load.
    warm_sent: u32,
    /// The first load is over: the feed has answered, or we've used up the first-load requests.
    warmed: bool,
    /// Where the state is saved, if anywhere.
    file: Option<PathBuf>,
}

impl FeedThrottle {
    /// Fresh state, not saved anywhere.
    pub fn new() -> Self {
        Self { saved: Saved::default(), sent: VecDeque::new(), warm_sent: 0, warmed: false, file: None }
    }

    /// The state saved at `file` by an earlier run (if readable and sane), saved back there as it
    /// changes. A missing or malformed file just means no backoff is pending.
    pub fn load(file: &Path, now: i64) -> Self {
        let mut saved = std::fs::read(file).ok().and_then(|b| serde_json::from_slice::<Saved>(&b).ok()).unwrap_or_default();
        // A saved wait outside the sane range means a broken file, an edited value or a clock
        // that jumped: drop it. A `not_before` near `i64::MIN` would otherwise wrap the
        // subtraction in `wait` into a centuries-long wait (#774).
        if !(0..=now.saturating_add(MAX_THROTTLE_BACKOFF * 2)).contains(&saved.not_before) {
            saved.not_before = 0;
        }
        Self { saved, file: Some(file.to_path_buf()), ..Self::new() }
    }

    /// Seconds to wait before the next request may be sent (0: now).
    pub fn wait(&self, now: i64) -> i64 {
        let warming = !self.warmed;
        let per_window = if warming { WARM_PER_WINDOW } else { STEADY_PER_WINDOW };
        let mut at = self.saved.not_before;
        let recent: Vec<i64> = self.sent.iter().copied().filter(|t| now - t < WINDOW).collect();
        if recent.len() >= per_window
            && let Some(oldest) = recent.get(recent.len() - per_window)
        {
            at = at.max(oldest + WINDOW);
        }
        if warming && let Some(last) = recent.last() {
            let gap = WARM_GAPS.get(self.saved.failures as usize).or(WARM_GAPS.last()).copied().unwrap_or(WINDOW);
            at = at.max(last + gap);
        }
        at.saturating_sub(now).max(0)
    }

    /// Notes that a request is being sent now.
    pub fn sending(&mut self, now: i64) {
        self.sent.push_back(now);
        while self.sent.front().is_some_and(|t| now - t >= WINDOW) {
            self.sent.pop_front();
        }
        if !self.warmed {
            self.warm_sent += 1;
            if self.warm_sent >= WARM_REQUESTS {
                self.warmed = true;
            }
        }
    }

    /// Notes how the request went. `jitter` is in `0.0..1.0` (see [`jitter`]).
    pub fn finished(&mut self, outcome: Outcome, now: i64, jitter: f64) {
        let before = self.saved;
        match outcome {
            Outcome::Answered => {
                self.warmed = true;
                self.saved = Saved::default();
            }
            Outcome::Failed { retry_after } => {
                self.saved.failures = self.saved.failures.saturating_add(1);
                self.saved.throttles = 0;
                // During the first load the doubling gaps in `wait` do the backing off.
                let backoff = if self.warmed { doubled(FAILURE_BACKOFF, self.saved.failures, MAX_FAILURE_BACKOFF) } else { 0 };
                let backoff = spread(backoff, 0.4 * jitter - 0.2);
                self.saved.not_before = now + backoff.max(sane(retry_after));
            }
            Outcome::Throttled { retry_after } => {
                self.warmed = true;
                self.saved.throttles = self.saved.throttles.saturating_add(1);
                let backoff = doubled(THROTTLE_BACKOFF, self.saved.throttles, MAX_THROTTLE_BACKOFF);
                // Only ever later than asked: spreading the herd out, never bringing it back early.
                self.saved.not_before = now + spread(backoff.max(sane(retry_after)), 0.5 * jitter);
            }
        }
        if self.saved != before {
            self.save();
        }
    }

    /// The server sent HTTP 429 recently and we're still waiting it out.
    pub fn throttled(&self, now: i64) -> bool {
        self.saved.throttles > 0 && self.saved.not_before > now
    }

    fn save(&self) {
        let Some(file) = &self.file else { return };
        let saved = serde_json::to_vec(&self.saved).unwrap_or_default();
        if let Err(e) = crate::paths::write_atomic(file, &saved) {
            log::warn!("couldn't save the ArtCraft API backoff: {e}");
        }
    }
}

impl Default for FeedThrottle {
    fn default() -> Self {
        Self::new()
    }
}

/// `base` doubled for each repeat after the first (`times` ≥ 1), at most `max`.
fn doubled(base: i64, times: u32, max: i64) -> i64 {
    let doublings = times.saturating_sub(1).min(16);
    base.saturating_mul(1 << doublings).min(max)
}

/// `secs` scaled by `1 + by`.
fn spread(secs: i64, by: f64) -> i64 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    let scaled = (secs as f64 * (1.0 + by)).round() as i64;
    scaled.max(0)
}

/// A `Retry-After` in seconds, ignoring negative or absurd values.
fn sane(retry_after: Option<i64>) -> i64 {
    retry_after.filter(|s| (0..=MAX_RETRY_AFTER).contains(s)).unwrap_or(0)
}

/// A random number in `0.0..1.0`, different in every launcher (std's hasher keys are random).
pub fn jitter() -> f64 {
    let bits = std::collections::hash_map::RandomState::new().build_hasher().finish();
    #[allow(clippy::cast_precision_loss)]
    let fraction = (bits >> 11) as f64 / (1u64 << 53) as f64;
    fraction
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_791_500_000;

    /// Sends requests as fast as the throttle allows for `secs`, each failing with `outcome`.
    /// Returns when they were sent.
    fn hammer(t: &mut FeedThrottle, from: i64, secs: i64, outcome: Outcome) -> Vec<i64> {
        let mut sent = Vec::new();
        let mut now = from;
        while now < from + secs {
            let wait = t.wait(now);
            if wait > 0 {
                now += wait;
                continue;
            }
            t.sending(now);
            sent.push(now);
            t.finished(outcome, now, 0.5);
        }
        sent.retain(|s| *s < from + secs);
        sent
    }

    fn per_minute_max(sent: &[i64]) -> usize {
        sent.iter().map(|s| sent.iter().filter(|t| **t >= *s && **t < s + WINDOW).count()).max().unwrap_or(0)
    }

    #[test]
    fn once_loaded_never_more_than_one_request_a_minute() {
        let mut t = FeedThrottle::new();
        t.sending(NOW);
        t.finished(Outcome::Answered, NOW, 0.5);
        let sent = hammer(&mut t, NOW, 3600, Outcome::Answered);
        assert_eq!(per_minute_max(&sent), 1);
        assert_eq!(sent.len(), 59);
        assert_eq!(sent.first(), Some(&(NOW + 60)));
    }

    #[test]
    fn the_first_load_may_go_faster_but_never_past_ten_a_minute() {
        let mut t = FeedThrottle::new();
        let sent = hammer(&mut t, NOW, 60, Outcome::Failed { retry_after: None });
        assert!(sent.len() > 1 && per_minute_max(&sent) <= 10, "{sent:?}");
        // Front-loaded and falling off: the gaps grow with each failure.
        let gaps: Vec<i64> = sent.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps.windows(2).all(|g| g[1] >= g[0]), "{gaps:?}");
        assert_eq!(sent.first(), Some(&NOW));
    }

    #[test]
    fn a_healthy_first_load_retries_quickly() {
        // Even with every request succeeding, after the first answer it's one a minute.
        let mut t = FeedThrottle::new();
        let sent = hammer(&mut t, NOW, 120, Outcome::Answered);
        assert_eq!(sent, vec![NOW, NOW + 60]);
    }

    #[test]
    fn the_first_load_ends_after_ten_requests_even_without_an_answer() {
        let mut t = FeedThrottle::new();
        for i in 0..WARM_REQUESTS {
            t.sending(NOW + i64::from(i));
        }
        // Back to one a minute.
        assert!(t.wait(NOW + 10) >= 50);
    }

    #[test]
    fn failures_back_off_after_loading() {
        let mut t = FeedThrottle::new();
        t.sending(NOW);
        t.finished(Outcome::Answered, NOW, 0.5);
        let sent = hammer(&mut t, NOW + 60, 6 * 3600, Outcome::Failed { retry_after: None });
        let gaps: Vec<i64> = sent.windows(2).map(|w| w[1] - w[0]).collect();
        assert_eq!(gaps.first(), Some(&(2 * 60)));
        assert!(gaps.windows(2).all(|g| g[1] >= g[0]), "{gaps:?}");
        assert_eq!(gaps.last(), Some(&MAX_FAILURE_BACKOFF));
    }

    #[test]
    fn a_good_answer_resets_the_backoff() {
        let mut t = FeedThrottle::new();
        t.warmed = true;
        for i in 0..5 {
            t.finished(Outcome::Failed { retry_after: None }, NOW + i, 0.5);
        }
        t.finished(Outcome::Answered, NOW + 10, 0.5);
        assert_eq!(t.wait(NOW + 10), 0);
    }

    #[test]
    fn a_429_throttles_hard_and_harder() {
        let mut t = FeedThrottle::new();
        t.sending(NOW);
        t.finished(Outcome::Throttled { retry_after: None }, NOW, 0.0);
        assert_eq!(t.wait(NOW), THROTTLE_BACKOFF);
        assert!(t.throttled(NOW));
        let sent = hammer(&mut t, NOW, 24 * 3600, Outcome::Throttled { retry_after: None });
        let gaps: Vec<i64> = sent.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps.windows(2).all(|g| g[1] >= g[0]), "{gaps:?}");
        assert!(gaps.iter().all(|g| *g >= THROTTLE_BACKOFF));
        assert!(gaps.last().is_some_and(|g| *g >= MAX_THROTTLE_BACKOFF));
    }

    #[test]
    fn a_429_during_the_first_load_ends_the_fast_start() {
        let mut t = FeedThrottle::new();
        t.sending(NOW);
        t.finished(Outcome::Throttled { retry_after: None }, NOW, 0.0);
        let sent = hammer(&mut t, NOW + 1, 3600, Outcome::Throttled { retry_after: None });
        assert!(sent.iter().all(|s| *s >= NOW + THROTTLE_BACKOFF), "{sent:?}");
    }

    #[test]
    fn retry_after_is_honoured_but_never_shortens_our_wait() {
        let mut t = FeedThrottle::new();
        t.finished(Outcome::Throttled { retry_after: Some(3 * 3600) }, NOW, 0.0);
        assert_eq!(t.wait(NOW), 3 * 3600);
        let mut t = FeedThrottle::new();
        t.finished(Outcome::Throttled { retry_after: Some(5) }, NOW, 0.0);
        assert_eq!(t.wait(NOW), THROTTLE_BACKOFF);
        // Nonsense is ignored.
        let mut t = FeedThrottle::new();
        t.finished(Outcome::Throttled { retry_after: Some(i64::MAX) }, NOW, 0.0);
        assert_eq!(t.wait(NOW), THROTTLE_BACKOFF);
        // A 503 with Retry-After is honoured too.
        let mut t = FeedThrottle::new();
        t.finished(Outcome::Failed { retry_after: Some(600) }, NOW, 0.5);
        assert_eq!(t.wait(NOW), 600);
    }

    #[test]
    fn jitter_only_ever_delays_a_429() {
        for j in [0.0, 0.25, 0.5, 0.99] {
            let mut t = FeedThrottle::new();
            t.finished(Outcome::Throttled { retry_after: None }, NOW, j);
            let wait = t.wait(NOW);
            assert!((THROTTLE_BACKOFF..=THROTTLE_BACKOFF * 3 / 2).contains(&wait), "{j}: {wait}");
        }
        let j = jitter();
        assert!((0.0..1.0).contains(&j));
    }

    #[test]
    fn the_backoff_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("feed-throttle.json");
        let mut t = FeedThrottle::load(&file, NOW);
        t.finished(Outcome::Throttled { retry_after: None }, NOW, 0.0);
        let again = FeedThrottle::load(&file, NOW + 60);
        assert_eq!(again.wait(NOW + 60), THROTTLE_BACKOFF - 60);
        assert!(again.throttled(NOW + 60));
        // A good answer clears it.
        let mut again = again;
        again.finished(Outcome::Answered, NOW + THROTTLE_BACKOFF, 0.0);
        assert_eq!(FeedThrottle::load(&file, NOW + THROTTLE_BACKOFF).wait(NOW + THROTTLE_BACKOFF), 0);
    }

    #[test]
    fn a_broken_or_absurd_saved_file_means_no_wait() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("feed-throttle.json");
        std::fs::write(&file, b"{not json").unwrap();
        assert_eq!(FeedThrottle::load(&file, NOW).wait(NOW), 0);
        std::fs::write(&file, br#"{"not_before": 99999999999, "throttles": 3}"#).unwrap();
        assert_eq!(FeedThrottle::load(&file, NOW).wait(NOW), 0);
        std::fs::write(&file, br#"{"unknown": true}"#).unwrap();
        assert_eq!(FeedThrottle::load(&file, NOW).wait(NOW), 0);
    }

    #[test]
    fn an_absurdly_old_saved_not_before_is_no_wait() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("feed-throttle.json");
        // A hostile or corrupted `not_before` near i64::MIN once wrapped `at - now` into a
        // wait of centuries (a debug-build panic, #774).
        std::fs::write(&file, br#"{"not_before": -9223372036854775808, "throttles": 1}"#).unwrap();
        assert_eq!(FeedThrottle::load(&file, NOW).wait(NOW), 0);
        std::fs::write(&file, br#"{"not_before": -60, "throttles": 1}"#).unwrap();
        assert_eq!(FeedThrottle::load(&file, NOW).wait(NOW), 0);
    }
}
