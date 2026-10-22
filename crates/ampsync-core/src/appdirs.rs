//! Where AmpSync keeps its own files. Nothing of AmpSync lives in a download destination once a job
//! is done; job state sits under the per-user data directory, keyed by the destination path.
//!
//! - data: `ProjectDirs::from("", "", "AmpSync").data_local_dir()` (Windows
//!   `%LOCALAPPDATA%\AmpSync\data`, Linux `~/.local/share/ampsync`). Holds `jobs/<key>/`, the sign-in
//!   profile, `queue.json` and `update-check.json`. The environment variable `AMPSYNC_DATA_DIR`
//!   replaces this directory (used by tests and for portable setups).
//! - config: `ProjectDirs::config_dir()`, holds `settings.toml`.
//! - webview cache: `<local data>/io.github.ampsync.desktop`, owned by the desktop window.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use directories::{BaseDirs, ProjectDirs};
use sha2::{Digest, Sha256};

use crate::Error;

/// Environment variable that overrides the per-user data directory (see the module docs).
pub const DATA_DIR_ENV: &str = "AMPSYNC_DATA_DIR";

/// Folder name of the desktop app's WebView cache inside the local data directory.
pub const WEBVIEW_CACHE_NAME: &str = "io.github.ampsync.desktop";

fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("", "", "AmpSync")
}

/// The OS default data directory, ignoring `AMPSYNC_DATA_DIR`.
pub fn default_data_dir() -> Option<PathBuf> {
    project_dirs().map(|d| d.data_local_dir().to_path_buf())
}

/// The data directory in use: `AMPSYNC_DATA_DIR` when set and non-empty, else the OS default.
pub fn data_dir() -> Option<PathBuf> {
    match std::env::var_os(DATA_DIR_ENV) {
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => default_data_dir(),
    }
}

/// The per-user config directory (`settings.toml` lives here).
pub fn config_dir() -> Option<PathBuf> {
    project_dirs().map(|d| d.config_dir().to_path_buf())
}

/// The desktop app's WebView cache directory.
pub fn webview_cache_dir() -> Option<PathBuf> {
    BaseDirs::new().map(|d| d.data_local_dir().join(WEBVIEW_CACHE_NAME))
}

/// `explicit` if given, else [`data_dir`], else an error (no home directory).
pub fn resolve_data_dir(explicit: Option<&Path>) -> Result<PathBuf, Error> {
    match explicit {
        Some(p) => Ok(p.to_path_buf()),
        None => data_dir().ok_or_else(|| Error::State("no per-user data directory".into())),
    }
}

/// `<data_dir>/jobs`.
pub fn jobs_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("jobs")
}

/// Remove the `\\?\` / `\\?\UNC\` prefix Windows canonicalisation adds.
fn strip_verbatim(path: PathBuf) -> PathBuf {
    let Some(s) = path.to_str() else {
        return path;
    };
    if let Some(unc) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{unc}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path
}

/// `dest` made absolute with symlinks resolved as far as the path exists (the rest is appended
/// unchanged), without any `\\?\` prefix. Stable across the folder being created later.
pub fn canonical_dest(dest: &Path) -> PathBuf {
    let abs = std::path::absolute(dest).unwrap_or_else(|_| dest.to_path_buf());
    let mut tail: Vec<OsString> = Vec::new();
    let mut base = abs.clone();
    loop {
        match std::fs::canonicalize(&base) {
            Ok(mut c) => {
                for part in tail.iter().rev() {
                    c.push(part);
                }
                return strip_verbatim(c);
            }
            Err(_) => match (base.file_name().map(|n| n.to_os_string()), base.parent()) {
                (Some(name), Some(parent)) => {
                    tail.push(name);
                    base = parent.to_path_buf();
                }
                _ => return strip_verbatim(abs),
            },
        }
    }
}

