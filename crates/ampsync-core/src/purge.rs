//! Remove everything AmpSync keeps for the current user: for uninstall and "sign out of everything".
//!
//! Deleted: every saved session in the OS keychain (all hosts in the index, then the index), the
//! settings directory, the whole per-user data directory (job state, sign-in profile, queue,
//! update-check cache) and the desktop app's WebView cache. Download destinations are never
//! touched: the only reference to them is a text file inside the data directory.
//!
//! Best effort: a failure is recorded in [`PurgeReport::errors`] and the rest carries on. Symbolic
//! links and junctions inside these directories are removed as links, never followed.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::Error;
use crate::appdirs;
use crate::auth::{KeyringStore, SessionStore};

/// What [`purge_all`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PurgeReport {
    /// Directories that existed and were removed (as far as the errors below allow).
    pub removed: Vec<PathBuf>,
    /// Keychain entries removed: one per saved session plus the host index.
    pub keychain_entries: usize,
    /// Everything that could not be removed, as readable text.
    pub errors: Vec<String>,
}

/// Directories [`purge_with`] removes. [`purge_all`] fills this from the per-user locations.
#[derive(Debug, Clone, Default)]
pub struct PurgeTargets {
    /// Removed recursively (without following links).
    pub dirs: Vec<PathBuf>,
    /// Removed only if empty afterwards (for example the `AmpSync` folder that held `data`).
    pub empty_parents: Vec<PathBuf>,
}

impl PurgeTargets {
    /// The real per-user locations. `AMPSYNC_DATA_DIR`, when set, is not purged as a whole (it may
    /// point anywhere); only its `jobs` folder is.
    pub fn per_user() -> (PurgeTargets, Vec<String>) {
        let mut t = PurgeTargets::default();
        let mut errors = Vec::new();
        match appdirs::default_data_dir() {
            Some(d) => {
                if let Some(parent) = d
                    .parent()
                    .filter(|p| p.file_name().is_some_and(|n| n == "AmpSync"))
                {
                    t.empty_parents.push(parent.to_path_buf());
                }
                t.dirs.push(d);
            }
            None => errors.push("no per-user data directory".to_string()),
        }
        if let Some(over) = appdirs::data_dir()
            && Some(&over) != appdirs::default_data_dir().as_ref()
        {
            t.dirs.push(appdirs::jobs_dir(&over));
        }
        match appdirs::config_dir() {
            Some(d) => {
                if let Some(parent) = d
                    .parent()
                    .filter(|p| p.file_name().is_some_and(|n| n == "AmpSync"))
                {
                    t.empty_parents.push(parent.to_path_buf());
                }
                t.dirs.push(d);
            }
            None => errors.push("no per-user config directory".to_string()),
        }
        if let Some(d) = appdirs::webview_cache_dir() {
            t.dirs.push(d);
        }
        (t, errors)
    }
}

/// Delete every keychain session, the settings, the app data and the WebView cache of this user.
/// Never fails as a whole: see [`PurgeReport::errors`]. Close any running AmpSync window first, since
/// open files (the sign-in profile, a running job's lock) cannot be deleted on Windows.
pub fn purge_all() -> Result<PurgeReport, Error> {
    let (targets, setup_errors) = PurgeTargets::per_user();
    let store = KeyringStore::new();
    let mut report = purge_with(&targets, &store);
    // The index entry goes last; `purge_with` already deleted every host listed in it.
    match store.remove_index() {
        Ok(()) => report.keychain_entries += 1,
        Err(e) => report.errors.push(e.to_string()),
    }
    report.errors.extend(setup_errors);
    Ok(report)
}

/// [`purge_all`] with the locations and the session store injected (tests, custom setups).
pub fn purge_with(targets: &PurgeTargets, store: &dyn SessionStore) -> PurgeReport {
    let mut report = PurgeReport::default();

    match store.hosts() {
        Ok(hosts) => {
            for host in hosts {
                match store.delete(&host) {
                    Ok(()) => report.keychain_entries += 1,
                    Err(e) => report.errors.push(format!("session for {host}: {e}")),
                }
            }
        }
        Err(e) => report.errors.push(format!("saved sessions: {e}")),
    }

    for dir in &targets.dirs {
        remove_tree(dir, &mut report);
    }
    for dir in &targets.empty_parents {
        // Succeeds only when empty; anything else in there is not ours to delete.
        let _ = std::fs::remove_dir(appdirs_long(dir));
    }
    report
}

fn appdirs_long(p: &Path) -> PathBuf {
    crate::names::long_path(p)
}

/// Refuse paths that are certainly not one of our directories: roots, drive roots, relative paths.
fn is_safe_target(path: &Path) -> bool {
    path.is_absolute() && path.parent().is_some() && path.file_name().is_some()
}

fn remove_tree(path: &Path, report: &mut PurgeReport) {
    if !is_safe_target(path) {
        report
            .errors
            .push(format!("refusing to remove {}", path.display()));
        return;
    }
    let long = appdirs_long(path);
    let meta = match std::fs::symlink_metadata(&long) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            report.errors.push(format!("{}: {e}", path.display()));
            return;
        }
    };
    let before = report.errors.len();
    if meta.file_type().is_symlink() || !meta.is_dir() {
        remove_entry(&long, report);
    } else {
        remove_dir_contents(&long, report);
        if let Err(e) = std::fs::remove_dir(&long) {
            note_error(report, path, &e);
        }
    }
    if report.errors.len() == before || !long.exists() {
        report.removed.push(path.to_path_buf());
    }
}

