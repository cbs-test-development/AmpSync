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
//! - `Error::AuthExpired` on a file first asks `Remote::probe` whether the session itself is still
//!   good. If it is, the server refused just that file: the file fails (`FileFailed`, not
//!   retried) and the job carries on. Otherwise the job pauses (never fails): emit
//!   `Event::AuthExpired` then `Event::Paused`, and wait for `JobHandle::replace_session` /
//!   `resume` / `cancel`. A file refused again right after a new session arrived fails instead
//!   of pausing a second time.
//! - Only one AmpSync process may use a destination: `<data dir>/jobs/<key>/lock` is held from
//!   `plan` to the end of the job (`Error::DestinationBusy` otherwise). Job state lives in the same
//!   per-user directory, never inside the destination ([`crate::appdirs`]).
//! - When a job ends without being cancelled, no `.ampsync-part` file is left in the folders it
//!   covers, except the partial file of a failed download that has recorded ranges (the next
//!   successful run removes it).
//! - Before the rename, the bytes recorded as downloaded must add up to the file size; if they do
//!   not, the part file is reset and the file retried once.
//! - Windows: use `\\?\` long paths ([`crate::names::long_path`]); replace illegal characters with
//!   [`crate::names::sanitize`] and record the mapping in `state.db`.
//! - Nothing local is deleted unless `JobSpec::prune`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, broadcast, watch};
use tokio::task::JoinHandle;

use crate::Error;
use crate::auth::Session;
use crate::auth::SessionStatus;
use crate::autotune::{AutoTuneConfig, Change, Controller, LanesReason, RequestLog};
use crate::hash::QuickXorHash;
use crate::names;
use crate::plan::{self, Filters, JobSpec, Plan, PlannedAction, RelPath};
use crate::remote::{Remote, RemoteFile, RemoteHash, RetryPolicy};
use crate::state::{DestLock, FileRecord, FileStatus, StateDb};

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
    /// No state was recorded for the file; size and modification time equal the remote's.
    SizeAndMtime,
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

/// Progress notifications. Every job emits exactly one terminal event (`Finished`, `Failed` or
/// `Cancelled`) before `JobHandle::wait` resolves; treat the first one as the end of the job.
/// Serialised as one JSON object per event with a snake_case `type` tag,
/// e.g. `{"type":"file_started","rel_path":"a/b.txt","size":10,"lane":0}`; durations are
/// serialised as milliseconds under `*_ms` names. Used verbatim by `ampsync get --json` and by
/// the desktop frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// The job starts. `skipped` / `skipped_bytes` count the files left alone because they are
    /// already present and unchanged; there is deliberately no per-file event for those, so a
    /// 100k-file tree cannot flood the event channel.
    Planned {
        files: u64,
        bytes: u64,
        skipped: u64,
        skipped_bytes: u64,
    },
    /// A file's first request is about to go out. `done_ranges` lists the ranges (by index, see
    /// `RangeDone`) that are already on disk when a download resumes; empty otherwise and for
    /// files that are not split into ranges. A second `FileStarted` for the same path (the whole
    /// file is fetched again after a verification mismatch) starts its range map over.
    FileStarted {
        rel_path: RelPath,
        size: u64,
        lane: u8,
        done_ranges: Vec<u32>,
    },
    /// One byte range of a file split into ranges is on disk (D24). Range `index` of `count` is
    /// `index * chunk .. (index + 1) * chunk` of the file, `chunk` being the range size the file
    /// was started with. Sent exactly once per range of a file that completes (ranges listed in
    /// `FileStarted::done_ranges` are not sent again), never for single-request files.
    RangeDone {
        rel_path: RelPath,
        index: u32,
        count: u32,
    },
    /// Automatic tuning changed the number of parallel lanes (D22). With `reason` `budget` the
    /// count may be unchanged: growth just stopped. A job starts with 4 lanes and sends nothing
    /// for that.
    LanesChanged {
        lanes: u8,
        reason: LanesReason,
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
    /// A file skipped while the job runs. Files skipped at plan time are only counted in
    /// `Planned`, never announced one by one.
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
    /// A write failed because the disk is full (D21). The job is now paused and nothing was
    /// deleted; `resume` retries. `path` is the file being written, `needed` the bytes still to
    /// download and `free` the space available on the destination volume.
    DiskFull {
        path: String,
        needed: u64,
        free: u64,
    },
    Paused,
    Resumed,
    /// Terminal: the job ran to the end (some files may still have failed, see `Summary`).
    Finished(Summary),
    /// Terminal: the job could not run or was aborted by an error (for example the destination
    /// cannot be opened or the state database is unreadable).
    Failed {
        error: String,
    },
    /// Terminal: `JobHandle::cancel` was honoured. `.ampsync-part` files and the job state are kept.
    Cancelled,
}

/// Knobs that are not user settings: backoff timing and buffer sizes. The defaults are what
/// production uses; tests shorten them through [`Engine::with_tuning`].
#[derive(Debug, Clone)]
pub struct Tuning {
    /// First retry delay for a failed range; doubles per attempt (with jitter). Default 1 s.
    pub backoff_base: Duration,
    /// Upper bound of the retry delay. Default 60 s.
    pub backoff_max: Duration,
    /// How often `Progress`/`LaneProgress` events are produced. Default 250 ms (4 Hz).
    pub progress_interval: Duration,
    /// Attempts to rename a finished file over its destination (Windows sharing violations).
    pub rename_attempts: u32,
    /// Base delay between rename attempts (grows linearly).
    pub rename_delay: Duration,
    /// Bytes buffered per range before a positional write. Default 1 MiB.
    pub write_buffer: usize,
    /// Retry policy for listing and metadata requests (`Remote::set_retry_policy`).
    pub retry: RetryPolicy,
    /// Automatic lane tuning (used when `Settings::auto_tune` is on).
    pub autotune: AutoTuneConfig,
    /// Replaces [`crate::plan::free_space`] when reporting `Event::DiskFull`.
    pub free_space: Option<FreeSpaceHook>,
    /// Called before every positional write; an `Err` is treated as that write's own error.
    pub before_write: Option<WriteHook>,
    /// Directory that holds job state (`jobs/<key>/`). `None` uses the per-user data directory
    /// (`AMPSYNC_DATA_DIR` overrides it); tests point this at a temporary directory.
    pub data_dir: Option<PathBuf>,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning {
            backoff_base: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
            progress_interval: Duration::from_millis(250),
            rename_attempts: 10,
            rename_delay: Duration::from_millis(100),
            write_buffer: 1024 * 1024,
            retry: RetryPolicy::default(),
            autotune: AutoTuneConfig::default(),
            free_space: None,
            before_write: None,
            data_dir: None,
        }
    }
}

/// Test hook: how much space is free on a volume.
#[derive(Clone)]
pub struct FreeSpaceHook(Arc<FreeSpaceFn>);

type FreeSpaceFn = dyn Fn(&Path) -> std::io::Result<u64> + Send + Sync;

impl FreeSpaceHook {
    pub fn new(f: impl Fn(&Path) -> std::io::Result<u64> + Send + Sync + 'static) -> Self {
        FreeSpaceHook(Arc::new(f))
    }
}

impl std::fmt::Debug for FreeSpaceHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FreeSpaceHook")
    }
}

/// Test hook: runs before each positional write with the part file, the offset and the length.
#[derive(Clone)]
pub struct WriteHook(Arc<WriteFn>);

type WriteFn = dyn Fn(&Path, u64, usize) -> std::io::Result<()> + Send + Sync;

impl WriteHook {
    pub fn new(f: impl Fn(&Path, u64, usize) -> std::io::Result<()> + Send + Sync + 'static) -> Self {
        WriteHook(Arc::new(f))
    }
}

impl std::fmt::Debug for WriteHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WriteHook")
    }
}

/// Plans and runs jobs against a [`Remote`].
pub struct Engine<R: Remote> {
    remote: Arc<R>,
    settings: Settings,
    tuning: Tuning,
}

