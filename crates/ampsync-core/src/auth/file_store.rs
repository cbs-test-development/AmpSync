//! Opt-in session storage in plain files, for machines without an OS keychain (containers,
//! headless servers). Selected by the CLI through [`crate::auth::SESSION_STORE_ENV`]; nothing uses
//! it unless the user asks.
//!
//! One JSON file per host, `<dir>/<host>.json`, holding the same `Session` JSON the keychain keeps.
//! The files contain credentials: on Unix the directory is created with mode 0700 and files with
//! 0600, and writes go to a temporary file in the same directory that is then renamed over the
//! target, so a reader never sees a half-written session.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::{Session, SessionStore};
use crate::{Error, appdirs};

/// Extension of a stored session.
const EXT: &str = "json";

/// Distinguishes temporary files of concurrent writes within this process.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Sessions as files in one directory. See the module docs.
#[derive(Debug, Clone)]
pub struct FileStore {
    dir: PathBuf,
}

impl FileStore {
    /// A store that keeps its files in `dir` (created on the first save).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        FileStore { dir: dir.into() }
    }

    /// `<data dir>/sessions`; `AMPSYNC_DATA_DIR` is respected.
    pub fn default_location() -> Result<Self, Error> {
        let data = appdirs::data_dir().ok_or_else(|| Error::State("no per-user data directory".into()))?;
        Ok(Self::new(data.join("sessions")))
    }

    /// The directory holding the session files.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path_for(&self, host: &str) -> Result<PathBuf, Error> {
        let host = checked_host(host)?;
        Ok(self.dir.join(format!("{host}.{EXT}")))
    }

    fn ensure_dir(&self) -> Result<(), Error> {
        let mut b = std::fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        b.create(&self.dir).map_err(|e| Error::io(&self.dir, e))
    }
}

/// The lowercase host if it is safe to use as a file name: non-empty, only ASCII letters, digits,
/// `.` and `-`, and no leading `.` (which also rules out `.` and `..`).
fn checked_host(host: &str) -> Result<String, Error> {
    let ok = !host.is_empty()
        && !host.starts_with('.')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    if ok {
        Ok(host.to_ascii_lowercase())
    } else {
        Err(Error::State(format!(
            "invalid host name for a saved session: {host:?}"
        )))
    }
}

/// Create `path` (which must not exist) for writing, private to the current user on Unix.
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

