//! Stores PC-built editor sites. The database gates discovery and activation of immutable builds.

mod manifest;
#[cfg(test)]
mod tests;

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use manifest::{check_directory, create_directory, inspect, sync_directory, valid_path};

pub(super) const MAX_FILE_BYTES: usize = 96 * 1024 * 1024;

const MAX_PENDING: i64 = 16;
const MAX_BUILDS: i64 = 64;
const MAX_STORE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const HISTORY: i64 = 128;
const LEASE_SECONDS: i64 = 2 * 60 * 60;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS release_jobs (
    id TEXT PRIMARY KEY,
    app TEXT NOT NULL,
    source_commit TEXT NOT NULL,
    source_tree TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN (
        'queued', 'claimed', 'ready', 'cancelled', 'failed', 'interrupted'
    )),
    queued_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    reason TEXT,
    worker TEXT,
    claim_id TEXT,
    expires_at INTEGER,
    manifest TEXT,
    published INTEGER NOT NULL DEFAULT 0,
    bytes INTEGER NOT NULL DEFAULT 0,
    garbage INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS release_jobs_state ON release_jobs(state, queued_at);
CREATE TABLE IF NOT EXISTS active_releases (
    app TEXT PRIMARY KEY,
    active_id TEXT NOT NULL REFERENCES release_jobs(id),
    previous_id TEXT REFERENCES release_jobs(id)
);";

#[derive(Debug)]
pub(super) enum ReleaseError {
    Invalid,
    NotFound,
    Conflict,
    Incomplete,
    Limit,
    Database(rusqlite::Error),
    Filesystem(io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for ReleaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid => f.write_str("invalid release request or artifact"),
            Self::NotFound => f.write_str("release or artifact not found"),
            Self::Conflict => f.write_str("release state or claim has changed"),
            Self::Incomplete => f.write_str("release files are incomplete"),
            Self::Limit => f.write_str("release queue or storage limit reached"),
            Self::Database(error) => write!(f, "release database: {error}"),
            Self::Filesystem(error) => write!(f, "release filesystem: {error}"),
            Self::Json(error) => write!(f, "release manifest: {error}"),
        }
    }
}

impl std::error::Error for ReleaseError {}

impl From<rusqlite::Error> for ReleaseError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

impl From<io::Error> for ReleaseError {
    fn from(error: io::Error) -> Self {
        Self::Filesystem(error)
    }
}

impl From<serde_json::Error> for ReleaseError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

