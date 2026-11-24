//! Separate bounded Nightly discovery and metadata-only scheduling. Ordinary versions(),
//! Stable/beta/Pinned and disk release/descriptor caches do not participate.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::time::Duration;

use artcraft_launcher_core::protocol::NightlyStatus;
use artcraft_launcher_core::{CraftApp, Platform, Release, version};
use serde::{Deserialize, Serialize};

use crate::http::NightlyHttpError;
use crate::nightly::VerifiedRelease;
use crate::paths::Paths;

pub(crate) const EVERY: i64 = 6 * 60 * 60;
const MANUAL_COALESCE: i64 = 60;
const PER_PAGE: usize = 100;
const MAX_PAGES: usize = 5;
const MAX_REQUESTS: usize = 32;
const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_TIME: Duration = Duration::from_secs(30);
const MAX_RECORDS: usize = 64;
const MAX_METADATA: u64 = 64 * 1024;

type Fetch<'a> = dyn FnMut(&str, usize, Duration, i64) -> Result<Option<Vec<u8>>, NightlyHttpError> + 'a;

#[derive(Debug)]
pub(crate) enum Outcome {
    Verified(Box<VerifiedRelease>),
    NoCompatibleBuild,
    Indeterminate(NightlyHttpError),
}

struct Budget<'a> {
    fetch: &'a mut Fetch<'a>,
    elapsed: &'a dyn Fn() -> Duration,
    enabled: &'a dyn Fn() -> bool,
    now: i64,
    requests: usize,
    bytes: usize,
}

fn failure(why: impl Into<String>) -> NightlyHttpError {
    NightlyHttpError { why: why.into(), retry_at: None }
}

impl Budget<'_> {
    fn get(&mut self, url: &str, max: usize) -> Result<Vec<u8>, NightlyHttpError> {
        if !(self.enabled)() {
            return Err(failure("Pre-Release discovery was turned off."));
        }
        let remaining = MAX_TIME
            .checked_sub((self.elapsed)())
            .filter(|time| !time.is_zero())
            .ok_or_else(|| failure("The Nightly discovery elapsed-time budget was exhausted."))?;
        if self.requests >= MAX_REQUESTS || self.bytes >= MAX_BYTES {
            return Err(failure("The Nightly discovery request or returned-byte budget was exhausted."));
        }
        self.requests += 1;
        let max = max.min(MAX_BYTES - self.bytes);
        let now = self.now.saturating_add((self.elapsed)().as_secs().try_into().unwrap_or(i64::MAX));
        let body = (self.fetch)(url, max, remaining, now)?
            .ok_or_else(|| failure("A required Nightly discovery resource was not found; absence is indeterminate."))?;
        if body.len() > max {
            return Err(failure("A Nightly response exceeded its returned-byte budget."));
        }
        self.bytes += body.len();
        if (self.elapsed)() >= MAX_TIME {
            return Err(failure("The Nightly discovery elapsed-time budget was exhausted."));
        }
        if !(self.enabled)() {
            return Err(failure("Pre-Release discovery was turned off."));
        }
        Ok(body)
    }

    fn coverage_complete(&self) -> Result<(), NightlyHttpError> {
        if !(self.enabled)() {
            return Err(failure("Pre-Release discovery was turned off."));
        }
        if (self.elapsed)() >= MAX_TIME {
            return Err(failure("The Nightly discovery elapsed-time budget was exhausted before final coverage."));
        }
        Ok(())
    }

    fn verify(&mut self, app: &CraftApp, mut release: Release, platform: Platform) -> Result<Option<VerifiedRelease>, NightlyHttpError> {
        if let Some(sums) = release.sums_asset() {
            if !artcraft_launcher_core::release::valid_file_name(&sums.name)
                || sums.url != format!("https://github.com/{}/releases/download/{}/{}", app.repo, release.tag, sums.name)
            {
                return Err(failure("The Nightly checksums are outside the exact app release."));
            }
            let body = self.get(&sums.url, 1024 * 1024)?;
            let text = std::str::from_utf8(&body).map_err(|_| failure("The Nightly checksum file is not UTF-8."))?;
            release.apply_sums(&artcraft_launcher_core::release::parse_sha256sums(text));
        }
        let descriptor = release
            .assets
            .iter()
            .find(|asset| asset.name == artcraft_launcher_core::nightly::MANIFEST_NAME)
            .ok_or_else(|| failure("A Nightly candidate is missing its isolation descriptor."))?;
        if descriptor.size == 0
            || descriptor.size > artcraft_launcher_core::nightly::MAX_MANIFEST_BYTES as u64
            || descriptor.sha256.is_none()
            || descriptor.url != format!("https://github.com/{}/releases/download/{}/NIGHTLY.json", app.repo, release.tag)
        {
            return Err(failure("A Nightly candidate has no bounded, exact-release checksum-verifiable descriptor."));
        }
        let body = self.get(&descriptor.url, artcraft_launcher_core::nightly::MAX_MANIFEST_BYTES)?;
        crate::nightly::verified_descriptor(app, release, &body, platform).map_err(failure)
    }
}