impl<R: Remote + 'static> Engine<R> {
    /// `settings` is passed through [`Settings::sanitized`].
    pub fn new(remote: Arc<R>, settings: Settings) -> Self {
        Engine {
            remote,
            settings: settings.sanitized(),
            tuning: Tuning::default(),
        }
    }

    /// Like [`Engine::new`] with explicit [`Tuning`] (used by tests to make backoff fast). The
    /// tuning's retry policy is handed to the remote.
    pub fn with_tuning(remote: Arc<R>, settings: Settings, tuning: Tuning) -> Self {
        remote.set_retry_policy(tuning.retry.clone());
        Engine {
            remote,
            settings: settings.sanitized(),
            tuning,
        }
    }

    /// List the remote tree (`Remote::resolve` + `Remote::walk`), apply `include`/`exclude`
    /// globs, compare with the destination and `state.db`, and decide an action per file.
    /// Downloads nothing and writes nothing into the destination; job state goes to the per-user
    /// data directory.
    pub async fn plan(&self, spec: &JobSpec) -> Result<Plan, Error> {
        plan::build(self.remote.as_ref(), spec, self.tuning.data_dir.as_deref()).await
    }

    /// Start transferring `plan` in the background. Must be called inside a tokio runtime.
    ///
    /// The first call to [`JobHandle::events`] returns a receiver that already holds every event
    /// since the job started, so nothing is missed between `start` and subscribing.
    pub fn start(&self, spec: JobSpec, plan: Plan) -> JobHandle {
        let (events, first_rx) = broadcast::channel(4096);
        let ctl = Arc::new(Control::new(events.clone()));
        let remote: Arc<dyn Remote> = self.remote.clone();
        let task = tokio::spawn(run_job(
            remote.clone(),
            self.settings.clone(),
            self.tuning.clone(),
            ctl.clone(),
            spec,
            plan,
        ));
        JobHandle {
            events,
            first_rx: Mutex::new(Some(first_rx)),
            task,
            ctl,
            remote,
        }
    }
}

/// Control and observation handle for a running job.
pub struct JobHandle {
    events: broadcast::Sender<Event>,
    first_rx: Mutex<Option<broadcast::Receiver<Event>>>,
    task: JoinHandle<Result<Summary, Error>>,
    ctl: Arc<Control>,
    remote: Arc<dyn Remote>,
}

impl JobHandle {
    /// Subscribe to events. The first call sees every event since the job started; later calls
    /// see events from now on. Slow receivers may lag (`RecvError::Lagged`); `Progress` is the only
    /// high-rate event.
    pub fn events(&self) -> broadcast::Receiver<Event> {
        let first = self.first_rx.lock().unwrap_or_else(|p| p.into_inner()).take();
        first.unwrap_or_else(|| self.events.subscribe())
    }

    /// Stop starting new requests and let in-flight ranges finish. Emits `Event::Paused`.
    pub fn pause(&self) {
        self.ctl.pause();
    }

    /// Continue after `pause` or after `AuthExpired`. Emits `Event::Resumed`.
    pub fn resume(&self) {
        self.ctl.resume();
    }

    /// Swap the cookies (re-sign-in), via `Remote::replace_session`, then resume.
    pub fn replace_session(&self, s: Session) {
        self.remote.replace_session(s);
        self.ctl.session_unproven.store(true, Ordering::SeqCst);
        self.ctl.session_gen.fetch_add(1, Ordering::SeqCst);
        self.ctl.resume();
    }

    /// Abort as soon as possible. `.ampsync-part` files and the job state are kept so a rerun
    /// resumes. `wait` then returns `Err(Error::Cancelled)`.
    pub fn cancel(&self) {
        self.ctl.cancel();
    }

