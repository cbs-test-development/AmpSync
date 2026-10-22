//! The transfer engine: plans a job, then downloads it with parallel ranges, verification and
//! resume.
//!
//! Engine rules (from the architecture contract):
//! - Files <= `large_file_threshold` are one request each; larger files are split into
//!   `range_size` ranges spread across lanes. Total in-flight requests never exceed
//!   `parallel_files`.
//! - Each range is written with positional writes into the preallocated `<name>.ampsync-part`;
//!   completed ranges are recorded in `state.db` so a restart only fetches missing ranges.
//! - After the last byte: verify the hash if the remote supplied one (`VerifiedBy::Hash`), else
//!   size + `Remote::stat` eTag unchanged (`VerifiedBy::SizeAndEtag`). Then atomically rename over
//!   the destination. On mismatch: delete the part file and retry the file once, then
//!   `Event::FileFailed`.
//! - Throttling: `Retry-After` on 429/503 pauses all lanes (`Event::Throttled`); otherwise
//!   exponential backoff with jitter, up to `max_retries` per range.
//! - `Error::AuthExpired` pauses the job (never fails it): emit `Event::AuthExpired` then
//!   `Event::Paused`, and wait for `JobHandle::replace_session` / `resume` / `cancel`.
//! - Windows: use `\\?\` long paths ([`crate::names::long_path`]); replace illegal characters with
//!   [`crate::names::sanitize`] and record the mapping in `state.db`.
//! - Nothing local is deleted unless `JobSpec::prune`.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::Error;
use crate::auth::Session;
use crate::plan::{JobSpec, Plan, RelPath};
use crate::remote::Remote;

/// Engine tuning plus application preferences; see [`crate::settings::Settings`].
pub use crate::settings::Settings;

/// How a finished file was verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifiedBy {
    /// Hash reported by the server matched.
    Hash,
    /// Size matched and `stat()` showed the eTag unchanged (D18).
    SizeAndEtag,
}

/// A file that did not complete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileFailure {
    pub rel_path: RelPath,
    pub error: String,
}

/// Result of a finished job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Summary {
    /// Files downloaded and verified in this run.
    pub downloaded: u64,
    /// Files skipped as unchanged.
    pub skipped: u64,
    /// Files that failed after retries. The CLI exits 4 when this is non-zero.
    pub failed: u64,
    /// Bytes transferred in this run.
    pub bytes: u64,
    #[serde(rename = "elapsed_ms", serialize_with = "as_millis")]
    pub elapsed: Duration,
    pub failures: Vec<FileFailure>,
}

fn as_millis<S: serde::Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_u64(u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Progress notifications. Serialised as one JSON object per event with a snake_case `type` tag,
/// e.g. `{"type":"file_started","rel_path":"a/b.txt","size":10,"lane":0}`; durations are
/// serialised as milliseconds under `*_ms` names. Used verbatim by `ampsync get --json` and by
/// the desktop frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Planned {
        files: u64,
        bytes: u64,
        skipped: u64,
    },
    FileStarted {
        rel_path: RelPath,
        size: u64,
        lane: u8,
    },
    /// Overall progress, throttled to about 4 Hz.
    Progress {
        bytes_done: u64,
        bytes_total: u64,
        rate_bps: u64,
    },
    LaneProgress {
        lane: u8,
        rel_path: RelPath,
        offset: u64,
        size: u64,
    },
    FileVerified {
        rel_path: RelPath,
        by: VerifiedBy,
    },
    FileSkipped {
        rel_path: RelPath,
    },
    FileFailed {
        rel_path: RelPath,
        error: String,
        will_retry: bool,
    },
    Throttled {
        #[serde(rename = "retry_after_ms", serialize_with = "as_millis")]
        retry_after: Duration,
    },
    /// The job is now paused until a new session arrives (D9).
    AuthExpired {
        host: String,
    },
    Paused,
    Resumed,
    Finished(Summary),
}

/// Plans and runs jobs against a [`Remote`].
#[allow(dead_code)] // stub: fields are used once the implementation lands
pub struct Engine<R: Remote> {
    remote: Arc<R>,
    settings: Settings,
}

impl<R: Remote + 'static> Engine<R> {
    /// `settings` is passed through [`Settings::sanitized`].
    pub fn new(remote: Arc<R>, settings: Settings) -> Self {
        Engine {
            remote,
            settings: settings.sanitized(),
        }
    }

    /// List the remote tree (`Remote::resolve` + `Remote::walk`), apply `include`/`exclude`
    /// globs, compare with the destination and `state.db`, and decide an action per file.
    /// Downloads nothing and writes nothing outside `<dest>/.ampsync/`.
    #[allow(unused_variables)]
    pub async fn plan(&self, spec: &JobSpec) -> Result<Plan, Error> {
        todo!()
    }

    /// Start transferring `plan` in the background. Must be called inside a tokio runtime.
    #[allow(unused_variables)]
    pub fn start(&self, spec: JobSpec, plan: Plan) -> JobHandle {
        todo!()
    }
}

/// Control and observation handle for a running job.
#[allow(dead_code)] // stub: fields are used once the implementation lands
pub struct JobHandle {
    events: broadcast::Sender<Event>,
    task: JoinHandle<Result<Summary, Error>>,
}

impl JobHandle {
    /// Subscribe to events from now on. Slow receivers may lag (`RecvError::Lagged`); `Progress`
    /// is the only high-rate event.
    pub fn events(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// Stop starting new requests and let in-flight ranges finish. Emits `Event::Paused`.
    pub fn pause(&self) {
        todo!()
    }

    /// Continue after `pause` or after `AuthExpired`. Emits `Event::Resumed`.
    pub fn resume(&self) {
        todo!()
    }

    /// Swap the cookies (re-sign-in), via `Remote::replace_session`, then resume.
    #[allow(unused_variables)]
    pub fn replace_session(&self, s: Session) {
        todo!()
    }

    /// Abort as soon as possible. `.ampsync-part` files and `state.db` are kept so a rerun
    /// resumes. `wait` then returns `Err(Error::Cancelled)`.
    pub fn cancel(&self) {
        todo!()
    }

    /// Wait for the job to finish. Emits `Event::Finished` before returning `Ok(summary)`.
    pub async fn wait(self) -> Result<Summary, Error> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_serialise_tagged() {
        let e = Event::FileStarted {
            rel_path: RelPath::new("a/b.txt").unwrap(),
            size: 10,
            lane: 2,
        };
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"type":"file_started","rel_path":"a/b.txt","size":10,"lane":2}"#
        );
        assert_eq!(
            serde_json::to_string(&Event::Paused).unwrap(),
            r#"{"type":"paused"}"#
        );
        let t = Event::Throttled {
            retry_after: Duration::from_millis(1500),
        };
        assert_eq!(
            serde_json::to_string(&t).unwrap(),
            r#"{"type":"throttled","retry_after_ms":1500}"#
        );
        let v = Event::FileVerified {
            rel_path: RelPath::new("x").unwrap(),
            by: VerifiedBy::SizeAndEtag,
        };
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            r#"{"type":"file_verified","rel_path":"x","by":"size_and_etag"}"#
        );
    }

    #[test]
    fn finished_event_embeds_summary() {
        let s = Summary {
            downloaded: 1,
            skipped: 2,
            failed: 0,
            bytes: 3,
            elapsed: Duration::from_secs(4),
            failures: vec![],
        };
        let json = serde_json::to_string(&Event::Finished(s)).unwrap();
        assert!(json.starts_with(r#"{"type":"finished","downloaded":1"#), "{json}");
        assert!(json.contains(r#""elapsed_ms":4000"#));
    }
}