/// Text hashed to make the job key: the canonical destination, case-folded with `\` separators and
/// no trailing separator on Windows (where paths are case-insensitive).
fn key_text(canonical: &Path) -> String {
    let s = canonical.to_string_lossy().to_string();
    if cfg!(windows) {
        let s = s.replace('/', "\\").to_lowercase();
        let trimmed = s.trim_end_matches('\\');
        if trimmed.ends_with(':') {
            // `c:\` must stay distinct from `c:` (the drive's current directory).
            format!("{trimmed}\\")
        } else {
            trimmed.to_string()
        }
    } else if s.len() > 1 {
        s.trim_end_matches('/').to_string()
    } else {
        s
    }
}

/// Lowercase hex of the first 16 bytes of SHA-256 over the canonical destination path.
pub fn job_key(dest: &Path) -> String {
    let digest = Sha256::digest(key_text(&canonical_dest(dest)).as_bytes());
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// `<data_dir>/jobs/<key>` for `dest`. Nothing is created.
pub fn job_dir(data_dir: &Path, dest: &Path) -> PathBuf {
    jobs_dir(data_dir).join(job_key(dest))
}

/// Create the job directory for `dest` (if needed) and keep a human-readable `dest.txt` in it.
pub fn ensure_job_dir(data_dir: &Path, dest: &Path) -> Result<PathBuf, Error> {
    let dir = job_dir(data_dir, dest);
    let long = crate::names::long_path(&dir);
    std::fs::create_dir_all(&long).map_err(|e| Error::io(&long, e))?;
    let text = canonical_dest(dest).to_string_lossy().to_string();
    let file = long.join("dest.txt");
    if std::fs::read_to_string(&file).ok().as_deref() != Some(text.as_str()) {
        // Only a debugging aid: never fail a job because of it.
        if let Err(e) = std::fs::write(&file, &text) {
            tracing::warn!("cannot write {}: {e}", file.display());
        }
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_stable_32_hex_and_differs_per_destination() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let ka = job_key(a.path());
        assert_eq!(ka.len(), 32);
        assert!(
            ka.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_eq!(ka, job_key(a.path()));
        assert_ne!(ka, job_key(b.path()));
    }

    #[test]
    fn key_does_not_change_when_the_folder_is_created_later() {
        let base = tempfile::tempdir().unwrap();
        let dest = base.path().join("later").join("deeper");
        let before = job_key(&dest);
        std::fs::create_dir_all(&dest).unwrap();
        assert_eq!(before, job_key(&dest));
        assert_eq!(before, job_key(&dest.join("..").join("deeper")));
    }

    #[test]
    fn verbatim_prefix_is_stripped() {
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\C:\Users\x")),
            PathBuf::from(r"C:\Users\x")
        );
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\UNC\srv\share\d")),
            PathBuf::from(r"\\srv\share\d")
        );
        assert_eq!(strip_verbatim(PathBuf::from("/a/b")), PathBuf::from("/a/b"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_keys_ignore_case_and_separators() {
        let base = tempfile::tempdir().unwrap();
        let dest = base.path().join("MixedCase");
        std::fs::create_dir_all(&dest).unwrap();
        let swapped = PathBuf::from(dest.to_string_lossy().to_lowercase().replace('\\', "/"));
        assert_eq!(job_key(&dest), job_key(&swapped));
    }

    #[test]
    fn ensure_job_dir_writes_dest_txt() {
        let data = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        let dir = ensure_job_dir(data.path(), dest.path()).unwrap();
        assert!(dir.starts_with(data.path().join("jobs")));
        let text = std::fs::read_to_string(dir.join("dest.txt")).unwrap();
        assert_eq!(PathBuf::from(text), canonical_dest(dest.path()));
        assert_eq!(dir, ensure_job_dir(data.path(), dest.path()).unwrap());
    }

    #[test]
    fn data_dir_names_are_ampsync_owned() {
        if let Some(p) = default_data_dir() {
            assert!(p.to_string_lossy().to_lowercase().contains("ampsync"));
        }
        if let Some(p) = webview_cache_dir() {
            assert!(p.ends_with(WEBVIEW_CACHE_NAME));
        }
    }
}
