//! Tauri commands. Argument names are snake_case on the JavaScript side too.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ampsync_core::Error;
use ampsync_core::auth::{BrowserKind, BrowserSignIn, PastedSignIn, Session, SignIn, SignInMethod};
use ampsync_core::link::{self, ParsedLink};
use ampsync_core::names;
use ampsync_core::plan::{ExistingPolicy, JobSpec};
use ampsync_core::remote::{ClassicRemote, Remote, RemoteEntry, RemoteRoot};
use ampsync_core::settings::{CloseBehavior, Settings};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State, Url};

use crate::AppState;
use crate::error::{CmdError, CmdResult};
use crate::queue::{ItemStatus, QueueItem};

// ---------------------------------------------------------------- links and listing

#[derive(Serialize)]
pub struct Resolved {
    pub host: String,
    pub site_name: String,
    pub library: String,
    pub display_name: String,
    pub server_relative_path: String,
    pub site_url: String,
}

fn start_url(parsed: &ParsedLink) -> Url {
    match &parsed.kind {
        link::LinkKind::Sharing { url } => url.clone(),
        link::LinkKind::Folder { site_url, .. } => site_url.clone(),
    }
}

/// Parse, sign in if needed, and resolve to a folder. Retries once after a fresh sign-in.
async fn resolve_root(
    app: &AppHandle,
    text: &str,
) -> Result<(ParsedLink, Session, ClassicRemote, RemoteRoot), Error> {
    let parsed = link::parse(text)?;
    let state = app.state::<AppState>();
    let mut force = false;
    for attempt in 0..2 {
        let session = state
            .signin
            .obtain(app, &parsed.host, &start_url(&parsed), force, true)
            .await?;
        let remote = ClassicRemote::new(&session);
        match remote.resolve(&parsed).await {
            Ok(root) => return Ok((parsed, session, remote, root)),
            Err(Error::AuthExpired { .. }) if attempt == 0 => force = true,
            Err(e) => return Err(e),
        }
    }
    Err(Error::AuthExpired { host: parsed.host })
}

#[tauri::command(rename_all = "snake_case")]
pub async fn resolve(app: AppHandle, link: String) -> CmdResult<Resolved> {
    let (parsed, _session, _remote, root) = resolve_root(&app, &link).await?;
    let split = link::split_site(&root.server_relative_path);
    let (site_name, library) = match split {
        Some(s) => (
            s.site
                .rsplit('/')
                .next()
                .filter(|x| !x.is_empty())
                .unwrap_or(&parsed.host)
                .to_string(),
            s.library,
        ),
        None => (parsed.host.clone(), String::new()),
    };
    Ok(Resolved {
        host: parsed.host,
        site_name,
        library,
        display_name: root.display_name,
        server_relative_path: root.server_relative_path,
        site_url: root.site_url.to_string(),
    })
}

