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
//! - Only one AmpSync process may use a destination: `<dest>/.ampsync/lock` is held from `plan`
//!   to the end of the job (`Error::DestinationBusy` otherwise).
//! - Before the rename, the bytes recorded as downloaded must add up to the file size; if they do
//!   not, the part file is reset and the file retried once.
//! - Windows: use `\\?\` long paths ([`crate::names::long_path`]); replace illegal characters with
//!   [`crate::names::sanitize`] and record the mapping in `state.db`.
//! - Nothing local is deleted unless `JobSpec::prune`.

use std::collections::{HashSet, VecDeque};
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
    Paused,
    Resumed,
    /// Terminal: the job ran to the end (some files may still have failed, see `Summary`).
    Finished(Summary),
    /// Terminal: the job could not run or was aborted by an error (for example the destination
    /// cannot be opened or the state database is unreadable).
    Failed {
        error: String,
    },
    /// Terminal: `JobHandle::cancel` was honoured. `.ampsync-part` files and `state.db` are kept.
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
        }
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
    /// Downloads nothing and writes nothing outside `<dest>/.ampsync/`.
    pub async fn plan(&self, spec: &JobSpec) -> Result<Plan, Error> {
        plan::build(self.remote.as_ref(), spec).await
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

    /// Abort as soon as possible. `.ampsync-part` files and `state.db` are kept so a rerun
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

    /// All lanes wait `retry_after` before their next request (D8).
    fn throttle(&self, retry_after: Duration) {
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
    results: Mutex<Results>,
}

/// The server refused one file although the session as a whole is fine.
fn refused(rel: &RelPath) -> Error {
    Error::Http {
        status: 403,
        context: format!("{rel}: SharePoint refused access to this file"),
    }
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
    let held = match plan.lock.take() {
        Some(l) if l.dest() == dest => l,
        _ => {
            let dest = dest.clone();
            blocking(move || DestLock::acquire(&dest)).await?
        }
    };
    let state = {
        let dest = dest.clone();
        blocking(move || StateDb::open(&dest)).await?
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

    let lane_count = usize::from(settings.parallel_files);
    let job = Arc::new(Job {
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
    let mut lanes = Vec::with_capacity(lane_count);
    for lane in 0..lane_count {
        lanes.push(tokio::spawn(lane_loop(job.clone(), lane)));
    }
    for l in lanes {
        if let Err(e) = l.await {
            tracing::error!("a transfer lane stopped unexpectedly: {e}");
        }
    }
    ticker.abort();

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
                Err(e) => return Err(e),
            }
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
    loop {
        if job.ctl.gate().await.is_err() {
            return;
        }
        if let Some((ctx, range)) = job.take_range() {
            process_range(&job, lane, ctx, range).await;
            continue;
        }
        let queued = lock(&job.queue).pop_front();
        if let Some(q) = queued {
            activate(&job, lane, q).await;
            continue;
        }
        // Nothing to take right now. Wait for new work (a retry of a file) or for the end.
        let notified = job.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if job.has_work() {
            continue;
        }
        if job.unfinished.load(Ordering::SeqCst) == 0 {
            return;
        }
        let mut cancelled = job.ctl.cancelled.subscribe();
        tokio::select! {
            _ = &mut notified => {}
            _ = cancelled.wait_for(|c| *c) => return,
        }
    }
}

/// Prepare a file for download: create/validate the part file, work out which ranges are missing.
async fn activate(job: &Arc<Job>, lane: usize, q: Queued) {
    let Queued { file, attempt } = q;
    let rel = file.rel_path.clone();
    let prepared = {
        let job2 = job.clone();
        let file2 = file.clone();
        blocking(move || prepare_file(&job2, file2, attempt)).await
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
                let end = (start + job.settings.range_size).min(gap.end);
                ranges.push_back(start..end);
                start = end;
            }
        } else {
            ranges.push_back(gap);
        }
    }

    Ok(FileCtx {
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
            job.db(move |db| db.add_range(&rel, r)).await
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
            flush(&handle, &mut buf, buf_start, &ctx.part_path).await?;
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
    flush(&handle, &mut buf, buf_start, &ctx.part_path).await?;
    if ctx.record_ranges {
        // The range is about to be recorded as complete: make sure it is really on disk.
        let (h, p) = (handle.clone(), ctx.part_path.clone());
        blocking(move || h.sync_data().map_err(|e| Error::io(p, e))).await?;
    }
    Ok(())
}

async fn flush(handle: &Arc<std::fs::File>, buf: &mut Vec<u8>, pos: u64, path: &Path) -> Result<(), Error> {
    if buf.is_empty() {
        return Ok(());
    }
    let data = std::mem::take(buf);
    let (h, p) = (handle.clone(), path.to_path_buf());
    blocking(move || write_all_at(&h, &data, pos).map_err(|e| Error::io(p, e))).await
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
        let (h, p) = (handle.clone(), ctx.part_path.clone());
        let actual = blocking(move || {
            h.sync_all().map_err(|e| Error::io(&p, e))?;
            h.metadata().map(|m| m.len()).map_err(|e| Error::io(&p, e))
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
                .retrying(&ctx.file.rel_path, || job.remote.stat(&ctx.file))
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

/// Delete local files inside the job's scope (matching the include/exclude globs) that are not part
/// of the plan, plus leftover part files, then remove directories left empty. Returns files removed.
fn prune(dest: &Path, spec: &JobSpec, planned: &[RelPath]) -> Result<u64, Error> {
    let filters = Filters::new(&spec.include, &spec.exclude)?;
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