    /// Wait for the job to finish. Emits `Event::Finished` before returning `Ok(summary)`.
    pub async fn wait(self) -> Result<Summary, Error> {
        match self.task.await {
            Ok(r) => r,
            Err(e) => Err(Error::State(format!(
                "the transfer task stopped unexpectedly: {e}"
            ))),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Job internals
// ---------------------------------------------------------------------------------------------

/// Throttling window starts only count as "new" when they extend the current one by this much.
const THROTTLE_EVENT_MARGIN: Duration = Duration::from_millis(100);
/// Consecutive 429/503 answers for one request before it is given up on.
const MAX_THROTTLES_PER_REQUEST: u32 = 200;

/// Pause / cancel / throttle state shared by all lanes and the handle.
struct Control {
    events: broadcast::Sender<Event>,
    paused: watch::Sender<bool>,
    cancelled: watch::Sender<bool>,
    throttle_until: Mutex<Option<Instant>>,
    /// Bumped by every `replace_session`, so a request that started on the old cookies and fails
    /// with `AuthExpired` afterwards is simply retried instead of pausing the job again.
    session_gen: AtomicU64,
    /// A new session arrived and no request has succeeded with it yet.
    session_unproven: AtomicBool,
    /// Bumped by every `throttle`, with the latest `Retry-After` in `throttle_last`; the tuner
    /// polls these.
    throttle_seq: AtomicU64,
    throttle_last: Mutex<Duration>,
    /// `DiskFull` was announced since the last resume (guards against one event per lane).
    disk_full: Mutex<bool>,
}

impl Control {
    fn new(events: broadcast::Sender<Event>) -> Self {
        Control {
            events,
            paused: watch::channel(false).0,
            cancelled: watch::channel(false).0,
            throttle_until: Mutex::new(None),
            session_gen: AtomicU64::new(0),
            session_unproven: AtomicBool::new(false),
            throttle_seq: AtomicU64::new(0),
            throttle_last: Mutex::new(Duration::ZERO),
            disk_full: Mutex::new(false),
        }
    }

    fn is_paused(&self) -> bool {
        *self.paused.borrow()
    }

    fn emit(&self, e: Event) {
        let _ = self.events.send(e);
    }

    fn is_cancelled(&self) -> bool {
        *self.cancelled.borrow()
    }

    fn pause(&self) {
        if !self.paused.send_replace(true) {
            self.emit(Event::Paused);
        }
    }

    fn resume(&self) {
        *lock(&self.disk_full) = false;
        if self.paused.send_replace(false) {
            self.emit(Event::Resumed);
        }
    }

    fn cancel(&self) {
        self.cancelled.send_replace(true);
    }

    /// The session expired: pause everything once and tell the UI (D9).
    fn auth_expired(&self, host: &str) {
        if !self.paused.send_replace(true) {
            self.emit(Event::AuthExpired {
                host: host.to_string(),
            });
            self.emit(Event::Paused);
        }
    }

    /// The disk is full: pause everything once and tell the UI (D21). `event` is only built for
    /// the first lane that reports it after a resume.
    fn disk_full(&self, event: impl FnOnce() -> Event) {
        let mut announced = lock(&self.disk_full);
        if !*announced {
            *announced = true;
            self.emit(event());
        }
        // Still holding the lock: a second lane's `Paused` cannot overtake the `DiskFull`.
        self.pause();
    }

    /// Waiting out a `Retry-After`.
    fn is_throttled(&self) -> bool {
        lock(&self.throttle_until).is_some_and(|until| until > Instant::now())
    }

    /// All lanes wait `retry_after` before their next request (D8).
    fn throttle(&self, retry_after: Duration) {
        *lock(&self.throttle_last) = retry_after;
        self.throttle_seq.fetch_add(1, Ordering::SeqCst);
        let until = Instant::now() + retry_after;
        let mut guard = self.throttle_until.lock().unwrap_or_else(|p| p.into_inner());
        let announce = match *guard {
            Some(current) => until > current + THROTTLE_EVENT_MARGIN,
            None => true,
        };
        if guard.is_none_or(|current| until > current) {
            *guard = Some(until);
        }
        drop(guard);
        if announce {
            self.emit(Event::Throttled { retry_after });
        }
    }

    /// Returns when requests may start: not paused, not throttled. `Err(Cancelled)` on cancel.
    async fn gate(&self) -> Result<(), Error> {
        let mut paused = self.paused.subscribe();
        let mut cancelled = self.cancelled.subscribe();
        loop {
            if *cancelled.borrow() {
                return Err(Error::Cancelled);
            }
            if *paused.borrow() {
                tokio::select! {
                    _ = paused.changed() => {}
                    _ = cancelled.changed() => {}
                }
                continue;
            }
            let until = *self.throttle_until.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(until) = until
                && until > Instant::now()
            {
                tokio::select! {
                    _ = tokio::time::sleep_until(until.into()) => {}
                    _ = cancelled.changed() => {}
                }
                continue;
            }
            return Ok(());
        }
    }

    /// Sleep, but wake early with `Err(Cancelled)` when the job is cancelled.
    async fn sleep(&self, d: Duration) -> Result<(), Error> {
        let mut cancelled = self.cancelled.subscribe();
        tokio::select! {
            _ = tokio::time::sleep(d) => Ok(()),
            _ = cancelled.wait_for(|c| *c) => Err(Error::Cancelled),
        }
    }
}

/// Token bucket shared by all lanes. One second of burst.
struct Limiter {
    rate: Option<u64>,
    state: Mutex<(f64, Instant)>,
}

impl Limiter {
    fn new(rate: Option<u64>) -> Self {
        Limiter {
            rate,
            state: Mutex::new((rate.unwrap_or(0) as f64, Instant::now())),
        }
    }

    /// Account for `n` bytes; returns how long the caller must wait before using them.
    fn reserve(&self, n: usize) -> Duration {
        let Some(rate) = self.rate else {
            return Duration::ZERO;
        };
        let rate = rate as f64;
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        let elapsed = now.duration_since(st.1).as_secs_f64();
        st.0 = (st.0 + elapsed * rate).min(rate);
        st.1 = now;
        st.0 -= n as f64;
        if st.0 < 0.0 {
            Duration::from_secs_f64(-st.0 / rate)
        } else {
            Duration::ZERO
        }
    }
}

/// What one lane is doing, for `LaneProgress`.
#[derive(Default)]
struct LaneInfo {
    file: Mutex<Option<(RelPath, u64)>>,
    offset: AtomicU64,
}

/// A file waiting to be activated.
struct Queued {
    file: RemoteFile,
    /// 0 for the first try, 1 for the whole-file retry after a verification mismatch.
    attempt: u8,
}

/// A file being downloaded.
struct FileCtx {
    file: RemoteFile,
    attempt: u8,
    final_path: PathBuf,
    part_path: PathBuf,
    handle: Mutex<Option<Arc<std::fs::File>>>,
    pending: Mutex<VecDeque<Range<u64>>>,
    /// Ranges not yet completed (queued or in flight).
    outstanding: AtomicUsize,
    /// Record each completed range in `state.db` (only worth it for large files).
    record_ranges: bool,
    started: AtomicBool,
    /// Bytes of ranges that completed in this run (the backstop for files that record no ranges).
    written: AtomicU64,
    failed: Mutex<Option<Error>>,
    failed_flag: AtomicBool,
    /// Range grid of a file split into ranges: cell `i` is `i * chunk .. (i + 1) * chunk`.
    chunk: u64,
    cell_count: u32,
    /// Cells already complete on disk at activation.
    done_cells: Vec<u32>,
    /// Pieces still to download per cell (a cell is reported done when its count reaches 0).
    cells_pending: Mutex<HashMap<u32, u32>>,
}

impl FileCtx {
    fn is_failed(&self) -> bool {
        self.failed_flag.load(Ordering::SeqCst)
    }

    /// Records the first failure; returns whether this call was the first.
    fn mark_failed(&self, e: Error) -> bool {
        let mut slot = self.failed.lock().unwrap_or_else(|p| p.into_inner());
        if slot.is_none() {
            *slot = Some(e);
            self.failed_flag.store(true, Ordering::SeqCst);
            true
        } else {
            false
        }
    }

    fn take_failure(&self) -> Option<Error> {
        self.failed.lock().unwrap_or_else(|p| p.into_inner()).take()
    }

    fn handle(&self) -> Result<Arc<std::fs::File>, Error> {
        self.handle
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .ok_or_else(|| Error::io(&self.part_path, std::io::Error::other("part file is closed")))
    }
}

#[derive(Default)]
struct Results {
    downloaded: u64,
    failed: u64,
    failures: Vec<FileFailure>,
}

struct Job {
    remote: Arc<dyn Remote>,
    settings: Settings,
    tuning: Tuning,
    state: StateDb,
    ctl: Arc<Control>,
    limiter: Limiter,
    dest: PathBuf,
    queue: Mutex<VecDeque<Queued>>,
    active: Mutex<VecDeque<Arc<FileCtx>>>,
    /// Files that have neither been verified nor failed for good.
    unfinished: AtomicUsize,
    notify: Notify,
    /// Progress counter; dips when a failed attempt's bytes are taken back.
    bytes_done: AtomicU64,
    bytes_total: AtomicU64,
    /// Bytes received in this run, never reduced (for `Summary::bytes`).
    transferred: AtomicU64,
    lanes: Vec<LaneInfo>,
    /// Lanes allowed to take new work: lane `i` works while `i < target`. Lanes above it finish
    /// what they hold and wait.
    lane_target: watch::Sender<usize>,
    /// Requests in the rolling budget window (only filled when tuning automatically).
    requests: RequestLog,
    /// Over the request budget: files started now use the larger range size.
    budget_ranges: AtomicBool,
    results: Mutex<Results>,
}

/// The server refused one file although the session as a whole is fine.
fn refused(rel: &RelPath) -> Error {
    Error::Http {
        status: 403,
        context: format!("{rel}: SharePoint refused access to this file"),
    }
}

/// A path as shown to the user: without the Windows long-path prefix or the part-file suffix.
fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    text.strip_suffix(plan::PART_SUFFIX).unwrap_or(text).to_string()
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Runs the job and guarantees exactly one terminal event: `Finished` (success), `Cancelled`, or
/// `Failed` (any other error), emitted before the task result becomes available to `wait`.
async fn run_job(
    remote: Arc<dyn Remote>,
    settings: Settings,
    tuning: Tuning,
    ctl: Arc<Control>,
    spec: JobSpec,
    plan: Plan,
) -> Result<Summary, Error> {
    let result = run_job_inner(remote, settings, tuning, ctl.clone(), spec, plan).await;
    match &result {
        Ok(_) => {} // `Finished` was emitted by run_job_inner
        Err(Error::Cancelled) => ctl.emit(Event::Cancelled),
        Err(e) => ctl.emit(Event::Failed { error: e.to_string() }),
    }
    result
}

async fn run_job_inner(
    remote: Arc<dyn Remote>,
    settings: Settings,
    tuning: Tuning,
    ctl: Arc<Control>,
    spec: JobSpec,
    plan: Plan,
) -> Result<Summary, Error> {
    let started = Instant::now();
    let dest = plan::absolute_dest(&spec.dest);
    // The plan normally carries the destination lock; otherwise take it now.
    let data_dir = tuning.data_dir.clone();
    let held = match plan.lock.take() {
        Some(l) if l.dest() == dest => l,
        _ => {
            let (dest, data_dir) = (dest.clone(), data_dir.clone());
            blocking(move || DestLock::acquire(&dest, data_dir.as_deref())).await?
        }
    };
    let state = {
        let dest = dest.clone();
        blocking(move || {
            // The destination is the user's folder: create it, but put nothing else in it.
            std::fs::create_dir_all(names::long_path(&dest)).map_err(|e| Error::io(&dest, e))?;
            StateDb::open_in(data_dir.as_deref(), &dest)
        })
        .await?
    };
    {
        let (state, spec, root) = (state.clone(), spec.clone(), plan.root.clone());
        blocking(move || state.save_job(&spec, &root)).await?;
    }

    let totals = plan.totals;
    ctl.emit(Event::Planned {
        files: totals.files,
        bytes: totals.bytes,
        skipped: totals.skipped,
        skipped_bytes: totals.skipped_bytes,
    });

    let mut queue = VecDeque::new();
    for item in &plan.items {
        match item.action {
            PlannedAction::SkipUnchanged => {}
            PlannedAction::Download | PlannedAction::Resume { .. } => queue.push_back(Queued {
                file: item.file.clone(),
                attempt: 0,
            }),
        }
    }

    // Automatic tuning starts a few lanes and moves within its bounds; otherwise the fixed number
    // of lanes is all there is.
    let auto = settings.auto_tune;
    let (lane_count, initial_lanes) = if auto {
        (tuning.autotune.max(), tuning.autotune.initial_lanes())
    } else {
        let n = usize::from(settings.parallel_files);
        (n, n)
    };
    let job = Arc::new(Job {
        requests: RequestLog::new(tuning.autotune.request_window),
        lane_target: watch::channel(initial_lanes).0,
        budget_ranges: AtomicBool::new(false),
        limiter: Limiter::new(settings.bandwidth_limit),
        remote,
        settings,
        tuning,
        state,
        ctl: ctl.clone(),
        dest,
        unfinished: AtomicUsize::new(queue.len()),
        queue: Mutex::new(queue),
        active: Mutex::new(VecDeque::new()),
        notify: Notify::new(),
        bytes_done: AtomicU64::new(0),
        bytes_total: AtomicU64::new(totals.bytes),
        transferred: AtomicU64::new(0),
        lanes: (0..lane_count).map(|_| LaneInfo::default()).collect(),
        results: Mutex::new(Results::default()),
    });

    let ticker = tokio::spawn(progress_ticker(job.clone()));
    let tuner = auto.then(|| tokio::spawn(tuner_task(job.clone())));
    let mut lanes = Vec::with_capacity(lane_count);
    for lane in 0..lane_count {
        lanes.push(tokio::spawn(lane_loop(job.clone(), lane)));
    }
    for l in lanes {
        if let Err(e) = l.await {
            tracing::error!("a transfer lane stopped unexpectedly: {e}");
        }
    }
    // `abort` only takes effect at a task's next await, so wait for each to actually stop:
    // otherwise a tick already in progress can emit Progress after `Finished`.
    ticker.abort();
    let _ = ticker.await;
    if let Some(tuner) = tuner {
        tuner.abort();
        let _ = tuner.await;
    }

    if ctl.is_cancelled() {
        return Err(Error::Cancelled);
    }

    let (downloaded, failed, failures) = {
        let r = lock(&job.results);
        (r.downloaded, r.failed, r.failures.clone())
    };
    if spec.prune && failed == 0 {
        let (dest, spec, plan_items) = (
            job.dest.clone(),
            spec.clone(),
            plan.items
                .iter()
                .map(|i| i.file.rel_path.clone())
                .collect::<Vec<_>>(),
        );
        match blocking(move || prune(&dest, &spec, &plan_items)).await {
            Ok(n) => tracing::info!("pruned {n} local files"),
            Err(e) => tracing::warn!("prune failed: {e}"),
        }
    }

    {
        let (state, dest, spec2, items) = (
            job.state.clone(),
            job.dest.clone(),
            spec.clone(),
            plan.items
                .iter()
                .map(|i| i.file.rel_path.clone())
                .collect::<Vec<_>>(),
        );
        let failed_paths: Vec<RelPath> = failures.iter().map(|f| f.rel_path.clone()).collect();
        match blocking(move || remove_part_files(&state, &dest, &spec2, &items, &failed_paths)).await {
            Ok(n) if n > 0 => tracing::info!("removed {n} leftover part files"),
            Ok(_) => {}
            Err(e) => tracing::warn!("part file cleanup failed: {e}"),
        }
    }

    job.emit_progress(&mut RateTracker::new());
    // Release the folder before announcing the end, so a caller that starts the next job on
    // `Finished` never trips over this job's lock.
    drop(held);
    let summary = Summary {
        downloaded,
        skipped: totals.skipped,
        failed,
        bytes: job.transferred.load(Ordering::SeqCst),
        elapsed: started.elapsed(),
        failures,
    };
    ctl.emit(Event::Finished(summary.clone()));
    Ok(summary)
}

/// Run blocking filesystem or SQLite work off the async threads.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    match tokio::task::spawn_blocking(f).await {
        Ok(r) => r,
        Err(e) => Err(Error::State(format!("background task failed: {e}"))),
    }
}

/// Exponentially smoothed transfer rate.
struct RateTracker {
    last_bytes: u64,
    last_at: Instant,
    rate: f64,
    first: bool,
}

impl RateTracker {
    fn new() -> Self {
        RateTracker {
            last_bytes: 0,
            last_at: Instant::now(),
            rate: 0.0,
            first: true,
        }
    }
}

impl Job {
    fn emit_progress(&self, tracker: &mut RateTracker) -> bool {
        let now = Instant::now();
        let done = self.bytes_done.load(Ordering::SeqCst);
        let dt = now.duration_since(tracker.last_at).as_secs_f64();
        if dt > 0.0 {
            let instant = done.saturating_sub(tracker.last_bytes) as f64 / dt;
            tracker.rate = if tracker.first {
                instant
            } else {
                0.7 * tracker.rate + 0.3 * instant
            };
        }
        let changed = tracker.first || done != tracker.last_bytes || tracker.rate >= 1.0;
        tracker.last_bytes = done;
        tracker.last_at = now;
        tracker.first = false;
        if changed {
            let total = self.bytes_total.load(Ordering::SeqCst).max(done);
            self.ctl.emit(Event::Progress {
                bytes_done: done,
                bytes_total: total,
                rate_bps: tracker.rate as u64,
            });
        }
        changed
    }

