//! Account-scoped project history and replayable save receipts.

use std::error::Error;
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use jiff::Timestamp;
use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Builder;

pub(crate) const MAX_FILE_BYTES: usize = 80 * 1024 * 1024;
const MAX_RECENTS: i64 = 100;
const MAX_ACCOUNT_BYTES: usize = 255;
const MAX_OPERATION_BYTES: usize = 128;
const MAX_ID_BYTES: usize = 64;
const MAX_NAME_BYTES: usize = 255;
// Charge metadata, indexes and the idempotency receipt for every retained version.
const SAVE_OVERHEAD: u64 = 4096;

// Project heads refer to an immutable row in the same account and project. The circular foreign
// keys are deferred so a new project and its first version can be inserted atomically.
const SCHEMA: &str = "
    CREATE TABLE projects (
        account TEXT NOT NULL,
        id TEXT NOT NULL,
        app TEXT NOT NULL CHECK (app IN ('photocraft', 'wordcraft', 'gridcraft', 'cadcraft')),
        current_sequence INTEGER NOT NULL,
        PRIMARY KEY (account, id),
        UNIQUE (account, app, id),
        FOREIGN KEY (account, id, current_sequence)
            REFERENCES project_versions (account, project_id, sequence) DEFERRABLE INITIALLY DEFERRED
    ) STRICT, WITHOUT ROWID;
    CREATE INDEX projects_recent ON projects (account, current_sequence DESC);
    CREATE INDEX projects_recent_by_app ON projects (account, app, current_sequence DESC);

    CREATE TABLE project_versions (
        sequence INTEGER PRIMARY KEY,
        account TEXT NOT NULL,
        project_id TEXT NOT NULL,
        version INTEGER NOT NULL CHECK (version > 0),
        name TEXT NOT NULL,
        size INTEGER NOT NULL CHECK (size BETWEEN 1 AND 83886080 AND size = length(bytes)),
        sha256 TEXT NOT NULL CHECK (length(sha256) = 64),
        updated_at INTEGER NOT NULL,
        bytes BLOB NOT NULL,
        UNIQUE (account, project_id, version),
        UNIQUE (account, project_id, sequence),
        FOREIGN KEY (account, project_id)
            REFERENCES projects (account, id) DEFERRABLE INITIALLY DEFERRED
    ) STRICT;

    CREATE TABLE project_operations (
        account TEXT NOT NULL,
        app TEXT NOT NULL,
        operation TEXT NOT NULL,
        requested_project_id TEXT,
        expected_version INTEGER,
        project_id TEXT NOT NULL,
        version INTEGER NOT NULL,
        PRIMARY KEY (account, app, operation),
        FOREIGN KEY (account, app, project_id) REFERENCES projects (account, app, id),
        FOREIGN KEY (account, project_id, version) REFERENCES project_versions (account, project_id, version),
        CHECK ((requested_project_id IS NULL AND expected_version IS NULL AND version = 1)
            OR (requested_project_id IS NOT NULL AND requested_project_id = project_id
                AND expected_version IS NOT NULL AND expected_version > 0 AND expected_version = version - 1))
    ) STRICT, WITHOUT ROWID;

    CREATE TABLE account_bytes (
        account TEXT PRIMARY KEY,
        used INTEGER NOT NULL CHECK (used >= 0)
    ) STRICT, WITHOUT ROWID;
    CREATE TABLE store_bytes (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        used INTEGER NOT NULL CHECK (used >= 0)
    ) STRICT;
    INSERT INTO store_bytes VALUES (1, 0);
    PRAGMA user_version = 2;
";

#[derive(Clone, Copy)]
pub(crate) struct StoreLimits {
    pub(crate) per_user_bytes: u64,
    pub(crate) total_file_bytes: u64,
}

