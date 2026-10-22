//! The single error type shared by every module.

use std::path::PathBuf;
use std::time::Duration;

use crate::plan::RelPath;

/// Every failure AmpSync can report.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// The session is no longer accepted: HTTP 401/403, or a redirect to `login.microsoftonline.com`.
    /// The engine pauses the job and emits `Event::AuthExpired`; it never fails the job (D9).
    #[error("sign-in for {host} has expired")]
    AuthExpired { host: String },
    /// HTTP 429/503. `retry_after` comes from the `Retry-After` header (or a default when absent).
    #[error("server is throttling requests; retry after {retry_after:?}")]
    Throttled { retry_after: Duration },
    #[error("not found: {0}")]
    NotFound(String),
    /// Downloaded bytes did not match the expected hash or size.
    #[error("integrity check failed for {rel_path}: expected {expected}, got {actual}")]
    Integrity {
        rel_path: RelPath,
        expected: String,
        actual: String,
    },
    /// The remote eTag changed while the file was downloading.
    #[error("{rel_path} changed on the server during download")]
    RemoteChanged { rel_path: RelPath },
    /// The text the user pasted is not a usable SharePoint folder link.
    #[error("invalid link: {0}")]
    InvalidLink(String),
    /// Any other unexpected HTTP status. `context` says which request (never include cookies).
    #[error("HTTP {status}: {context}")]
    Http { status: u16, context: String },
    /// Connection reset, timeout, DNS failure, truncated body, ...
    #[error("network error: {0}")]
    Network(String),
    #[error("I/O error on {}: {source}", path.display())]
    Io { path: PathBuf, source: std::io::Error },
    /// SQLite / settings / keychain problems.
    #[error("state error: {0}")]
    State(String),
    /// Another AmpSync process (or job) holds the destination folder's lock.
    #[error("Another AmpSync download is already using this folder.")]
    DestinationBusy { path: PathBuf },
    #[error("cancelled")]
    Cancelled,
}

impl Error {
    /// Whether retrying the same request (after a backoff) can reasonably succeed.
    ///
    /// Retryable: `Throttled`, `Network`, and `Http` with status 408, 429 or 5xx.
    /// Not retryable: everything else. `Integrity` and `RemoteChanged` are handled at file level by
    /// the engine (delete the part file, retry the whole file once), not per range. `AuthExpired`
    /// pauses the job instead of retrying.
    pub fn is_retryable(&self) -> bool {
        match self {
            Error::Throttled { .. } | Error::Network(_) => true,
            Error::Http { status, .. } => matches!(*status, 408 | 429 | 500..=599),
            _ => false,
        }
    }

    /// Convenience constructor for [`Error::Io`].
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_classification() {
        assert!(
            Error::Throttled {
                retry_after: Duration::from_secs(1)
            }
            .is_retryable()
        );
        assert!(Error::Network("reset".into()).is_retryable());
        for s in [408, 429, 500, 502, 503, 504] {
            assert!(
                Error::Http {
                    status: s,
                    context: String::new()
                }
                .is_retryable(),
                "{s}"
            );
        }
        for s in [400, 404, 416] {
            assert!(
                !Error::Http {
                    status: s,
                    context: String::new()
                }
                .is_retryable(),
                "{s}"
            );
        }
        assert!(!Error::AuthExpired { host: "h".into() }.is_retryable());
        assert!(!Error::NotFound("x".into()).is_retryable());
        assert!(!Error::InvalidLink("x".into()).is_retryable());
        assert!(!Error::Cancelled.is_retryable());
        assert!(!Error::DestinationBusy { path: "d".into() }.is_retryable());
        assert!(!Error::State("x".into()).is_retryable());
        assert!(
            !Error::RemoteChanged {
                rel_path: RelPath::new("a").unwrap()
            }
            .is_retryable()
        );
        assert!(!Error::io("p", std::io::Error::other("x")).is_retryable());
    }

    #[test]
    fn destination_busy_is_friendly() {
        let e = Error::DestinationBusy {
            path: PathBuf::from("/some/where"),
        };
        assert_eq!(
            e.to_string(),
            "Another AmpSync download is already using this folder."
        );
    }

    #[test]
    fn display_includes_path() {
        let e = Error::Integrity {
            rel_path: RelPath::new("a/b").unwrap(),
            expected: "1".into(),
            actual: "2".into(),
        };
        assert!(e.to_string().contains("a/b"));
    }
}