#[derive(Serialize)]
pub struct TreeNode {
    pub path: String,
    pub name: String,
    pub depth: usize,
    pub kind: &'static str,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Serialize)]
pub struct PlanTree {
    pub total_files: u64,
    pub total_bytes: u64,
    pub listed_ms: u64,
    pub nodes: Vec<TreeNode>,
}

/// Deepest folder level shown in the picker (0-based). Deeper folders count towards their parent.
const MAX_TREE_DEPTH: usize = 5;

#[tauri::command(rename_all = "snake_case")]
pub async fn plan(app: AppHandle, link: String) -> CmdResult<PlanTree> {
    let mut forced = false;
    loop {
        let (parsed, _session, remote, root) = resolve_root(&app, &link).await?;
        let _ = &parsed;
        match list_tree(&app, &remote, &root).await {
            Err(Error::AuthExpired { host }) if !forced => {
                // The next pass checks the stored session, then refreshes it (silent first).
                forced = true;
                app.state::<AppState>().set_status(&app, &host, "expired");
            }
            other => return other.map_err(Into::into),
        }
    }
}

async fn list_tree(app: &AppHandle, remote: &ClassicRemote, root: &RemoteRoot) -> Result<PlanTree, Error> {
    let began = Instant::now();
    let mut folders: BTreeMap<Vec<String>, (u64, u64)> = BTreeMap::new();
    let mut root_files: Vec<(String, u64)> = Vec::new();
    let (mut total_files, mut total_bytes) = (0u64, 0u64);
    let mut last_emit = Instant::now();
    let mut stream = remote.walk(root);
    while let Some(entry) = stream.next().await {
        match entry? {
            RemoteEntry::File(f) => {
                let comps: Vec<String> = f.rel_path.components().map(str::to_string).collect();
                total_files += 1;
                total_bytes += f.size;
                if comps.len() == 1 {
                    root_files.push((comps[0].clone(), f.size));
                } else {
                    let dirs = comps.len() - 1;
                    for i in 0..dirs.min(MAX_TREE_DEPTH) {
                        let e = folders.entry(comps[..=i].to_vec()).or_default();
                        e.0 += 1;
                        e.1 += f.size;
                    }
                }
            }
            RemoteEntry::Folder { rel_path } => {
                let comps: Vec<String> = rel_path.components().map(str::to_string).collect();
                for i in 0..comps.len().min(MAX_TREE_DEPTH) {
                    folders.entry(comps[..=i].to_vec()).or_default();
                }
            }
        }
        if last_emit.elapsed() > Duration::from_millis(400) {
            last_emit = Instant::now();
            let _ = app.emit(
                "listing-progress",
                serde_json::json!({ "files": total_files, "bytes": total_bytes }),
            );
        }
    }
    drop(stream);
    let mut nodes: Vec<TreeNode> = folders
        .iter()
        .map(|(comps, (files, bytes))| TreeNode {
            path: comps.join("/"),
            name: comps.last().cloned().unwrap_or_default(),
            depth: comps.len() - 1,
            kind: "folder",
            files: *files,
            bytes: *bytes,
        })
        .collect();
    // BTreeMap order is byte order of the component lists; re-sort case-insensitively.
    let key = |p: &str| p.split('/').map(str::to_lowercase).collect::<Vec<_>>();
    nodes.sort_by_key(|n| key(&n.path));
    root_files.sort_by_key(|(n, _)| n.to_lowercase());
    nodes.extend(root_files.into_iter().map(|(name, size)| TreeNode {
        path: name.clone(),
        name,
        depth: 0,
        kind: "file",
        files: 1,
        bytes: size,
    }));
    Ok(PlanTree {
        total_files,
        total_bytes,
        listed_ms: u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX),
        nodes,
    })
}

// ---------------------------------------------------------------- queue

#[derive(Deserialize)]
pub struct EnqueueReq {
    pub source: String,
    /// Parent folder chosen by the user; the remote folder's name is appended.
    pub save_to: String,
    pub folder_name: String,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub existing: ExistingPolicy,
    pub title: String,
    pub path_label: String,
    pub est_files: u64,
    pub est_bytes: u64,
}

#[tauri::command(rename_all = "snake_case")]
pub fn enqueue(app: AppHandle, state: State<'_, AppState>, req: EnqueueReq) -> CmdResult<u64> {
    let parsed = link::parse(&req.source)?;
    if req.save_to.trim().is_empty() {
        return Err(CmdError::new("error", "Choose a folder to save into."));
    }
    let dest = PathBuf::from(req.save_to.trim()).join(names::sanitize(&req.folder_name));
    let mut spec = JobSpec::new(req.source, dest);
    spec.include = req.include;
    spec.exclude = req.exclude;
    spec.existing = req.existing;
    let item = QueueItem {
        id: 0,
        title: req.title,
        path_label: req.path_label,
        host: parsed.host,
        spec,
        est_files: req.est_files,
        est_bytes: req.est_bytes,
        status: ItemStatus::Queued,
        error: None,
        summary: None,
    };
    Ok(state.queue.add(&app, item))
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_queue(state: State<'_, AppState>) -> Vec<QueueItem> {
    state.queue.snapshot()
}

#[tauri::command(rename_all = "snake_case")]
pub fn pause(app: AppHandle, state: State<'_, AppState>, job_id: u64) {
    state.queue.pause(&app, job_id);
}

#[tauri::command(rename_all = "snake_case")]
pub fn resume(app: AppHandle, state: State<'_, AppState>, job_id: u64) {
    state.queue.resume(&app, job_id);
}

#[tauri::command(rename_all = "snake_case")]
pub fn cancel(app: AppHandle, state: State<'_, AppState>, job_id: u64) {
    state.queue.cancel(&app, job_id);
}

#[tauri::command(rename_all = "snake_case")]
pub fn retry(app: AppHandle, state: State<'_, AppState>, job_id: u64) {
    state.queue.retry(&app, job_id);
}

#[tauri::command(rename_all = "snake_case")]
pub fn clear_finished(app: AppHandle, state: State<'_, AppState>) {
    state.queue.clear_finished(&app);
}

// ---------------------------------------------------------------- sign-in

#[derive(Serialize)]
pub struct SessionInfo {
    pub host: String,
    /// "webview", "edge", "chrome" or "paste".
    pub method: &'static str,
    pub captured_at: String,
    /// "active", "expired", "checking" or "unknown".
    pub status: String,
}

fn method_name(m: &SignInMethod) -> &'static str {
    match m {
        SignInMethod::Webview => "webview",
        SignInMethod::Browser {
            kind: BrowserKind::Edge,
        } => "edge",
        SignInMethod::Browser {
            kind: BrowserKind::Chrome,
        } => "chrome",
        SignInMethod::Pasted => "paste",
    }
}