    /// Apply a decision of the tuner: move the lane target, switch the range size, announce.
    fn apply_lanes(&self, change: Option<Change>, controller: &Controller) {
        self.budget_ranges
            .store(controller.over_budget(), Ordering::SeqCst);
        if let Some(change) = change {
            self.lane_target.send_replace(change.lanes);
            self.ctl.emit(Event::LanesChanged {
                lanes: u8::try_from(change.lanes).unwrap_or(u8::MAX),
                reason: change.reason,
            });
        }
    }

    fn has_work(&self) -> bool {
        !lock(&self.queue).is_empty() || !lock(&self.active).is_empty()
    }

    fn finish_one_file(&self) {
        if self.unfinished.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.notify.notify_waiters();
        }
    }

    /// Take the next byte range of an active file.
    fn take_range(&self) -> Option<(Arc<FileCtx>, Range<u64>)> {
        let mut active = lock(&self.active);
        while let Some(ctx) = active.front().cloned() {
            let next = lock(&ctx.pending).pop_front();
            match next {
                Some(range) => {
                    if lock(&ctx.pending).is_empty() {
                        active.pop_front();
                    }
                    return Some((ctx, range));
                }
                None => {
                    active.pop_front();
                }
            }
        }
        None
    }

    async fn db<T: Send + 'static>(
        &self,
        f: impl FnOnce(&StateDb) -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        let state = self.state.clone();
        blocking(move || f(&state)).await
    }

    /// Run `op` with the standard error policy: wait while paused or throttled, honour
    /// `Retry-After`, back off exponentially with jitter on other retryable errors (at most
    /// `max_retries` times). On `AuthExpired` the remote is probed: a still-valid session means
    /// this one file was refused (`Http 403`, the file fails); otherwise the job pauses and the
    /// request is retried afterwards, once. A second `AuthExpired` for the same request after a
    /// new session arrived also fails the file instead of pausing again.
    async fn retrying<T, Fut>(&self, rel: &RelPath, mut op: impl FnMut() -> Fut) -> Result<T, Error>
    where
        Fut: Future<Output = Result<T, Error>>,
    {
        let mut attempt = 0u32;
        let mut throttles = 0u32;
        let mut auth_pauses = 0u32;
        let mut cancelled = self.ctl.cancelled.subscribe();
        loop {
            self.ctl.gate().await?;
            let generation = self.ctl.session_gen.load(Ordering::SeqCst);
            let outcome = tokio::select! {
                r = op() => r,
                _ = cancelled.wait_for(|c| *c) => Err(Error::Cancelled),
            };
            match outcome {
                Ok(v) => {
                    self.ctl.session_unproven.store(false, Ordering::SeqCst);
                    return Ok(v);
                }
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                Err(Error::AuthExpired { host }) => {
                    let stale = || self.ctl.session_gen.load(Ordering::SeqCst) != generation;
                    // Refused again right after a new session arrived: this file is refused, the
                    // job must not pause a second time.
                    if (auth_pauses > 0 || self.ctl.session_unproven.load(Ordering::SeqCst)) && !stale() {
                        return Err(refused(rel));
                    }
                    if auth_pauses == 0
                        && !stale()
                        && !self.ctl.is_paused()
                        && self.probe().await == SessionStatus::Valid
                    {
                        return Err(refused(rel));
                    }
                    // Sent with the old cookies and a new session arrived meanwhile (also while
                    // probing): try again without pausing. A refusal with the new cookies then
                    // means the file itself is refused.
                    auth_pauses += 1;
                    if !stale() {
                        self.ctl.auth_expired(&host);
                    }
                }
                Err(Error::Throttled { retry_after }) => {
                    throttles += 1;
                    if throttles > MAX_THROTTLES_PER_REQUEST {
                        return Err(Error::Throttled { retry_after });
                    }
                    self.ctl.throttle(retry_after);
                }
                Err(e) if e.is_retryable() && attempt < self.settings.max_retries => {
                    attempt += 1;
                    self.ctl.emit(Event::FileFailed {
                        rel_path: rel.clone(),
                        error: e.to_string(),
                        will_retry: true,
                    });
                    self.ctl.sleep(self.backoff(attempt)).await?;
                }
                // Pause; the next turn of the loop waits for `resume` and then tries again.
                Err(e) if e.is_disk_full() => self.pause_for_disk_full(&e),
                Err(e) => return Err(e),
            }
        }
    }

    /// A write failed because the disk is full (D21): pause the job and announce it once. Nothing
    /// is deleted, and the work that failed is retried after `resume`.
    fn pause_for_disk_full(&self, e: &Error) {
        self.ctl.disk_full(|| {
            let path = match e {
                Error::Io { path, .. } => display_path(path),
                _ => display_path(&self.dest),
            };
            let needed = self
                .bytes_total
                .load(Ordering::SeqCst)
                .saturating_sub(self.bytes_done.load(Ordering::SeqCst));
            let free = match &self.tuning.free_space {
                Some(hook) => (hook.0)(&self.dest),
                None => plan::free_space(&self.dest),
            }
            .unwrap_or(0);
            Event::DiskFull { path, needed, free }
        });
    }

    /// Count one request against the rolling budget (only needed when tuning automatically).
    fn count_request(&self) {
        if self.settings.auto_tune {
            self.requests.record();
        }
    }

    /// Range size for files started now: larger while over the request budget.
    fn range_size(&self) -> u64 {
        if self.budget_ranges.load(Ordering::SeqCst) {
            self.settings
                .range_size
                .max(self.tuning.autotune.budget_range_size)
        } else {
            self.settings.range_size
        }
    }

    /// Ask the remote whether the session is still accepted (cancellable).
    async fn probe(&self) -> SessionStatus {
        let mut cancelled = self.ctl.cancelled.subscribe();
        tokio::select! {
            s = self.remote.probe() => s,
            _ = cancelled.wait_for(|c| *c) => SessionStatus::Unknown,
        }
    }

    fn backoff(&self, attempt: u32) -> Duration {
        let exp = self
            .tuning
            .backoff_base
            .saturating_mul(1u32 << attempt.saturating_sub(1).min(16));
        let capped = exp.min(self.tuning.backoff_max);
        capped.mul_f64(0.5 + fastrand::f64() * 0.5)
    }
}

