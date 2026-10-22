//! Where the CLI keeps its own files.

use std::path::PathBuf;
use std::time::Duration;

use directories::ProjectDirs;

/// AmpSync-owned sign-in browser profile (D15): `<per-user local data dir>/AmpSync/signin-profile`
/// (Windows: `%LOCALAPPDATA%\AmpSync\data\signin-profile`). Never a path inside any
/// browser's own profile (D16).
pub fn webview_profile_dir() -> Option<PathBuf> {
    ProjectDirs::from("", "", "AmpSync").map(|d| d.data_local_dir().join("signin-profile"))
}

/// Delete the sign-in profile. Browser helper processes can keep files locked for a moment after
/// the window closes, so retry briefly. Returns whether anything was removed.
pub fn delete_webview_profile() -> std::io::Result<bool> {
    let Some(dir) = webview_profile_dir() else {
        return Ok(false);
    };
    if !dir.exists() {
        return Ok(false);
    }
    let mut last = None;
    for _ in 0..10 {
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => return Ok(true),
            Err(_) if !dir.exists() => return Ok(true),
            Err(e) => last = Some(e),
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("could not delete the sign-in profile")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_dir_is_ampsync_owned() {
        if let Some(p) = webview_profile_dir() {
            assert!(p.ends_with("signin-profile"));
            assert!(p.to_string_lossy().contains("AmpSync"));
        }
    }
}
