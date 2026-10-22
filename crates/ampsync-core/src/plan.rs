//! What will happen before anything downloads: job description, relative paths, and the plan.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::engine::VerifiedBy;
use crate::link;
use crate::names;
use crate::remote::{Remote, RemoteEntry, RemoteFile, RemoteRoot};
use crate::state::{DestLock, FileRecord, FileStatus, StateDb};

/// A path relative to the download root, `/`-separated on every platform.
///
/// Invariants (checked by [`RelPath::new`] and on deserialisation): non-empty; no leading or
/// trailing `/`; no empty, `.` or `..` components; no `\` and no NUL. These make it safe to join
/// onto a local directory without escaping it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RelPath(String);

/// Why a string is not a valid [`RelPath`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid relative path `{0}`")]
pub struct InvalidRelPath(pub String);

impl From<InvalidRelPath> for Error {
    fn from(e: InvalidRelPath) -> Self {
        Error::State(e.to_string())
    }
}

impl RelPath {
    pub fn new(s: impl Into<String>) -> Result<Self, InvalidRelPath> {
        let s = s.into();
        let ok = !s.is_empty()
            && !s.starts_with('/')
            && !s.ends_with('/')
            && !s.contains(['\\', '\0'])
            && s.split('/').all(|c| !c.is_empty() && c != "." && c != "..");
        if ok {
            Ok(RelPath(s))
        } else {
            Err(InvalidRelPath(s))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Append one or more components (`"a"` or `"a/b"`), validated.
    pub fn join(&self, tail: &str) -> Result<Self, InvalidRelPath> {
        RelPath::new(format!("{}/{}", self.0, tail))
    }

    /// Everything before the last `/`, or `None` for a single component.
    pub fn parent(&self) -> Option<RelPath> {
        self.0.rsplit_once('/').map(|(p, _)| RelPath(p.to_string()))
    }

    /// The last component.
    pub fn file_name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }

    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }

    /// `base` joined with this path using the platform separator.
    pub fn to_path(&self, base: &Path) -> PathBuf {
        let mut p = base.to_path_buf();
        p.extend(self.components());
        p
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for RelPath {
    type Error = InvalidRelPath;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        RelPath::new(s)
    }
}

impl From<RelPath> for String {
    fn from(p: RelPath) -> String {
        p.0
    }
}

impl AsRef<str> for RelPath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// What to do with a file that already exists locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExistingPolicy {
    /// Skip a file whose local size matches and whose recorded remote eTag is unchanged;
    /// resume a partial `.ampsync-part`. (D1 mirror behaviour)
    #[default]
    SkipUnchanged,
    /// Download everything again.
    Redownload,
}

/// Everything the user decides about one download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobSpec {
    /// Whatever the user pasted; re-parsed with `link::parse` when the job starts.
    pub source: String,
    /// Local folder the remote root's contents are written into.
    pub dest: PathBuf,
    /// Glob patterns on `rel_path` (`globset` syntax, `/` separators); empty means everything.
    pub include: Vec<String>,
    /// Glob patterns that remove files matched by `include`.
    pub exclude: Vec<String>,
    pub existing: ExistingPolicy,
    /// Delete local files that no longer exist remotely. CLI only (`--prune`); default false (D1).
    pub prune: bool,
}

impl JobSpec {
    /// A spec with defaults: everything included, `SkipUnchanged`, no prune.
    pub fn new(source: impl Into<String>, dest: impl Into<PathBuf>) -> Self {
        JobSpec {
            source: source.into(),
            dest: dest.into(),
            include: Vec::new(),
            exclude: Vec::new(),
            existing: ExistingPolicy::default(),
            prune: false,
        }
    }
}

/// What the engine will do for one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlannedAction {
    /// Fetch the whole file.
    Download,
    /// A valid partial exists: `have` bytes are already on disk and recorded in `state.db`.
    Resume { have: u64 },
    /// Already present and unchanged.
    SkipUnchanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlannedFile {
    pub file: RemoteFile,
    pub action: PlannedAction,
}