async fn progress_ticker(job: Arc<Job>) {
    let mut interval = tokio::time::interval(job.tuning.progress_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut tracker = RateTracker::new();
    let mut last_offsets = vec![u64::MAX; job.lanes.len()];
    loop {
        interval.tick().await;
        job.emit_progress(&mut tracker);
        for (i, lane) in job.lanes.iter().enumerate() {
            let current = lock(&lane.file).clone();
            if let Some((rel_path, size)) = current {
                let offset = lane.offset.load(Ordering::SeqCst);
                if last_offsets[i] != offset {
                    last_offsets[i] = offset;
                    job.ctl.emit(Event::LaneProgress {
                        lane: i as u8,
                        rel_path,
                        offset,
                        size,
                    });
                }
            } else {
                last_offsets[i] = u64::MAX;
            }
        }
    }
}

async fn lane_loop(job: Arc<Job>, lane: usize) {
    let mut target = job.lane_target.subscribe();
    loop {
        if job.ctl.gate().await.is_err() {
            return;
        }
        // A lane above the target takes no new work: it has finished what it held, and waits to be
        // wanted again or for the end of the job.
        let wanted = lane < *target.borrow_and_update();
        if wanted {
            if let Some((ctx, range)) = job.take_range() {
                process_range(&job, lane, ctx, range).await;
                continue;
            }
            let queued = lock(&job.queue).pop_front();
            if let Some(q) = queued {
                activate(&job, lane, q).await;
                continue;
            }
        }
        // Nothing to take right now. Wait for new work (a retry of a file), for a change of the
        // lane target, or for the end.
        let notified = job.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if wanted && job.has_work() {
            continue;
        }
        if job.unfinished.load(Ordering::SeqCst) == 0 {
            return;
        }
        let mut cancelled = job.ctl.cancelled.subscribe();
        tokio::select! {
            _ = &mut notified => {}
            _ = target.changed() => {}
            _ = cancelled.wait_for(|c| *c) => return,
        }
    }
}

/// Automatic tuning (D22): every window of active transfer, hand the throughput and the request
/// count to the controller and apply what it decides.
async fn tuner_task(job: Arc<Job>) {
    let cfg = job.tuning.autotune.clone();
    let mut controller = Controller::new(cfg.clone());
    let mut interval = tokio::time::interval(cfg.sample.max(Duration::from_millis(1)));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_at = Instant::now();
    let mut last_bytes = job.transferred.load(Ordering::SeqCst);
    let mut seen_throttles = job.ctl.throttle_seq.load(Ordering::SeqCst);
    let (mut window_bytes, mut window_time) = (0u64, Duration::ZERO);
    let mut window_requests_from = job.requests.total();
    loop {
        interval.tick().await;
        let now = Instant::now();
        let elapsed = now.duration_since(last_at);
        last_at = now;
        let bytes = job.transferred.load(Ordering::SeqCst);
        let delta = bytes.saturating_sub(last_bytes);
        last_bytes = bytes;

        let throttles = job.ctl.throttle_seq.load(Ordering::SeqCst);
        if throttles != seen_throttles {
            seen_throttles = throttles;
            let retry_after = *lock(&job.ctl.throttle_last);
            let change = controller.on_throttle(now, retry_after);
            (window_bytes, window_time) = (0, Duration::ZERO);
            window_requests_from = job.requests.total();
            job.apply_lanes(change, &controller);
            continue;
        }
        // Only time spent actually transferring counts towards a window.
        if job.ctl.is_paused() || job.ctl.is_throttled() {
            continue;
        }
        window_bytes += delta;
        window_time += elapsed;
        if window_time >= cfg.window {
            let throughput = window_bytes as f64 / window_time.as_secs_f64();
            // The rolling count lags far behind a change of lanes (5 minutes against a 10 s
            // window), so judge the budget by what it will reach at the rate of this window.
            let rate = (job.requests.total() - window_requests_from) as f64 / window_time.as_secs_f64();
            let projected = (rate * cfg.request_window.as_secs_f64()) as u64;
            let requests = job.requests.count().max(projected);
            (window_bytes, window_time) = (0, Duration::ZERO);
            window_requests_from = job.requests.total();
            let change = controller.on_window(now, throughput, requests);
            job.apply_lanes(change, &controller);
        }
    }
}

/// Prepare a file for download: create/validate the part file, work out which ranges are missing.
async fn activate(job: &Arc<Job>, lane: usize, q: Queued) {
    let Queued { file, attempt } = q;
    let rel = file.rel_path.clone();
    let prepared = loop {
        let job2 = job.clone();
        let file2 = file.clone();
        match blocking(move || prepare_file(&job2, file2, attempt)).await {
            // No room for the part file: pause, and try again after `resume`.
            Err(e) if e.is_disk_full() => {
                job.pause_for_disk_full(&e);
                if job.ctl.gate().await.is_err() {
                    return;
                }
            }
            other => break other,
        }
    };
    match prepared {
        Err(e) => fail_permanently(job, &rel, &e).await,
        Ok(ctx) => {
            let ctx = Arc::new(ctx);
            if ctx.outstanding.load(Ordering::SeqCst) == 0 {
                announce_start(job, lane, &ctx);
                finish_file(job, lane, ctx).await;
            } else {
                lock(&job.active).push_back(ctx);
                job.notify.notify_waiters();
            }
        }
    }
}

fn announce_start(job: &Job, lane: usize, ctx: &FileCtx) {
    if !ctx.started.swap(true, Ordering::SeqCst) {
        job.ctl.emit(Event::FileStarted {
            rel_path: ctx.file.rel_path.clone(),
            size: ctx.file.size,
            lane: lane as u8,
            done_ranges: ctx.done_cells.clone(),
        });
    }
}

/// Blocking part of activation (filesystem and database).
fn prepare_file(job: &Job, file: RemoteFile, attempt: u8) -> Result<FileCtx, Error> {
    let (final_path, part_path) = plan::local_paths(&job.dest, &file.rel_path);
    if let Some(dir) = part_path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    }
    let size = file.size;
    let record = job.state.file(&file.rel_path)?;
    let completed: Vec<Range<u64>> = {
        let reusable = attempt == 0
            && record
                .as_ref()
                .is_some_and(|r| r.etag == file.etag && r.size == size && r.status != FileStatus::Verified)
            && std::fs::metadata(&part_path).is_ok_and(|m| m.is_file() && m.len() == size);
        if reusable {
            job.state
                .completed_ranges(&file.rel_path)?
                .into_iter()
                .filter(|r| r.end <= size)
                .collect()
        } else {
            Vec::new()
        }
    };
    let reuse = !completed.is_empty();
    if !reuse {
        match std::fs::remove_file(&part_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::io(&part_path, e)),
        }
    }
    job.state.upsert_file(&FileRecord {
        rel_path: file.rel_path.clone(),
        size,
        etag: file.etag.clone(),
        hash: plan::hash_text(&file),
        status: FileStatus::InProgress,
        bytes_done: if reuse {
            completed.iter().map(|r| r.end - r.start).sum()
        } else {
            0
        },
        verified_by: None,
        error: None,
        local_name: plan::local_name_record(&file.rel_path),
    })?;
    if !reuse {
        job.state.reset_file(&file.rel_path)?;
        job.state
            .set_status(&file.rel_path, FileStatus::InProgress, None)?;
    }
    let handle = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&part_path)
        .map_err(|e| Error::io(&part_path, e))?;
    if handle.metadata().map_err(|e| Error::io(&part_path, e))?.len() != size {
        handle.set_len(size).map_err(|e| Error::io(&part_path, e))?;
    }

    let large = size > job.settings.large_file_threshold;
    // Ranges of a large file lie on a fixed grid of `chunk` bytes, so each has an index that stays
    // the same across resumes (and the fill map of the UI can use it).
    let chunk = job.range_size().max(1);
    let cell_count = if large {
        u32::try_from(size.div_ceil(chunk)).unwrap_or(u32::MAX)
    } else {
        0
    };
    let mut cells_pending: HashMap<u32, u32> = HashMap::new();
    let mut ranges = VecDeque::new();
    let mut cursor = 0u64;
    let mut gaps: Vec<Range<u64>> = Vec::new();
    for done in &completed {
        if done.start > cursor {
            gaps.push(cursor..done.start);
        }
        cursor = cursor.max(done.end);
    }
    if cursor < size {
        gaps.push(cursor..size);
    }
    for gap in gaps {
        if large {
            let mut start = gap.start;
            while start < gap.end {
                let end = ((start / chunk + 1) * chunk).min(gap.end);
                *cells_pending
                    .entry(u32::try_from(start / chunk).unwrap_or(u32::MAX))
                    .or_default() += 1;
                ranges.push_back(start..end);
                start = end;
            }
        } else {
            ranges.push_back(gap);
        }
    }

    let done_cells: Vec<u32> = (0..cell_count)
        .filter(|i| !cells_pending.contains_key(i))
        .collect();
    Ok(FileCtx {
        chunk,
        cell_count,
        done_cells,
        cells_pending: Mutex::new(cells_pending),
        file,
        attempt,
        final_path,
        part_path,
        handle: Mutex::new(Some(Arc::new(handle))),
        outstanding: AtomicUsize::new(ranges.len()),
        pending: Mutex::new(ranges),
        record_ranges: large,
        started: AtomicBool::new(false),
        written: AtomicU64::new(0),
        failed: Mutex::new(None),
        failed_flag: AtomicBool::new(false),
    })
}

