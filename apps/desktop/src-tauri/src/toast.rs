//! Windows: notifications carry AmpSync's own name and icon in every kind of build.
//!
//! Windows labels a toast with whatever registered its AppUserModelID. The installer's Start menu
//! shortcut registers ours, but the portable exe has no shortcut (Windows silently drops toasts
//! from an unregistered ID), and the notification plugin uses PowerShell's ID for builds run from
//! `target\release`. So before the first toast AmpSync registers the ID for the current user under
//! `HKCU\Software\Classes\AppUserModelId` and shows toasts with it directly. Nothing is written
//! for someone who never turns notifications on, and every purge (uninstall, "Remove all AmpSync
//! data", `ampsync purge`) deletes the key again (`ampsync_core::purge_all`).

use std::path::Path;

use winreg::RegKey;
use winreg::enums::HKEY_CURRENT_USER;

/// Must match `identifier` in `tauri.conf.json` (checked by a test): the installer stamps the same
/// ID on its shortcuts.
use ampsync_core::purge::WINDOWS_APP_ID as APP_ID;

const ICON: &[u8] = include_bytes!("../icons/128x128.png");

fn key_path() -> String {
    format!(r"Software\Classes\AppUserModelId\{APP_ID}")
}

/// Register (or refresh) the ID with a display name and an icon kept in `data_dir`. Cheap enough
/// to run before every toast.
pub fn register(name: &str, data_dir: &Path) -> std::io::Result<()> {
    let icon = data_dir.join("notification-icon.png");
    if std::fs::read(&icon).ok().as_deref() != Some(ICON) {
        std::fs::create_dir_all(data_dir)?;
        std::fs::write(&icon, ICON)?;
    }
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(key_path())?;
    key.set_value("DisplayName", &name)?;
    key.set_value("IconUri", &icon.as_os_str())?;
    Ok(())
}

/// Show a toast under AmpSync's ID; `on_click` runs (on a system thread) when it is clicked.
pub fn show(title: &str, body: &str, on_click: impl Fn() + Send + 'static) -> Result<(), String> {
    tauri_winrt_notification::Toast::new(APP_ID)
        .title(title)
        .text1(body)
        .on_activated(move |_| {
            on_click();
            Ok(())
        })
        .show()
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_id_matches_the_bundle_identifier() {
        let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(conf["identifier"], APP_ID);
    }
}
