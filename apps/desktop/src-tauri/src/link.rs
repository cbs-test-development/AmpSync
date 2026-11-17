//! Browser link, app side (see `docs/plans/browser-link.md`).
//!
//! The AmpSync extension in the user's own Edge or Chrome profile talks to a relay process
//! (`relay.rs`, the same exe in host mode); the relay talks to this server over a per-user named
//! pipe (Windows) or unix socket (Linux). This module owns the server, the connection state
//! shown in the UI, request/response matching for `refresh` and `ping`, and the small stats file
//! behind the "link your browser" nudge.
//!
//! Cookie values pass through here: nothing in this file logs a session or a message.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use ampsync_core::Error;
use ampsync_core::auth::{BrowserKind, Session};
use ampsync_core::browser_link::{
    self, ErrorCode, FromExtension, MAX_MESSAGE, PROTOCOL_VERSION, ToExtension, validate_session,
};
use ampsync_core::settings::Settings;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, Url};
use time::OffsetDateTime;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::AppState;

/// How long a silent refresh may take. The extension itself gives up after about 45 s of waiting
/// (plus a quick probe), so the app waits a little longer to hear its answer instead of racing it.
const SILENT_TIMEOUT: Duration = Duration::from_secs(60);
/// An interactive refresh: the extension waits up to 3 minutes for the user.
const INTERACTIVE_TIMEOUT: Duration = Duration::from_secs(200);
const PING_TIMEOUT: Duration = Duration::from_secs(5);
/// Background renewal cadence while a job runs.
pub const RENEW_EVERY: Duration = Duration::from_secs(30 * 60);

// ---------------------------------------------------------------- stats (signin-stats.json)

const KEEP_SECS: i64 = 30 * 24 * 3600;
const WEEK_SECS: i64 = 7 * 24 * 3600;
const NUDGE_QUIET_SECS: i64 = 14 * 24 * 3600;

/// Interactive sign-in prompts and sessions received from the link, as Unix seconds, last 30 days.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Stats {
    pub prompts: Vec<i64>,
    pub renewals: Vec<i64>,
}

impl Stats {
    /// A missing or damaged file is just "no history".
    pub fn load(path: &std::path::Path) -> Stats {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(&tmp, path)
    }

    /// Drop entries older than 30 days.
    pub fn prune(&mut self, now: i64) {
        self.prompts.retain(|t| *t > now - KEEP_SECS);
        self.renewals.retain(|t| *t > now - KEEP_SECS);
    }

    pub fn prompts_7d(&self, now: i64) -> usize {
        self.prompts.iter().filter(|t| **t > now - WEEK_SECS).count()
    }

    pub fn renewals_7d(&self, now: i64) -> usize {
        self.renewals.iter().filter(|t| **t > now - WEEK_SECS).count()
    }

    pub fn last_renewal(&self) -> Option<i64> {
        self.renewals.iter().copied().max()
    }
}

/// The nudge under "Sign-in expired": the link is not set up, the person was asked to sign in at
/// least twice this week, and has not said "not now" in the last 14 days.
pub fn nudge(enabled: bool, prompts_7d: usize, dismissed_at: Option<OffsetDateTime>, now: OffsetDateTime) -> bool {
    !enabled
        && prompts_7d >= 2
        && dismissed_at.is_none_or(|d| now.unix_timestamp() - d.unix_timestamp() >= NUDGE_QUIET_SECS)
}

// ---------------------------------------------------------------- status for the frontend

/// Exactly the `LinkStatus` of the frontend contract.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LinkStatus {
    pub enabled: bool,
    pub connected: bool,
    pub browser: Option<BrowserKind>,
    pub ext_version: Option<String>,
    pub last_renewed: Option<String>,
    pub renewals_7d: usize,
    pub prompts_7d: usize,
    pub nudge: bool,
    pub folder: String,
}

/// What the extension said about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub browser: BrowserKind,
    pub ext_version: String,
}

