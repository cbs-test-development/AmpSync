//! Process exit codes (part of the CLI contract in docs/ARCHITECTURE.md) and the mapping from
//! core errors to codes and human-friendly messages.
//!
//! Ctrl+C: the first press cancels the job cleanly (state and partial files are kept, a rerun
//! resumes) and the process exits with [`ERROR`] (1) rather than the conventional 130; a second
//! press quits immediately.

use ampsync_core::Error;
use ampsync_core::engine::Summary;

/// Everything succeeded.
pub const OK: u8 = 0;
/// Any other error.
pub const ERROR: u8 = 1;
/// Bad command line or unusable input (clap also exits with 2 for usage errors).
pub const USAGE: u8 = 2;
/// The session expired mid-job; rerunning the same command resumes.
pub const AUTH_EXPIRED: u8 = 3;
/// The job finished but some files failed.
pub const SOME_FAILED: u8 = 4;

/// Exit code for a core error.
pub fn for_error(e: &Error) -> u8 {
    match e {
        Error::AuthExpired { .. } => AUTH_EXPIRED,
        Error::InvalidLink(_) => USAGE,
        _ => ERROR,
    }
}

/// Exit code for a job that ran to the end.
pub fn for_summary(s: &Summary) -> u8 {
    if s.failed > 0 { SOME_FAILED } else { OK }
}

/// A command failure: what to tell the user and which code to exit with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub message: String,
    pub code: u8,
}

impl Failure {
    pub fn new(message: impl Into<String>, code: u8) -> Self {
        Failure {
            message: message.into(),
            code,
        }
    }

    /// A generic error (exit code 1).
    pub fn error(message: impl Into<String>) -> Self {
        Self::new(message, ERROR)
    }

    /// Bad input (exit code 2).
    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(message, USAGE)
    }

    /// Translate a core error; `link` is what the user typed, used in "sign in again" advice.
    pub fn from_core(e: &Error, link: &str) -> Self {
        Failure::new(describe(e, link), for_error(e))
    }
}

/// A friendly, single-paragraph description of a core error. Never a `Debug` dump.
pub fn describe(e: &Error, link: &str) -> String {
    match e {
        Error::AuthExpired { host } => format!(
            "Sign-in expired for {host}. Run `ampsync login \"{link}\"`, then run the same command again to resume."
        ),
        Error::Throttled { retry_after } => format!(
            "SharePoint is asking us to slow down. Wait {} and run the command again.",
            crate::human::duration(*retry_after)
        ),
        Error::NotFound(what) => {
            format!("Couldn't find it on SharePoint: {what}. Check the link and your access.")
        }
        Error::Integrity { rel_path, .. } => {
            format!("{rel_path} did not download correctly. Run the command again to retry it.")
        }
        Error::RemoteChanged { rel_path } => {
            format!("{rel_path} changed on SharePoint while downloading. Run the command again.")
        }
        Error::InvalidLink(why) => format!("This link isn't a SharePoint folder link ({why})."),
        Error::Http { status, context } => {
            format!("SharePoint answered with an unexpected error (HTTP {status}) while {context}.")
        }
        Error::Network(why) => {
            format!("Network problem: {why}. Check your connection and run the command again.")
        }
        Error::Io { path, source } => format!("Can't use {}: {source}", path.display()),
        Error::State(why) => why.clone(),
        Error::DestinationBusy { path } => format!(
            "Another AmpSync download is already using this folder ({}). Wait for it to finish, or choose a different folder.",
            path.display()
        ),
        Error::Cancelled => "Cancelled. Run the same command again to resume.".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    use ampsync_core::plan::RelPath;

    fn summary(failed: u64) -> Summary {
        Summary {
            downloaded: 1,
            skipped: 0,
            failed,
            bytes: 1,
            elapsed: Duration::from_secs(1),
            failures: vec![],
        }
    }

    #[test]
    fn codes_match_the_contract() {
        assert_eq!((OK, ERROR, USAGE, AUTH_EXPIRED, SOME_FAILED), (0, 1, 2, 3, 4));
    }

    #[test]
    fn errors_map_to_codes() {
        assert_eq!(for_error(&Error::AuthExpired { host: "h".into() }), 3);
        assert_eq!(for_error(&Error::InvalidLink("x".into())), 2);
        assert_eq!(for_error(&Error::Cancelled), 1);
        assert_eq!(for_error(&Error::Network("x".into())), 1);
        assert_eq!(for_error(&Error::NotFound("x".into())), 1);
        assert_eq!(for_error(&Error::State("x".into())), 1);
        assert_eq!(
            for_error(&Error::DestinationBusy {
                path: PathBuf::from("x")
            }),
            1
        );
    }

    #[test]
    fn summaries_map_to_codes() {
        assert_eq!(for_summary(&summary(0)), OK);
        assert_eq!(for_summary(&summary(2)), SOME_FAILED);
    }

    #[test]
    fn messages_are_friendly() {
        let all = [
            Error::AuthExpired {
                host: "contoso.sharepoint.com".into(),
            },
            Error::Throttled {
                retry_after: Duration::from_secs(90),
            },
            Error::NotFound("folder".into()),
            Error::Integrity {
                rel_path: RelPath::new("a/b").unwrap(),
                expected: "1".into(),
                actual: "2".into(),
            },
            Error::RemoteChanged {
                rel_path: RelPath::new("a/b").unwrap(),
            },
            Error::InvalidLink("no host".into()),
            Error::Http {
                status: 500,
                context: "listing a folder".into(),
            },
            Error::Network("timed out".into()),
            Error::Io {
                path: PathBuf::from("x"),
                source: std::io::Error::other("denied"),
            },
            Error::State("bad".into()),
            Error::DestinationBusy {
                path: PathBuf::from("dest"),
            },
            Error::Cancelled,
        ];
        for e in &all {
            let m = describe(e, "https://l");
            assert!(!m.contains("Error::") && !m.contains('{') && !m.is_empty(), "{m}");
        }
        let m = describe(&all[0], "https://l");
        assert!(m.contains("Sign-in expired for contoso.sharepoint.com"));
        assert!(m.contains("same command again to resume"));
        let busy = describe(&all[10], "x");
        assert!(busy.contains("already using this folder"), "{busy}");
        assert!(describe(&all[5], "x").starts_with("This link isn't a SharePoint folder link"));
    }
}