async fn process_range(job: &Arc<Job>, lane: usize, ctx: Arc<FileCtx>, range: Range<u64>) {
    announce_start(job, lane, &ctx);
    let info = &job.lanes[lane];
    *lock(&info.file) = Some((ctx.file.rel_path.clone(), ctx.file.size));
    info.offset.store(range.start, Ordering::SeqCst);

    let result = job
        .retrying(&ctx.file.rel_path, || fetch_range(job, lane, &ctx, range.clone()))
        .await;
    if result.is_ok() {
        ctx.written.fetch_add(range.end - range.start, Ordering::SeqCst);
    }
    let result = match result {
        Ok(()) if ctx.record_ranges => {
            let (rel, r) = (ctx.file.rel_path.clone(), range.clone());
            let recorded = job.db(move |db| db.add_range(&rel, r)).await;
            if recorded.is_ok() {
                range_done(job, &ctx, &range);
            }
            recorded
        }
        other => other,
    };
    *lock(&info.file) = None;

    match result {
        Ok(()) => settle(job, lane, ctx, 1).await,
        Err(Error::Cancelled) if job.ctl.is_cancelled() => {}
        Err(Error::Cancelled) => settle(job, lane, ctx, 1).await, // the file was aborted by a sibling
        Err(e) => {
            if ctx.mark_failed(e) {
                // Drop the ranges nobody has started yet.
                let dropped = {
                    let mut pending = lock(&ctx.pending);
                    let n = pending.len();
                    pending.clear();
                    n
                };
                lock(&job.active).retain(|c| !Arc::ptr_eq(c, &ctx));
                settle(job, lane, ctx, dropped + 1).await;
            } else {
                settle(job, lane, ctx, 1).await;
            }
        }
    }
}

/// Announce a grid cell once its last piece is on disk (D24).
fn range_done(job: &Job, ctx: &FileCtx, range: &Range<u64>) {
    let index = u32::try_from(range.start / ctx.chunk).unwrap_or(u32::MAX);
    let finished = {
        let mut cells = lock(&ctx.cells_pending);
        match cells.get_mut(&index) {
            Some(n) if *n > 1 => {
                *n -= 1;
                false
            }
            Some(_) => cells.remove(&index).is_some(),
            None => false,
        }
    };
    if finished {
        job.ctl.emit(Event::RangeDone {
            rel_path: ctx.file.rel_path.clone(),
            index,
            count: ctx.cell_count,
        });
    }
}

/// Mark `count` ranges of the file as done; the lane that completes the last one finishes the file.
async fn settle(job: &Arc<Job>, lane: usize, ctx: Arc<FileCtx>, count: usize) {
    if ctx.outstanding.fetch_sub(count, Ordering::SeqCst) == count {
        finish_file(job, lane, ctx).await;
    }
}

/// Download one range into the part file.
async fn fetch_range(
    job: &Arc<Job>,
    lane: usize,
    ctx: &Arc<FileCtx>,
    range: Range<u64>,
) -> Result<(), Error> {
    let mut counted = 0u64;
    let r = fetch_range_inner(job, lane, ctx, range, &mut counted).await;
    if r.is_err() {
        job.bytes_done.fetch_sub(counted, Ordering::SeqCst);
    }
    r
}

async fn fetch_range_inner(
    job: &Arc<Job>,
    lane: usize,
    ctx: &Arc<FileCtx>,
    range: Range<u64>,
    counted: &mut u64,
) -> Result<(), Error> {
    let handle = ctx.handle()?;
    job.count_request();
    let mut stream = job.remote.read_range(&ctx.file, range.clone()).await?;
    let mut cancelled = job.ctl.cancelled.subscribe();
    let mut buf: Vec<u8> = Vec::new();
    let mut buf_start = range.start;
    let mut offset = range.start;
    loop {
        let next = tokio::select! {
            n = stream.next() => n,
            _ = cancelled.wait_for(|c| *c) => return Err(Error::Cancelled),
        };
        let Some(chunk) = next else { break };
        let chunk = chunk?;
        if ctx.is_failed() {
            return Err(Error::Cancelled);
        }
        let wait = job.limiter.reserve(chunk.len());
        if !wait.is_zero() {
            job.ctl.sleep(wait).await?;
        }
        let n = chunk.len() as u64;
        *counted += n;
        job.bytes_done.fetch_add(n, Ordering::SeqCst);
        job.transferred.fetch_add(n, Ordering::SeqCst);
        offset += n;
        job.lanes[lane].offset.store(offset, Ordering::SeqCst);
        buf.extend_from_slice(&chunk);
        if buf.len() >= job.tuning.write_buffer {
            flush(job, &handle, &mut buf, buf_start, &ctx.part_path).await?;
            buf_start = offset;
        }
    }
    if offset != range.end {
        return Err(Error::Network(format!(
            "the connection ended after {} of {} bytes",
            offset - range.start,
            range.end - range.start
        )));
    }
    flush(job, &handle, &mut buf, buf_start, &ctx.part_path).await?;
    if ctx.record_ranges {
        // The range is about to be recorded as complete: make sure it is really on disk.
        let (h, p) = (handle.clone(), ctx.part_path.clone());
        blocking(move || h.sync_data().map_err(|e| Error::io(p, e))).await?;
    }
    Ok(())
}

async fn flush(
    job: &Job,
    handle: &Arc<std::fs::File>,
    buf: &mut Vec<u8>,
    pos: u64,
    path: &Path,
) -> Result<(), Error> {
    if buf.is_empty() {
        return Ok(());
    }
    let data = std::mem::take(buf);
    let (h, p) = (handle.clone(), path.to_path_buf());
    let hook = job.tuning.before_write.clone();
    blocking(move || {
        if let Some(hook) = &hook {
            (hook.0)(&p, pos, data.len()).map_err(|e| Error::io(&p, e))?;
        }
        write_all_at(&h, &data, pos).map_err(|e| Error::io(p, e))
    })
    .await
}

#[cfg(windows)]
fn write_all_at(file: &std::fs::File, mut buf: &[u8], mut pos: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = file.seek_write(buf, pos)?;
        if n == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        buf = &buf[n..];
        pos += n as u64;
    }
    Ok(())
}

#[cfg(unix)]
fn write_all_at(file: &std::fs::File, buf: &[u8], pos: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(buf, pos)
}

/// Why finalisation did not produce a verified file.
struct FinalizeError {
    error: Error,
    /// Fresh metadata from the server, when the file changed under us.
    refreshed: Option<Box<RemoteFile>>,
}

impl From<Error> for FinalizeError {
    fn from(error: Error) -> Self {
        FinalizeError {
            error,
            refreshed: None,
        }
    }
}

