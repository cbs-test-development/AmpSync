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
//! lanes never contend for the write lock; `StateDb` itself is a cheap, cloneable handle.

use std::ops::Range;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::Error;
use crate::engine::VerifiedBy;
use crate::plan::{JobSpec, RelPath};
use crate::remote::RemoteRoot;

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

/// Handle to a job's database.
#[derive(Clone)]
#[allow(dead_code)] // stub: used once the implementation lands
pub struct StateDb {
    conn: Arc<Mutex<rusqlite::Connection>>,
}

#[allow(unused_variables)]
impl StateDb {
    /// Open (creating if needed) `<dest>/.ampsync/state.db`, enable WAL, create/upgrade tables.
    pub fn open(dest: &Path) -> Result<Self, Error> {
        todo!()
    }

    /// Private in-memory database for tests.
    pub fn open_in_memory() -> Result<Self, Error> {
        todo!()
    }

    /// Store (replace) the job description.
    pub fn save_job(&self, spec: &JobSpec, root: &RemoteRoot) -> Result<(), Error> {
        todo!()
    }

    /// The stored job description, if a previous run left one.
    pub fn load_job(&self) -> Result<Option<(JobSpec, RemoteRoot)>, Error> {
        todo!()
    }

    /// Insert or replace a file row.
    pub fn upsert_file(&self, record: &FileRecord) -> Result<(), Error> {
        todo!()
    }

    pub fn file(&self, rel_path: &RelPath) -> Result<Option<FileRecord>, Error> {
        todo!()
    }

    /// All file rows, ordered by `rel_path`.
    pub fn files(&self) -> Result<Vec<FileRecord>, Error> {
        todo!()
    }

    /// Update status (and error text) of one file.
    pub fn set_status(
        &self,
        rel_path: &RelPath,
        status: FileStatus,
        error: Option<&str>,
    ) -> Result<(), Error> {
        todo!()
    }

    /// Mark a file verified: status `Verified`, `bytes_done = size`, clears recorded ranges.
    pub fn mark_verified(&self, rel_path: &RelPath, by: VerifiedBy) -> Result<(), Error> {
        todo!()
    }

    /// Record that `range` of the file has been written to the part file; adds to `bytes_done`.
    pub fn add_range(&self, rel_path: &RelPath, range: Range<u64>) -> Result<(), Error> {
        todo!()
    }

    /// Completed ranges of a file, sorted and merged.
    pub fn completed_ranges(&self, rel_path: &RelPath) -> Result<Vec<Range<u64>>, Error> {
        todo!()
    }

    /// Forget recorded ranges and progress (the part file was deleted).
    pub fn reset_file(&self, rel_path: &RelPath) -> Result<(), Error> {
        todo!()
    }
}