type Result<T> = std::result::Result<T, ReleaseError>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReleaseRequest {
    pub(super) app: String,
    pub(super) source_commit: String,
    pub(super) source_tree: String,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    pub(super) app: String,
    pub(super) source_commit: String,
    pub(super) source_tree: String,
    pub(super) files: Vec<ManifestFile>,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ManifestFile {
    pub(super) path: String,
    pub(super) size: u64,
    pub(super) sha256: String,
}

#[derive(Debug, Serialize)]
pub(super) struct Release {
    pub(super) id: String,
    #[serde(flatten)]
    pub(super) source: ReleaseRequest,
    pub(super) state: String,
    pub(super) published: bool,
    pub(super) queued_at: i64,
    pub(super) updated_at: i64,
    pub(super) reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct Claim {
    #[serde(flatten)]
    pub(super) release: Release,
    pub(super) claim_id: String,
    pub(super) expires_at: i64,
}

#[derive(Clone)]
pub(super) struct ReleaseService {
    root: PathBuf,
}

impl ReleaseService {
    /// root must be private to the server and separate from its served directories.
    pub(super) fn open(root: &Path) -> Result<Self> {
        fs::create_dir_all(root)?;
        check_directory(root)?;
        create_directory(root, "builds")?;
        let service = Self { root: root.into() };
        let connection = service.connect()?;
        connection.execute_batch("PRAGMA journal_mode = WAL;")?;
        connection.execute_batch(SCHEMA)?;
        service.write(|_, _| Ok(()))?;
        Ok(service)
    }

    pub(super) fn queue(&self, source: ReleaseRequest) -> Result<Release> {
        validate_source(&source)?;
        self.write(|tx, now| {
            let pending: i64 = tx.query_row("SELECT count(*) FROM release_jobs WHERE state IN ('queued', 'claimed')", [], |row| row.get(0))?;
            if pending >= MAX_PENDING {
                return Err(ReleaseError::Limit);
            }
            tx.execute(
                "DELETE FROM release_jobs WHERE id IN (
                    SELECT id FROM release_jobs
                    WHERE state IN ('cancelled', 'failed', 'interrupted') AND garbage = 0
                    ORDER BY updated_at DESC, id DESC LIMIT -1 OFFSET ?1
                )",
                [HISTORY],
            )?;
            let id = Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO release_jobs (id, app, source_commit, source_tree, state,
                    queued_at, updated_at) VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?5)",
                params![id, source.app, source.source_commit, source.source_tree, now],
            )?;
            record(tx, &id)
        })
    }

    pub(super) fn list(&self) -> Result<Vec<Release>> {
        self.write(|tx, _| {
            let mut query = tx.prepare(
                "SELECT id, app, source_commit, source_tree, state, published, queued_at, updated_at, reason
                 FROM release_jobs ORDER BY queued_at DESC, rowid DESC",
            )?;
            let releases = query.query_map([], release_row)?.collect::<rusqlite::Result<_>>()?;
            Ok(releases)
        })
    }

    pub(super) fn claim(&self, worker: &str) -> Result<Option<Claim>> {
        validate_worker(worker)?;
        self.write(|tx, now| {
            let existing = tx
                .query_row(
                    "SELECT id, claim_id, expires_at FROM release_jobs
                     WHERE state = 'claimed' AND worker = ?1",
                    [worker],
                    |row| Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            if let Some((id, claim_id, expires_at)) = existing {
                return Ok(Some(Claim { release: record(tx, &id)?, claim_id, expires_at }));
            }
            let queued = tx
                .query_row(
                    "SELECT id FROM release_jobs WHERE state = 'queued'
                     ORDER BY queued_at, rowid LIMIT 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            let Some(id) = queued else { return Ok(None) };
            let claim_id = Uuid::new_v4().to_string();
            let expires_at = now.saturating_add(LEASE_SECONDS);
            tx.execute(
                "UPDATE release_jobs SET state = 'claimed', worker = ?2, claim_id = ?3,
                    expires_at = ?4, updated_at = ?5 WHERE id = ?1",
                params![id, worker, claim_id, expires_at, now],
            )?;
            Ok(Some(Claim { release: record(tx, &id)?, claim_id, expires_at }))
        })
    }

    pub(super) fn renew(&self, id: &str, worker: &str, claim_id: &str) -> Result<i64> {
        self.write(|tx, now| {
            require_claim(tx, id, worker, claim_id)?;
            let expires_at = now.saturating_add(LEASE_SECONDS);
            tx.execute("UPDATE release_jobs SET expires_at = ?2, updated_at = ?3 WHERE id = ?1", params![id, expires_at, now])?;
            Ok(expires_at)
        })
    }

    pub(super) fn submit_manifest(&self, id: &str, worker: &str, claim_id: &str, mut manifest: Manifest) -> Result<()> {
        let bytes = manifest.validate()?;
        let body = serde_json::to_string(&manifest)?;
        self.write(|tx, now| {
            require_claim(tx, id, worker, claim_id)?;
            let source = record(tx, id)?.source;
            if manifest.app != source.app || manifest.source_commit != source.source_commit || manifest.source_tree != source.source_tree {
                return Err(ReleaseError::Conflict);
            }
            let previous = manifest_json(tx, id)?;
            if let Some(previous) = previous {
                return if previous == body { Ok(()) } else { Err(ReleaseError::Conflict) };
            }
            let (count, reserved): (i64, i64) = tx.query_row(
                "SELECT count(*), coalesce(sum(bytes), 0) FROM release_jobs
                 WHERE (manifest IS NOT NULL AND state IN ('claimed', 'ready')) OR garbage = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let reserved = u64::try_from(reserved).map_err(|_| ReleaseError::Invalid)?;
            if count >= MAX_BUILDS || bytes > MAX_STORE_BYTES.saturating_sub(reserved) {
                return Err(ReleaseError::Limit);
            }
            let bytes = i64::try_from(bytes).map_err(|_| ReleaseError::Limit)?;
            tx.execute("UPDATE release_jobs SET manifest = ?2, bytes = ?3, updated_at = ?4 WHERE id = ?1", params![id, body, bytes, now])?;
            Ok(())
        })
    }

    pub(super) fn upload(&self, id: &str, worker: &str, claim_id: &str, path: &str, bytes: &[u8]) -> Result<()> {
        if !valid_path(path) || bytes.len() > MAX_FILE_BYTES {
            return Err(ReleaseError::Invalid);
        }
        self.write(|tx, _| {
            require_claim(tx, id, worker, claim_id)?;
            let manifest = load_manifest(tx, id)?;
            let entry = manifest.file(path).ok_or(ReleaseError::Invalid)?;
            let build = create_directory(&self.root.join("builds"), id)?;
            let site = create_directory(&build, "site")?;
            manifest::upload(&build, &site, entry, bytes)
        })
    }

    pub(super) fn finalize(&self, id: &str, worker: &str, claim_id: &str) -> Result<Release> {
        self.write(|tx, now| {
            validate_worker(worker)?;
            validate_id(claim_id)?;
            let release = record(tx, id)?;
            if release.state == "ready" {
                let same_claim: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM release_jobs
                     WHERE id = ?1 AND worker = ?2 AND claim_id = ?3)",
                    params![id, worker, claim_id],
                    |row| row.get(0),
                )?;
                return if same_claim { Ok(release) } else { Err(ReleaseError::Conflict) };
            }
            require_claim(tx, id, worker, claim_id)?;
            let manifest = load_manifest(tx, id)?;
            let site = self.site(id)?;
            inspect(&site, &manifest)?;
            sync_directory(&self.root.join("builds").join(id))?;
            sync_directory(&self.root.join("builds"))?;
            sync_directory(&self.root)?;
            tx.execute(
                "UPDATE release_jobs SET state = 'ready', updated_at = ?2, expires_at = NULL
                 WHERE id = ?1",
                params![id, now],
            )?;
            record(tx, id)
        })
    }

    pub(super) fn fail(&self, id: &str, worker: &str, claim_id: &str) -> Result<Release> {
        self.write(|tx, now| {
            require_claim(tx, id, worker, claim_id)?;
            terminal(tx, id, "failed", "worker reported a build failure", now)
        })
    }

    pub(super) fn cancel(&self, id: &str) -> Result<Release> {
        self.write(|tx, now| {
            let current = record(tx, id)?;
            if current.state != "queued" && current.state != "claimed" {
                return Err(ReleaseError::Conflict);
            }
            terminal(tx, id, "cancelled", "cancelled by operator", now)
        })
    }

    pub(super) fn active(&self, app: &str) -> Result<Option<Release>> {
        validate_app(app)?;
        let connection = self.connect()?;
        let id = pointers(&connection, app)?.map(|(active, _)| active);
        id.map(|id| ready(&connection, app, &id)).transpose()
    }

    pub(super) fn activate(&self, app: &str, id: &str) -> Result<Release> {
        validate_app(app)?;
        self.write(|tx, _| {
            ready(tx, app, id)?;
            tx.execute("UPDATE release_jobs SET published = 1 WHERE id = ?1", [id])?;
            // The active and rollback pointers change in the same SQLite transaction.
            tx.execute(
                "INSERT INTO active_releases (app, active_id) VALUES (?1, ?2)
                 ON CONFLICT(app) DO UPDATE SET
                    previous_id = active_releases.active_id, active_id = excluded.active_id
                 WHERE active_releases.active_id != excluded.active_id",
                params![app, id],
            )?;
            record(tx, id)
        })
    }

    pub(super) fn rollback(&self, app: &str, expected_active: &str) -> Result<Release> {
        validate_app(app)?;
        validate_id(expected_active)?;
        self.write(|tx, _| {
            let Some((active, Some(previous))) = pointers(tx, app)? else {
                return Err(ReleaseError::Conflict);
            };
            if active != expected_active {
                return Err(ReleaseError::Conflict);
            }
            let release = ready(tx, app, &previous)?;
            tx.execute("UPDATE active_releases SET active_id = ?2, previous_id = NULL WHERE app = ?1", params![app, previous])?;
            Ok(release)
        })
    }

    /// Set allow_preview only for an authenticated operator. The HTTP layer controls cache policy.
    pub(super) fn asset_path(&self, app: &str, id: &str, path: &str, allow_preview: bool) -> Result<PathBuf> {
        validate_app(app)?;
        if !valid_path(path) {
            return Err(ReleaseError::Invalid);
        }
        let connection = self.connect()?;
        if !ready(&connection, app, id)?.published && !allow_preview {
            return Err(ReleaseError::NotFound);
        }
        let entry = load_manifest(&connection, id)?;
        let file = entry.file(path).ok_or(ReleaseError::NotFound)?;
        manifest::asset_path(&self.site(id)?, file)
    }

    fn connect(&self) -> Result<Connection> {
        let connection = Connection::open(self.root.join("releases.sqlite3"))?;
        connection.busy_timeout(Duration::from_secs(15))?;
        connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = FULL;")?;
        Ok(connection)
    }

    fn write<T>(&self, action: impl FnOnce(&Transaction<'_>, i64) -> Result<T>) -> Result<T> {
        let mut connection = self.connect()?;
        // An immediate transaction serializes claim changes and all staging writes across processes.
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        self.collect_garbage(&tx)?;
        let now = unix_now()?;
        tx.execute(
            "UPDATE release_jobs SET state = 'interrupted', reason = 'worker claim expired',
                updated_at = ?1, worker = NULL, claim_id = NULL, expires_at = NULL,
                manifest = NULL, garbage = 1
             WHERE state = 'claimed' AND expires_at <= ?1",
            [now],
        )?;
        // A rejected request still commits expiry. Garbage collection only deletes earlier,
        // committed terminal builds, so a crash never deletes a resumable claim's files.
        tx.execute_batch("SAVEPOINT release_operation;")?;
        let output = action(&tx, now);
        if output.is_err() {
            tx.execute_batch("ROLLBACK TO release_operation;")?;
        }
        tx.execute_batch("RELEASE release_operation;")?;
        tx.commit()?;
        output
    }

    fn collect_garbage(&self, tx: &Transaction<'_>) -> Result<()> {
        let mut query = tx.prepare("SELECT id FROM release_jobs WHERE garbage = 1")?;
        let ids = query.query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let builds = self.root.join("builds");
        check_directory(&builds)?;
        for id in ids {
            validate_id(&id)?;
            let build = builds.join(&id);
            match check_directory(&build) {
                Ok(()) => {
                    fs::remove_dir_all(&build)?;
                    sync_directory(&builds)?;
                }
                Err(ReleaseError::Filesystem(error)) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            tx.execute("UPDATE release_jobs SET garbage = 0, bytes = 0 WHERE id = ?1", [id])?;
        }
        Ok(())
    }

    fn site(&self, id: &str) -> Result<PathBuf> {
        validate_id(id)?;
        let builds = self.root.join("builds");
        check_directory(&builds)?;
        let build = builds.join(id);
        match check_directory(&build) {
            Err(ReleaseError::Filesystem(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Err(ReleaseError::Incomplete);
            }
            result => result?,
        }
        let site = build.join("site");
        match check_directory(&site) {
            Err(ReleaseError::Filesystem(error)) if error.kind() == io::ErrorKind::NotFound => Err(ReleaseError::Incomplete),
            result => result.map(|()| site),
        }
    }
}

fn release_row(row: &Row<'_>) -> rusqlite::Result<Release> {
    Ok(Release {
        id: row.get(0)?,
        source: ReleaseRequest { app: row.get(1)?, source_commit: row.get(2)?, source_tree: row.get(3)? },
        state: row.get(4)?,
        published: row.get(5)?,
        queued_at: row.get(6)?,
        updated_at: row.get(7)?,
        reason: row.get(8)?,
    })
}

fn record(db: &Connection, id: &str) -> Result<Release> {
    validate_id(id)?;
    db.query_row(
        "SELECT id, app, source_commit, source_tree, state, published, queued_at, updated_at, reason
         FROM release_jobs WHERE id = ?1",
        [id],
        release_row,
    )
    .optional()?
    .ok_or(ReleaseError::NotFound)
}

fn ready(db: &Connection, app: &str, id: &str) -> Result<Release> {
    let release = record(db, id)?;
    if release.state == "ready" && release.source.app == app { Ok(release) } else { Err(ReleaseError::NotFound) }
}

fn pointers(db: &Connection, app: &str) -> Result<Option<(String, Option<String>)>> {
    Ok(db.query_row("SELECT active_id, previous_id FROM active_releases WHERE app = ?1", [app], |row| Ok((row.get(0)?, row.get(1)?))).optional()?)
}

fn terminal(tx: &Transaction<'_>, id: &str, state: &str, reason: &str, now: i64) -> Result<Release> {
    // Cleanup is deferred until the next operation so the terminal state commits before deletion.
    tx.execute(
        "UPDATE release_jobs SET state = ?2, reason = ?3, updated_at = ?4, worker = NULL,
            claim_id = NULL, expires_at = NULL, manifest = NULL, garbage = 1 WHERE id = ?1",
        params![id, state, reason, now],
    )?;
    record(tx, id)
}

fn require_claim(db: &Connection, id: &str, worker: &str, claim_id: &str) -> Result<()> {
    validate_id(id)?;
    validate_id(claim_id)?;
    validate_worker(worker)?;
    let matches: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM release_jobs
         WHERE id = ?1 AND state = 'claimed' AND worker = ?2 AND claim_id = ?3)",
        params![id, worker, claim_id],
        |row| row.get(0),
    )?;
    if matches { Ok(()) } else { Err(ReleaseError::Conflict) }
}

fn manifest_json(db: &Connection, id: &str) -> Result<Option<String>> {
    Ok(db.query_row("SELECT manifest FROM release_jobs WHERE id = ?1", [id], |row| row.get(0))?)
}

fn load_manifest(db: &Connection, id: &str) -> Result<Manifest> {
    let body = manifest_json(db, id)?.ok_or(ReleaseError::Incomplete)?;
    Ok(serde_json::from_str(&body)?)
}

fn validate_id(id: &str) -> Result<()> {
    if Uuid::parse_str(id).is_ok_and(|parsed| parsed.to_string() == id) { Ok(()) } else { Err(ReleaseError::Invalid) }
}

fn validate_worker(worker: &str) -> Result<()> {
    if !worker.is_empty() && worker.len() <= 128 && worker.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte)) {
        Ok(())
    } else {
        Err(ReleaseError::Invalid)
    }
}

fn validate_app(app: &str) -> Result<()> {
    if matches!(app, "photocraft" | "wordcraft" | "gridcraft" | "cadcraft") { Ok(()) } else { Err(ReleaseError::Invalid) }
}

fn validate_source(source: &ReleaseRequest) -> Result<()> {
    validate_source_fields(&source.app, &source.source_commit, &source.source_tree)
}

fn validate_source_fields(app: &str, commit: &str, tree: &str) -> Result<()> {
    validate_app(app)?;
    if lowercase_hex(commit, 40) && lowercase_hex(tree, 40) { Ok(()) } else { Err(ReleaseError::Invalid) }
}

fn lowercase_hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn unix_now() -> Result<i64> {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).map_err(io::Error::other)?;
    i64::try_from(elapsed.as_secs()).map_err(|error| io::Error::other(error).into())
}