async fn finish_file(job: &Arc<Job>, lane: usize, ctx: Arc<FileCtx>) {
    let rel = ctx.file.rel_path.clone();
    if let Some(e) = ctx.take_failure() {
        fail_permanently(job, &rel, &e).await;
        return;
    }
    match finalize(job, &ctx).await {
        Ok(by) => {
            let rel2 = rel.clone();
            if let Err(e) = job.db(move |db| db.mark_verified(&rel2, by)).await {
                fail_permanently(job, &rel, &e).await;
                return;
            }
            job.ctl.emit(Event::FileVerified { rel_path: rel, by });
            lock(&job.results).downloaded += 1;
            job.finish_one_file();
        }
        Err(FinalizeError {
            error: Error::Cancelled,
            ..
        }) => {}
        Err(FinalizeError { error, refreshed })
            if ctx.attempt == 0 && matches!(error, Error::Integrity { .. } | Error::RemoteChanged { .. }) =>
        {
            // Throw the part file away and fetch the file once more, with fresh metadata if the
            // server changed it.
            job.ctl.emit(Event::FileFailed {
                rel_path: rel.clone(),
                error: error.to_string(),
                will_retry: true,
            });
            let (part, rel2) = (ctx.part_path.clone(), rel.clone());
            let _ = job
                .db(move |db| {
                    let _ = std::fs::remove_file(&part);
                    db.reset_file(&rel2)
                })
                .await;
            let file = refreshed.map(|b| *b).unwrap_or_else(|| ctx.file.clone());
            if file.size != ctx.file.size {
                let total = job.bytes_total.load(Ordering::SeqCst);
                job.bytes_total.store(
                    (total + file.size).saturating_sub(ctx.file.size),
                    Ordering::SeqCst,
                );
            }
            lock(&job.queue).push_front(Queued { file, attempt: 1 });
            job.notify.notify_waiters();
        }
        Err(FinalizeError { error, .. }) => {
            if matches!(error, Error::Integrity { .. } | Error::RemoteChanged { .. }) {
                let part = ctx.part_path.clone();
                let _ = blocking(move || Ok(std::fs::remove_file(part).ok())).await;
            }
            fail_permanently(job, &rel, &error).await;
        }
    }
    let _ = lane;
}

/// Record a final failure of a file: state, event, summary.
async fn fail_permanently(job: &Arc<Job>, rel: &RelPath, error: &Error) {
    let text = error.to_string();
    let (rel2, text2) = (rel.clone(), text.clone());
    let _ = job
        .db(move |db| db.set_status(&rel2, FileStatus::Failed, Some(&text2)))
        .await;
    job.ctl.emit(Event::FileFailed {
        rel_path: rel.clone(),
        error: text.clone(),
        will_retry: false,
    });
    {
        let mut r = lock(&job.results);
        r.failed += 1;
        r.failures.push(FileFailure {
            rel_path: rel.clone(),
            error: text,
        });
    }
    job.finish_one_file();
}

/// Verify a fully written part file and move it into place.
async fn finalize(job: &Arc<Job>, ctx: &Arc<FileCtx>) -> Result<VerifiedBy, FinalizeError> {
    let handle = ctx.handle()?;
    let size = ctx.file.size;
    {
        let actual = job
            .retrying(&ctx.file.rel_path, || {
                let (h, p) = (handle.clone(), ctx.part_path.clone());
                blocking(move || {
                    h.sync_all().map_err(|e| Error::io(&p, e))?;
                    h.metadata().map(|m| m.len()).map_err(|e| Error::io(&p, e))
                })
            })
            .await?;
        if actual != size {
            return Err(Error::Integrity {
                rel_path: ctx.file.rel_path.clone(),
                expected: format!("{size} bytes"),
                actual: format!("{actual} bytes"),
            }
            .into());
        }
    }

    let by = match &ctx.file.hash {
        Some(RemoteHash::QuickXor(expected)) => {
            let (p, expected, rel) = (ctx.part_path.clone(), expected.clone(), ctx.file.rel_path.clone());
            blocking(move || verify_quickxor(&p, &expected, &rel)).await?;
            VerifiedBy::Hash
        }
        _ => {
            let fresh = job
                .retrying(&ctx.file.rel_path, || {
                    job.count_request();
                    job.remote.stat(&ctx.file)
                })
                .await?;
            if fresh.etag != ctx.file.etag || fresh.size != ctx.file.size {
                return Err(FinalizeError {
                    error: Error::RemoteChanged {
                        rel_path: ctx.file.rel_path.clone(),
                    },
                    refreshed: Some(Box::new(fresh)),
                });
            }
            VerifiedBy::SizeAndEtag
        }
    };

    // Backstop: what was recorded as downloaded must add up to the whole file.
    let covered = if ctx.record_ranges {
        let rel = ctx.file.rel_path.clone();
        let ranges = job.db(move |db| db.completed_ranges(&rel)).await?;
        covered_bytes(&ranges, size)
    } else {
        ctx.written.load(Ordering::SeqCst)
    };
    if covered != size {
        return Err(Error::Integrity {
            rel_path: ctx.file.rel_path.clone(),
            expected: format!("{size} bytes downloaded"),
            actual: format!("{covered} bytes downloaded"),
        }
        .into());
    }

    // Close our handle, set the remote modification time, and rename over the destination.
    let taken = lock(&ctx.handle).take();
    let (part, dest, modified) = (ctx.part_path.clone(), ctx.final_path.clone(), ctx.file.modified);
    let (attempts, delay) = (job.tuning.rename_attempts, job.tuning.rename_delay);
    blocking(move || {
        if let Some(h) = &taken {
            let _ = h.set_modified(std::time::SystemTime::from(modified));
        }
        drop(taken);
        rename_with_retry(&part, &dest, attempts, delay).map_err(|e| Error::io(&dest, e))
    })
    .await?;
    Ok(by)
}

/// Total length of the union of `ranges`, clipped to `0..size`.
fn covered_bytes(ranges: &[Range<u64>], size: u64) -> u64 {
    let mut sorted: Vec<Range<u64>> = ranges
        .iter()
        .filter(|r| r.start < r.end && r.start < size)
        .map(|r| r.start..r.end.min(size))
        .collect();
    sorted.sort_by_key(|r| r.start);
    let (mut total, mut cursor) = (0u64, 0u64);
    for r in sorted {
        let start = r.start.max(cursor);
        if r.end > start {
            total += r.end - start;
            cursor = r.end;
        }
    }
    total
}

fn verify_quickxor(path: &Path, expected: &str, rel: &RelPath) -> Result<(), Error> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| Error::io(path, e))?;
    let mut hasher = QuickXorHash::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| Error::io(path, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let actual = hasher.finalize_base64();
    if actual == expected {
        Ok(())
    } else {
        Err(Error::Integrity {
            rel_path: rel.clone(),
            expected: expected.to_string(),
            actual,
        })
    }
}

/// `rename` that retries while another process (antivirus, indexer, preview pane) has the
/// destination open on Windows.
fn rename_with_retry(from: &Path, to: &Path, attempts: u32, delay: Duration) -> std::io::Result<()> {
    let attempts = attempts.max(1);
    let mut n = 0;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => {
                n += 1;
                let transient = e.kind() == std::io::ErrorKind::PermissionDenied
                    || (cfg!(windows) && matches!(e.raw_os_error(), Some(5 | 32 | 33)));
                if !transient || n >= attempts {
                    return Err(e);
                }
                std::thread::sleep(delay * n);
            }
        }
    }
}

/// Delete the `.ampsync-part` files in the folders a finished job covers (inside the include /
/// exclude scope), so nothing of AmpSync stays in the destination. The part file of a failed file
/// that has recorded ranges is kept: it is resumable and the next successful run removes it.
/// Returns the number of files removed.
fn remove_part_files(
    state: &StateDb,
    dest: &Path,
    spec: &JobSpec,
    planned: &[RelPath],
    failed: &[RelPath],
) -> Result<u64, Error> {
    let norm = |p: &Path| {
        let s = p.to_string_lossy().to_string();
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    let filters = Filters::for_spec(spec)?;
    let mut keep: HashSet<String> = HashSet::new();
    for rel in failed {
        if !state.completed_ranges(rel)?.is_empty() {
            keep.insert(norm(&plan::local_paths(dest, rel).1));
        }
    }
    // Each folder that holds a planned file, with the local relative prefix of its contents.
    let mut dirs: std::collections::BTreeMap<String, PathBuf> = std::collections::BTreeMap::new();
    for rel in planned {
        let local = plan::local_rel_path(rel);
        let prefix = local.parent().map(|p| p.as_str().to_string()).unwrap_or_default();
        let dir = plan::local_paths(dest, rel).0.parent().map(Path::to_path_buf);
        if let Some(dir) = dir {
            dirs.entry(prefix).or_insert(dir);
        }
    }
    let mut removed = 0u64;
    for (prefix, dir) in dirs {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(final_name) = name.strip_suffix(plan::PART_SUFFIX) else {
                continue;
            };
            if !entry.file_type().is_ok_and(|t| t.is_file()) {
                continue;
            }
            let final_rel = if prefix.is_empty() {
                final_name.to_string()
            } else {
                format!("{prefix}/{final_name}")
            };
            let path = entry.path();
            if !filters.matches(&final_rel) || keep.contains(&norm(&path)) {
                continue;
            }
            match std::fs::remove_file(&path) {
                Ok(()) => removed += 1,
                Err(e) => tracing::warn!("cannot remove {}: {e}", path.display()),
            }
        }
    }
    Ok(removed)
}

