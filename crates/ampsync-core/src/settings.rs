//! User settings, stored as `settings.toml` in the per-user config directory (D4).
//!
//! One flat struct holds both the transfer-engine tuning (`Engine::new` takes it) and the
//! application preferences edited in the GUI. Unknown keys are ignored and missing keys fall back
//! to defaults, so older and newer versions can share a file.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::Error;

const MIB: u64 = 1024 * 1024;

/// What the window close button does while a transfer is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseBehavior {
    /// Ask every time.
    #[default]
    Ask,
    /// Keep transferring in the system tray.
    Tray,
    /// Pause the transfer and quit.
    Pause,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    // --- engine ---
    /// Maximum simultaneous requests / files. Default 4, valid 1..=16.
    pub parallel_files: u8,
    /// Size of one byte-range request. Default 8 MiB.
    pub range_size: u64,
    /// Files above this size are split into parallel ranges. Default 64 MiB.
    pub large_file_threshold: u64,
    /// Bytes per second shared across all lanes; `None` = unlimited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bandwidth_limit: Option<u64>,
    /// Retries per range before the file fails. Default 8.
    pub max_retries: u32,

    // --- application ---
    /// The first-run walkthrough has been shown or skipped (D13).
    pub tour_seen: bool,
    /// Let the sign-in browser profile remember passwords (D15). Default off.
    pub save_passwords: bool,
    /// Look for a newer release on launch. Default on. (Never installs anything, D10.)
    pub check_updates: bool,
    /// Windows only: use the signed-in work/school account for single sign-on (D15). Default on.
    pub use_windows_account: bool,
    /// Pre-filled destination folder in the "New download" view.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_dest: Option<PathBuf>,
    pub close_behavior: CloseBehavior,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            parallel_files: 4,
            range_size: 8 * MIB,
            large_file_threshold: 64 * MIB,
            bandwidth_limit: None,
            max_retries: 8,
            tour_seen: false,
            save_passwords: false,
            check_updates: true,
            use_windows_account: true,
            default_dest: None,
            close_behavior: CloseBehavior::Ask,
        }
    }
}

impl Settings {
    /// A copy with out-of-range values pulled back into range: `parallel_files` to 1..=16,
    /// `range_size` to at least 256 KiB, `bandwidth_limit` of 0 to `None`.
    pub fn sanitized(mut self) -> Self {
        self.parallel_files = self.parallel_files.clamp(1, 16);
        self.range_size = self.range_size.max(256 * 1024);
        if self.large_file_threshold < self.range_size {
            self.large_file_threshold = self.range_size;
        }
        if self.bandwidth_limit == Some(0) {
            self.bandwidth_limit = None;
        }
        self
    }

    /// `<per-user config dir>/AmpSync/settings.toml`, or `None` if the OS reports no home directory.
    /// (Windows: `%APPDATA%\AmpSync\config\settings.toml`; Linux: `~/.config/AmpSync/settings.toml`.)
    pub fn default_path() -> Option<PathBuf> {
        ProjectDirs::from("", "", "AmpSync").map(|d| d.config_dir().join("settings.toml"))
    }

    /// Load from [`default_path`](Self::default_path). A missing file yields the defaults.
    pub fn load() -> Result<Self, Error> {
        let path = Self::default_path().ok_or_else(|| Error::State("no per-user config directory".into()))?;
        Self::load_from(&path)
    }

    /// Save to [`default_path`](Self::default_path), creating the directory.
    pub fn save(&self) -> Result<(), Error> {
        let path = Self::default_path().ok_or_else(|| Error::State("no per-user config directory".into()))?;
        self.save_to(&path)
    }

    /// Load from `path`. A missing file yields the defaults; a malformed file is `Error::State`.
    /// The result is passed through [`sanitized`](Self::sanitized).
    pub fn load_from(path: &Path) -> Result<Self, Error> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str::<Settings>(&text)
                .map(Settings::sanitized)
                .map_err(|e| Error::State(format!("{} is not valid: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(Error::io(path, e)),
        }
    }

    /// Save to `path` atomically (write a sibling temp file, then rename), creating parent
    /// directories.
    pub fn save_to(&self, path: &Path) -> Result<(), Error> {
        let text =
            toml::to_string_pretty(self).map_err(|e| Error::State(format!("cannot encode settings: {e}")))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|e| Error::io(&tmp, e))?;
        std::fs::rename(&tmp, path).map_err(|e| Error::io(path, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_contract() {
        let s = Settings::default();
        assert_eq!(s.parallel_files, 4);
        assert_eq!(s.range_size, 8 * MIB);
        assert_eq!(s.large_file_threshold, 64 * MIB);
        assert_eq!(s.bandwidth_limit, None);
        assert_eq!(s.max_retries, 8);
        assert!(!s.tour_seen);
        assert!(!s.save_passwords);
        assert!(s.check_updates);
        assert!(s.use_windows_account);
        assert_eq!(s.default_dest, None);
        assert_eq!(s.close_behavior, CloseBehavior::Ask);
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("settings.toml");
        let s = Settings {
            parallel_files: 9,
            bandwidth_limit: Some(5_000_000),
            tour_seen: true,
            save_passwords: true,
            check_updates: false,
            use_windows_account: false,
            default_dest: Some(PathBuf::from("D:/Downloads")),
            close_behavior: CloseBehavior::Tray,
            ..Settings::default()
        };
        s.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path).unwrap(), s);
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            Settings::load_from(&dir.path().join("nope.toml")).unwrap(),
            Settings::default()
        );
    }

    #[test]
    fn partial_and_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.toml");
        std::fs::write(
            &path,
            "tour_seen = true\nclose_behavior = \"pause\"\nfuture_key = 1\n",
        )
        .unwrap();
        let s = Settings::load_from(&path).unwrap();
        assert!(s.tour_seen);
        assert_eq!(s.close_behavior, CloseBehavior::Pause);
        assert_eq!(s.parallel_files, 4);
    }

    #[test]
    fn out_of_range_values_are_clamped_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.toml");
        std::fs::write(
            &path,
            "parallel_files = 200\nbandwidth_limit = 0\nrange_size = 1\n",
        )
        .unwrap();
        let s = Settings::load_from(&path).unwrap();
        assert_eq!(s.parallel_files, 16);
        assert_eq!(s.bandwidth_limit, None);
        assert_eq!(s.range_size, 256 * 1024);
        assert!(s.large_file_threshold >= s.range_size);
        assert_eq!(
            Settings {
                parallel_files: 0,
                ..Settings::default()
            }
            .sanitized()
            .parallel_files,
            1
        );
    }

    #[test]
    fn malformed_file_is_a_state_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.toml");
        std::fs::write(&path, "parallel_files = \"many\"").unwrap();
        assert!(matches!(Settings::load_from(&path), Err(Error::State(_))));
    }

    #[test]
    fn default_path_is_named_settings_toml() {
        if let Some(p) = Settings::default_path() {
            assert_eq!(p.file_name().unwrap(), "settings.toml");
        }
    }
}