impl SessionStore for FileStore {
    fn load(&self, host: &str) -> Result<Option<Session>, Error> {
        let path = self.path_for(host)?;
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::io(&path, e)),
        };
        // The message must not echo the file: it holds cookie values.
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|_| Error::State(format!("saved session file {} is damaged", path.display())))
    }

    fn save(&self, session: &Session) -> Result<(), Error> {
        let path = self.path_for(&session.host)?;
        let json = serde_json::to_string(session).map_err(|e| Error::State(format!("session file: {e}")))?;
        self.ensure_dir()?;
        let tmp = self.dir.join(format!(
            ".{}.{}-{}.tmp",
            session.host.to_ascii_lowercase(),
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let written = (|| {
            let mut f = create_private(&tmp)?;
            f.write_all(json.as_bytes())?;
            f.sync_all()?;
            drop(f);
            std::fs::rename(&tmp, &path)
        })();
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(Error::io(&path, e));
        }
        Ok(())
    }

    fn delete(&self, host: &str) -> Result<(), Error> {
        let path = self.path_for(host)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::io(&path, e)),
        }
    }

    fn hosts(&self) -> Result<Vec<String>, Error> {
        let read = match std::fs::read_dir(&self.dir) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::io(&self.dir, e)),
        };
        let mut hosts = Vec::new();
        for entry in read {
            let entry = entry.map_err(|e| Error::io(&self.dir, e))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            // Temporary files start with `.` and end in `.tmp`, so they never match.
            if let Some(stem) = name.strip_suffix(".json")
                && let Ok(host) = checked_host(stem)
                && host == stem
            {
                hosts.push(host);
            }
        }
        hosts.sort();
        Ok(hosts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{SessionCookie, SignInMethod};
    use time::OffsetDateTime;

    fn session(host: &str, value: &str) -> Session {
        Session {
            host: host.into(),
            cookies: vec![SessionCookie {
                name: "FedAuth".into(),
                value: value.into(),
                domain: host.into(),
                path: "/".into(),
                expires: None,
            }],
            method: SignInMethod::Pasted,
            captured_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn store() -> (tempfile::TempDir, FileStore) {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::new(tmp.path().join("sessions"));
        (tmp, store)
    }

    #[test]
    fn round_trip_and_missing() {
        let (_tmp, store) = store();
        assert!(store.load("a.sharepoint.com").unwrap().is_none());
        let s = session("a.sharepoint.com", "one");
        store.save(&s).unwrap();
        assert_eq!(store.load("a.sharepoint.com").unwrap().unwrap(), s);
        assert_eq!(store.load("A.SharePoint.com").unwrap().unwrap(), s);
    }

    #[test]
    fn save_overwrites() {
        let (_tmp, store) = store();
        store.save(&session("a.sharepoint.com", "one")).unwrap();
        let second = session("a.sharepoint.com", "two");
        store.save(&second).unwrap();
        assert_eq!(store.load("a.sharepoint.com").unwrap().unwrap(), second);
        assert_eq!(store.hosts().unwrap(), ["a.sharepoint.com"]);
    }

    #[test]
    fn delete_removes_and_tolerates_missing() {
        let (_tmp, store) = store();
        store.delete("a.sharepoint.com").unwrap(); // no directory yet
        store.save(&session("a.sharepoint.com", "one")).unwrap();
        store.delete("a.sharepoint.com").unwrap();
        store.delete("a.sharepoint.com").unwrap();
        assert!(store.load("a.sharepoint.com").unwrap().is_none());
        assert!(store.hosts().unwrap().is_empty());
    }

    #[test]
    fn hosts_are_sorted_and_ignore_other_files() {
        let (_tmp, store) = store();
        assert!(store.hosts().unwrap().is_empty(), "missing dir");
        store.save(&session("b.sharepoint.com", "x")).unwrap();
        store.save(&session("a.sharepoint.com", "x")).unwrap();
        std::fs::write(store.dir().join(".a.sharepoint.com.1-0.tmp"), "x").unwrap();
        std::fs::write(store.dir().join("notes.txt"), "x").unwrap();
        std::fs::write(store.dir().join(".hidden.json"), "x").unwrap();
        assert_eq!(store.hosts().unwrap(), ["a.sharepoint.com", "b.sharepoint.com"]);
    }

    #[test]
    fn invalid_hosts_are_rejected() {
        let (tmp, store) = store();
        for bad in [
            "",
            ".",
            "..",
            ".hidden",
            "../evil",
            "a/b",
            "a\\b",
            "a b",
            "h\u{e9}st",
            "a:b",
        ] {
            assert!(matches!(store.load(bad), Err(Error::State(_))), "load {bad:?}");
            assert!(
                matches!(store.delete(bad), Err(Error::State(_))),
                "delete {bad:?}"
            );
            assert!(
                matches!(store.save(&session(bad, "x")), Err(Error::State(_))),
                "save {bad:?}"
            );
        }
        assert!(!tmp.path().join("evil.json").exists());
    }

    #[test]
    fn damaged_file_is_reported_without_its_content() {
        let (_tmp, store) = store();
        store.save(&session("a.sharepoint.com", "x")).unwrap();
        std::fs::write(store.dir().join("a.sharepoint.com.json"), "SUPERSECRET not json").unwrap();
        match store.load("a.sharepoint.com") {
            Err(Error::State(m)) => assert!(!m.contains("SUPERSECRET"), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn files_and_directory_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, store) = store();
        store.save(&session("a.sharepoint.com", "x")).unwrap();
        let dir_mode = std::fs::metadata(store.dir()).unwrap().permissions().mode() & 0o777;
        let file_mode = std::fs::metadata(store.dir().join("a.sharepoint.com.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
        assert_eq!(file_mode, 0o600);
        // Overwriting keeps the file private and leaves no temporary file behind.
        store.save(&session("a.sharepoint.com", "y")).unwrap();
        let names: Vec<_> = std::fs::read_dir(store.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["a.sharepoint.com.json"]);
    }
}
