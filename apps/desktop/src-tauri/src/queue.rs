//! The download queue: jobs run one at a time (D16 in the UI), survive restarts, and wait for a
//! working sign-in before they start or resume.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ampsync_core::Error;
use ampsync_core::SessionStatus;
use ampsync_core::auth::Session;
use ampsync_core::engine::{Engine, Event, Summary};
use ampsync_core::link::{self, ParsedLink};
use ampsync_core::plan::{ExistingPolicy, JobSpec};
use ampsync_core::remote::ClassicRemote;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, Url};
use tokio::sync::{Notify, mpsc, watch};

use crate::AppState;
use crate::paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Queued,
    /// Signing in and listing files.
    Preparing,
    Running,
    Paused,
    /// The job is paused until the user signs in again.
    NeedsSignIn,
    /// The destination disk is full (D21): paused until the user frees space and retries.
    NeedsSpace,
    Done,
    Failed,
    Cancelled,
}

impl ItemStatus {
    fn is_finished(self) -> bool {
        matches!(
            self,
            ItemStatus::Done | ItemStatus::Failed | ItemStatus::Cancelled
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureLite {
    pub rel_path: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryLite {
    pub downloaded: u64,
    pub skipped: u64,
    pub failed: u64,
    pub bytes: u64,
    pub elapsed_ms: u64,
    pub failures: Vec<FailureLite>,
}

impl From<&Summary> for SummaryLite {
    fn from(s: &Summary) -> Self {
        SummaryLite {
            downloaded: s.downloaded,
            skipped: s.skipped,
            failed: s.failed,
            bytes: s.bytes,
            elapsed_ms: u64::try_from(s.elapsed.as_millis()).unwrap_or(u64::MAX),
            failures: s
                .failures
                .iter()
                .map(|f| FailureLite {
                    rel_path: f.rel_path.to_string(),
                    error: f.error.clone(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueItem {
    pub id: u64,
    pub title: String,
    /// Human-readable "where from -> where to" line.
    pub path_label: String,
    pub host: String,
    pub spec: JobSpec,
    pub est_files: u64,
    pub est_bytes: u64,
    pub status: ItemStatus,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub summary: Option<SummaryLite>,
}

/// What the frontend receives for a queue item: everything except the explicit file list
/// (`JobSpec::files`, D20), which can be megabytes for a large selection. The frontend only needs
/// to know how many files there are.
#[derive(Debug, Clone, Serialize)]
pub struct QueueItemView {
    pub id: u64,
    pub title: String,
    pub path_label: String,
    pub host: String,
    pub spec: SpecView,
    pub est_files: u64,
    pub est_bytes: u64,
    pub status: ItemStatus,
    pub error: Option<String>,
    pub summary: Option<SummaryLite>,
}

/// `JobSpec` without `files`; `file_count` is the length of that list (absent when the job uses
/// globs).
#[derive(Debug, Clone, Serialize)]
pub struct SpecView {
    pub source: String,
    pub dest: PathBuf,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub existing: ExistingPolicy,
    pub prune: bool,
    pub file_count: Option<usize>,
}

impl QueueItem {
    pub fn view(&self) -> QueueItemView {
        QueueItemView {
            id: self.id,
            title: self.title.clone(),
            path_label: self.path_label.clone(),
            host: self.host.clone(),
            spec: SpecView {
                source: self.spec.source.clone(),
                dest: self.spec.dest.clone(),
                include: self.spec.include.clone(),
                exclude: self.spec.exclude.clone(),
                existing: self.spec.existing,
                prune: self.spec.prune,
                file_count: self.spec.files.as_ref().map(Vec::len),
            },
            est_files: self.est_files,
            est_bytes: self.est_bytes,
            status: self.status,
            error: self.error.clone(),
            summary: self.summary.clone(),
        }
    }
}

/// The status a restart would restore an item to: a job that was mid-run (or waiting for the
/// user to sign in or free space) goes back in line.
fn persisted_status(s: ItemStatus) -> ItemStatus {
    match s {
        ItemStatus::Running | ItemStatus::Preparing | ItemStatus::NeedsSignIn | ItemStatus::NeedsSpace => {
            ItemStatus::Queued
        }
        other => other,
    }
}

/// A fingerprint of what `queue.json` would say, without reading the file list (which never
/// changes after an item is added). Equal fingerprints mean writing the file again would change
/// nothing a restart can see, so the write is skipped: moves between preparing, running and the
/// two waiting-for-the-user states all restore as "queued".
fn persist_fingerprint(items: &[QueueItem]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for it in items {
        let mut v = it.view();
        v.status = persisted_status(v.status);
        // `view` leaves the list out; its length stands in for it.
        serde_json::to_string(&v).unwrap_or_default().hash(&mut h);
    }
    h.finish()
}

pub enum Ctl {
    Pause,
    Resume,
    Cancel,
    Replace(Session),
}

struct Current {
    id: u64,
    tx: mpsc::UnboundedSender<Ctl>,
}

#[derive(Default)]
struct Inner {
    items: Vec<QueueItem>,
    current: Option<Current>,
    next_id: u64,
}

pub struct Queue {
    inner: Mutex<Inner>,
    /// Held across snapshot, write, rename and event, so concurrent publishes cannot interleave
    /// or let an older snapshot overwrite a newer one. Always taken before `inner`, never after.
    /// Holds the fingerprint of what is on disk (see [`persist_fingerprint`]).
    persist: Mutex<Option<u64>>,
    wake: Notify,
    gate: watch::Sender<bool>,
}

impl Queue {
    pub fn new() -> Self {
        Queue {
            inner: Mutex::new(Inner {
                next_id: 1,
                ..Inner::default()
            }),
            persist: Mutex::new(None),
            wake: Notify::new(),
            gate: watch::channel(false).0,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Load the saved queue (falling back to the last good backup if the file is damaged). Jobs
    /// that were mid-run go back to "queued".
    pub fn restore(&self, path: &Path) {
        let Some(mut items) = load_queue(path) else {
            return;
        };
        for it in &mut items {
            it.status = persisted_status(it.status);
        }
        let mut g = self.lock();
        g.next_id = items.iter().map(|i| i.id).max().unwrap_or(0) + 1;
        g.items = items;
    }

    /// The queue as the frontend sees it (no file lists).
    pub fn snapshot(&self) -> Vec<QueueItemView> {
        self.lock().items.iter().map(QueueItem::view).collect()
    }

    /// Just the status (cheap: `item` copies the whole file list).
    pub fn status_of(&self, id: u64) -> Option<ItemStatus> {
        self.lock().items.iter().find(|i| i.id == id).map(|i| i.status)
    }

    /// Hosts that have work waiting (used by the launch-time sign-in check).
    pub fn pending_hosts(&self) -> Vec<String> {
        let g = self.lock();
        let mut hosts: Vec<String> = g
            .items
            .iter()
            .filter(|i| i.status == ItemStatus::Queued)
            .map(|i| i.host.clone())
            .collect();
        hosts.sort();
        hosts.dedup();
        hosts
    }

    /// Where to probe a host's session: the site of a queued job for it, else of any job for it
    /// (the link that was pasted is the only place the site is known). `None` when no job names
    /// the host; callers then fall back to the tenant root.
    pub fn site_hint(&self, host: &str) -> Option<Url> {
        let g = self.lock();
        let mut mine = g.items.iter().filter(|i| i.host == host);
        let pick = mine
            .clone()
            .find(|i| i.status == ItemStatus::Queued)
            .or_else(|| mine.next())?;
        link::parse(&pick.spec.source)
            .ok()
            .map(|p| crate::commands::start_url(&p))
    }

    pub fn has_active(&self) -> bool {
        let g = self.lock();
        g.current.is_some()
            || g.items.iter().any(|i| {
                matches!(
                    i.status,
                    ItemStatus::Queued | ItemStatus::Running | ItemStatus::Preparing
                )
            })
    }

    /// Let queued jobs start (called once the launch-time sign-in check is done).
    pub fn open_gate(&self) {
        self.gate.send_replace(true);
    }

    fn publish(&self, app: &AppHandle) {
        let mut saved = self.persist.lock().unwrap_or_else(|p| p.into_inner());
        let (views, to_write) = {
            let mut g = self.lock();
            // Keep only the most recent finished jobs.
            let finished: Vec<u64> = g
                .items
                .iter()
                .filter(|i| i.status.is_finished())
                .map(|i| i.id)
                .collect();
            if finished.len() > 20 {
                let drop_ids: Vec<u64> = finished[..finished.len() - 20].to_vec();
                g.items.retain(|i| !drop_ids.contains(&i.id));
            }
            let views: Vec<QueueItemView> = g.items.iter().map(QueueItem::view).collect();
            // Rewrite `queue.json` (which carries the whole file lists) only when what a restart
            // would see has changed, not for every move between running and waiting states.
            let fingerprint = persist_fingerprint(&g.items);
            let to_write = (*saved != Some(fingerprint)).then(|| (fingerprint, serialize_queue(&g.items)));
            (views, to_write)
        };
        if let Some((fingerprint, bytes)) = to_write {
            match bytes
                .map_err(std::io::Error::other)
                .and_then(|b| write_queue(&paths::queue_file(), &b))
            {
                Ok(()) => *saved = Some(fingerprint),
                Err(e) => tracing::warn!("could not save the download queue: {e}"),
            }
        }
        let _ = app.emit("queue-changed", &views);
    }

    fn set_status(&self, app: &AppHandle, id: u64, status: ItemStatus, error: Option<String>) {
        {
            let mut g = self.lock();
            if let Some(it) = g.items.iter_mut().find(|i| i.id == id) {
                it.status = status;
                it.error = error;
            }
        }
        self.publish(app);
    }

    pub fn add(&self, app: &AppHandle, mut item: QueueItem) -> u64 {
        let id = {
            let mut g = self.lock();
            let id = g.next_id;
            g.next_id += 1;
            item.id = id;
            item.status = ItemStatus::Queued;
            g.items.push(item);
            id
        };
        self.publish(app);
        self.wake.notify_one();
        id
    }

    /// Pause, resume and cancel decide and act under one lock, so they cannot interleave with the
    /// runner picking the job up: either the running job gets the control message, or a waiting
    /// job gets its new status before the runner can start it.
    pub fn pause(&self, app: &AppHandle, id: u64) {
        let changed = {
            let mut g = self.lock();
            if let Some(c) = g.current.as_ref().filter(|c| c.id == id) {
                let _ = c.tx.send(Ctl::Pause);
                false
            } else if let Some(it) = g
                .items
                .iter_mut()
                .find(|i| i.id == id && i.status == ItemStatus::Queued)
            {
                it.status = ItemStatus::Paused;
                it.error = None;
                true
            } else {
                false
            }
        };
        if changed {
            self.publish(app);
        }
    }

    pub fn resume(&self, app: &AppHandle, id: u64) {
        let changed = {
            let mut g = self.lock();
            if let Some(c) = g.current.as_ref().filter(|c| c.id == id) {
                let _ = c.tx.send(Ctl::Resume);
                false
            } else if let Some(it) = g
                .items
                .iter_mut()
                .find(|i| i.id == id && i.status == ItemStatus::Paused)
            {
                it.status = ItemStatus::Queued;
                it.error = None;
                true
            } else {
                false
            }
        };
        if changed {
            self.publish(app);
            self.wake.notify_one();
        }
    }

    pub fn cancel(&self, app: &AppHandle, id: u64) {
        let changed = {
            let mut g = self.lock();
            if let Some(c) = g.current.as_ref().filter(|c| c.id == id) {
                let _ = c.tx.send(Ctl::Cancel);
                false
            } else {
                let before = g.items.len();
                g.items
                    .retain(|i| !(i.id == id && matches!(i.status, ItemStatus::Queued | ItemStatus::Paused)));
                g.items.len() != before
            }
        };
        if changed {
            self.publish(app);
        }
    }

    /// Put a finished or failed job back in line (a rerun resumes and skips what is saved).
    pub fn retry(&self, app: &AppHandle, id: u64) {
        let ok = self.status_of(id).is_some_and(ItemStatus::is_finished);
        if ok {
            {
                let mut g = self.lock();
                if let Some(it) = g.items.iter_mut().find(|i| i.id == id) {
                    it.summary = None;
                }
            }
            self.set_status(app, id, ItemStatus::Queued, None);
            self.wake.notify_one();
        }
    }

    pub fn clear_finished(&self, app: &AppHandle) {
        self.lock().items.retain(|i| !i.status.is_finished());
        self.publish(app);
    }

    /// A new session arrived (the user signed in from the Sign-ins view or the banner).
    pub fn deliver_session(&self, session: &Session) {
        let g = self.lock();
        if let Some(c) = &g.current
            && g.items
                .iter()
                .any(|i| i.id == c.id && i.host == session.host && i.status == ItemStatus::NeedsSignIn)
        {
            let _ = c.tx.send(Ctl::Replace(session.clone()));
        }
    }

    /// Close behaviour "Pause": remember that the running job was paused, then stop.
    pub fn pause_for_exit(&self, app: &AppHandle, mark_paused: bool) {
        {
            let mut g = self.lock();
            for it in &mut g.items {
                if persisted_status(it.status) == ItemStatus::Queued && it.status != ItemStatus::Queued {
                    it.status = if mark_paused {
                        ItemStatus::Paused
                    } else {
                        ItemStatus::Queued
                    };
                }
            }
        }
        self.publish(app);
    }
}

impl Default for Queue {
    fn default() -> Self {
        Self::new()
    }
}

/// Writing and reading `queue.json`.
///
/// Writes go to `queue.json.tmp` and are renamed over the real file; the previous good file is
/// kept as `queue.json.bak`. A damaged file is never silently dropped: reading falls back to the
/// backup, and the damaged copy is kept as `queue.json.corrupt`.
fn backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
}

fn serialize_queue(items: &[QueueItem]) -> Result<Vec<u8>, serde_json::Error> {
    // Compact: a selection of 100k files makes this file megabytes, and nobody edits it by hand.
    serde_json::to_vec(items)
}

fn write_queue(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    if path.exists() {
        // The file on disk is the last thing this function (or `load_queue`) vouched for.
        if let Err(e) = std::fs::copy(path, backup_path(path)) {
            tracing::warn!("could not back up the download queue: {e}");
        }
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
fn save_queue(path: &Path, items: &[QueueItem]) -> std::io::Result<()> {
    write_queue(path, &serialize_queue(items).map_err(std::io::Error::other)?)
}

fn parse_queue(path: &Path) -> Result<Vec<QueueItem>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

fn load_queue(path: &Path) -> Option<Vec<QueueItem>> {
    if !path.exists() {
        return None;
    }
    match parse_queue(path) {
        Ok(items) => Some(items),
        Err(why) => {
            tracing::warn!("the download queue file is damaged ({why}); trying the backup");
            let bak = backup_path(path);
            let corrupt = path.with_extension("json.corrupt");
            if let Err(e) = std::fs::rename(path, &corrupt) {
                tracing::warn!("could not set the damaged queue file aside: {e}");
            }
            match parse_queue(&bak) {
                Ok(items) => {
                    tracing::warn!("restored the download queue from its backup");
                    // Put the good copy back so the next save backs up good data.
                    if let Err(e) = std::fs::copy(&bak, path) {
                        tracing::warn!("could not restore queue.json from the backup: {e}");
                    }
                    Some(items)
                }
                Err(e) => {
                    tracing::error!(
                        "the download queue could not be restored ({e}); the damaged file was kept as {}",
                        corrupt.display()
                    );
                    None
                }
            }
        }
    }
}

/// The text to show for a failed job.
fn failure_text(e: &Error) -> String {
    match e {
        Error::DestinationBusy { .. } => {
            "Another AmpSync download is already using this folder. Wait for it to finish, or save to a different folder.".into()
        }
        other => other.to_string(),
    }
}

/// A refresh that followed a replaced session this recently counts as "just renewed".
const REFRESH_LOOP_WINDOW: Duration = Duration::from_secs(30);

const REFUSED_MESSAGE: &str = "SharePoint refused some files even though your sign-in is valid. This is usually a permissions issue on those files. Progress is saved; use Retry to try again.";

#[derive(Serialize, Clone)]
struct JobEvent {
    job_id: u64,
    event: Event,
}

/// Start the single task that runs queued jobs one after another.
pub fn spawn_runner(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let mut gate = state.queue.gate.subscribe();
        let _ = gate.wait_for(|open| *open).await;
        loop {
            let next = state
                .queue
                .lock()
                .items
                .iter()
                .find(|i| i.status == ItemStatus::Queued)
                .map(|i| i.id);
            match next {
                Some(id) => run_item(&app, id).await,
                None => state.queue.wake.notified().await,
            }
        }
    });
}

enum Outcome {
    Proceed(Session),
    Stop,
}

async fn run_item(app: &AppHandle, id: u64) {
    let state = app.state::<AppState>();
    let queue = &state.queue;
    let (tx, mut rx) = mpsc::unbounded_channel::<Ctl>();
    // Check the status and take the job in one step: a Pause or Cancel that landed after the
    // runner chose this job but before now has already changed the status, and wins.
    let item = {
        let mut g = queue.lock();
        let Some(it) = g
            .items
            .iter_mut()
            .find(|i| i.id == id && i.status == ItemStatus::Queued)
        else {
            return;
        };
        it.status = ItemStatus::Preparing;
        it.error = None;
        let item = it.clone();
        g.current = Some(Current { id, tx: tx.clone() });
        item
    };
    queue.publish(app);

    let result = run_item_inner(app, &item, &tx, &mut rx).await;

    queue.lock().current = None;
    match result {
        Ok(Some(summary)) => {
            {
                let mut g = queue.lock();
                if let Some(it) = g.items.iter_mut().find(|i| i.id == id) {
                    it.summary = Some(SummaryLite::from(&summary));
                }
            }
            queue.set_status(app, id, ItemStatus::Done, None);
        }
        Ok(None) => {} // paused or cancelled: status already set
        Err(Error::Cancelled) => queue.set_status(app, id, ItemStatus::Cancelled, None),
        Err(e) => queue.set_status(app, id, ItemStatus::Failed, Some(failure_text(&e))),
    }
    crate::tray::update_tooltip(app, None);
}

/// `Ok(Some)` finished, `Ok(None)` stopped by the user with the status already recorded.
async fn run_item_inner(
    app: &AppHandle,
    item: &QueueItem,
    tx: &mpsc::UnboundedSender<Ctl>,
    rx: &mut mpsc::UnboundedReceiver<Ctl>,
) -> Result<Option<Summary>, Error> {
    let state = app.state::<AppState>();
    let queue = &state.queue;
    let id = item.id;
    let parsed: ParsedLink = link::parse(&item.spec.source)?;
    let start_url = crate::commands::start_url(&parsed);
    let site_url = crate::signin::site_url_of(&start_url);

    let mut force = false;
    let mut attempts = 0;
    loop {
        // 1. A working sign-in.
        let session = match acquire(app, rx, queue, id, &parsed.host, &start_url, force).await {
            Outcome::Proceed(s) => s,
            Outcome::Stop => return Ok(None),
        };
        // 2. Compare the remote tree with the destination.
        let settings = state.settings();
        let remote = Arc::new(ClassicRemote::new(&session));
        let engine = Engine::new(remote, settings);
        let plan = tokio::select! {
            p = engine.plan(&item.spec) => p,
            () = wait_stop(rx, queue, app, id) => { return Ok(None); }
        };
        let plan = match plan {
            Ok(p) => p,
            Err(Error::AuthExpired { .. }) if attempts < 2 => {
                attempts += 1;
                force = true;
                continue;
            }
            Err(e) => return Err(e),
        };
        // 3. Transfer.
        let handle = engine.start(item.spec.clone(), plan);
        let mut events = handle.events();
        queue.set_status(app, id, ItemStatus::Running, None);
        let mut refreshing = false;
        let mut cancelled = false;
        let mut refused = false;
        let mut current_session = session.clone();
        let mut renewed_at: Option<Instant> = None;
        loop {
            tokio::select! {
                ctl = rx.recv() => match ctl {
                    Some(Ctl::Pause) => { handle.pause(); }
                    Some(Ctl::Resume) => { handle.resume(); }
                    Some(Ctl::Replace(s)) => {
                        refreshing = false;
                        renewed_at = Some(Instant::now());
                        current_session = s.clone();
                        handle.replace_session(s);
                    }
                    Some(Ctl::Cancel) => { handle.cancel(); cancelled = true; break; }
                    None => break,
                },
                ev = events.recv() => match ev {
                    Ok(ev) => {
                        let _ = app.emit("job-event", JobEvent { job_id: id, event: ev.clone() });
                        match &ev {
                            Event::Progress { bytes_done, bytes_total, .. } => {
                                let pct = if *bytes_total > 0 { Some(*bytes_done as f64 / *bytes_total as f64) } else { None };
                                crate::tray::update_tooltip(app, pct);
                            }
                            Event::DiskFull { .. } => {
                                // `Paused` follows; the job waits for space, not for the user.
                                queue.set_status(app, id, ItemStatus::NeedsSpace, None);
                            }
                            Event::Paused => {
                                if queue.status_of(id).is_some_and(|s| !matches!(s, ItemStatus::NeedsSignIn | ItemStatus::NeedsSpace)) {
                                    queue.set_status(app, id, ItemStatus::Paused, None);
                                }
                            }
                            Event::Resumed => queue.set_status(app, id, ItemStatus::Running, None),
                            Event::AuthExpired { host } => {
                                // Loop guard: the session was swapped a moment ago and SharePoint
                                // accepts it, yet something still asked for sign-in. Another
                                // refresh would only hand back the same cookies, so stop here.
                                let just_renewed = renewed_at.is_some_and(|t| t.elapsed() < REFRESH_LOOP_WINDOW);
                                if just_renewed
                                    && matches!(
                                        ampsync_core::check_session(&current_session, &site_url).await,
                                        SessionStatus::Valid
                                    )
                                {
                                    refused = true;
                                    handle.cancel();
                                    break;
                                }
                                state.set_status(app, host, "expired");
                                queue.set_status(app, id, ItemStatus::NeedsSignIn, None);
                                if !refreshing {
                                    refreshing = true;
                                    let (app2, tx2, host2, url2) = (app.clone(), tx.clone(), host.clone(), start_url.clone());
                                    tauri::async_runtime::spawn(async move {
                                        let st = app2.state::<AppState>();
                                        if let Ok(s) = st.signin.obtain(&app2, &host2, &url2, true, true).await {
                                            let _ = tx2.send(Ctl::Replace(s));
                                        }
                                    });
                                }
                            }
                            Event::Finished(_) | Event::Failed { .. } | Event::Cancelled => break,
                            _ => {}
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        // Progress is cumulative and recovers by itself; per-file counters in the
                        // window are corrected from the summary when the job ends.
                        tracing::warn!("job {id}: the event stream fell behind and skipped {n} events");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
            }
        }
        if cancelled {
            state.signin.cancel(app);
        }
        if refused {
            let _ = handle.wait().await;
            return Err(Error::State(REFUSED_MESSAGE.into()));
        }
        return handle.wait().await.map(Some);
    }
}

/// Get a session for `host`, waiting for the user when sign-in is needed.
async fn acquire(
    app: &AppHandle,
    rx: &mut mpsc::UnboundedReceiver<Ctl>,
    queue: &Queue,
    id: u64,
    host: &str,
    start_url: &Url,
    force: bool,
) -> Outcome {
    let state = app.state::<AppState>();
    let mut force = force;
    loop {
        let attempt = state.signin.obtain(app, host, start_url, force, true);
        tokio::pin!(attempt);
        let res = tokio::select! {
            r = &mut attempt => r,
            () = wait_stop(rx, queue, app, id) => { state.signin.cancel(app); return Outcome::Stop; }
        };
        match res {
            Ok(s) => return Outcome::Proceed(s),
            Err(Error::Cancelled | Error::AuthExpired { .. }) => {
                // The window was closed without signing in: wait for the user to try again.
                queue.set_status(app, id, ItemStatus::NeedsSignIn, None);
                match rx.recv().await {
                    Some(Ctl::Replace(s)) => return Outcome::Proceed(s),
                    Some(Ctl::Resume) => force = true,
                    Some(Ctl::Pause) => {
                        queue.set_status(app, id, ItemStatus::Paused, None);
                        return Outcome::Stop;
                    }
                    Some(Ctl::Cancel) | None => {
                        remove_item(app, queue, id);
                        return Outcome::Stop;
                    }
                }
            }
            Err(e) => {
                queue.set_status(app, id, ItemStatus::Failed, Some(failure_text(&e)));
                return Outcome::Stop;
            }
        }
    }
}

fn remove_item(app: &AppHandle, queue: &Queue, id: u64) {
    queue.set_status(app, id, ItemStatus::Cancelled, None);
}

/// Resolves when the user pauses or cancels before the transfer has started. Sets the status.
async fn wait_stop(rx: &mut mpsc::UnboundedReceiver<Ctl>, queue: &Queue, app: &AppHandle, id: u64) {
    loop {
        match rx.recv().await {
            Some(Ctl::Pause) => {
                queue.set_status(app, id, ItemStatus::Paused, None);
                return;
            }
            Some(Ctl::Cancel) | None => {
                remove_item(app, queue, id);
                return;
            }
            Some(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("ampsync-queue-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn item(id: u64, title: &str) -> QueueItem {
        QueueItem {
            id,
            title: title.into(),
            path_label: String::new(),
            host: "contoso.sharepoint.com".into(),
            spec: JobSpec::new("https://contoso.sharepoint.com/sites/T/Docs/P", "dest"),
            est_files: 1,
            est_bytes: 1,
            status: ItemStatus::Queued,
            error: None,
            summary: None,
        }
    }

    #[test]
    fn save_then_load_round_trips_and_leaves_no_temp_file() {
        let dir = temp_dir("rt");
        let path = dir.join("queue.json");
        save_queue(&path, &[item(1, "one")]).unwrap();
        let loaded = load_queue(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].title, "one");
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn the_previous_good_file_is_kept_as_backup() {
        let dir = temp_dir("bak");
        let path = dir.join("queue.json");
        save_queue(&path, &[item(1, "first")]).unwrap();
        save_queue(&path, &[item(1, "first"), item(2, "second")]).unwrap();
        assert_eq!(parse_queue(&backup_path(&path)).unwrap().len(), 1);
        assert_eq!(parse_queue(&path).unwrap().len(), 2);
    }

    #[test]
    fn a_damaged_file_falls_back_to_the_backup_and_is_kept() {
        let dir = temp_dir("corrupt");
        let path = dir.join("queue.json");
        save_queue(&path, &[item(1, "first")]).unwrap();
        save_queue(&path, &[item(1, "first"), item(2, "second")]).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        let loaded = load_queue(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(path.with_extension("json.corrupt").exists());
        // The good copy is back in place, so the next save backs up good data.
        assert_eq!(parse_queue(&path).unwrap().len(), 1);
    }

    #[test]
    fn a_damaged_file_without_backup_is_set_aside_not_lost() {
        let dir = temp_dir("nobak");
        let path = dir.join("queue.json");
        std::fs::write(&path, "garbage").unwrap();
        assert!(load_queue(&path).is_none());
        assert_eq!(
            std::fs::read_to_string(path.with_extension("json.corrupt")).unwrap(),
            "garbage"
        );
    }

    #[test]
    fn a_missing_file_is_just_an_empty_queue() {
        let dir = temp_dir("none");
        assert!(load_queue(&dir.join("queue.json")).is_none());
    }

    #[test]
    fn concurrent_saves_never_leave_a_torn_file() {
        // Mirrors `publish`: one lock around snapshot + write.
        let dir = temp_dir("race");
        let path = Arc::new(dir.join("queue.json"));
        let lock = Arc::new(Mutex::new(0u64));
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let (path, lock) = (path.clone(), lock.clone());
                std::thread::spawn(move || {
                    for k in 0..25 {
                        let mut n = lock.lock().unwrap();
                        *n += 1;
                        let items: Vec<_> = (0..*n).map(|i| item(i, &format!("t{t}k{k}"))).collect();
                        save_queue(&path, &items).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(parse_queue(&path).unwrap().len(), 200);
    }

    fn item_with_files(id: u64, n: usize) -> QueueItem {
        let mut it = item(id, "big");
        it.spec.files = Some(
            (0..n)
                .map(|i| ampsync_core::plan::RelPath::new(format!("folder/sub/file-{i}.bin")).unwrap())
                .collect(),
        );
        it
    }

    #[test]
    fn the_frontend_view_leaves_the_file_list_out_but_counts_it() {
        let view = item_with_files(1, 5000).view();
        let json = serde_json::to_value(&view).unwrap();
        assert!(
            json["spec"].get("files").is_none(),
            "the list must not reach the frontend"
        );
        assert_eq!(json["spec"]["file_count"], 5000);
        assert!(serde_json::to_string(&view).unwrap().len() < 2_000);
        // A glob-based job has no list and no count.
        assert!(serde_json::to_value(item(2, "globs").view()).unwrap()["spec"]["file_count"].is_null());
    }

    #[test]
    fn queue_json_still_keeps_the_file_list_for_resuming_after_a_restart() {
        let dir = temp_dir("files");
        let path = dir.join("queue.json");
        save_queue(&path, &[item_with_files(1, 300)]).unwrap();
        let loaded = load_queue(&path).unwrap();
        assert_eq!(loaded[0].spec.files.as_ref().map(Vec::len), Some(300));
        assert_eq!(
            loaded[0].spec.files.as_ref().unwrap()[299].as_str(),
            "folder/sub/file-299.bin"
        );
    }

    #[test]
    fn progress_and_waiting_state_changes_do_not_rewrite_queue_json() {
        let mut items = vec![item_with_files(1, 100), item(2, "next")];
        let queued = persist_fingerprint(&items);
        // Queued -> preparing -> running -> waiting for sign-in or space: a restart restores
        // all of them as "queued", so none of them is worth writing again.
        for s in [
            ItemStatus::Preparing,
            ItemStatus::Running,
            ItemStatus::NeedsSignIn,
            ItemStatus::NeedsSpace,
        ] {
            items[0].status = s;
            assert_eq!(persist_fingerprint(&items), queued, "{s:?}");
        }
        // These are visible after a restart, so they are written.
        items[0].status = ItemStatus::Paused;
        let paused = persist_fingerprint(&items);
        assert_ne!(paused, queued);
        items[0].status = ItemStatus::Done;
        assert_ne!(persist_fingerprint(&items), paused);
        items[0].status = ItemStatus::Queued;
        items[1].title = "renamed".into();
        assert_ne!(persist_fingerprint(&items), queued);
    }

    #[test]
    fn jobs_waiting_for_space_come_back_queued_after_a_restart() {
        let dir = temp_dir("space");
        let path = dir.join("queue.json");
        let mut a = item(1, "full disk");
        a.status = ItemStatus::NeedsSpace;
        save_queue(&path, &[a]).unwrap();
        let q = Queue::new();
        q.restore(&path);
        assert_eq!(q.snapshot()[0].status, ItemStatus::Queued);
    }

    #[test]
    fn the_new_status_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&ItemStatus::NeedsSpace).unwrap(),
            "\"needs_space\""
        );
    }

    #[test]
    fn destination_busy_has_a_friendly_text() {
        let e = Error::DestinationBusy {
            path: PathBuf::from("x"),
        };
        assert!(failure_text(&e).contains("already using this folder"));
    }
}