/// Counts shown in the "ready to download" confirmation and in `Event::Planned`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Totals {
    /// Files that will be (re)downloaded or resumed.
    pub files: u64,
    /// Bytes still to transfer for those files (a resumed file counts only its missing bytes).
    pub bytes: u64,
    /// Files skipped as unchanged.
    pub skipped: u64,
    /// Total size of the skipped files.
    pub skipped_bytes: u64,
}

/// The full result of listing and comparing: nothing has been downloaded yet.
///
/// A plan keeps the destination folder locked (a lock file in the job's app-data directory) until it is dropped or
/// handed to `Engine::start`, which moves the lock into the job. While it is held, planning or
/// running another download into the same folder fails with `Error::DestinationBusy`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Plan {
    pub root: RemoteRoot,
    pub items: Vec<PlannedFile>,
    pub totals: Totals,
    #[serde(skip)]
    pub(crate) lock: PlanLock,
}

/// Shared slot for the destination lock; clones of a plan share it, and the job that starts first
/// takes the lock out so a leftover clone does not keep the folder locked after the job ends.
#[derive(Clone, Default)]
pub(crate) struct PlanLock(Arc<Mutex<Option<DestLock>>>);

impl PlanLock {
    fn holding(lock: DestLock) -> Self {
        PlanLock(Arc::new(Mutex::new(Some(lock))))
    }

    /// Take the lock out of the plan (once).
    pub(crate) fn take(&self) -> Option<DestLock> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).take()
    }
}

impl fmt::Debug for PlanLock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PlanLock")
    }
}

/// The lock is not part of a plan's identity.
impl PartialEq for PlanLock {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for PlanLock {}

/// Suffix of the file a download is written to before it is verified and renamed.
pub(crate) const PART_SUFFIX: &str = ".ampsync-part";

/// Compiled `include` / `exclude` globs. Matching is case-insensitive (SharePoint is) and runs on
/// the `/`-separated relative path; `*` also crosses `/`, so `*.pdf` matches at any depth.
#[derive(Debug)]
pub(crate) struct Filters {
    include: Option<GlobSet>,
    exclude: Option<GlobSet>,
}

impl Filters {
    pub(crate) fn new(include: &[String], exclude: &[String]) -> Result<Self, Error> {
        fn build(patterns: &[String]) -> Result<Option<GlobSet>, Error> {
            if patterns.is_empty() {
                return Ok(None);
            }
            let mut b = GlobSetBuilder::new();
            for p in patterns {
                let glob = GlobBuilder::new(p)
                    .case_insensitive(true)
                    .build()
                    .map_err(|e| Error::State(format!("invalid pattern `{p}`: {e}")))?;
                b.add(glob);
            }
            b.build()
                .map(Some)
                .map_err(|e| Error::State(format!("invalid patterns: {e}")))
        }
        Ok(Filters {
            include: build(include)?,
            exclude: build(exclude)?,
        })
    }