/// Complete bounded release-page coverage, then verify candidates in canonical preference
/// order. Invalid potentially winning evidence is indeterminate; a compatible winner proves
/// dominated descriptor bodies unnecessary. A full last page never proves complete coverage.
pub(crate) fn lookup<'a>(
    app: &CraftApp,
    platform: Platform,
    now: i64,
    fetch: &'a mut Fetch<'a>,
    elapsed: &'a dyn Fn() -> Duration,
    enabled: &'a dyn Fn() -> bool,
) -> Outcome {
    let mut budget = Budget { fetch, elapsed, enabled, now, requests: 0, bytes: 0 };
    match scan(app, platform, &mut budget) {
        Ok(Some(verified)) => Outcome::Verified(Box::new(verified)),
        Ok(None) => Outcome::NoCompatibleBuild,
        Err(error) => Outcome::Indeterminate(error),
    }
}

fn scan(app: &CraftApp, platform: Platform, budget: &mut Budget<'_>) -> Result<Option<VerifiedRelease>, NightlyHttpError> {
    crate::nightly_profile::admit_new(app, platform).map_err(failure)?;
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();
    let mut covered = false;
    for page in 1..=MAX_PAGES {
        let mut url = format!("https://api.github.com/repos/{}/releases?per_page={PER_PAGE}", app.repo);
        if page > 1 {
            url.push_str(&format!("&page={page}"));
        }
        let body = budget.get(&url, MAX_PAGE_BYTES)?;
        let json: serde_json::Value = serde_json::from_slice(&body).map_err(|_| failure("Unreadable Nightly release page."))?;
        let entries =
            json.as_array().filter(|list| list.len() <= PER_PAGE).ok_or_else(|| failure("The Nightly page was not a bounded release array."))?;
        for item in entries {
            match item["draft"].as_bool() {
                Some(true) => continue,
                Some(false) => {}
                None => return Err(failure("The Nightly page has indeterminate publication state.")),
            }
            if item["prerelease"].as_bool() == Some(false) {
                continue; // Ordinary releases need not use the Nightly tag grammar.
            }
            if item["prerelease"].as_bool().is_none() {
                return Err(failure("The Nightly page has indeterminate channel state."));
            }
            let release = artcraft_launcher_core::release::parse_github_release(&item.to_string(), app.repo).map_err(failure)?;
            let declared_nightly = release.prerelease && release.tag.split(['.', '-']).any(|part| part.eq_ignore_ascii_case("nightly"));
            if !declared_nightly && !artcraft_launcher_core::release::is_nightly(&release) {
                continue;
            }
            artcraft_launcher_core::release::canonical_nightly_version(&release)
                .ok_or_else(|| failure("A Nightly candidate has a noncanonical or mismatched SemVer identity."))?;
            if !seen.insert(release.tag.clone()) {
                return Err(failure("The Nightly pages repeated a release; coverage is indeterminate."));
            }
            candidates.push(release);
        }
        if entries.len() < PER_PAGE {
            budget.coverage_complete()?;
            covered = true;
            break;
        }
    }
    if !covered {
        return Err(failure("The Nightly page budget ended before complete supported-target coverage, even if a candidate was seen."));
    }
    // Candidate identity errors above stay indeterminate; unlike ordinary selection, missing
    // checksum/descriptor evidence must reach verification instead of silently disappearing.
    for release in
        artcraft_launcher_core::release::ranked_nightly_candidates(&candidates, artcraft_launcher_core::release::NightlySelection::Canonical)
    {
        if let Some(verified) = budget.verify(app, release.clone(), platform)? {
            budget.coverage_complete()?;
            return Ok(Some(verified));
        }
    }
    budget.coverage_complete()?;
    Ok(None)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    pub attempted_at: Option<i64>,
    pub verified_at: Option<i64>,
    pub retry_at: Option<i64>,
    /// A bounded display identity only, never an install token or cached release.
    pub tag: Option<String>,
    pub message: Option<String>,
    pub complete: bool,
}