pub fn build_status(
    settings: &Settings,
    hello: Option<&Hello>,
    stats: &Stats,
    now: OffsetDateTime,
    folder: String,
) -> LinkStatus {
    let t = now.unix_timestamp();
    let prompts_7d = stats.prompts_7d(t);
    LinkStatus {
        enabled: settings.browser_link,
        connected: hello.is_some(),
        browser: hello.map(|h| h.browser),
        ext_version: hello.map(|h| h.ext_version.clone()),
        last_renewed: stats
            .last_renewal()
            .and_then(|s| OffsetDateTime::from_unix_timestamp(s).ok())
            .and_then(|d| d.format(&time::format_description::well_known::Rfc3339).ok()),
        renewals_7d: stats.renewals_7d(t),
        prompts_7d,
        nudge: nudge(settings.browser_link, prompts_7d, settings.link_nudge_dismissed_at, now),
        folder,
    }
}

// ---------------------------------------------------------------- connection state

struct ConnEntry {
    id: u64,
    tx: mpsc::UnboundedSender<Vec<u8>>,
    hello: Option<Hello>,
}

struct Pending {
    conn: u64,
    host: String,
    /// `Ok(Some(session))` for a refresh, `Ok(None)` for a pong.
    tx: oneshot::Sender<Result<Option<Session>, Error>>,
}

struct Inner {
    conns: Vec<ConnEntry>,
    next_conn: u64,
    next_request: u64,
    pending: HashMap<String, Pending>,
    stats: Stats,
}

/// The state of the link: attached relays, requests waiting for an answer, and the stats.
pub struct Link {
    inner: Mutex<Inner>,
    stats_path: PathBuf,
}

/// Removes a waiting request when the caller stops waiting (timeout, or the future is dropped by
/// Pause or Cancel).
struct PendingGuard<'a> {
    link: &'a Link,
    id: String,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.link.lock().pending.remove(&self.id);
    }
}