    pub(crate) fn matches(&self, rel_path: &str) -> bool {
        self.include.as_ref().is_none_or(|g| g.is_match(rel_path))
            && !self.exclude.as_ref().is_some_and(|g| g.is_match(rel_path))
    }
}

/// The relative path a file gets on this machine: unchanged except on Windows, where illegal
/// characters and reserved names are replaced (the mapping is recorded in `state.db`).
pub(crate) fn local_rel_path(rel: &RelPath) -> RelPath {
    if cfg!(windows) {
        names::sanitize_rel_path(rel)
    } else {
        rel.clone()
    }
}

/// `dest` made absolute so `\\?\` long-path handling applies on Windows.
pub(crate) fn absolute_dest(dest: &Path) -> PathBuf {
    std::path::absolute(dest).unwrap_or_else(|_| dest.to_path_buf())
}

/// Final and part paths of a file (both long-path prefixed on Windows), ready for `std::fs`.
pub(crate) fn local_paths(dest_abs: &Path, rel: &RelPath) -> (PathBuf, PathBuf) {
    let final_path = local_rel_path(rel).to_path(dest_abs);
    let mut part_name = final_path.file_name().unwrap_or_default().to_os_string();
    part_name.push(PART_SUFFIX);
    let part_path = final_path.with_file_name(part_name);
    (names::long_path(&final_path), names::long_path(&part_path))
}

/// `name` differs between remote and local naming: the value to store as `local_name`.
pub(crate) fn local_name_record(rel: &RelPath) -> Option<String> {
    let local = local_rel_path(rel);
    (local != *rel).then(|| local.as_str().to_string())
}

/// Hash as stored in `state.db` (algorithm-prefixed text).
pub(crate) fn hash_text(file: &RemoteFile) -> Option<String> {
    use crate::remote::RemoteHash;
    file.hash.as_ref().map(|h| match h {
        RemoteHash::QuickXor(v) => format!("quickxor:{v}"),
        RemoteHash::Sha256(v) => format!("sha256:{v}"),
    })
}

/// List the remote tree, apply the filters, compare with the destination and `state.db`.
pub(crate) async fn build<R: Remote + ?Sized>(
    remote: &R,
    spec: &JobSpec,
    data_dir: Option<&Path>,
) -> Result<Plan, Error> {
    let filters = Filters::new(&spec.include, &spec.exclude)?;
    let dest = absolute_dest(&spec.dest);
    let lock = {
        let (dest, data_dir) = (dest.clone(), data_dir.map(Path::to_path_buf));
        tokio::task::spawn_blocking(move || DestLock::acquire(&dest, data_dir.as_deref()))
            .await
            .map_err(|e| Error::State(format!("background task failed: {e}")))??
    };
    let parsed = link::parse(&spec.source)?;
    let root = remote.resolve(&parsed).await?;
    let db = StateDb::open_in(data_dir, &dest)?;
    let known: HashMap<RelPath, FileRecord> =
        db.files()?.into_iter().map(|r| (r.rel_path.clone(), r)).collect();

    let mut items = Vec::new();
    let mut totals = Totals::default();
    let mut walk = remote.walk(&root);
    while let Some(entry) = walk.next().await {
        let RemoteEntry::File(file) = entry? else {
            continue;
        };
        if !filters.matches(file.rel_path.as_str()) {
            continue;
        }
        let action = decide(&file, known.get(&file.rel_path), &db, &dest, spec.existing)?;
        match action {
            PlannedAction::SkipUnchanged => {
                totals.skipped += 1;
                totals.skipped_bytes += file.size;
            }
            PlannedAction::Resume { have } => {
                totals.files += 1;
                totals.bytes += file.size.saturating_sub(have);
            }
            PlannedAction::Download => {
                totals.files += 1;
                totals.bytes += file.size;
            }
        }
        items.push(PlannedFile { file, action });
    }
    drop(walk);
    Ok(Plan {
        root,
        items,
        totals,
        lock: PlanLock::holding(lock),
    })
}

fn decide(
    file: &RemoteFile,
    record: Option<&FileRecord>,
    db: &StateDb,
    dest: &Path,
    existing: ExistingPolicy,
) -> Result<PlannedAction, Error> {
    let (final_path, part_path) = local_paths(dest, &file.rel_path);
    let same_version = record.is_some_and(|r| r.etag == file.etag && r.size == file.size);

    if existing == ExistingPolicy::SkipUnchanged
        && same_version
        && record.is_some_and(|r| r.status == FileStatus::Verified)
        && std::fs::metadata(&final_path).is_ok_and(|m| m.is_file() && m.len() == file.size)
    {
        return Ok(PlannedAction::SkipUnchanged);
    }

    // No record at all (a copied folder, deleted state): trust a local file whose size and
    // modification time equal the remote's. The engine sets the mtime from the remote after the
    // rename, so a finished download always satisfies this. Remember the file as verified.
    if existing == ExistingPolicy::SkipUnchanged
        && record.is_none()
        && local_matches_remote(&final_path, file)
    {
        db.upsert_file(&FileRecord {
            rel_path: file.rel_path.clone(),
            size: file.size,
            etag: file.etag.clone(),
            hash: hash_text(file),
            status: FileStatus::Verified,
            bytes_done: file.size,
            verified_by: Some(VerifiedBy::SizeAndMtime),
            error: None,
            local_name: local_name_record(&file.rel_path),
        })?;
        return Ok(PlannedAction::SkipUnchanged);
    }

    if same_version
        && record.is_some_and(|r| r.status != FileStatus::Verified)
        && std::fs::metadata(&part_path).is_ok_and(|m| m.is_file() && m.len() == file.size)
    {
        let have: u64 = db
            .completed_ranges(&file.rel_path)?
            .iter()
            .map(|r| r.end.min(file.size).saturating_sub(r.start))
            .sum();
        if have > 0 {
            return Ok(PlannedAction::Resume { have });
        }
    }
    Ok(PlannedAction::Download)
}

/// Slack when comparing modification times (FAT stores 2-second steps).
const MTIME_SLACK: std::time::Duration = std::time::Duration::from_secs(2);

/// The local file has the remote's size and (within [`MTIME_SLACK`]) its modification time.
fn local_matches_remote(path: &Path, file: &RemoteFile) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() || meta.len() != file.size {
        return false;
    }
    let Ok(local) = meta.modified() else {
        return false;
    };
    let remote = std::time::SystemTime::from(file.modified);
    let diff = match local.duration_since(remote) {
        Ok(d) => d,
        Err(e) => e.duration(),
    };
    diff <= MTIME_SLACK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rel_path_validation() {
        for ok in ["a", "a/b", "a b/c.txt", "ü/é", "a:b/c"] {
            assert!(RelPath::new(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", "/a", "a/", "a//b", "../a", "a/..", "a/./b", ".", "..", "a\\b", "a\0b",
        ] {
            assert!(RelPath::new(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn rel_path_helpers() {
        let p = RelPath::new("a/b/c.txt").unwrap();
        assert_eq!(p.file_name(), "c.txt");
        assert_eq!(p.parent().unwrap().as_str(), "a/b");
        assert!(RelPath::new("x").unwrap().parent().is_none());
        assert_eq!(p.components().collect::<Vec<_>>(), ["a", "b", "c.txt"]);
        assert_eq!(p.join("d").unwrap().as_str(), "a/b/c.txt/d");
        assert!(p.join("..").is_err());
        assert_eq!(
            p.to_path(Path::new("base")),
            Path::new("base").join("a").join("b").join("c.txt")
        );
        assert_eq!(p.to_string(), "a/b/c.txt");
    }

    #[test]
    fn rel_path_serde_validates() {
        let p = RelPath::new("a/b").unwrap();
        assert_eq!(serde_json::to_string(&p).unwrap(), "\"a/b\"");
        assert_eq!(serde_json::from_str::<RelPath>("\"a/b\"").unwrap(), p);
        assert!(serde_json::from_str::<RelPath>("\"../x\"").is_err());
    }

    #[test]
    fn job_spec_defaults() {
        let s = JobSpec::new("link", "dest");
        assert_eq!(s.existing, ExistingPolicy::SkipUnchanged);
        assert!(!s.prune && s.include.is_empty() && s.exclude.is_empty());
    }

    #[test]
    fn filters_are_case_insensitive_and_cross_directories() {
        let f = Filters::new(&["*.pdf".into()], &["tmp/**".into()]).unwrap();
        assert!(f.matches("a.PDF") && f.matches("x/y/a.pdf"));
        assert!(!f.matches("a.txt") && !f.matches("tmp/a.pdf"));
        let all = Filters::new(&[], &[]).unwrap();
        assert!(all.matches("anything/at/all"));
        assert!(Filters::new(&["[".into()], &[]).is_err());
    }

    #[test]
    fn local_paths_put_the_part_file_next_to_the_file() {
        let (fin, part) = local_paths(Path::new("dest"), &RelPath::new("a/b.txt").unwrap());
        assert!(part.to_string_lossy().ends_with("b.txt.ampsync-part"));
        assert_eq!(fin.parent(), part.parent());
    }
}