#[derive(Clone)]
pub(crate) struct Store {
    db: Arc<Mutex<Connection>>,
    limits: StoreLimits,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Project {
    pub(crate) id: String,
    pub(crate) app: String,
    pub(crate) name: String,
    pub(crate) version: u64,
    pub(crate) size: u64,
    pub(crate) sha256: String,
    pub(crate) updated_at: i64,
}

pub(crate) struct StoredFile {
    pub(crate) project: Project,
    pub(crate) bytes: Vec<u8>,
}

pub(crate) struct Save<'a> {
    pub(crate) app: &'a str,
    pub(crate) operation: &'a str,
    pub(crate) name: &'a str,
    pub(crate) project_id: Option<&'a str>,
    pub(crate) expected_version: Option<u64>,
    pub(crate) bytes: &'a [u8],
}

#[derive(Debug)]
pub(crate) enum StoreError {
    InvalidInput(&'static str),
    InvalidLimits,
    NotFound,
    VersionConflict { current_version: u64 },
    OperationConflict,
    FileTooLarge,
    QuotaExceeded,
    Busy,
    Unavailable,
    UnsupportedSchema(i64),
    Database(rusqlite::Error),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(reason) => f.write_str(reason),
            Self::InvalidLimits => f.write_str("Invalid project store limits"),
            Self::NotFound => f.write_str("Project or version not found"),
            Self::VersionConflict { current_version } => write!(f, "The current project version is {current_version}"),
            Self::OperationConflict => f.write_str("The save operation ID is already bound to a different request"),
            Self::FileTooLarge => f.write_str("The project exceeds the maximum file size"),
            Self::QuotaExceeded => f.write_str("The project storage quota was exceeded"),
            Self::Busy => f.write_str("The project store is busy"),
            Self::Unavailable | Self::Database(_) => f.write_str("The project store is unavailable"),
            Self::UnsupportedSchema(version) => write!(f, "Unsupported project store schema version {version}"),
        }
    }
}

impl Error for StoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        match error.sqlite_error_code() {
            Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => Self::Busy,
            _ => Self::Database(error),
        }
    }
}

