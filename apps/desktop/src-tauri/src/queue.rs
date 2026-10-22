//! The download queue: jobs run one at a time (D16 in the UI), survive restarts, and wait for a
//! working sign-in before they start or resume.

use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

use ampsync_core::Error;
use ampsync_core::auth::Session;
use ampsync_core::engine::{Engine, Event, Summary};
use ampsync_core::link::{self, ParsedLink};
use ampsync_core::plan::JobSpec;
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
            wake: Notify::new(),
            gate: watch::channel(false).0,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Load the saved queue. Jobs that were mid-run go back to "queued".
    pub fn restore(&self, path: &Path) {
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        let Ok(mut items) = serde_json::from_str::<Vec<QueueItem>>(&text) else {
            return;
        };
        for it in &mut items {
            if matches!(
                it.status,
                ItemStatus::Running | ItemStatus::Preparing | ItemStatus::NeedsSignIn
            ) {
                it.status = ItemStatus::Queued;
            }
        }
        let mut g = self.lock();
        g.next_id = items.iter().map(|i| i.id).max().unwrap_or(0) + 1;
        g.items = items;
    }

    pub fn snapshot(&self) -> Vec<QueueItem> {
        self.lock().items.clone()
    }

    pub fn item(&self, id: u64) -> Option<QueueItem> {
        self.lock().items.iter().find(|i| i.id == id).cloned()
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
        let snap = {
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
            g.items.clone()
        };
        if let Ok(text) = serde_json::to_string_pretty(&snap) {
            let path = paths::queue_file();
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
        let _ = app.emit("queue-changed", &snap);
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

    fn send_ctl(&self, id: u64, ctl: Ctl) -> bool {
        let g = self.lock();
        match &g.current {
            Some(c) if c.id == id => c.tx.send(ctl).is_ok(),
            _ => false,
        }
    }

    pub fn pause(&self, app: &AppHandle, id: u64) {
        if self.send_ctl(id, Ctl::Pause) {
            return;
        }
        let queued = self.item(id).is_some_and(|i| i.status == ItemStatus::Queued);
        if queued {
            self.set_status(app, id, ItemStatus::Paused, None);
        }
    }

    pub fn resume(&self, app: &AppHandle, id: u64) {
        if self.send_ctl(id, Ctl::Resume) {
            return;
        }
        let paused = self.item(id).is_some_and(|i| i.status == ItemStatus::Paused);
        if paused {
            self.set_status(app, id, ItemStatus::Queued, None);
            self.wake.notify_one();
        }
    }

    pub fn cancel(&self, app: &AppHandle, id: u64) {
        if self.send_ctl(id, Ctl::Cancel) {
            return;
        }
        let waiting = self
            .item(id)
            .is_some_and(|i| matches!(i.status, ItemStatus::Queued | ItemStatus::Paused));
        if waiting {
            {
                let mut g = self.lock();
                g.items.retain(|i| i.id != id);
            }
            self.publish(app);
        }
    }

    /// Put a finished or failed job back in line (a rerun resumes and skips what is saved).
    pub fn retry(&self, app: &AppHandle, id: u64) {
        let ok = self.item(id).is_some_and(|i| i.status.is_finished());
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
                if matches!(
                    it.status,
                    ItemStatus::Running | ItemStatus::Preparing | ItemStatus::NeedsSignIn
                ) {
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

fn root_url(host: &str) -> Url {
    Url::parse(&format!("https://{host}/")).expect("host came from a parsed link")
}

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
    let Some(item) = queue.item(id) else { return };
    let (tx, mut rx) = mpsc::unbounded_channel::<Ctl>();
    queue.lock().current = Some(Current { id, tx: tx.clone() });
    queue.set_status(app, id, ItemStatus::Preparing, None);

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
        Err(e) => queue.set_status(app, id, ItemStatus::Failed, Some(e.to_string())),
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
    let start_url = root_url(&parsed.host);

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
        loop {
            tokio::select! {
                ctl = rx.recv() => match ctl {
                    Some(Ctl::Pause) => { handle.pause(); }
                    Some(Ctl::Resume) => { handle.resume(); }
                    Some(Ctl::Replace(s)) => {
                        refreshing = false;
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
                            Event::Paused => {
                                if queue.item(id).is_some_and(|i| i.status != ItemStatus::NeedsSignIn) {
                                    queue.set_status(app, id, ItemStatus::Paused, None);
                                }
                            }
                            Event::Resumed => queue.set_status(app, id, ItemStatus::Running, None),
                            Event::AuthExpired { host } => {
                                state.set_status(app, host, "expired");
                                queue.set_status(app, id, ItemStatus::NeedsSignIn, None);
                                if !refreshing {
                                    refreshing = true;
                                    let (app2, tx2, host2) = (app.clone(), tx.clone(), host.clone());
                                    tauri::async_runtime::spawn(async move {
                                        let st = app2.state::<AppState>();
                                        if let Ok(s) = st.signin.obtain(&app2, &host2, &root_url(&host2), true, true).await {
                                            let _ = tx2.send(Ctl::Replace(s));
                                        }
                                    });
                                }
                            }
                            Event::Finished(_) | Event::Failed { .. } | Event::Cancelled => break,
                            _ => {}
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
            }
        }
        if cancelled {
            state.signin.cancel(app);
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
                queue.set_status(app, id, ItemStatus::Failed, Some(e.to_string()));
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