impl Record {
    pub(crate) fn status(&self, fresh: bool) -> NightlyStatus {
        NightlyStatus {
            attempted_at: self.attempted_at,
            verified_at: self.verified_at,
            retry_at: self.retry_at,
            stale: !fresh,
            message: self.message.clone(),
        }
    }

    fn due(&self, now: i64, manual: bool) -> bool {
        if self.retry_at.is_some_and(|at| now < at) {
            return false;
        }
        let wait = if manual { MANUAL_COALESCE } else { EVERY };
        // A future attempt after clock rollback is stale, not an indefinite cadence lockout.
        self.attempted_at.is_none_or(|at| now < at || now.saturating_sub(at) >= wait)
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    records: BTreeMap<String, Record>,
}

#[derive(Default)]
pub(crate) struct State {
    records: BTreeMap<String, Record>,
    pub generation: u64,
    in_flight: bool,
    load_error: Option<String>,
    pending_save: bool,
}

pub(crate) fn key(app: &CraftApp, platform: Platform) -> String {
    let adapter = crate::nightly_profile::for_app(app, platform).map(|adapter| format!("{adapter:?}")).unwrap_or_else(|_| "unsupported".into());
    format!("craft-nightly/v1:{}:{}:{:?}:{:?}:{adapter}", app.repo, app.id, platform.os, platform.arch)
}

impl State {
    fn read_metadata(paths: &Paths) -> Result<Option<Metadata>, String> {
        crate::nightly_profile::validate_paths(paths)?;
        match std::fs::symlink_metadata(paths.nightly_checks()) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
            Ok(_) => {}
        }
        crate::nightly_profile::validate_file(&paths.nightly_checks())?;
        let mut bytes = Vec::new();
        std::fs::File::open(paths.nightly_checks())
            .and_then(|file| file.take(MAX_METADATA + 1).read_to_end(&mut bytes))
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_METADATA {
            return Err("Nightly scheduling metadata exceeds its limit.".into());
        }
        let data: Metadata = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if data.records.len() > MAX_RECORDS
            || data.records.iter().any(|(key, record)| {
                key.len() > 256
                    || record.message.as_ref().is_some_and(|text| text.len() > 512)
                    || record.tag.as_ref().is_some_and(|tag| {
                        tag.len() > 64
                            || artcraft_launcher_core::release::canonical_nightly_version(&Release {
                                tag: tag.clone(),
                                version: version::parse(tag),
                                prerelease: true,
                                ..Default::default()
                            })
                            .is_none()
                    })
                    || [record.attempted_at, record.verified_at, record.retry_at].into_iter().flatten().any(|at| at < 0)
            })
        {
            return Err("Invalid Nightly scheduling metadata.".into());
        }
        Ok(Some(data))
    }

    pub(crate) fn load(paths: &Paths) -> Self {
        match Self::read_metadata(paths) {
            Ok(data) => Self { records: data.unwrap_or_default().records, ..Default::default() },
            Err(error) => Self { load_error: Some(error.chars().take(120).collect()), ..Default::default() },
        }
    }

    /// Only a valid existing file clears a previous read error; absence is not a reset policy.
    pub(crate) fn prepare(&mut self, paths: &Paths) -> Result<(), String> {
        if self.load_error.is_some() {
            match Self::read_metadata(paths) {
                Ok(Some(data)) => {
                    self.records = data.records;
                    self.load_error = None;
                }
                result => {
                    let error = result
                        .err()
                        .unwrap_or_else(|| "Previously unreadable Nightly scheduling metadata is now absent; restore a valid file.".into());
                    self.load_error = Some(error.chars().take(120).collect());
                    return Err(error);
                }
            }
        }
        if self.pending_save {
            self.save(paths)?;
            self.pending_save = false;
        }
        Ok(())
    }