/// Delete local files inside the job's scope (matching the include/exclude globs) that are not part
/// of the plan, plus leftover part files, then remove directories left empty. Returns files removed.
fn prune(dest: &Path, spec: &JobSpec, planned: &[RelPath]) -> Result<u64, Error> {
    let filters = Filters::for_spec(spec)?;
    let norm = |s: &str| {
        if cfg!(windows) {
            s.to_lowercase()
        } else {
            s.to_string()
        }
    };
    let keep: HashSet<String> = planned
        .iter()
        .map(|r| norm(plan::local_rel_path(r).as_str()))
        .collect();
    let mut removed = 0u64;
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut stack = vec![(dest.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let read = std::fs::read_dir(names::long_path(&dir)).map_err(|e| Error::io(&dir, e))?;
        for entry in read {
            let entry = entry.map_err(|e| Error::io(&dir, e))?;
            let name = entry.file_name().to_string_lossy().to_string();
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let path = entry.path();
            let kind = entry.file_type().map_err(|e| Error::io(&path, e))?;
            if kind.is_dir() {
                if prefix.is_empty() && name == ".ampsync" {
                    continue;
                }
                dirs.push(path.clone());
                stack.push((path, rel));
            } else {
                let final_rel = rel.strip_suffix(plan::PART_SUFFIX).unwrap_or(&rel);
                if filters.matches(final_rel) && !keep.contains(&norm(final_rel)) {
                    std::fs::remove_file(&path).map_err(|e| Error::io(&path, e))?;
                    removed += 1;
                }
            }
        }
    }
    // Deepest first; `remove_dir` only succeeds on empty directories.
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for d in dirs {
        let _ = std::fs::remove_dir(names::long_path(&d));
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_files_are_swept_except_resumable_failures() {
        let dest = tempfile::tempdir().unwrap();
        let rel = |s: &str| RelPath::new(s).unwrap();
        let touch = |p: &str| {
            let path = dest.path().join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"x").unwrap();
            path
        };
        let resumable = touch("sub/resumable.bin.ampsync-part");
        let dead = touch("sub/dead.bin.ampsync-part");
        let stray = touch("sub/stray.bin.ampsync-part");
        let root_stray = touch("root.bin.ampsync-part");
        let excluded = touch("sub/skip.tmp.ampsync-part");
        let elsewhere = touch("other/unrelated.bin.ampsync-part");
        let real_file = touch("sub/keep.txt");

        let state = StateDb::open_in_memory().unwrap();
        for (name, ranges) in [("sub/resumable.bin", true), ("sub/dead.bin", false)] {
            state
                .upsert_file(&FileRecord {
                    rel_path: rel(name),
                    size: 100,
                    etag: "e".into(),
                    hash: None,
                    status: FileStatus::Failed,
                    bytes_done: 0,
                    verified_by: None,
                    error: Some("boom".into()),
                    local_name: None,
                })
                .unwrap();
            if ranges {
                state.add_range(&rel(name), 0..10).unwrap();
            }
        }
        let mut spec = JobSpec::new("link", dest.path());
        spec.exclude = vec!["*.tmp".into()];
        let planned = [
            rel("sub/resumable.bin"),
            rel("sub/dead.bin"),
            rel("root.bin"),
            rel("sub/keep.txt"),
        ];
        let failed = [rel("sub/resumable.bin"), rel("sub/dead.bin")];
        let n = remove_part_files(&state, dest.path(), &spec, &planned, &failed).unwrap();

        assert_eq!(n, 3);
        assert!(
            resumable.exists(),
            "a failed file with recorded ranges stays resumable"
        );
        assert!(!dead.exists() && !stray.exists() && !root_stray.exists());
        assert!(excluded.exists(), "outside the include/exclude scope");
        assert!(
            elsewhere.exists(),
            "folders the job does not cover are left alone"
        );
        assert!(real_file.exists());

        // A clean run removes the resumable leftover as well.
        let n = remove_part_files(&state, dest.path(), &spec, &planned, &[]).unwrap();
        assert_eq!(n, 1);
        assert!(!resumable.exists());
    }

    #[test]
    fn events_serialise_tagged() {
        let e = Event::FileStarted {
            rel_path: RelPath::new("a/b.txt").unwrap(),
            size: 10,
            lane: 2,
            done_ranges: vec![0, 3],
        };
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"type":"file_started","rel_path":"a/b.txt","size":10,"lane":2,"done_ranges":[0,3]}"#
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
    fn v1_1_events_serialise() {
        let r = Event::RangeDone {
            rel_path: RelPath::new("big.bin").unwrap(),
            index: 4,
            count: 9,
        };
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"type":"range_done","rel_path":"big.bin","index":4,"count":9}"#
        );
        let l = Event::LanesChanged {
            lanes: 7,
            reason: LanesReason::ProbeUp,
        };
        assert_eq!(
            serde_json::to_string(&l).unwrap(),
            r#"{"type":"lanes_changed","lanes":7,"reason":"probe_up"}"#
        );
        let d = Event::DiskFull {
            path: "/d/x.bin".into(),
            needed: 10,
            free: 2,
        };
        assert_eq!(
            serde_json::to_string(&d).unwrap(),
            r#"{"type":"disk_full","path":"/d/x.bin","needed":10,"free":2}"#
        );
    }

    #[test]
    fn display_path_hides_the_plumbing() {
        assert_eq!(
            display_path(Path::new(r"\\?\D:\dest\a.bin.ampsync-part")),
            r"D:\dest\a.bin"
        );
        assert_eq!(display_path(Path::new("/d/a.bin.ampsync-part")), "/d/a.bin");
        assert_eq!(display_path(Path::new("/d/a.bin")), "/d/a.bin");
    }

    #[test]
    fn disk_full_is_announced_once_per_resume() {
        let (tx, mut rx) = broadcast::channel(16);
        let c = Control::new(tx);
        let ev = || Event::DiskFull {
            path: "p".into(),
            needed: 1,
            free: 0,
        };
        c.disk_full(ev);
        c.disk_full(ev); // a second lane hit the same wall
        c.resume();
        c.disk_full(ev);
        let mut kinds = Vec::new();
        while let Ok(e) = rx.try_recv() {
            kinds.push(
                serde_json::to_value(&e).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            );
        }
        assert_eq!(kinds, ["disk_full", "paused", "resumed", "disk_full", "paused"]);
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

    #[test]
    fn limiter_charges_debt_after_the_burst() {
        let l = Limiter::new(Some(1000));
        assert_eq!(l.reserve(1000), Duration::ZERO, "one second of burst is free");
        let wait = l.reserve(500);
        assert!(
            wait > Duration::from_millis(400) && wait <= Duration::from_millis(500),
            "{wait:?}"
        );
        assert_eq!(Limiter::new(None).reserve(1 << 30), Duration::ZERO);
    }

    #[test]
    fn covered_bytes_is_the_clipped_union() {
        assert_eq!(covered_bytes(&[0..10, 10..20], 20), 20);
        assert_eq!(covered_bytes(&[0..10, 12..20], 20), 18, "a gap is not covered");
        assert_eq!(covered_bytes(&[0..15, 10..20], 20), 20, "overlaps count once");
        assert_eq!(covered_bytes(&[0..30, 0..1], 20), 20, "clipped to the file size");
        assert_eq!(covered_bytes(&[], 20), 0);
        assert_eq!(covered_bytes(&[5..5, 25..30], 20), 0);
    }

    #[test]
    fn planned_event_carries_skipped_bytes() {
        let e = Event::Planned {
            files: 1,
            bytes: 2,
            skipped: 3,
            skipped_bytes: 4,
        };
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"type":"planned","files":1,"bytes":2,"skipped":3,"skipped_bytes":4}"#
        );
    }

    #[test]
    fn terminal_events_serialise() {
        assert_eq!(
            serde_json::to_string(&Event::Failed { error: "x".into() }).unwrap(),
            r#"{"type":"failed","error":"x"}"#
        );
        assert_eq!(
            serde_json::to_string(&Event::Cancelled).unwrap(),
            r#"{"type":"cancelled"}"#
        );
    }

    #[test]
    fn control_pause_and_throttle_events_are_deduplicated() {
        let (tx, mut rx) = broadcast::channel(16);
        let c = Control::new(tx);
        c.pause();
        c.pause();
        c.auth_expired("h"); // already paused: no second Paused, no AuthExpired
        c.resume();
        c.resume();
        c.auth_expired("h");
        c.throttle(Duration::from_secs(5));
        c.throttle(Duration::from_secs(5));
        let mut kinds = Vec::new();
        while let Ok(e) = rx.try_recv() {
            kinds.push(
                serde_json::to_value(&e).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            );
        }
        assert_eq!(
            kinds,
            ["paused", "resumed", "auth_expired", "paused", "throttled"]
        );
    }
}
