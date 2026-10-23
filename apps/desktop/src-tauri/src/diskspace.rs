//! Free space and a short volume name for a destination folder, for the New download picker and
//! the disk-full banner (D21). Local calls only: nothing here talks to the network.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{CmdError, CmdResult};

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct DiskSpace {
    /// Bytes available to the current user on the volume that holds the destination.
    pub free: u64,
    /// How to name that volume to a person: `D:` on Windows, the mount point (`/home`) elsewhere.
    pub volume: String,
}

/// `D:` for `D:\Downloads\x` and `\\?\D:\x`; the share (`\\server\share`) for a UNC path.
fn windows_volume(path: &str) -> Option<String> {
    let p = path.strip_prefix(r"\\?\").unwrap_or(path);
    let mut chars = p.chars();
    if let (Some(letter), Some(':')) = (chars.next(), chars.next())
        && letter.is_ascii_alphabetic()
    {
        return Some(format!("{}:", letter.to_ascii_uppercase()));
    }
    let unc = p.strip_prefix(r"UNC\").or_else(|| p.strip_prefix(r"\\"))?;
    let mut parts = unc.split('\\');
    let (server, share) = (parts.next()?, parts.next()?);
    (!server.is_empty() && !share.is_empty()).then(|| format!(r"\\{server}\{share}"))
}

/// The mount point that holds `path`, from the text of `/proc/self/mountinfo` (the longest mount
/// point that is a prefix of the path wins).
fn linux_mount(path: &str, mountinfo: &str) -> Option<String> {
    let mut best: Option<String> = None;
    for line in mountinfo.lines() {
        // "36 35 98:0 /mnt1 /mnt2 rw,noatime master:1 - ext3 /dev/root rw": field 5 is the mount point.
        let Some(raw) = line.split(' ').nth(4) else {
            continue;
        };
        let mount = unescape_mount(raw);
        let covers = path == mount
            || mount == "/"
            || path
                .strip_prefix(mount.as_str())
                .is_some_and(|r| r.starts_with('/'));
        if covers && best.as_ref().is_none_or(|b| mount.len() >= b.len()) {
            best = Some(mount);
        }
    }
    best
}

/// mountinfo writes space, tab, newline and backslash as `\040`, `\011`, `\012`, `\134`.
fn unescape_mount(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 4 <= bytes.len()
            && bytes[i + 1..i + 4].iter().all(|b| (b'0'..=b'7').contains(b))
            && let Ok(code) = u8::from_str_radix(&raw[i + 1..i + 4], 8)
        {
            out.push(code);
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The nearest existing ancestor, made absolute, with symlinks resolved when the platform allows.
fn existing_ancestor(dest: &Path) -> Option<PathBuf> {
    let abs = std::path::absolute(dest).ok()?;
    let found = abs.ancestors().find(|p| p.exists())?;
    Some(found.canonicalize().unwrap_or_else(|_| found.to_path_buf()))
}

fn volume_of(dest: &Path) -> String {
    let Some(base) = existing_ancestor(dest) else {
        return "this disk".into();
    };
    let text = base.to_string_lossy();
    if cfg!(windows) {
        windows_volume(&text)
    } else {
        std::fs::read_to_string("/proc/self/mountinfo")
            .ok()
            .and_then(|info| linux_mount(&text, &info))
    }
    .unwrap_or_else(|| "this disk".into())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn disk_space(dest: String) -> CmdResult<DiskSpace> {
    let text = dest.trim().to_string();
    if text.is_empty() {
        return Err(CmdError::new("error", "Choose a folder first."));
    }
    // A disconnected network drive can take seconds to answer; never block the UI thread on it.
    tauri::async_runtime::spawn_blocking(move || {
        let path = PathBuf::from(&text);
        let free = ampsync_core::free_space(&path)
            .map_err(|e| CmdError::new("error", format!("Could not read the free space there: {e}")))?;
        Ok(DiskSpace {
            free,
            volume: volume_of(&path),
        })
    })
    .await
    .map_err(|e| CmdError::new("error", e.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_drive_letters_are_upper_case_with_a_colon() {
        assert_eq!(windows_volume(r"D:\Downloads\x").as_deref(), Some("D:"));
        assert_eq!(windows_volume(r"c:\").as_deref(), Some("C:"));
        assert_eq!(windows_volume(r"\\?\E:\long\path").as_deref(), Some("E:"));
    }

    #[test]
    fn windows_network_shares_name_the_share() {
        assert_eq!(
            windows_volume(r"\\nas\media\films").as_deref(),
            Some(r"\\nas\media")
        );
        assert_eq!(
            windows_volume(r"\\?\UNC\nas\media\films").as_deref(),
            Some(r"\\nas\media")
        );
        assert_eq!(windows_volume(r"\\nas").as_deref(), None);
        assert_eq!(windows_volume("relative/path"), None);
    }

    const INFO: &str = "\
22 28 0:21 / /sys rw,nosuid shared:7 - sysfs sysfs rw
28 1 8:2 / / rw,relatime shared:1 - ext4 /dev/sda2 rw
40 28 8:3 / /home rw,relatime shared:25 - ext4 /dev/sda3 rw
41 40 8:4 / /home/big\\040disk rw,relatime shared:26 - ext4 /dev/sda4 rw
42 28 8:5 / /homework rw,relatime shared:27 - ext4 /dev/sda5 rw
";

    #[test]
    fn the_longest_mount_point_that_covers_the_path_wins() {
        assert_eq!(linux_mount("/home/me/Downloads", INFO).as_deref(), Some("/home"));
        assert_eq!(linux_mount("/home", INFO).as_deref(), Some("/home"));
        assert_eq!(linux_mount("/var/tmp", INFO).as_deref(), Some("/"));
        // `/homework` is its own volume, not a folder of `/home`.
        assert_eq!(linux_mount("/homework/a", INFO).as_deref(), Some("/homework"));
    }

    #[test]
    fn escaped_spaces_in_mount_points_are_restored() {
        assert_eq!(
            linux_mount("/home/big disk/films", INFO).as_deref(),
            Some("/home/big disk")
        );
        assert_eq!(unescape_mount(r"/mnt/a\040b\134c"), "/mnt/a b\\c");
    }

    #[test]
    fn an_empty_mountinfo_names_nothing() {
        assert_eq!(linux_mount("/x", ""), None);
    }

    #[test]
    fn a_folder_that_does_not_exist_yet_uses_its_nearest_parent() {
        let dir = std::env::temp_dir();
        let ghost = dir.join("ampsync-no-such-folder-1").join("deeper");
        let here = volume_of(&dir);
        assert_eq!(volume_of(&ghost), here);
        assert!(!here.is_empty());
    }
}
