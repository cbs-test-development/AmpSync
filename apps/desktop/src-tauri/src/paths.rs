//! Where AmpSync keeps its own files (never inside a browser profile, D16).

use std::path::PathBuf;

use directories::ProjectDirs;

fn dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("", "", "AmpSync")
}

/// Per-user local data directory (Windows: `%LOCALAPPDATA%\AmpSync\data`).
pub fn data_dir() -> PathBuf {
    dirs()
        .map(|d| d.data_local_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("AmpSync"))
}

/// AmpSync-only browser profile used by the sign-in window (D15).
pub fn webview_profile() -> PathBuf {
    data_dir().join("signin-profile")
}

pub fn queue_file() -> PathBuf {
    data_dir().join("queue.json")
}

pub fn update_file() -> PathBuf {
    data_dir().join("update-check.json")
}
