//! Disk space: the check before a download starts (D21) and the wait when the disk fills up
//! during one.
//!
//! The decisions are pure functions so they can be tested without a disk or a SharePoint.

use std::io;
use std::path::Path;
use std::time::Duration;

use crate::exit::{self, Failure};
use crate::human;

/// Space to keep free on top of what the download needs (1 GiB), so the volume is not left at
/// zero and other programs keep working.
pub const MARGIN: u64 = 1 << 30;

/// While waiting on a full disk: free space is looked at this often.
pub const RECHECK_EVERY: Duration = Duration::from_secs(15);

/// While waiting on a full disk: this much more free space than at the moment the disk filled up
/// is enough to try again, even if the whole download would still not fit.
pub const PROGRESS_STEP: u64 = 1 << 30;

/// Whether `needed` bytes (plus the margin) fit into `free`; otherwise how many more bytes have
/// to be freed.
pub fn shortfall(needed: u64, free: u64) -> Option<u64> {
    let want = needed.saturating_add(MARGIN);
    want.checked_sub(free).filter(|s| *s > 0)
}

/// The refusal printed when the download does not fit:
/// `Not enough space on D:. Needs 31.2 GiB, 24.9 GiB free. Free up 7.3 GiB or choose another folder.`
///
/// "Free up" includes the margin, so freeing exactly that much passes the check.
pub fn refusal_message(volume: &str, needed: u64, free: u64, missing: u64) -> String {
    format!(
        "Not enough space on {volume}. Needs {}, {} free. Free up {} or choose another folder.",
        human::bytes(needed),
        human::bytes(free),
        human::bytes(missing)
    )
}

/// The "Disk space:" line of the summary shown before the download starts.
pub fn summary_line(volume: &str, needed: u64, free: Option<u64>, ignored: bool) -> String {
    let mut line = match free {
        Some(free) => format!(
            "Disk space:   needs {}, {} free on {volume}",
            human::bytes(needed),
            human::bytes(free)
        ),
        None => format!(
            "Disk space:   needs {} (free space unknown)",
            human::bytes(needed)
        ),
    };
    if ignored {
        line.push_str(" (check skipped)");
    }
    line
}

/// The check before the download starts. `free` is the result of asking the OS; when it cannot
/// say, the download is allowed (a failed lookup must not block a working disk) and the real
/// "disk full" handling still applies. `needed == 0` always passes.
pub fn check_before_start(
    volume: &str,
    needed: u64,
    free: &io::Result<u64>,
    ignore: bool,
) -> Result<(), Failure> {
    if ignore || needed == 0 {
        return Ok(());
    }
    let Ok(free) = free else {
        return Ok(());
    };
    match shortfall(needed, *free) {
        None => Ok(()),
        Some(missing) => Err(Failure::new(
            refusal_message(volume, needed, *free, missing),
            exit::ERROR,
        )),
    }
}

/// The line shown when the disk fills up while downloading.
pub fn disk_full_line(volume: &str, needed: u64, free: u64, can_press_enter: bool) -> String {
    let retry = if can_press_enter {
        format!(
            "checking every {}s, or press Enter to try now",
            RECHECK_EVERY.as_secs()
        )
    } else {
        format!("checking every {}s", RECHECK_EVERY.as_secs())
    };
    format!(
        "Disk full on {volume} ({} free, {} still to download). Paused. Free up space and it \
         continues by itself ({retry}); Ctrl+C stops and keeps what is downloaded.",
        human::bytes(free),
        human::bytes(needed)
    )
}

/// What to do about a disk that filled up: remembers the numbers from the `disk_full` event and
/// decides, from the free space seen later, when trying again makes sense.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskWait {
    needed: u64,
    free_when_full: u64,
}

impl DiskWait {
    pub fn new(needed: u64, free_when_full: u64) -> Self {
        DiskWait {
            needed,
            free_when_full,
        }
    }

    /// True when a retry is worth it:
    /// - everything that is left fits now (needed plus the margin) and free space has grown
    ///   since the disk filled up, or
    /// - at least [`PROGRESS_STEP`] more is free than when it filled up: enough to make real
    ///   progress, after which the job simply pauses again if the disk fills a second time.
    ///
    /// Free space that has not grown at all never triggers a retry on its own (some volumes
    /// report free space but still refuse writes, for example under a quota, and retrying every
    /// few seconds would achieve nothing); pressing Enter forces a retry.
    pub fn ready(&self, free_now: u64) -> bool {
        let grown = free_now.saturating_sub(self.free_when_full);
        let all_fits = free_now >= self.needed.saturating_add(MARGIN);
        (all_fits && grown > 0) || grown >= PROGRESS_STEP
    }
}