#[tauri::command(rename_all = "snake_case")]
pub fn sessions(state: State<'_, AppState>) -> CmdResult<Vec<SessionInfo>> {
    let mut out = Vec::new();
    for host in state.store.hosts()? {
        if let Some(s) = state.store.load(&host)? {
            out.push(SessionInfo {
                method: method_name(&s.method),
                captured_at: s
                    .captured_at
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap_or_default(),
                status: state.status_of(&host).to_string(),
                host,
            });
        }
    }
    Ok(out)
}

/// Re-check every saved session (the Sign-ins view's Refresh button).
#[tauri::command(rename_all = "snake_case")]
pub async fn check_sessions(app: AppHandle) -> CmdResult<()> {
    crate::startup::check_all(&app, false).await;
    Ok(())
}

#[derive(Deserialize)]
pub struct SignInReq {
    /// A SharePoint link or just the address; the host is taken from it.
    pub link: String,
    /// "webview", "edge", "chrome" or "paste".
    pub method: String,
    pub cookie_header: Option<String>,
}

/// A sign-in needs only the tenant: accept a full folder link, or just the site address.
fn host_and_url(text: &str) -> Result<(String, Url), Error> {
    if let Ok(parsed) = link::parse(text) {
        let url = start_url(&parsed);
        return Ok((parsed.host, url));
    }
    let with_scheme = if text.contains("://") {
        text.to_string()
    } else {
        format!("https://{text}")
    };
    let url = Url::parse(&with_scheme)
        .map_err(|_| Error::InvalidLink("that is not a SharePoint address".into()))?;
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    if !host.contains(".sharepoint.") {
        return Err(Error::InvalidLink("that is not a SharePoint address".into()));
    }
    let root = Url::parse(&format!("https://{host}/")).map_err(|e| Error::InvalidLink(e.to_string()))?;
    Ok((host, root))
}

#[tauri::command(rename_all = "snake_case")]
pub async fn sign_in(app: AppHandle, req: SignInReq) -> CmdResult<SessionInfo> {
    let state = app.state::<AppState>();
    let (host, url) = host_and_url(req.link.trim())?;
    if state.signin.is_window_open() {
        state.signin.focus(&app);
        return Err(CmdError::new("busy", "A sign-in window is already open."));
    }
    let session = match req.method.as_str() {
        "webview" => state.signin.obtain(&app, &host, &url, true, true).await?,
        "edge" | "chrome" => {
            let kind = if req.method == "edge" {
                BrowserKind::Edge
            } else {
                BrowserKind::Chrome
            };
            let s = BrowserSignIn { kind }.sign_in(&host, &url).await?;
            state.store.save(&s)?;
            s
        }
        "paste" => {
            let header = req.cookie_header.unwrap_or_default();
            let s = PastedSignIn {
                cookie_header: header,
            }
            .sign_in(&host, &url)
            .await?;
            state.store.save(&s)?;
            s
        }
        _ => return Err(CmdError::new("error", "Unknown sign-in method.")),
    };
    state.set_status(&app, &session.host, "active");
    state.queue.deliver_session(&session);
    Ok(SessionInfo {
        method: method_name(&session.method),
        captured_at: session
            .captured_at
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default(),
        status: "active".into(),
        host: session.host,
    })
}

