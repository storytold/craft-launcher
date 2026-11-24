//! The GitHub budget: the ceiling one launcher sets itself on GitHub API requests an hour, below
//! GitHub's own rate limit, which every machine on the network shares (#713, docs/github-budget.md).

use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use artcraft_launcher_core::release::Priority;
use serde::{Deserialize, Serialize};

/// Requests an hour for the stable launcher.
pub(crate) const STABLE: usize = 30;
/// Requests an hour for the Developer Builds backend, which shares the machine's network.
pub(crate) const DEVELOPER_BUILDS: usize = 15;
const HOUR: i64 = 60 * 60;
/// Requests left to the rest of the network: stop this many short of GitHub's own count.
const NETWORK_RESERVE: i64 = 10;

/// Requests kept back for more urgent ones.
// lean: fixed reserves per priority; a queue ordered by priority if user actions still starve.
fn held_back(priority: Priority) -> i64 {
    match priority {
        Priority::User => 0,
        Priority::Installed => 2,
        Priority::NotInstalled => 4,
        Priority::NightlyTrack => 6,
        Priority::Discovery => 8,
    }
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    /// When each request of the last hour was made (Unix seconds), oldest first.
    asked: Vec<i64>,
    /// GitHub's own count from its last answer: requests left, and when they refill.
    github: Option<(i64, i64)>,
}

pub(crate) struct GitHubBudget {
    limit: usize,
    /// Where the last hour is kept, so quitting and starting again doesn't refill it.
    path: Option<PathBuf>,
    state: Mutex<State>,
}

impl GitHubBudget {
    pub(crate) fn in_memory(limit: usize) -> Self {
        Self { limit, path: None, state: Mutex::new(State::default()) }
    }

    /// The budget kept in `path`; an unreadable file starts afresh.
    pub(crate) fn load(path: PathBuf, limit: usize) -> Self {
        let state = std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        Self { limit, path: Some(path), state: Mutex::new(state) }
    }

    /// Takes one request for `priority` at `now`, or says when the next one is free.
    pub(crate) fn take(&self, priority: Priority, now: i64) -> Result<(), i64> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        // A clock set back can't keep requests in the window for longer than an hour.
        state.asked.retain(|at| *at > now - HOUR && *at <= now);
        let network = state.github.filter(|(_, reset)| *reset > now);
        let own = self.limit as i64 - state.asked.len() as i64;
        let left = network.map_or(own, |(remaining, _)| own.min(remaining - NETWORK_RESERVE));
        let needed = held_back(priority) + 1;
        if left >= needed {
            state.asked.push(now);
            self.save(&state);
            return Ok(());
        }
        // Free once enough of this hour's requests have aged out, and GitHub's count has refilled
        // if that is what stands in the way.
        let expiring = usize::try_from(needed - own).unwrap_or(0);
        let own_free = if expiring == 0 { now } else { state.asked.get(expiring - 1).map_or(now, |at| at + HOUR) };
        let network_free = network.filter(|(remaining, _)| remaining - NETWORK_RESERVE < needed).map_or(now, |(_, reset)| reset);
        Err(own_free.max(network_free))
    }

    /// Notes GitHub's own count from an answer's `X-RateLimit-Remaining` and `X-RateLimit-Reset`.
    pub(crate) fn observe(&self, remaining: Option<i64>, reset: Option<i64>) {
        if let (Some(remaining), Some(reset)) = (remaining, reset) {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.github = Some((remaining, reset));
            self.save(&state);
        }
    }

    fn save(&self, state: &State) {
        let Some(path) = &self.path else { return };
        match serde_json::to_vec(state) {
            Ok(bytes) => {
                if let Err(e) = crate::paths::write_atomic(path, &bytes) {
                    log::warn!("GitHub budget: {e}");
                }
            }
            Err(e) => log::warn!("GitHub budget: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_791_500_000;

    #[test]
    fn the_budget_holds_back_requests_for_the_user_and_refills_within_the_hour() {
        let budget = GitHubBudget::in_memory(STABLE);
        let mut background = 0;
        while budget.take(Priority::NotInstalled, NOW).is_ok() {
            background += 1;
        }
        assert!(background < STABLE, "background checks leave room for the user");
        let mut user = 0;
        while budget.take(Priority::User, NOW + 1).is_ok() {
            user += 1;
        }
        assert!(user > 0 && background + user == STABLE, "the user gets the rest, and no more");
        let free = budget.take(Priority::User, NOW + 2).unwrap_err();
        assert!(free > NOW + 2 && free <= NOW + HOUR, "the next request frees within the hour");
        assert!(budget.take(Priority::User, free).is_ok());
    }

    #[test]
    fn githubs_own_count_stops_the_launcher_early_until_it_refills() {
        let budget = GitHubBudget::in_memory(STABLE);
        budget.observe(Some(NETWORK_RESERVE + 1), Some(NOW + 600));
        assert!(budget.take(Priority::User, NOW).is_ok(), "one left before the reserve");
        budget.observe(Some(NETWORK_RESERVE), Some(NOW + 600));
        assert_eq!(budget.take(Priority::User, NOW), Err(NOW + 600), "the rest is the network's");
        assert!(budget.take(Priority::User, NOW + 600).is_ok(), "GitHub's count refilled");
    }

    #[test]
    fn the_last_hour_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("github-budget.json");
        let budget = GitHubBudget::load(path.clone(), DEVELOPER_BUILDS);
        while budget.take(Priority::User, NOW).is_ok() {}
        assert!(GitHubBudget::load(path, DEVELOPER_BUILDS).take(Priority::User, NOW + 60).is_err());
    }
}