impl Store {
    pub(crate) fn open(path: &Path, limits: StoreLimits) -> Result<Self, StoreError> {
        if limits.per_user_bytes == 0 || limits.per_user_bytes > limits.total_file_bytes || i64::try_from(limits.total_file_bytes).is_err() {
            return Err(StoreError::InvalidLimits);
        }
        if path.as_os_str().is_empty() || path.as_os_str() == ":memory:" {
            return Err(StoreError::InvalidInput("The project store requires a database file"));
        }
        let mut db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE)?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.pragma_update(None, "foreign_keys", "ON")?;
        let journal: String = db.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        if journal != "wal" {
            return Err(StoreError::Unavailable);
        }
        db.pragma_update(None, "synchronous", "FULL")?;

        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        match tx.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))? {
            0 => tx.execute_batch(SCHEMA)?,
            1 => charge_existing_metadata(&tx)?,
            2 => (),
            version => return Err(StoreError::UnsupportedSchema(version)),
        }
        tx.commit()?;
        Ok(Self { db: Arc::new(Mutex::new(db)), limits })
    }

    pub(crate) fn list(&self, account: &str, app: Option<&str>) -> Result<Vec<Project>, StoreError> {
        validate_account(account)?;
        if let Some(app) = app {
            validate_app(app)?;
        }
        let db = self.connection()?;
        let mut statement = db.prepare(
            "SELECT p.id, p.app, v.name, v.version, v.size, v.sha256, v.updated_at
             FROM projects p JOIN project_versions v
               ON v.account = p.account AND v.project_id = p.id AND v.sequence = p.current_sequence
             WHERE p.account = ?1 AND (?2 IS NULL OR p.app = ?2)
             ORDER BY p.current_sequence DESC LIMIT ?3",
        )?;
        let rows = statement.query_map(params![account, app, MAX_RECENTS], project_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub(crate) fn read(&self, account: &str, id: &str, version: Option<u64>) -> Result<StoredFile, StoreError> {
        validate_account(account)?;
        if !bounded(id, MAX_ID_BYTES) {
            return Err(StoreError::InvalidInput("Invalid project ID"));
        }
        let version = version.map(valid_version).transpose()?;
        let db = self.connection()?;
        db.query_row(
            "SELECT p.id, p.app, v.name, v.version, v.size, v.sha256, v.updated_at, v.bytes
             FROM projects p JOIN project_versions head
               ON head.account = p.account AND head.project_id = p.id AND head.sequence = p.current_sequence
             JOIN project_versions v
               ON v.account = p.account AND v.project_id = p.id AND v.version = COALESCE(?3, head.version)
             WHERE p.account = ?1 AND p.id = ?2 AND v.size BETWEEN 1 AND ?4 AND v.size = length(v.bytes)",
            params![account, id, version, MAX_FILE_BYTES as i64],
            |row| Ok(StoredFile { project: project_row(row)?, bytes: row.get(7)? }),
        )
        .optional()?
        .ok_or(StoreError::NotFound)
    }

    pub(crate) fn save(&self, account: &str, input: Save<'_>) -> Result<Project, StoreError> {
        let expected = validate_save(account, &input)?;
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(project) = replay(&tx, account, &input, expected)? {
            tx.commit()?;
            return Ok(project);
        }

        let (id, version) = next_revision(&tx, account, &input, expected)?;
        let size = u64::try_from(input.bytes.len()).map_err(|_| StoreError::FileTooLarge)?;
        reserve_bytes(&tx, account, size + SAVE_OVERHEAD, self.limits)?;
        let project = Project {
            id,
            app: input.app.to_owned(),
            name: input.name.to_owned(),
            version,
            size,
            sha256: format!("{:x}", Sha256::digest(input.bytes)),
            updated_at: Timestamp::try_from(SystemTime::now()).map_err(|_| StoreError::Unavailable)?.as_second(),
        };
        write_save(&tx, account, &input, expected, &project)?;
        tx.commit()?;
        Ok(project)
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, StoreError> {
        self.db.lock().map_err(|_| StoreError::Unavailable)
    }
}

fn validate_account(account: &str) -> Result<(), StoreError> {
    if !bounded(account, MAX_ACCOUNT_BYTES) {
        return Err(StoreError::InvalidInput("Invalid account ID"));
    }
    Ok(())
}

fn validate_app(app: &str) -> Result<(), StoreError> {
    if !matches!(app, "photocraft" | "wordcraft" | "gridcraft" | "cadcraft") {
        return Err(StoreError::InvalidInput("Unsupported app ID"));
    }
    Ok(())
}

fn bounded(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn valid_version(version: u64) -> Result<i64, StoreError> {
    i64::try_from(version).ok().filter(|&version| version > 0).ok_or(StoreError::InvalidInput("Invalid project version"))
}

fn validate_save(account: &str, input: &Save<'_>) -> Result<Option<i64>, StoreError> {
    validate_account(account)?;
    validate_app(input.app)?;
    if !bounded(input.operation, MAX_OPERATION_BYTES) {
        return Err(StoreError::InvalidInput("Invalid save operation ID"));
    }
    if !bounded(input.name, MAX_NAME_BYTES) || input.name.trim().is_empty() || matches!(input.name, "." | "..") || input.name.contains(['/', '\\']) {
        return Err(StoreError::InvalidInput("Invalid project filename"));
    }
    if input.bytes.is_empty() {
        return Err(StoreError::InvalidInput("Project bytes are empty"));
    }
    if input.bytes.len() > MAX_FILE_BYTES {
        return Err(StoreError::FileTooLarge);
    }
    match (input.project_id, input.expected_version) {
        (None, None) => Ok(None),
        (Some(id), Some(version)) if bounded(id, MAX_ID_BYTES) => valid_version(version).map(Some),
        _ => Err(StoreError::InvalidInput("An update requires a project ID and its expected version")),
    }
}

fn project_row(row: &Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: row.get(0)?,
        app: row.get(1)?,
        name: row.get(2)?,
        version: unsigned_column(row, 3)?,
        size: unsigned_column(row, 4)?,
        sha256: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn unsigned_column(row: &Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}

fn replay(tx: &Transaction<'_>, account: &str, input: &Save<'_>, expected: Option<i64>) -> Result<Option<Project>, StoreError> {
    // Compare the original immutable BLOB and request metadata without copying the BLOB back to Rust.
    let row = tx
        .query_row(
            "SELECT p.id, p.app, v.name, v.version, v.size, v.sha256, v.updated_at,
                o.requested_project_id IS ?4 AND o.expected_version IS ?5 AND v.name = ?6 AND v.bytes = ?7
         FROM project_operations o JOIN projects p
           ON p.account = o.account AND p.app = o.app AND p.id = o.project_id
         JOIN project_versions v ON v.account = o.account AND v.project_id = o.project_id AND v.version = o.version
         WHERE o.account = ?1 AND o.app = ?2 AND o.operation = ?3",
            params![account, input.app, input.operation, input.project_id, expected, input.name, input.bytes],
            |row| Ok((project_row(row)?, row.get::<_, bool>(7)?)),
        )
        .optional()?;
    match row {
        Some((project, true)) => Ok(Some(project)),
        Some((_, false)) => Err(StoreError::OperationConflict),
        None => Ok(None),
    }
}

fn next_revision(tx: &Transaction<'_>, account: &str, input: &Save<'_>, expected: Option<i64>) -> Result<(String, u64), StoreError> {
    let Some(id) = input.project_id else {
        let random: Vec<u8> = tx.query_row("SELECT randomblob(16)", [], |row| row.get(0))?;
        let bytes = random.try_into().map_err(|_| StoreError::Unavailable)?;
        return Ok((Builder::from_random_bytes(bytes).into_uuid().to_string(), 1));
    };
    let current: i64 = tx
        .query_row(
            "SELECT v.version FROM projects p JOIN project_versions v
           ON v.account = p.account AND v.project_id = p.id AND v.sequence = p.current_sequence
         WHERE p.account = ?1 AND p.app = ?2 AND p.id = ?3",
            params![account, input.app, id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(StoreError::NotFound)?;
    if expected != Some(current) {
        return Err(StoreError::VersionConflict { current_version: as_unsigned(current)? });
    }
    let version = current.checked_add(1).ok_or(StoreError::Unavailable)?;
    Ok((id.to_owned(), as_unsigned(version)?))
}

fn as_unsigned(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::Unavailable)
}

fn within_quota(used: i64, size: u64, limit: u64) -> Result<i64, StoreError> {
    let next = as_unsigned(used)?.checked_add(size).filter(|&next| next <= limit).ok_or(StoreError::QuotaExceeded)?;
    i64::try_from(next).map_err(|_| StoreError::QuotaExceeded)
}

fn charge_existing_metadata(tx: &Transaction<'_>) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE account_bytes SET used = used + ?1 * (
            SELECT count(*) FROM project_versions v WHERE v.account = account_bytes.account
        )",
        [SAVE_OVERHEAD as i64],
    )?;
    tx.execute("UPDATE store_bytes SET used = used + ?1 * (SELECT count(*) FROM project_versions)", [SAVE_OVERHEAD as i64])?;
    tx.pragma_update(None, "user_version", 2)?;
    Ok(())
}

fn reserve_bytes(tx: &Transaction<'_>, account: &str, size: u64, limits: StoreLimits) -> Result<(), StoreError> {
    let used = tx.query_row("SELECT used FROM account_bytes WHERE account = ?1", [account], |row| row.get(0)).optional()?.unwrap_or_default();
    let total = tx.query_row("SELECT used FROM store_bytes WHERE id = 1", [], |row| row.get(0))?;
    let used = within_quota(used, size, limits.per_user_bytes)?;
    let total = within_quota(total, size, limits.total_file_bytes)?;
    tx.execute(
        "INSERT INTO account_bytes (account, used) VALUES (?1, ?2)
         ON CONFLICT(account) DO UPDATE SET used = excluded.used",
        params![account, used],
    )?;
    tx.execute("UPDATE store_bytes SET used = ?1 WHERE id = 1", [total])?;
    Ok(())
}

fn write_save(tx: &Transaction<'_>, account: &str, input: &Save<'_>, expected: Option<i64>, project: &Project) -> Result<(), StoreError> {
    let version = valid_version(project.version)?;
    let size = i64::try_from(project.size).map_err(|_| StoreError::FileTooLarge)?;
    tx.execute(
        "INSERT INTO project_versions (account, project_id, version, name, size, sha256, updated_at, bytes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![account, project.id, version, project.name, size, project.sha256, project.updated_at, input.bytes],
    )?;
    let sequence = tx.last_insert_rowid();
    if input.project_id.is_some() {
        tx.execute(
            "UPDATE projects SET current_sequence = ?1 WHERE account = ?2 AND app = ?3 AND id = ?4",
            params![sequence, account, project.app, project.id],
        )?;
    } else {
        tx.execute(
            "INSERT INTO projects (account, id, app, current_sequence) VALUES (?1, ?2, ?3, ?4)",
            params![account, project.id, project.app, sequence],
        )?;
    }
    tx.execute(
        "INSERT INTO project_operations (account, app, operation, requested_project_id, expected_version, project_id, version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![account, project.app, input.operation, input.project_id, expected, project.id, version],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, Barrier};
    use std::thread;

    use rusqlite::Connection;
    use uuid::Uuid;

    use super::{Project, SAVE_OVERHEAD, Save, Store, StoreError, StoreLimits};

    struct TestDb(PathBuf);

    impl TestDb {
        fn new() -> Result<Self, Box<dyn Error>> {
            let directory = std::env::temp_dir().join(format!("artcraft-project-store-{}", Uuid::new_v4()));
            fs::create_dir(&directory)?;
            Ok(Self(directory))
        }

        fn file(&self) -> PathBuf {
            self.0.join("test.sqlite3")
        }

        fn store(&self, per_user_bytes: u64, total_file_bytes: u64) -> Result<Store, StoreError> {
            Store::open(&self.file(), StoreLimits { per_user_bytes, total_file_bytes })
        }
    }

    impl Drop for TestDb {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn create<'a>(operation: &'a str, bytes: &'a [u8]) -> Save<'a> {
        Save { app: "photocraft", operation, name: "Única.psd", project_id: None, expected_version: None, bytes }
    }

    fn update<'a>(operation: &'a str, base: &'a Project, bytes: &'a [u8]) -> Save<'a> {
        Save { app: &base.app, operation, name: &base.name, project_id: Some(&base.id), expected_version: Some(base.version), bytes }
    }

    #[test]
    fn isolates_accounts_and_apps_and_preserves_version_bytes() -> Result<(), Box<dyn Error>> {
        let db = TestDb::new()?;
        let store = db.store(100 + 3 * SAVE_OVERHEAD, 200 + 3 * SAVE_OVERHEAD)?;
        let original = store.save("alice", create("request", b"\0\xffpsd"))?;
        assert_eq!(store.read("alice", &original.id, None)?.bytes, b"\0\xffpsd");
        assert_eq!(original.name, "Única.psd");
        assert!(store.list("bob", None)?.is_empty());
        assert!(matches!(store.read("bob", &original.id, Some(1)), Err(StoreError::NotFound)));
        assert!(matches!(store.save("bob", update("request", &original, b"intruder")), Err(StoreError::NotFound)));
        assert!(matches!(store.save("alice", Save { app: "wordcraft", ..update("request", &original, b"wrong app") }), Err(StoreError::NotFound)));
        let word = store.save("alice", Save { app: "wordcraft", name: "Native.docm", ..create("request", b"word") })?;
        assert_eq!(store.list("alice", Some("wordcraft"))?, vec![word]);
        assert_eq!(store.list("alice", Some("photocraft"))?, vec![original.clone()]);
        let revised = store.save("alice", Save { name: "Edited.psd", ..update("next", &original, b"revised") })?;
        assert_eq!(store.read("alice", &original.id, None)?.project, revised);
        let first = store.read("alice", &original.id, Some(1))?;
        assert_eq!(first.project, original);
        assert_eq!(first.bytes, b"\0\xffpsd");
        assert!(matches!(store.read("alice", &original.id, Some(3)), Err(StoreError::NotFound)));
        Ok(())
    }

    #[test]
    fn replays_exact_operations_even_after_the_head_advances() -> Result<(), Box<dyn Error>> {
        let db = TestDb::new()?;
        let store = db.store(20 + 2 * SAVE_OVERHEAD, 20 + 2 * SAVE_OVERHEAD)?;
        let first = store.save("alice", create("initial", b"same"))?;
        let second = store.save("alice", update("second", &first, b"next"))?;
        assert_eq!(store.save("alice", create("initial", b"same"))?, first);
        assert_eq!(store.save("alice", update("second", &first, b"next"))?, second);
        assert!(matches!(store.save("alice", create("initial", b"diff")), Err(StoreError::OperationConflict)));
        assert!(matches!(store.save("alice", Save { name: "Renamed.psd", ..create("initial", b"same") }), Err(StoreError::OperationConflict)));
        assert!(matches!(store.save("alice", update("second", &second, b"next")), Err(StoreError::OperationConflict)));
        assert!(matches!(store.save("alice", update("stale", &first, b"late")), Err(StoreError::VersionConflict { current_version: 2 })));
        assert_eq!(store.read("alice", &first.id, None)?.project, second);
        Ok(())
    }

    #[test]
    fn enforces_both_quotas_across_retained_versions() -> Result<(), Box<dyn Error>> {
        let db = TestDb::new()?;
        let store = db.store(6 + 2 * SAVE_OVERHEAD, 8 + 3 * SAVE_OVERHEAD)?;
        let first = store.save("alice", create("one", b"123"))?;
        let second = store.save("alice", update("two", &first, b"456"))?;
        assert_eq!(store.save("alice", update("two", &first, b"456"))?, second);
        assert!(matches!(store.save("alice", create("over-user", b"7")), Err(StoreError::QuotaExceeded)));
        store.save("bob", create("fits-total", b"78"))?;
        assert!(matches!(store.save("bob", create("over-total", b"9")), Err(StoreError::QuotaExceeded)));
        assert_eq!(store.read("alice", &first.id, Some(1))?.bytes, b"123");
        Ok(())
    }

    #[test]
    fn existing_versions_are_charged_once_and_replays_remain_available() -> Result<(), Box<dyn Error>> {
        let db = TestDb::new()?;
        let store = db.store(10 + SAVE_OVERHEAD, 10 + SAVE_OVERHEAD)?;
        let first = store.save("alice", create("existing", b"x"))?;
        drop(store);
        let raw = Connection::open(db.file())?;
        raw.execute("UPDATE account_bytes SET used = 1", [])?;
        raw.execute("UPDATE store_bytes SET used = 1", [])?;
        raw.pragma_update(None, "user_version", 1)?;
        drop(raw);
        for _ in 0..2 {
            let reopened = db.store(1 + SAVE_OVERHEAD, 1 + SAVE_OVERHEAD)?;
            assert_eq!(reopened.save("alice", create("existing", b"x"))?, first);
            assert_eq!(reopened.read("alice", &first.id, None)?.bytes, b"x");
            assert!(matches!(reopened.save("alice", update("new", &first, b"y")), Err(StoreError::QuotaExceeded)));
        }
        let raw = Connection::open(db.file())?;
        let charged: i64 = raw.query_row("SELECT used FROM store_bytes", [], |row| row.get(0))?;
        assert_eq!(u64::try_from(charged)?, 1 + SAVE_OVERHEAD);
        Ok(())
    }

    #[test]
    fn a_failed_receipt_write_rolls_back_the_head_bytes_and_quota() -> Result<(), Box<dyn Error>> {
        let db = TestDb::new()?;
        let store = db.store(6 + 2 * SAVE_OVERHEAD, 6 + 2 * SAVE_OVERHEAD)?;
        let first = store.save("alice", create("one", b"one"))?;
        let raw = Connection::open(db.file())?;
        raw.execute_batch(
            "CREATE TRIGGER reject_receipt BEFORE INSERT ON project_operations
             WHEN NEW.operation = 'two' BEGIN SELECT RAISE(ABORT, 'test write interruption'); END;",
        )?;
        assert!(matches!(store.save("alice", update("two", &first, b"two")), Err(StoreError::Database(_))));
        assert_eq!(store.read("alice", &first.id, None)?.project, first);
        assert!(matches!(store.read("alice", &first.id, Some(2)), Err(StoreError::NotFound)));
        raw.execute_batch("DROP TRIGGER reject_receipt")?;
        drop(raw);
        drop(store);

        let reopened = db.store(6 + 2 * SAVE_OVERHEAD, 6 + 2 * SAVE_OVERHEAD)?;
        let second = reopened.save("alice", update("two", &first, b"two"))?;
        assert_eq!(reopened.save("alice", update("two", &first, b"two"))?, second);
        assert_eq!(reopened.read("alice", &first.id, Some(2))?.bytes, b"two");
        assert_eq!(reopened.read("alice", &first.id, Some(1))?.bytes, b"one");
        Ok(())
    }

    #[test]
    fn competing_connections_commit_only_one_revision() -> Result<(), Box<dyn Error>> {
        let db = TestDb::new()?;
        let left = db.store(20 + 2 * SAVE_OVERHEAD, 20 + 2 * SAVE_OVERHEAD)?;
        let right = db.store(20 + 2 * SAVE_OVERHEAD, 20 + 2 * SAVE_OVERHEAD)?;
        let base = left.save("alice", create("first", b"base"))?;
        let start = Arc::new(Barrier::new(2));
        let writer = |store: Store, operation: &'static str, bytes: &'static [u8]| {
            let start = Arc::clone(&start);
            let base = base.clone();
            thread::spawn(move || {
                start.wait();
                store.save("alice", update(operation, &base, bytes))
            })
        };
        let a = writer(left, "left", b"left");
        let b = writer(right, "right", b"right");
        let a = a.join().map_err(|_| "left writer panicked")?;
        let b = b.join().map_err(|_| "right writer panicked")?;
        assert!(matches!(
            (&a, &b),
            (Ok(_), Err(StoreError::VersionConflict { current_version: 2 })) | (Err(StoreError::VersionConflict { current_version: 2 }), Ok(_))
        ));
        Ok(())
    }

    #[test]
    fn recents_are_bounded_and_follow_commit_order() -> Result<(), Box<dyn Error>> {
        let db = TestDb::new()?;
        let store = db.store(1000 + 102 * SAVE_OVERHEAD, 1000 + 102 * SAVE_OVERHEAD)?;
        let earliest = store.save("alice", create("old", b"old"))?;
        for number in 0..100 {
            store.save("alice", create(&format!("create-{number}"), b"a"))?;
        }
        let recent = store.list("alice", None)?;
        assert_eq!(recent.len(), 100);
        assert!(!recent.iter().any(|project| project.id == earliest.id));
        let newest = store.save("alice", update("bring-forward", &earliest, b"new"))?;
        assert_eq!(store.list("alice", Some("photocraft"))?.first(), Some(&newest));
        assert_eq!(store.read("alice", &earliest.id, Some(1))?.bytes, b"old");
        Ok(())
    }
}