impl Link {
    pub fn new(data_dir: PathBuf) -> Link {
        let stats_path = browser_link::signin_stats_path(&data_dir);
        let mut stats = Stats::load(&stats_path);
        stats.prune(OffsetDateTime::now_utc().unix_timestamp());
        Link {
            inner: Mutex::new(Inner {
                conns: Vec::new(),
                next_conn: 1,
                next_request: 1,
                pending: HashMap::new(),
                stats,
            }),
            stats_path,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The extension that answers requests: the most recently attached one that said hello.
    pub fn hello(&self) -> Option<Hello> {
        self.lock().conns.iter().rev().find_map(|c| c.hello.clone())
    }

    pub fn is_connected(&self) -> bool {
        self.hello().is_some()
    }

    /// Wait for an extension to attach (the relay retries the pipe every 5 s, so after a launch
    /// the first hello can take a few seconds).
    pub async fn wait_connected(&self, max: Duration) -> bool {
        let began = std::time::Instant::now();
        loop {
            if self.is_connected() {
                return true;
            }
            if began.elapsed() >= max {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    fn add_conn(&self, tx: mpsc::UnboundedSender<Vec<u8>>) -> u64 {
        let mut g = self.lock();
        let id = g.next_conn;
        g.next_conn += 1;
        g.conns.push(ConnEntry { id, tx, hello: None });
        id
    }

    fn remove_conn(&self, id: u64) {
        let mut g = self.lock();
        g.conns.retain(|c| c.id != id);
        // Requests sent through this connection will never be answered; dropping the sender wakes
        // their callers with an error.
        g.pending.retain(|_, p| p.conn != id);
    }

    fn set_hello(&self, id: u64, hello: Hello) {
        if let Some(c) = self.lock().conns.iter_mut().find(|c| c.id == id) {
            c.hello = Some(hello);
        }
    }

    fn browser_of(&self, id: u64) -> Option<BrowserKind> {
        self.lock()
            .conns
            .iter()
            .find(|c| c.id == id)
            .and_then(|c| c.hello.as_ref().map(|h| h.browser))
    }

    fn resolve(&self, id: &str, result: Result<Option<Session>, Error>) {
        if let Some(p) = self.lock().pending.remove(id) {
            let _ = p.tx.send(result);
        }
    }

    fn pending_host(&self, id: &str) -> Option<String> {
        self.lock().pending.get(id).map(|p| p.host.clone())
    }

    fn has_pending(&self, id: &str) -> bool {
        self.lock().pending.contains_key(id)
    }

    /// Record an interactive sign-in prompt (the chooser shown, or a window or browser opened).
    pub fn record_prompt(&self) {
        self.push_stat(|s, now| s.prompts.push(now));
    }

    /// Record a session received from the link.
    fn record_renewal(&self) {
        self.push_stat(|s, now| s.renewals.push(now));
    }

    fn push_stat(&self, add: impl FnOnce(&mut Stats, i64)) {
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let mut g = self.lock();
        add(&mut g.stats, now);
        g.stats.prune(now);
        if let Err(e) = g.stats.save(&self.stats_path) {
            tracing::warn!("could not save signin-stats.json: {e}");
        }
    }

    pub fn status(&self, settings: &Settings) -> LinkStatus {
        let hello = self.hello();
        let g = self.lock();
        build_status(
            settings,
            hello.as_ref(),
            &g.stats,
            OffsetDateTime::now_utc(),
            crate::link_setup::extension_folder().to_string_lossy().to_string(),
        )
    }

    /// Send one request to the extension and wait for the matching answer.
    async fn round_trip(
        &self,
        prefix: char,
        host: &str,
        build: impl FnOnce(String) -> ToExtension,
        timeout: Duration,
    ) -> Result<Option<Session>, Error> {
        let (rx, id) = {
            let mut g = self.lock();
            let Some((conn, tx)) = g
                .conns
                .iter()
                .rev()
                .find(|c| c.hello.is_some())
                .map(|c| (c.id, c.tx.clone()))
            else {
                return Err(Error::State("The browser link is not connected.".into()));
            };
            let id = format!("{prefix}{}", g.next_request);
            g.next_request += 1;
            let line = browser_link::encode_line(&build(id.clone())).map_err(|e| Error::State(e.to_string()))?;
            let (otx, orx) = oneshot::channel();
            g.pending.insert(
                id.clone(),
                Pending {
                    conn,
                    host: host.to_string(),
                    tx: otx,
                },
            );
            if tx.send(line).is_err() {
                g.pending.remove(&id);
                return Err(Error::State("The browser link was disconnected.".into()));
            }
            (orx, id)
        };
        // Created after the lock is released (its Drop takes the lock).
        let _guard = PendingGuard { link: self, id };
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(Error::State("The browser link was disconnected.".into())),
            Err(_) => Err(Error::AuthExpired { host: host.to_string() }),
        }
    }

    /// Ask the extension for the browser's current session for `host`. Silent: no tab the user has
    /// to look at, unless the browser needs a moment to sign in. `interactive`: leave a tab open so
    /// the user can finish signing in (up to 200 s).
    pub async fn request_refresh(&self, host: &str, start_url: &Url, interactive: bool) -> Result<Session, Error> {
        let timeout = if interactive { INTERACTIVE_TIMEOUT } else { SILENT_TIMEOUT };
        let reply = self
            .round_trip(
                'r',
                host,
                |id| ToExtension::Refresh {
                    id,
                    host: host.to_string(),
                    start_url: start_url.to_string(),
                    interactive,
                },
                timeout,
            )
            .await?;
        reply.ok_or_else(|| Error::State("The browser link gave an unexpected answer.".into()))
    }

    /// Round trip to the extension: `true` when it answers within 5 s.
    pub async fn ping(&self) -> bool {
        self.round_trip('p', "", |id| ToExtension::Ping { id }, PING_TIMEOUT)
            .await
            .is_ok()
    }
}

// ---------------------------------------------------------------- settings and events

/// Change and save the settings, keeping the in-memory copy in step.
pub fn update_settings(app: &AppHandle, change: impl FnOnce(&mut Settings)) -> Result<(), Error> {
    let state = app.state::<AppState>();
    let mut s = state.settings();
    change(&mut s);
    s.save()?;
    *state.settings.write().unwrap_or_else(|p| p.into_inner()) = s;
    Ok(())
}

pub fn status(app: &AppHandle) -> LinkStatus {
    let state = app.state::<AppState>();
    state.link.status(&state.settings())
}

/// Tell the frontend the link changed (`link-state`, payload `LinkStatus`).
pub fn emit_state(app: &AppHandle) {
    let _ = app.emit("link-state", status(app));
}

// ---------------------------------------------------------------- handling messages

fn error_for(code: ErrorCode, host: &str) -> Error {
    match code {
        ErrorCode::SigninRequired | ErrorCode::Timeout => Error::AuthExpired { host: host.to_string() },
        ErrorCode::NoPermission => Error::State(format!(
            "The AmpSync browser extension is not allowed to read {host}. Allow it on SharePoint sites in the browser's extension settings."
        )),
        ErrorCode::BadRequest | ErrorCode::Other => {
            Error::State("The browser extension could not handle the request.".into())
        }
    }
}

/// A session reached us (asked for, or pushed): save it and treat it like a finished sign-in.
fn adopt_session(app: &AppHandle, session: &Session) {
    let state = app.state::<AppState>();
    state.link.record_renewal();
    state.set_status(app, &session.host, "active");
    // A job waiting for sign-in resumes; a running job swaps to the fresh cookies.
    state.queue.deliver_session(session);
    emit_state(app);
}

/// A push nobody asked for is only kept for a tenant AmpSync already knows (a saved session or a
/// queued job). Otherwise browsing any other SharePoint tenant would store its sign-in here.
fn wanted_push(app: &AppHandle, host: &str) -> bool {
    let state = app.state::<AppState>();
    state.store.load(host).ok().flatten().is_some() || state.queue.site_hint(host).is_some()
}

async fn handle_message(app: &AppHandle, conn: u64, msg: FromExtension) {
    let state = app.state::<AppState>();
    match msg {
        FromExtension::Hello {
            ext_version,
            browser,
            protocol,
        } => {
            if protocol > PROTOCOL_VERSION {
                // A newer extension than this app understands: stay disconnected rather than guess.
                tracing::warn!("the browser extension speaks protocol {protocol}; update AmpSync");
                return;
            }
            state.link.set_hello(conn, Hello { browser, ext_version });
            if !state.settings().browser_link
                && let Err(e) = update_settings(app, |s| s.browser_link = true)
            {
                tracing::warn!("could not save the browser link setting: {e}");
            }
            emit_state(app);
            // Sessions that expired while no extension was attached can be renewed now.
            let app2 = app.clone();
            tauri::async_runtime::spawn(async move { renew_expired(&app2).await });
        }
        FromExtension::Session { id, host, cookies, .. } => {
            let Some(kind) = state.link.browser_of(conn) else {
                return; // no hello yet
            };
            let asked = id.as_deref().is_some_and(|i| state.link.has_pending(i));
            if !asked && !wanted_push(app, &host.to_ascii_lowercase()) {
                return;
            }
            match validate_session(&host, &cookies, kind, OffsetDateTime::now_utc()) {
                Ok(session) => match state.store.save(&session) {
                    Ok(()) => {
                        adopt_session(app, &session);
                        if let Some(id) = id {
                            state.link.resolve(&id, Ok(Some(session)));
                        }
                    }
                    Err(e) => {
                        tracing::warn!("could not save a session from the browser link: {e}");
                        if let Some(id) = id {
                            state.link.resolve(&id, Err(e));
                        }
                    }
                },
                Err(e) => {
                    tracing::info!("ignored a session from the browser link: {e}");
                    if let Some(id) = id {
                        state.link.resolve(&id, Err(Error::AuthExpired { host }));
                    }
                }
            }
        }
        FromExtension::Error { id, host, code } => {
            if let Some(id) = id {
                let host = host.or_else(|| state.link.pending_host(&id)).unwrap_or_default();
                state.link.resolve(&id, Err(error_for(code, &host)));
            }
        }
        FromExtension::Pong { id } => state.link.resolve(&id, Ok(None)),
        FromExtension::Unknown => {}
    }
}

/// Where to probe or sign in for `host` when only the tenant is known.
pub fn start_url_for(app: &AppHandle, host: &str) -> Option<Url> {
    app.state::<AppState>()
        .queue
        .site_hint(host)
        .or_else(|| Url::parse(&format!("https://{host}/")).ok())
}

/// Silent refresh of every saved session marked expired; a success is adopted by the message
/// handler.
async fn renew_expired(app: &AppHandle) {
    let state = app.state::<AppState>();
    for host in state.store.hosts().unwrap_or_default() {
        if state.status_of(&host) != "expired" {
            continue;
        }
        if let Some(url) = start_url_for(app, &host) {
            let _ = state.link.request_refresh(&host, &url, false).await;
        }
    }
}

// ---------------------------------------------------------------- background renewal

/// While a job runs for `host`, ask the browser for fresh cookies every 30 minutes and swap them
/// into the job. Cancel-safe: the loop lives in the returned future; drop it to stop.
pub async fn renew_while_running(app: AppHandle, host: String, start_url: Url) {
    loop {
        tokio::time::sleep(RENEW_EVERY).await;
        let state = app.state::<AppState>();
        if !state.settings().renew_in_background || !state.link.is_connected() {
            continue;
        }
        // On success the message handler saves the session and hands it to the running job.
        if let Err(e) = state.link.request_refresh(&host, &start_url, false).await {
            tracing::info!("background renewal through the browser link did not work: {e}");
        }
    }
}

// ---------------------------------------------------------------- the server

async fn serve_conn<S>(app: AppHandle, stream: S)
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (rd, mut wr) = tokio::io::split(stream);
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let id = app.state::<AppState>().link.add_conn(tx);
    let writer = tauri::async_runtime::spawn(async move {
        while let Some(line) = rx.recv().await {
            if wr.write_all(&line).await.is_err() || wr.flush().await.is_err() {
                break;
            }
        }
    });
    let mut rd = BufReader::new(rd);
    loop {
        let mut buf = Vec::new();
        match (&mut rd).take(MAX_MESSAGE as u64 + 1).read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        // No newline: the stream ended mid-line, or the line is over the limit. Either way, hang up.
        if buf.pop() != Some(b'\n') {
            break;
        }
        if let Ok(msg) = serde_json::from_slice::<FromExtension>(&buf) {
            handle_message(&app, id, msg).await;
        }
    }
    writer.abort();
    app.state::<AppState>().link.remove_conn(id);
    emit_state(&app);
}

#[cfg(windows)]
fn create_pipe(
    name: &str,
    first: bool,
) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    use tokio::net::windows::named_pipe::ServerOptions;
    let sid = crate::link_ipc::current_user_sid()?;
    // Only this user may open the pipe; remote clients are refused outright.
    let mut security = crate::link_ipc::PipeSecurity::only_for(&sid)?;
    let mut options = ServerOptions::new();
    // The first instance must be ours: if another process already owns the name, creating it fails
    // instead of silently joining a pipe someone else controls.
    options.first_pipe_instance(first).reject_remote_clients(true);
    // SAFETY: the pointer refers to a live SECURITY_ATTRIBUTES with a valid descriptor; the system
    // copies the descriptor during the call.
    unsafe { options.create_with_security_attributes_raw(name, security.as_ptr()) }
}

#[cfg(windows)]
async fn serve(app: AppHandle) {
    let name = match crate::link_ipc::pipe_name() {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!("browser link is off: cannot name its pipe: {e}");
            return;
        }
    };
    let mut server = match create_pipe(&name, true) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("browser link is off: cannot create its pipe: {e}");
            return;
        }
    };
    loop {
        if let Err(e) = server.connect().await {
            tracing::warn!("browser link pipe: {e}");
            tokio::time::sleep(Duration::from_millis(500)).await;
            server = loop {
                match create_pipe(&name, false) {
                    Ok(s) => break s,
                    Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
                }
            };
            continue;
        }
        let connected = server;
        // The next instance exists before this client is served, so the name never goes away.
        server = loop {
            match create_pipe(&name, false) {
                Ok(s) => break s,
                Err(e) => {
                    tracing::warn!("browser link pipe: {e}");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        };
        tauri::async_runtime::spawn(serve_conn(app.clone(), connected));
    }
}

#[cfg(unix)]
async fn serve(app: AppHandle) {
    use std::os::unix::fs::PermissionsExt;
    use tokio::net::{UnixListener, UnixStream};
    let path = crate::link_ipc::socket_path();
    if path.exists() {
        // A socket nobody listens on is left over from a crash; a live one means another AmpSync.
        if UnixStream::connect(&path).await.is_ok() {
            tracing::warn!("browser link is off: another AmpSync owns its socket");
            return;
        }
        let _ = std::fs::remove_file(&path);
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("browser link is off: cannot create its socket: {e}");
            return;
        }
    };
    if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
        tracing::warn!("browser link is off: cannot restrict its socket: {e}");
        let _ = std::fs::remove_file(&path);
        return;
    }
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tauri::async_runtime::spawn(serve_conn(app.clone(), stream));
            }
            Err(e) => {
                tracing::warn!("browser link socket: {e}");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

/// Start the server. Always on (even before the link is set up): the first `hello` from a freshly
/// installed extension is what turns `settings.browser_link` on.
pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(serve(app));
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Duration as TD;

    fn now() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + TD::days(20000)
    }

    #[test]
    fn nudge_rule() {
        let n = now();
        // Not linked, two prompts this week, never dismissed.
        assert!(nudge(false, 2, None, n));
        assert!(!nudge(false, 1, None, n));
        // Already linked.
        assert!(!nudge(true, 5, None, n));
        // Dismissed 13 days ago: still quiet; 14 days ago: back.
        assert!(!nudge(false, 3, Some(n - TD::days(13)), n));
        assert!(nudge(false, 3, Some(n - TD::days(14)), n));
    }

    #[test]
    fn stats_prune_and_count() {
        let t = now().unix_timestamp();
        let day = 24 * 3600;
        let mut s = Stats {
            prompts: vec![t - 31 * day, t - 8 * day, t - 6 * day, t - 1],
            renewals: vec![t - 40 * day, t - 2 * day, t - 3600],
        };
        s.prune(t);
        assert_eq!(s.prompts.len(), 3);
        assert_eq!(s.renewals.len(), 2);
        assert_eq!(s.prompts_7d(t), 2);
        assert_eq!(s.renewals_7d(t), 2);
        assert_eq!(s.last_renewal(), Some(t - 3600));
    }

    #[test]
    fn stats_round_trip_and_tolerate_damage() {
        let dir = std::env::temp_dir().join(format!("ampsync-linkstats-{}", std::process::id()));
        let path = dir.join("signin-stats.json");
        let s = Stats {
            prompts: vec![1, 2],
            renewals: vec![3],
        };
        s.save(&path).unwrap();
        assert_eq!(Stats::load(&path), s);
        std::fs::write(&path, "{ nope").unwrap();
        assert_eq!(Stats::load(&path), Stats::default());
        assert_eq!(Stats::load(&dir.join("missing.json")), Stats::default());
        // A file without some keys still loads.
        std::fs::write(&path, r#"{"prompts":[5]}"#).unwrap();
        assert_eq!(Stats::load(&path).prompts, vec![5]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_matches_the_contract() {
        let t = now().unix_timestamp();
        let stats = Stats {
            prompts: vec![t - 10, t - 20],
            renewals: vec![t - 100],
        };
        let hello = Hello {
            browser: BrowserKind::Edge,
            ext_version: "1.0.0".into(),
        };
        let off = build_status(&Settings::default(), None, &stats, now(), "C:/x".into());
        assert!(!off.enabled && !off.connected && off.browser.is_none() && off.ext_version.is_none());
        assert!(off.nudge);
        assert_eq!((off.prompts_7d, off.renewals_7d), (2, 1));
        assert!(off.last_renewed.is_some());

        let on = Settings {
            browser_link: true,
            ..Settings::default()
        };
        let linked = build_status(&on, Some(&hello), &stats, now(), "C:/x".into());
        assert!(linked.enabled && linked.connected && !linked.nudge);
        let json = serde_json::to_value(&linked).unwrap();
        assert_eq!(json["browser"], "edge");
        assert_eq!(json["ext_version"], "1.0.0");
        assert_eq!(json["folder"], "C:/x");
        assert_eq!(json["renewals_7d"], 1);
        for key in [
            "enabled",
            "connected",
            "browser",
            "ext_version",
            "last_renewed",
            "renewals_7d",
            "prompts_7d",
            "nudge",
            "folder",
        ] {
            assert!(json.get(key).is_some(), "{key}");
        }
    }

    #[tokio::test]
    async fn requests_need_a_connection_and_match_their_answers() {
        let link = Link::new(std::env::temp_dir().join(format!("ampsync-linkreq-{}", std::process::id())));
        assert!(!link.is_connected());
        let url = Url::parse("https://contoso.sharepoint.com/").unwrap();
        assert!(link.request_refresh("contoso.sharepoint.com", &url, false).await.is_err());

        let (tx, mut rx) = mpsc::unbounded_channel();
        let conn = link.add_conn(tx);
        // Attached but silent: still not connected.
        assert!(!link.is_connected());
        link.set_hello(
            conn,
            Hello {
                browser: BrowserKind::Chrome,
                ext_version: "1.0.0".into(),
            },
        );
        assert!(link.is_connected());

        // A ping is answered by the matching pong.
        let ping = link.ping();
        let answer = async {
            let line = rx.recv().await.unwrap();
            let msg: ToExtension = serde_json::from_slice(&line).unwrap();
            let ToExtension::Ping { id } = msg else { panic!("{msg:?}") };
            link.resolve(&id, Ok(None));
        };
        let (ok, ()) = tokio::join!(ping, answer);
        assert!(ok);

        // A dropped connection fails the waiting request instead of leaving it hanging.
        let refresh = link.request_refresh("contoso.sharepoint.com", &url, false);
        let hang_up = async {
            let _ = rx.recv().await;
            link.remove_conn(conn);
        };
        let (res, ()) = tokio::join!(refresh, hang_up);
        assert!(res.is_err());
        assert!(!link.is_connected());
    }

    #[test]
    fn extension_errors_map_to_app_errors() {
        assert!(matches!(
            error_for(ErrorCode::SigninRequired, "a.sharepoint.com"),
            Error::AuthExpired { .. }
        ));
        assert!(matches!(error_for(ErrorCode::Timeout, "a"), Error::AuthExpired { .. }));
        assert!(matches!(error_for(ErrorCode::NoPermission, "a"), Error::State(_)));
        assert!(matches!(error_for(ErrorCode::BadRequest, "a"), Error::State(_)));
    }
}
