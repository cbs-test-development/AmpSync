//! Removing everything AmpSync stored, for two callers:
//!
//! - the Windows uninstaller runs `ampsync-desktop.exe --purge` (see `windows/hooks.nsh`);
//! - Settings > "Remove all AmpSync data" starts a detached copy of the app with
//!   `--purge-and-restart` and quits, because the running window holds its WebView profile open
//!   and Windows cannot delete open files.
//!
//! Both modes are handled at the very start of `run()`, before any window or WebView exists and
//! before the single-instance plugin is installed, so nothing is held open and nothing is
//! forwarded to a running instance. They print nothing (the release build is a GUI-subsystem app)
//! and always exit with code 0; what happened goes to `%TEMP%\ampsync-purge.log`.

use std::ffi::OsString;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use ampsync_core::{Error, PurgeReport};

const FLAG_PURGE: &str = "--purge";
const FLAG_PURGE_RESTART: &str = "--purge-and-restart";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Uninstaller: purge and exit.
    Purge,
    /// Settings action: wait for the old process to end, purge, then start AmpSync again.
    PurgeAndRestart,
}

/// The purge mode requested on the command line, if any. Only the first argument counts, so a
/// file or URL argument that happens to look like a flag never triggers a purge.
pub fn mode_from_args<I: IntoIterator<Item = OsString>>(args: I) -> Option<Mode> {
    let first = args.into_iter().nth(1)?;
    if first == FLAG_PURGE {
        Some(Mode::Purge)
    } else if first == FLAG_PURGE_RESTART {
        Some(Mode::PurgeAndRestart)
    } else {
        None
    }
}

fn log_path() -> PathBuf {
    std::env::temp_dir().join("ampsync-purge.log")
}

/// Purge until nothing is reported as failing or the attempts run out (a just-closed WebView
/// process may hold its folder for a moment). Returns a readable log.
fn purge_with_retries<F>(attempts: u32, delay: Duration, mut purge: F) -> Vec<String>
where
    F: FnMut() -> Result<PurgeReport, Error>,
{
    let mut log = Vec::new();
    for attempt in 1..=attempts.max(1) {
        match purge() {
            Ok(report) => {
                log.push(format!(
                    "attempt {attempt}: removed {} folder(s), {} keychain entr(ies), {} problem(s)",
                    report.removed.len(),
                    report.keychain_entries,
                    report.errors.len()
                ));
                for e in &report.errors {
                    log.push(format!("  {e}"));
                }
                if report.errors.is_empty() {
                    return log;
                }
            }
            Err(e) => log.push(format!("attempt {attempt}: failed: {e}")),
        }
        if attempt < attempts {
            std::thread::sleep(delay);
        }
    }
    log
}

fn write_log(lines: &[String]) {
    let mut text = lines.join("\n");
    text.push('\n');
    let _ = std::fs::write(log_path(), text);
}

/// Run a purge mode to completion. Never panics outwards and never reports failure to the caller.
pub fn run(mode: Mode) {
    let result = std::panic::catch_unwind(|| match mode {
        Mode::Purge => {
            let log = purge_with_retries(5, Duration::from_millis(1000), ampsync_core::purge_all);
            write_log(&log);
        }
        Mode::PurgeAndRestart => {
            // The parent keeps the other end of our stdin open and ends it by exiting.
            let mut sink = [0u8; 64];
            let mut stdin = std::io::stdin();
            while matches!(stdin.read(&mut sink), Ok(n) if n > 0) {}
            let mut log = purge_with_retries(20, Duration::from_millis(500), ampsync_core::purge_all);
            // Let the old process finish exiting so the single-instance check does not see it.
            std::thread::sleep(Duration::from_millis(1500));
            match std::env::current_exe().and_then(|exe| std::process::Command::new(exe).spawn()) {
                Ok(_) => log.push("restarted AmpSync".to_string()),
                Err(e) => log.push(format!("could not restart AmpSync: {e}")),
            }
            write_log(&log);
        }
    });
    if result.is_err() {
        write_log(&["purge stopped unexpectedly".to_string()]);
    }
}

/// Start the detached helper that purges after this process has exited. The returned handle must
/// stay alive until the process exits: it is the pipe the helper waits on.
pub fn spawn_restart_helper() -> std::io::Result<std::process::Child> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg(FLAG_PURGE_RESTART)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
    }
    cmd.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn recognises_only_the_first_argument() {
        assert_eq!(mode_from_args(args(&["x", "--purge"])), Some(Mode::Purge));
        assert_eq!(
            mode_from_args(args(&["x", "--purge-and-restart"])),
            Some(Mode::PurgeAndRestart)
        );
        assert_eq!(mode_from_args(args(&["x"])), None);
        assert_eq!(mode_from_args(args(&["x", "--purged"])), None);
        assert_eq!(mode_from_args(args(&["x", "file.txt", "--purge"])), None);
    }

    #[test]
    fn stops_after_a_clean_pass() {
        let mut calls = 0;
        let log = purge_with_retries(5, Duration::ZERO, || {
            calls += 1;
            Ok(PurgeReport::default())
        });
        assert_eq!(calls, 1);
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn retries_problems_then_gives_up_and_logs_them() {
        let mut calls = 0;
        let log = purge_with_retries(3, Duration::ZERO, || {
            calls += 1;
            Ok(PurgeReport {
                errors: vec!["locked".into()],
                ..PurgeReport::default()
            })
        });
        assert_eq!(calls, 3);
        assert!(log.iter().any(|l| l.contains("locked")));
    }

    #[test]
    fn a_later_clean_pass_ends_the_loop() {
        let mut calls = 0;
        purge_with_retries(5, Duration::ZERO, || {
            calls += 1;
            Ok(PurgeReport {
                errors: if calls < 2 { vec!["busy".into()] } else { vec![] },
                ..PurgeReport::default()
            })
        });
        assert_eq!(calls, 2);
    }
}
