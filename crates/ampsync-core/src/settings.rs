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

/// How the startup splash plays (D25).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplashMode {
    /// Full animation.
    #[default]
    On,
    /// Static mark and name with a short fade; also what `On` becomes under the OS reduce-motion
    /// preference.
    Reduced,
    /// No splash; the window shows when ready.
    Off,
}

/// How the desktop signs in when a link needs it and the stored session does not work (D26).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SignInChoice {
    /// The sign-in window inside AmpSync.
    Window,
    /// Installed Edge, separate temporary profile.
    Edge,
    /// Installed Chrome, separate temporary profile.
    Chrome,
}

impl SignInChoice {
    /// From the name the frontend sends (`"webview"`, `"edge"`, `"chrome"`).
    pub fn from_method(name: &str) -> Option<Self> {
        match name {
            "webview" | "window" => Some(Self::Window),
            "edge" => Some(Self::Edge),
            "chrome" => Some(Self::Chrome),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    // --- engine ---
    /// Simultaneous requests / files when `auto_tune` is off. Valid 1..=16.
    pub parallel_files: u8,
    /// Let the engine choose the number of simultaneous requests (D22). Default on; when off,
    /// `parallel_files` is used as a fixed value.
    pub auto_tune: bool,
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
    /// Notify when a download finishes, fails or needs sign-in while the window is not focused
    /// (D23). `None` = the user has not been asked yet; it is left out of the file in that case.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notify_on_finish: Option<bool>,
    /// The remembered way to sign in when a link needs it (D26). `None` = ask every time; it is
    /// left out of the file in that case.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sign_in_with: Option<SignInChoice>,
    /// Startup animation (D25).
    pub splash: SplashMode,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            parallel_files: 6,
            auto_tune: true,
            range_size: 16 * MIB,
            large_file_threshold: 64 * MIB,
            bandwidth_limit: None,
            max_retries: 8,
            tour_seen: false,
            save_passwords: false,
            check_updates: true,
            use_windows_account: true,
            default_dest: None,
            close_behavior: CloseBehavior::Ask,
            notify_on_finish: None,
            sign_in_with: None,
            splash: SplashMode::On,
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
        assert_eq!(s.parallel_files, 6);
        assert_eq!(s.range_size, 16 * MIB);
        assert_eq!(s.large_file_threshold, 64 * MIB);
        assert_eq!(s.bandwidth_limit, None);
        assert_eq!(s.max_retries, 8);
        assert!(!s.tour_seen);
        assert!(!s.save_passwords);
        assert!(s.check_updates);
        assert!(s.use_windows_account);
        assert_eq!(s.default_dest, None);
        assert_eq!(s.close_behavior, CloseBehavior::Ask);
        assert!(s.auto_tune);
        assert_eq!(s.notify_on_finish, None);
        assert_eq!(s.splash, SplashMode::On);
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
    fn notify_on_finish_round_trips_all_three_states() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        for state in [None, Some(true), Some(false)] {
            let s = Settings {
                notify_on_finish: state,
                ..Settings::default()
            };
            s.save_to(&path).unwrap();
            let text = std::fs::read_to_string(&path).unwrap();
            assert_eq!(text.contains("notify_on_finish"), state.is_some(), "{text}");
            assert_eq!(Settings::load_from(&path).unwrap().notify_on_finish, state);
        }
    }

    #[test]
    fn sign_in_with_is_optional_and_lowercase() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        for (state, name) in [
            (None, ""),
            (Some(SignInChoice::Window), "window"),
            (Some(SignInChoice::Edge), "edge"),
            (Some(SignInChoice::Chrome), "chrome"),
        ] {
            let s = Settings {
                sign_in_with: state,
                ..Settings::default()
            };
            s.save_to(&path).unwrap();
            let text = std::fs::read_to_string(&path).unwrap();
            assert_eq!(text.contains("sign_in_with"), state.is_some(), "{text}");
            if state.is_some() {
                assert!(text.contains(&format!("sign_in_with = \"{name}\"")), "{text}");
            }
            assert_eq!(Settings::load_from(&path).unwrap().sign_in_with, state);
        }
        // A file from before v1.2 loads with "ask every time".
        std::fs::write(
            &path,
            "tour_seen = true
",
        )
        .unwrap();
        assert_eq!(Settings::load_from(&path).unwrap().sign_in_with, None);
        assert_eq!(serde_json::to_string(&SignInChoice::Edge).unwrap(), "\"edge\"");
    }

    #[test]
    fn sign_in_choice_from_method_names() {
        assert_eq!(SignInChoice::from_method("webview"), Some(SignInChoice::Window));
        assert_eq!(SignInChoice::from_method("edge"), Some(SignInChoice::Edge));
        assert_eq!(SignInChoice::from_method("chrome"), Some(SignInChoice::Chrome));
        assert_eq!(SignInChoice::from_method("paste"), None);
    }

    #[test]
    fn splash_and_auto_tune_round_trip_and_use_snake_case() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        for mode in [SplashMode::On, SplashMode::Reduced, SplashMode::Off] {
            let s = Settings {
                splash: mode,
                auto_tune: false,
                ..Settings::default()
            };
            s.save_to(&path).unwrap();
            assert_eq!(Settings::load_from(&path).unwrap(), s);
        }
        std::fs::write(
            &path,
            "splash = \"reduced\"
auto_tune = false
notify_on_finish = false
",
        )
        .unwrap();
        let s = Settings::load_from(&path).unwrap();
        assert_eq!(s.splash, SplashMode::Reduced);
        assert!(!s.auto_tune);
        assert_eq!(s.notify_on_finish, Some(false));
        assert_eq!(
            serde_json::to_string(&SplashMode::Reduced).unwrap(),
            "\"reduced\""
        );
        // A file from before v1.1 gets the new defaults.
        std::fs::write(
            &path,
            "tour_seen = true
",
        )
        .unwrap();
        let s = Settings::load_from(&path).unwrap();
        assert!(s.auto_tune && s.notify_on_finish.is_none() && s.splash == SplashMode::On);
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
        assert_eq!(s.parallel_files, 6);
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