#[tauri::command(rename_all = "snake_case")]
pub fn cancel_sign_in(app: AppHandle, state: State<'_, AppState>) {
    state.signin.cancel(&app);
}

#[tauri::command(rename_all = "snake_case")]
pub async fn sign_out(app: AppHandle, host: String) -> CmdResult<()> {
    let state = app.state::<AppState>();
    state.store.delete(&host)?;
    let remaining = state.store.hosts()?.into_iter().filter(|h| *h != host).count();
    state.forget_status(&app, &host);
    // Best effort: a locked profile must not make sign-out look like it failed.
    if let Err(e) = state.signin.clear_profile(&app, &host, remaining == 0).await {
        tracing::warn!("could not clear the sign-in profile: {e}");
    }
    Ok(())
}

// ---------------------------------------------------------------- settings and app info

#[tauri::command(rename_all = "snake_case")]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings()
}

#[tauri::command(rename_all = "snake_case")]
pub fn set_settings(state: State<'_, AppState>, settings: Settings) -> CmdResult<Settings> {
    let settings = settings.sanitized();
    settings.save()?;
    *state.settings.write().unwrap_or_else(|p| p.into_inner()) = settings.clone();
    Ok(settings)
}

#[tauri::command(rename_all = "snake_case")]
pub fn mark_tour_seen(state: State<'_, AppState>) -> CmdResult<Settings> {
    let mut s = state.settings();
    s.tour_seen = true;
    s.save()?;
    *state.settings.write().unwrap_or_else(|p| p.into_inner()) = s.clone();
    Ok(s)
}

#[derive(Serialize)]
pub struct AppInfo {
    pub version: &'static str,
    pub repo: &'static str,
    pub build: &'static str,
    pub target: &'static str,
    pub credential_store: &'static str,
    pub os: &'static str,
}

#[tauri::command(rename_all = "snake_case")]
pub fn app_info() -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        repo: crate::REPO_SLUG,
        build: env!("AMPSYNC_BUILD_ID"),
        target: env!("AMPSYNC_TARGET"),
        credential_store: if cfg!(windows) {
            "Windows Credential Manager"
        } else if cfg!(target_os = "macos") {
            "Keychain"
        } else {
            "the system keyring"
        },
        os: std::env::consts::OS,
    }
}

#[tauri::command(rename_all = "snake_case")]
pub fn contributors() -> serde_json::Value {
    crate::contributors::load()
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_update(state: State<'_, AppState>) -> Option<crate::update::UpdateInfo> {
    state.update.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

#[tauri::command(rename_all = "snake_case")]
pub fn open_external(app: AppHandle, url: String) -> CmdResult<()> {
    let parsed = Url::parse(&url).map_err(|e| CmdError::new("error", e.to_string()))?;
    if parsed.scheme() != "https" {
        return Err(CmdError::new("error", "Only https links can be opened."));
    }
    tauri_plugin_opener::OpenerExt::opener(&app)
        .open_url(parsed.as_str(), None::<&str>)
        .map_err(|e| CmdError::new("error", e.to_string()))
}

/// The user's answer to the "Closing AmpSync" question.
#[derive(Deserialize)]
pub struct CloseDecision {
    /// "tray" or "pause"
    pub choice: String,
    pub remember: bool,
}

#[tauri::command(rename_all = "snake_case")]
pub fn close_decision(app: AppHandle, state: State<'_, AppState>, decision: CloseDecision) -> CmdResult<()> {
    let behavior = if decision.choice == "tray" {
        CloseBehavior::Tray
    } else {
        CloseBehavior::Pause
    };
    if decision.remember {
        let mut s = state.settings();
        s.close_behavior = behavior;
        s.save()?;
        *state.settings.write().unwrap_or_else(|p| p.into_inner()) = s;
    }
    crate::tray::apply_close(&app, behavior);
    Ok(())
}

/// Directories the frontend may want to show (for the settings hint).
#[tauri::command(rename_all = "snake_case")]
pub fn known_hosts(state: State<'_, AppState>) -> CmdResult<BTreeSet<String>> {
    Ok(state.store.hosts()?.into_iter().collect())
}
