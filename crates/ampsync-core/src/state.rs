//! SQLite job state in `<dest>/.ampsync/state.db` (D7).
//!
//! Tables:
//! - `job`: the `JobSpec` and `RemoteRoot` as JSON plus created/updated timestamps (one row).
//! - `file`: `rel_path` (primary key), `size`, `etag`, `hash`, `status`, `bytes_done`,
//!   `verified_by`, `error`, and `local_name` (the sanitised on-disk relative path when it differs
//!   from `rel_path`, see `names::sanitize`).
//! - `range`: `(rel_path, start, end)` completed byte ranges of the file being downloaded.
//!
//! The database is opened in WAL mode. Engine code must funnel all writes through a single task so
//! lanes never contend for the write lock; `StateDb` itself is a cheap, cloneable handle whose
//! connection sits behind a mutex, so every write is serialised.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use time::OffsetDateTime;

use crate::Error;
use crate::engine::VerifiedBy;
use crate::plan::{JobSpec, RelPath};
use crate::remote::RemoteRoot;

const SCHEMA_VERSION: i64 = 1;

/// Lifecycle of one file in `state.db`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    Pending,
    InProgress,
    Verified,
    Skipped,
    Failed,
}

/// One row of the `file` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileRecord {
    pub rel_path: RelPath,
    pub size: u64,
    pub etag: String,
    /// Server hash as reported (algorithm-prefixed text), if any.
    pub hash: Option<String>,
    pub status: FileStatus,
    pub bytes_done: u64,
    pub verified_by: Option<VerifiedBy>,
    pub error: Option<String>,
    /// On-disk relative path when sanitising changed it.
    pub local_name: Option<String>,
}

/// Exclusive advisory lock on `<dest>/.ampsync/lock`, held from planning to the end of a job so
/// two AmpSync processes (or two jobs) never write into one destination at the same time. The OS
/// releases it when the value is dropped or the process dies, so a crash never leaves it stuck.
#[derive(Debug)]
pub(crate) struct DestLock {
    _file: std::fs::File,
    dest: PathBuf,
}

impl DestLock {
    /// Take the lock for `dest` (already absolute), or `Error::DestinationBusy` when it is held.
    pub(crate) fn acquire(dest: &Path) -> Result<DestLock, Error> {
        let dir = crate::names::long_path(&dest.join(".ampsync"));
        std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        let path = dir.join("lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| Error::io(&path, e))?;
        match file.try_lock() {
            Ok(()) => Ok(DestLock {
                _file: file,
                dest: dest.to_path_buf(),
            }),
            Err(std::fs::TryLockError::WouldBlock) => Err(Error::DestinationBusy {
                path: dest.to_path_buf(),
            }),
            Err(std::fs::TryLockError::Error(e)) => Err(Error::io(&path, e)),
        }
    }

    pub(crate) fn dest(&self) -> &Path {
        &self.dest
    }
}

/// Handle to a job's database.
#[derive(Clone)]
pub struct StateDb {
    conn: Arc<Mutex<Connection>>,
}

fn db_err(e: rusqlite::Error) -> Error {
    Error::State(format!("state database: {e}"))
}

fn json_err(e: serde_json::Error) -> Error {
    Error::State(format!("state database: {e}"))
}

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn to_u64(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

fn status_str(s: FileStatus) -> &'static str {
    match s {
        FileStatus::Pending => "pending",
        FileStatus::InProgress => "in_progress",
        FileStatus::Verified => "verified",
        FileStatus::Skipped => "skipped",
        FileStatus::Failed => "failed",
    }
}

fn parse_status(s: &str) -> Result<FileStatus, Error> {
    Ok(match s {
        "pending" => FileStatus::Pending,
        "in_progress" => FileStatus::InProgress,
        "verified" => FileStatus::Verified,
        "skipped" => FileStatus::Skipped,
        "failed" => FileStatus::Failed,
        other => {
            return Err(Error::State(format!(
                "unknown file status `{other}` in state database"
            )));
        }
    })
}

fn verified_by_str(v: VerifiedBy) -> &'static str {
    match v {
        VerifiedBy::Hash => "hash",
        VerifiedBy::SizeAndEtag => "size_and_etag",
    }
}