/// Remove a file, or a link (symlink / junction) without following it.
fn remove_entry(path: &Path, report: &mut PurgeReport) {
    let first = std::fs::remove_file(path);
    let result = match first {
        Ok(()) => Ok(()),
        // A directory symlink or junction on Windows is removed with remove_dir.
        Err(_) => std::fs::remove_dir(path).or(first),
    };
    if let Err(e) = result {
        note_error(report, path, &e);
    }
}

/// Record a failure, unless the thing is already gone.
fn note_error(report: &mut PurgeReport, path: &Path, e: &std::io::Error) {
    if e.kind() != std::io::ErrorKind::NotFound {
        report.errors.push(format!("{}: {e}", path.display()));
    }
}

fn remove_dir_contents(dir: &Path, report: &mut PurgeReport) {
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) => {
            report.errors.push(format!("{}: {e}", dir.display()));
            return;
        }
    };
    for entry in read.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() && !t.is_symlink() => {
                remove_dir_contents(&path, report);
                if let Err(e) = std::fs::remove_dir(&path) {
                    note_error(report, &path, &e);
                }
            }
            _ => remove_entry(&path, report),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{MemoryStore, Session, SignInMethod};
    use time::OffsetDateTime;

    fn session(host: &str) -> Session {
        Session {
            host: host.into(),
            cookies: Vec::new(),
            method: SignInMethod::Pasted,
            captured_at: OffsetDateTime::now_utc(),
        }
    }

    fn fill(dir: &Path) {
        std::fs::create_dir_all(dir.join("jobs").join("abc")).unwrap();
        std::fs::write(dir.join("jobs").join("abc").join("state.db"), b"x").unwrap();
        std::fs::write(dir.join("queue.json"), b"[]").unwrap();
    }

    #[test]
    fn removes_sessions_and_directories() {
        let root = tempfile::tempdir().unwrap();
        let (data, config, cache) = (
            root.path().join("AmpSync").join("data"),
            root.path().join("AmpSync").join("config"),
            root.path().join("io.github.ampsync.desktop"),
        );
        fill(&data);
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join("settings.toml"), b"tour_seen = true").unwrap();
        fill(&cache);
        let store = MemoryStore::new();
        store.save(&session("a.sharepoint.com")).unwrap();
        store.save(&session("b.sharepoint.com")).unwrap();

        let targets = PurgeTargets {
            dirs: vec![
                data.clone(),
                config.clone(),
                cache.clone(),
                root.path().join("missing"),
            ],
            empty_parents: vec![root.path().join("AmpSync")],
        };
        let report = purge_with(&targets, &store);

        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.keychain_entries, 2);
        assert_eq!(report.removed, vec![data.clone(), config.clone(), cache.clone()]);
        assert!(store.hosts().unwrap().is_empty());
        assert!(!data.exists() && !config.exists() && !cache.exists());
        assert!(!root.path().join("AmpSync").exists(), "empty parent removed");
    }

    #[test]
    fn a_parent_with_other_content_is_kept() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("AmpSync").join("data");
        fill(&data);
        std::fs::write(root.path().join("AmpSync").join("other.txt"), b"keep").unwrap();
        let targets = PurgeTargets {
            dirs: vec![data.clone()],
            empty_parents: vec![root.path().join("AmpSync")],
        };
        purge_with(&targets, &MemoryStore::new());
        assert!(!data.exists());
        assert!(root.path().join("AmpSync").join("other.txt").exists());
    }

    #[test]
    fn download_destinations_are_never_touched() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let dest = root.path().join("Downloads").join("Project");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("report.pdf"), b"pdf").unwrap();
        let job = crate::appdirs::ensure_job_dir(&data, &dest).unwrap();
        assert!(job.join("dest.txt").exists());
        purge_with(
            &PurgeTargets {
                dirs: vec![data.clone()],
                empty_parents: Vec::new(),
            },
            &MemoryStore::new(),
        );
        assert!(!data.exists());
        assert_eq!(std::fs::read(dest.join("report.pdf")).unwrap(), b"pdf");
    }

    #[test]
    fn links_are_removed_not_followed() {
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("precious.txt"), b"keep").unwrap();
        let data = root.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let link = data.join("link");
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&outside, &link).is_ok();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(&outside, &link).is_ok();
        if !made {
            return; // no permission to create links here
        }
        let report = purge_with(
            &PurgeTargets {
                dirs: vec![data.clone()],
                empty_parents: Vec::new(),
            },
            &MemoryStore::new(),
        );
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!data.exists());
        assert!(outside.join("precious.txt").exists(), "the link target survives");
    }

    #[test]
    fn a_target_that_is_a_link_is_unlinked_only() {
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("precious.txt"), b"keep").unwrap();
        let data = root.path().join("data");
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&outside, &data).is_ok();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(&outside, &data).is_ok();
        if !made {
            return;
        }
        purge_with(
            &PurgeTargets {
                dirs: vec![data.clone()],
                empty_parents: Vec::new(),
            },
            &MemoryStore::new(),
        );
        assert!(std::fs::symlink_metadata(&data).is_err());
        assert!(outside.join("precious.txt").exists());
    }

    #[test]
    fn unsafe_targets_are_refused_and_errors_do_not_stop_the_rest() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        fill(&data);
        let targets = PurgeTargets {
            dirs: vec![
                PathBuf::from("relative"),
                PathBuf::from(if cfg!(windows) { r"C:\" } else { "/" }),
                data.clone(),
            ],
            empty_parents: Vec::new(),
        };
        let report = purge_with(&targets, &MemoryStore::new());
        assert_eq!(report.errors.len(), 2, "{:?}", report.errors);
        assert!(!data.exists());
        assert_eq!(report.removed, vec![data]);
    }
}
