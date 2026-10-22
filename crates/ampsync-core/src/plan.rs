//! What will happen before anything downloads: job description, relative paths, and the plan.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::remote::{RemoteFile, RemoteRoot};

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Plan {
    pub root: RemoteRoot,
    pub items: Vec<PlannedFile>,
    pub totals: Totals,
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
}