    pub(crate) fn save(&self, paths: &Paths) -> Result<(), String> {
        if let Some(error) = &self.load_error {
            return Err(format!("Nightly scheduling metadata could not be read: {error}"));
        }
        crate::nightly_profile::validate_paths(paths)?;
        crate::nightly_profile::validate_file(&paths.nightly_checks())?;
        let bytes = serde_json::to_vec(&Metadata { records: self.records.clone() }).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_METADATA {
            return Err("Nightly scheduling metadata exceeds its limit.".into());
        }
        crate::paths::write_atomic(&paths.nightly_checks(), &bytes)
    }

    pub(crate) fn record(&self, key: &str) -> Record {
        let mut record = self.records.get(key).cloned().unwrap_or_default();
        if let Some(error) = &self.load_error {
            record.complete = false;
            record.message = Some(format!("Could not read Nightly scheduling metadata: {error}").chars().take(120).collect());
        }
        record
    }

    pub(crate) fn begin(&mut self, keys: &[String], now: i64, manual: bool, enabled: bool) -> Vec<String> {
        if !enabled || self.in_flight || self.load_error.is_some() || self.pending_save {
            return Vec::new();
        }
        // Retain only current source-owned row identities, not an unbounded history.
        self.records.retain(|key, _| keys.contains(key));
        if self.backoff(now).is_some() {
            return Vec::new();
        }
        let chosen: Vec<_> = keys.iter().filter(|key| self.record(key).due(now, manual)).take(MAX_RECORDS).cloned().collect();
        if chosen.is_empty() {
            return chosen;
        }
        for key in &chosen {
            let record = self.records.entry(key.clone()).or_default();
            record.attempted_at = Some(now);
            record.complete = false;
        }
        self.in_flight = true;
        chosen
    }

    pub(crate) fn backoff(&self, now: i64) -> Option<i64> {
        self.records.values().filter_map(|record| record.retry_at).filter(|at| *at > now).max()
    }

    pub(crate) fn finish(&mut self) {
        self.in_flight = false;
    }

    pub(crate) fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        for record in self.records.values_mut() {
            record.complete = false;
        }
    }

    pub(crate) fn fresh(&self, key: &str, now: i64) -> bool {
        let record = self.record(key);
        record.complete && record.verified_at.is_some_and(|at| now >= at && now.saturating_sub(at) < EVERY)
    }

    pub(crate) fn persist_outcome(&mut self, paths: &Paths, key: &str, now: i64, outcome: &Outcome) -> Result<Record, String> {
        let previous = self.records.clone();
        let record = self.outcome(key, now, outcome);
        if let Err(error) = self.save(paths) {
            self.records = previous;
            self.outcome(
                key,
                now,
                &Outcome::Indeterminate(NightlyHttpError {
                    why: format!("Could not persist Nightly result metadata: {error}"),
                    retry_at: record.retry_at,
                }),
            );
            self.pending_save = true;
            return Err(error);
        }
        self.pending_save = false;
        Ok(record)
    }

    pub(crate) fn outcome(&mut self, key: &str, now: i64, outcome: &Outcome) -> Record {
        let record = self.records.entry(key.into()).or_default();
        match outcome {
            Outcome::Verified(verified) => {
                record.verified_at = Some(now);
                record.tag = Some(verified.release().tag.clone());
                record.retry_at = None;
                record.message = None;
                record.complete = true;
            }
            Outcome::NoCompatibleBuild => {
                record.verified_at = Some(now);
                record.tag = None;
                record.retry_at = None;
                record.message = None;
                record.complete = true;
            }
            Outcome::Indeterminate(error) => {
                record.message = Some(error.why.chars().take(120).collect());
                record.retry_at = record.retry_at.into_iter().chain(error.retry_at).filter(|at| *at > now).max();
                record.complete = false;
            }
        }
        record.clone()
    }
}

#[cfg(test)]
#[path = "nightly_lookup_tests.rs"]
pub(crate) mod tests;