/// A short name for the disk that holds `dest`: the drive letter on Windows (`D:`), the mount
/// point on Linux (`/home`). Falls back to the folder itself when it cannot be worked out.
pub fn volume_label(dest: &Path) -> String {
    let abs = std::path::absolute(dest).unwrap_or_else(|_| dest.to_path_buf());
    platform_label(&abs).unwrap_or_else(|| format!("the disk holding {}", dest.display()))
}

#[cfg(windows)]
fn platform_label(abs: &Path) -> Option<String> {
    windows_volume(&abs.to_string_lossy())
}

#[cfg(unix)]
fn platform_label(abs: &Path) -> Option<String> {
    // Symlinks decide which disk a folder really lives on; the folder may not exist yet.
    let existing = abs.ancestors().find(|p| p.exists())?;
    let real = existing.canonicalize().ok()?;
    let mounts = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    mount_point(&real.to_string_lossy(), &mounts)
}

#[cfg(not(any(windows, unix)))]
fn platform_label(_abs: &Path) -> Option<String> {
    None
}

/// `C:\x`, `\\?\C:\x` -> `C:`; `\\server\share\x`, `\\?\UNC\server\share\x` -> `\\server\share`.
#[cfg(any(windows, test))]
fn windows_volume(path: &str) -> Option<String> {
    let p = path.strip_prefix(r"\\?\").unwrap_or(path);
    let unc = path
        .strip_prefix(r"\\?\UNC\")
        .or_else(|| path.strip_prefix(r"\\").filter(|rest| !rest.starts_with('?')));
    if let Some(rest) = unc {
        let mut parts = rest.split(['\\', '/']).filter(|s| !s.is_empty());
        let (server, share) = (parts.next()?, parts.next()?);
        return Some(format!(r"\\{server}\{share}"));
    }
    let mut chars = p.chars();
    let letter = chars.next().filter(char::is_ascii_alphabetic)?;
    (chars.next() == Some(':')).then(|| format!("{}:", letter.to_ascii_uppercase()))
}

/// The mount point that holds `path` (an absolute, symlink-free path), from the text of
/// `/proc/self/mountinfo`: the longest mount point that is a parent of `path`.
#[cfg(any(unix, test))]
fn mount_point(path: &str, mountinfo: &str) -> Option<String> {
    mountinfo
        .lines()
        .filter_map(|line| line.split(' ').nth(4).map(unescape_mount))
        .filter(|mp| {
            path == mp
                || mp == "/"
                || path
                    .strip_prefix(mp.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        })
        .max_by_key(String::len)
}

/// mountinfo writes space, tab, newline and backslash as `\040`, `\011`, `\012`, `\134`.
#[cfg(any(unix, test))]
fn unescape_mount(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let octal = (bytes[i] == b'\\' && i + 3 < bytes.len())
            .then(|| std::str::from_utf8(&bytes[i + 1..i + 4]).ok())
            .flatten()
            .and_then(|s| u8::from_str_radix(s, 8).ok());
        match octal {
            Some(b) => {
                out.push(b);
                i += 4;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    #[test]
    fn fits_with_the_margin() {
        assert_eq!(shortfall(10 * GIB, 11 * GIB), None);
        assert_eq!(shortfall(10 * GIB, 12 * GIB), None);
        assert_eq!(shortfall(10 * GIB, 11 * GIB - 1), Some(1));
        // Needs 31 GiB, has 24: 8 GiB to free including the 1 GiB margin.
        assert_eq!(shortfall(31 * GIB, 24 * GIB), Some(8 * GIB));
        assert_eq!(shortfall(u64::MAX, 0), Some(u64::MAX));
    }

    #[test]
    fn refusal_names_the_volume_and_what_to_free() {
        let missing = shortfall(31 * GIB, 24 * GIB).unwrap();
        assert_eq!(
            refusal_message("D:", 31 * GIB, 24 * GIB, missing),
            "Not enough space on D:. Needs 31.0 GiB, 24.0 GiB free. Free up 8.0 GiB or choose another folder."
        );
    }

    #[test]
    fn insufficient_space_is_exit_code_one() {
        let err = check_before_start("/data", 31 * GIB, &Ok(24 * GIB), false).unwrap_err();
        assert_eq!(err.code, exit::ERROR);
        assert_eq!(err.code, 1);
        assert!(
            err.message.starts_with("Not enough space on /data."),
            "{}",
            err.message
        );
        assert!(err.message.contains("choose another folder"));
    }

    #[test]
    fn enough_space_ignore_and_unknown_pass() {
        assert!(check_before_start("D:", 5 * GIB, &Ok(50 * GIB), false).is_ok());
        // `--ignore-space` overrides.
        assert!(check_before_start("D:", 31 * GIB, &Ok(GIB), true).is_ok());
        // Nothing to download never needs space.
        assert!(check_before_start("D:", 0, &Ok(0), false).is_ok());
        // The OS could not say: carry on, disk-full handling still protects the run.
        let unknown = Err(io::Error::other("no such volume"));
        assert!(check_before_start("D:", 31 * GIB, &unknown, false).is_ok());
    }

    #[test]
    fn summary_line_shapes() {
        assert_eq!(
            summary_line("D:", 31 * GIB, Some(112 * GIB), false),
            "Disk space:   needs 31.0 GiB, 112.0 GiB free on D:"
        );
        assert!(summary_line("D:", GIB, Some(GIB), true).ends_with("(check skipped)"));
        assert!(summary_line("D:", GIB, None, false).contains("unknown"));
    }

    #[test]
    fn disk_full_message_is_one_line() {
        let with = disk_full_line("D:", 20 * GIB, 12 * 1024 * 1024, true);
        assert!(!with.contains('\n'));
        assert!(
            with.starts_with("Disk full on D: (12.0 MiB free, 20.0 GiB still to download)."),
            "{with}"
        );
        assert!(
            with.contains("every 15s") && with.contains("press Enter"),
            "{with}"
        );
        assert!(with.contains("Ctrl+C"), "{with}");
        let without = disk_full_line("/home", GIB, 0, false);
        assert!(!without.contains("Enter"), "{without}");
    }

    #[test]
    fn waiting_resumes_when_everything_fits() {
        // Not much was freed (less than a step), but what is left to download fits now.
        let w = DiskWait::new(2 * GIB, 2 * GIB + 512 * 1024 * 1024);
        assert!(!w.ready(2 * GIB + 512 * 1024 * 1024));
        // 2 GiB + 1 GiB margin needed.
        assert!(!w.ready(3 * GIB - 1));
        assert!(w.ready(3 * GIB));
        // Far from fitting and little freed: keep waiting.
        let w = DiskWait::new(5 * GIB, 10 * 1024 * 1024);
        assert!(!w.ready(10 * 1024 * 1024));
        assert!(!w.ready(512 * 1024 * 1024));
    }

    #[test]
    fn waiting_resumes_on_enough_progress_room() {
        let w = DiskWait::new(100 * GIB, 10 * 1024 * 1024);
        assert!(!w.ready(GIB));
        assert!(w.ready(GIB + 10 * 1024 * 1024));
        assert!(w.ready(8 * GIB));
    }

    #[test]
    fn free_space_that_never_grew_does_not_retry_by_itself() {
        // The volume claims room (quota, odd filesystem) but refused the write.
        let w = DiskWait::new(GIB, 40 * GIB);
        assert!(!w.ready(40 * GIB));
        assert!(!w.ready(30 * GIB));
        assert!(w.ready(40 * GIB + 1));
    }

    #[test]
    fn windows_volumes() {
        assert_eq!(windows_volume(r"D:\data\out").as_deref(), Some("D:"));
        assert_eq!(windows_volume(r"d:/data").as_deref(), Some("D:"));
        assert_eq!(windows_volume(r"\\?\C:\x\y").as_deref(), Some("C:"));
        assert_eq!(
            windows_volume(r"\\nas\share\folder").as_deref(),
            Some(r"\\nas\share")
        );
        assert_eq!(
            windows_volume(r"\\?\UNC\nas\share\folder").as_deref(),
            Some(r"\\nas\share")
        );
        assert_eq!(windows_volume("relative/path"), None);
        assert_eq!(windows_volume(r"\\nas"), None);
    }

    #[test]
    fn mount_points_come_from_mountinfo() {
        let info = "\
22 28 0:21 / /proc rw,nosuid - proc proc rw
26 28 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw
40 26 8:17 / /data rw,relatime shared:2 - ext4 /dev/sdb1 rw
41 40 8:33 / /data/big\\040disk rw,relatime shared:3 - ext4 /dev/sdc1 rw
42 26 8:49 / /database rw,relatime shared:4 - ext4 /dev/sdd1 rw
";
        assert_eq!(mount_point("/home/me/out", info).as_deref(), Some("/"));
        assert_eq!(mount_point("/data/out", info).as_deref(), Some("/data"));
        assert_eq!(mount_point("/data", info).as_deref(), Some("/data"));
        // `/database` is not inside `/data`.
        assert_eq!(mount_point("/database/x", info).as_deref(), Some("/database"));
        assert_eq!(
            mount_point("/data/big disk/x", info).as_deref(),
            Some("/data/big disk")
        );
        assert_eq!(mount_point("/x", ""), None);
    }

    #[test]
    fn volume_label_always_gives_something() {
        let label = volume_label(Path::new("some-new-folder"));
        assert!(!label.is_empty());
        #[cfg(windows)]
        assert!(
            label.len() == 2 && label.ends_with(':') || label.starts_with(r"\\"),
            "{label}"
        );
        #[cfg(unix)]
        assert!(label.starts_with('/'), "{label}");
    }
}