fn parse_verified_by(s: &str) -> Option<VerifiedBy> {
    match s {
        "hash" => Some(VerifiedBy::Hash),
        "size_and_etag" => Some(VerifiedBy::SizeAndEtag),
        _ => None,
    }
}

const FILE_COLUMNS: &str = "rel_path, size, etag, hash, status, bytes_done, verified_by, error, local_name";

fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<FileRecord, Error>> {
    let rel: String = row.get(0)?;
    let size: i64 = row.get(1)?;
    let etag: String = row.get(2)?;
    let hash: Option<String> = row.get(3)?;
    let status: String = row.get(4)?;
    let bytes_done: i64 = row.get(5)?;
    let verified_by: Option<String> = row.get(6)?;
    let error: Option<String> = row.get(7)?;
    let local_name: Option<String> = row.get(8)?;
    Ok((|| {
        Ok(FileRecord {
            rel_path: RelPath::new(rel)?,
            size: to_u64(size),
            etag,
            hash,
            status: parse_status(&status)?,
            bytes_done: to_u64(bytes_done),
            verified_by: verified_by.as_deref().and_then(parse_verified_by),
            error,
            local_name,
        })
    })())
}

/// Sorted union of `v`: overlapping and adjacent ranges merged, empty ranges dropped.
pub(crate) fn merge_ranges(mut v: Vec<Range<u64>>) -> Vec<Range<u64>> {
    v.retain(|r| r.start < r.end);
    v.sort_by_key(|r| r.start);
    let mut out: Vec<Range<u64>> = Vec::with_capacity(v.len());
    for r in v {
        match out.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

impl StateDb {
    /// Open (creating if needed) `<dest>/.ampsync/state.db`, enable WAL, create/upgrade tables.
    pub fn open(dest: &Path) -> Result<Self, Error> {
        let dir = crate::names::long_path(&dest.join(".ampsync"));
        std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        let path = dir.join("state.db");
        let conn = Connection::open(&path).map_err(db_err)?;
        Self::init(conn, true)
    }

    /// Private in-memory database for tests.
    pub fn open_in_memory() -> Result<Self, Error> {
        let conn = Connection::open_in_memory().map_err(db_err)?;
        Self::init(conn, false)
    }

    fn init(conn: Connection, wal: bool) -> Result<Self, Error> {
        conn.busy_timeout(std::time::Duration::from_secs(30))
            .map_err(db_err)?;
        if wal {
            conn.pragma_update(None, "journal_mode", "WAL").map_err(db_err)?;
            conn.pragma_update(None, "synchronous", "NORMAL")
                .map_err(db_err)?;
        }
        Self::migrate(&conn)?;
        Ok(StateDb {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Bring the schema up to [`SCHEMA_VERSION`] using `PRAGMA user_version`.
    fn migrate(conn: &Connection) -> Result<(), Error> {
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(db_err)?;
        if version > SCHEMA_VERSION {
            return Err(Error::State(format!(
                "this download folder was created by a newer version of AmpSync (state schema {version})"
            )));
        }
        if version < 1 {
            conn.execute_batch(
                "BEGIN;
                 CREATE TABLE IF NOT EXISTS job (
                     id INTEGER PRIMARY KEY CHECK (id = 1),
                     spec TEXT NOT NULL,
                     root TEXT NOT NULL,
                     created INTEGER NOT NULL,
                     updated INTEGER NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS file (
                     rel_path TEXT PRIMARY KEY NOT NULL,
                     size INTEGER NOT NULL,
                     etag TEXT NOT NULL,
                     hash TEXT,
                     status TEXT NOT NULL,
                     bytes_done INTEGER NOT NULL DEFAULT 0,
                     verified_by TEXT,
                     error TEXT,
                     local_name TEXT
                 );
                 CREATE TABLE IF NOT EXISTS range (
                     rel_path TEXT NOT NULL,
                     start INTEGER NOT NULL,
                     end INTEGER NOT NULL,
                     PRIMARY KEY (rel_path, start)
                 );
                 PRAGMA user_version = 1;
                 COMMIT;",
            )
            .map_err(db_err)?;
        }
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Store (replace) the job description.
    pub fn save_job(&self, spec: &JobSpec, root: &RemoteRoot) -> Result<(), Error> {
        let spec_json = serde_json::to_string(spec).map_err(json_err)?;
        let root_json = serde_json::to_string(root).map_err(json_err)?;
        let now = OffsetDateTime::now_utc().unix_timestamp();
        self.lock()
            .execute(
                "INSERT INTO job (id, spec, root, created, updated) VALUES (1, ?1, ?2, ?3, ?3)
                 ON CONFLICT(id) DO UPDATE SET spec = ?1, root = ?2, updated = ?3",
                params![spec_json, root_json, now],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// The stored job description, if a previous run left one.
    pub fn load_job(&self) -> Result<Option<(JobSpec, RemoteRoot)>, Error> {
        let row: Option<(String, String)> = self
            .lock()
            .query_row("SELECT spec, root FROM job WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
            .map_err(db_err)?;
        match row {
            None => Ok(None),
            Some((spec, root)) => Ok(Some((
                serde_json::from_str(&spec).map_err(json_err)?,
                serde_json::from_str(&root).map_err(json_err)?,
            ))),
        }
    }

    /// Insert or replace a file row.
    pub fn upsert_file(&self, record: &FileRecord) -> Result<(), Error> {
        self.lock()
            .execute(
                "INSERT OR REPLACE INTO file
                 (rel_path, size, etag, hash, status, bytes_done, verified_by, error, local_name)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    record.rel_path.as_str(),
                    to_i64(record.size),
                    record.etag,
                    record.hash,
                    status_str(record.status),
                    to_i64(record.bytes_done),
                    record.verified_by.map(verified_by_str),
                    record.error,
                    record.local_name,
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn file(&self, rel_path: &RelPath) -> Result<Option<FileRecord>, Error> {
        let conn = self.lock();
        let found = conn
            .query_row(
                &format!("SELECT {FILE_COLUMNS} FROM file WHERE rel_path = ?1"),
                [rel_path.as_str()],
                row_to_record,
            )
            .optional()
            .map_err(db_err)?;
        found.transpose()
    }

    /// All file rows, ordered by `rel_path`.
    pub fn files(&self) -> Result<Vec<FileRecord>, Error> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(&format!("SELECT {FILE_COLUMNS} FROM file ORDER BY rel_path"))
            .map_err(db_err)?;
        let rows = stmt.query_map([], row_to_record).map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(db_err)??);
        }
        Ok(out)
    }

    /// Update status (and error text) of one file.
    pub fn set_status(
        &self,
        rel_path: &RelPath,
        status: FileStatus,
        error: Option<&str>,
    ) -> Result<(), Error> {
        self.lock()
            .execute(
                "UPDATE file SET status = ?2, error = ?3 WHERE rel_path = ?1",
                params![rel_path.as_str(), status_str(status), error],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Mark a file verified: status `Verified`, `bytes_done = size`, clears recorded ranges.
    pub fn mark_verified(&self, rel_path: &RelPath, by: VerifiedBy) -> Result<(), Error> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(db_err)?;
        tx.execute(
            "UPDATE file SET status = 'verified', bytes_done = size, verified_by = ?2, error = NULL
             WHERE rel_path = ?1",
            params![rel_path.as_str(), verified_by_str(by)],
        )
        .map_err(db_err)?;
        tx.execute("DELETE FROM range WHERE rel_path = ?1", [rel_path.as_str()])
            .map_err(db_err)?;
        tx.commit().map_err(db_err)
    }

    /// Record that `range` of the file has been written to the part file; adds to `bytes_done`.
    ///
    /// Overlapping and adjacent ranges are merged, so recording the same range twice is harmless
    /// and `bytes_done` always equals the number of distinct bytes recorded.
    pub fn add_range(&self, rel_path: &RelPath, range: Range<u64>) -> Result<(), Error> {
        if range.start >= range.end {
            return Ok(());
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(db_err)?;
        let existing = Self::read_ranges(&tx, rel_path)?;
        let mut all = existing.clone();
        all.push(range);
        let merged = merge_ranges(all);
        if merged != existing {
            tx.execute("DELETE FROM range WHERE rel_path = ?1", [rel_path.as_str()])
                .map_err(db_err)?;
            for r in &merged {
                tx.execute(
                    "INSERT INTO range (rel_path, start, end) VALUES (?1, ?2, ?3)",
                    params![rel_path.as_str(), to_i64(r.start), to_i64(r.end)],
                )
                .map_err(db_err)?;
            }
        }
        let done: u64 = merged.iter().map(|r| r.end - r.start).sum();
        tx.execute(
            "UPDATE file SET bytes_done = ?2 WHERE rel_path = ?1",
            params![rel_path.as_str(), to_i64(done)],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)
    }

    fn read_ranges(conn: &Connection, rel_path: &RelPath) -> Result<Vec<Range<u64>>, Error> {
        let mut stmt = conn
            .prepare("SELECT start, end FROM range WHERE rel_path = ?1 ORDER BY start")
            .map_err(db_err)?;
        let rows = stmt
            .query_map([rel_path.as_str()], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (s, e) = row.map_err(db_err)?;
            out.push(to_u64(s)..to_u64(e));
        }
        Ok(merge_ranges(out))
    }

    /// Completed ranges of a file, sorted and merged.
    pub fn completed_ranges(&self, rel_path: &RelPath) -> Result<Vec<Range<u64>>, Error> {
        Self::read_ranges(&self.lock(), rel_path)
    }

    /// Forget recorded ranges and progress (the part file was deleted).
    pub fn reset_file(&self, rel_path: &RelPath) -> Result<(), Error> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(db_err)?;
        tx.execute("DELETE FROM range WHERE rel_path = ?1", [rel_path.as_str()])
            .map_err(db_err)?;
        tx.execute(
            "UPDATE file SET bytes_done = 0, status = 'pending', verified_by = NULL WHERE rel_path = ?1",
            [rel_path.as_str()],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::ExistingPolicy;
    use url::Url;

    fn rel(s: &str) -> RelPath {
        RelPath::new(s).unwrap()
    }

    fn record(path: &str, size: u64) -> FileRecord {
        FileRecord {
            rel_path: rel(path),
            size,
            etag: "\"e1\"".into(),
            hash: None,
            status: FileStatus::Pending,
            bytes_done: 0,
            verified_by: None,
            error: None,
            local_name: None,
        }
    }

    #[test]
    fn file_round_trip_and_ordering() {
        let db = StateDb::open_in_memory().unwrap();
        assert!(db.file(&rel("a")).unwrap().is_none());
        let mut r = record("b/x.txt", 10);
        r.local_name = Some("b/x\u{FF1F}.txt".into());
        r.hash = Some("quickxor:AAAA".into());
        db.upsert_file(&r).unwrap();
        db.upsert_file(&record("a.txt", 5)).unwrap();
        assert_eq!(db.file(&rel("b/x.txt")).unwrap().unwrap(), r);
        let all = db.files().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].rel_path.as_str(), "a.txt");
        r.size = 11;
        db.upsert_file(&r).unwrap();
        assert_eq!(db.files().unwrap().len(), 2);
        assert_eq!(db.file(&rel("b/x.txt")).unwrap().unwrap().size, 11);
    }

    #[test]
    fn ranges_merge_and_count() {
        let db = StateDb::open_in_memory().unwrap();
        db.upsert_file(&record("f", 100)).unwrap();
        db.add_range(&rel("f"), 10..20).unwrap();
        db.add_range(&rel("f"), 30..40).unwrap();
        assert_eq!(db.completed_ranges(&rel("f")).unwrap(), vec![10..20, 30..40]);
        assert_eq!(db.file(&rel("f")).unwrap().unwrap().bytes_done, 20);
        db.add_range(&rel("f"), 20..30).unwrap();
        assert_eq!(db.completed_ranges(&rel("f")).unwrap(), vec![10..40]);
        db.add_range(&rel("f"), 15..25).unwrap(); // duplicate coverage
        db.add_range(&rel("f"), 5..10).unwrap(); // adjacent before
        assert_eq!(db.completed_ranges(&rel("f")).unwrap(), vec![5..40]);
        assert_eq!(db.file(&rel("f")).unwrap().unwrap().bytes_done, 35);
        db.add_range(&rel("f"), 7..7).unwrap();
        assert_eq!(db.completed_ranges(&rel("f")).unwrap(), vec![5..40]);
    }

    #[test]
    fn verify_and_reset() {
        let db = StateDb::open_in_memory().unwrap();
        db.upsert_file(&record("f", 100)).unwrap();
        db.set_status(&rel("f"), FileStatus::InProgress, None).unwrap();
        db.add_range(&rel("f"), 0..50).unwrap();
        db.reset_file(&rel("f")).unwrap();
        let r = db.file(&rel("f")).unwrap().unwrap();
        assert_eq!((r.bytes_done, r.status), (0, FileStatus::Pending));
        assert!(db.completed_ranges(&rel("f")).unwrap().is_empty());

        db.add_range(&rel("f"), 0..50).unwrap();
        db.set_status(&rel("f"), FileStatus::Failed, Some("boom"))
            .unwrap();
        assert_eq!(
            db.file(&rel("f")).unwrap().unwrap().error.as_deref(),
            Some("boom")
        );
        db.mark_verified(&rel("f"), VerifiedBy::SizeAndEtag).unwrap();
        let r = db.file(&rel("f")).unwrap().unwrap();
        assert_eq!(r.status, FileStatus::Verified);
        assert_eq!(r.bytes_done, 100);
        assert_eq!(r.verified_by, Some(VerifiedBy::SizeAndEtag));
        assert_eq!(r.error, None);
        assert!(db.completed_ranges(&rel("f")).unwrap().is_empty());
    }

    #[test]
    fn job_round_trip() {
        let db = StateDb::open_in_memory().unwrap();
        assert!(db.load_job().unwrap().is_none());
        let mut spec = JobSpec::new("https://contoso.sharepoint.com/x", "dest");
        spec.include = vec!["*.pdf".into()];
        spec.existing = ExistingPolicy::Redownload;
        let root = RemoteRoot {
            site_url: Url::parse("https://contoso.sharepoint.com/sites/Team").unwrap(),
            drive_id: None,
            item_id: None,
            server_relative_path: "/sites/Team/Shared Documents/A".into(),
            display_name: "A".into(),
        };
        db.save_job(&spec, &root).unwrap();
        db.save_job(&spec, &root).unwrap();
        assert_eq!(db.load_job().unwrap().unwrap(), (spec, root));
    }

    #[test]
    fn open_creates_wal_database_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let db = StateDb::open(dir.path()).unwrap();
            db.upsert_file(&record("keep", 1)).unwrap();
            let mode: String = db
                .lock()
                .query_row("PRAGMA journal_mode", [], |r| r.get(0))
                .unwrap();
            assert_eq!(mode.to_lowercase(), "wal");
        }
        assert!(dir.path().join(".ampsync").join("state.db").is_file());
        let db = StateDb::open(dir.path()).unwrap();
        assert!(db.file(&rel("keep")).unwrap().is_some());
    }

    #[test]
    fn newer_schema_is_refused() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        assert!(matches!(StateDb::init(conn, false), Err(Error::State(_))));
    }

    #[test]
    fn concurrent_writers_do_not_lose_updates() {
        let db = StateDb::open_in_memory().unwrap();
        db.upsert_file(&record("f", 1000)).unwrap();
        let handles: Vec<_> = (0..8u64)
            .map(|i| {
                let db = db.clone();
                std::thread::spawn(move || {
                    for k in 0..10u64 {
                        let s = (i * 10 + k) * 10;
                        db.add_range(&rel("f"), s..s + 10).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(db.completed_ranges(&rel("f")).unwrap(), vec![0..800]);
    }
}
