//! Tauri commands. Argument names are snake_case on the JavaScript side too.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ampsync_core::Error;
use ampsync_core::auth::{BrowserKind, BrowserSignIn, PastedSignIn, Session, SignIn, SignInMethod};
use ampsync_core::link::{self, ParsedLink};
use ampsync_core::names;
use ampsync_core::plan::{ExistingPolicy, JobSpec, RelPath};
use ampsync_core::remote::{ClassicRemote, Remote, RemoteEntry, RemoteRoot};
use ampsync_core::settings::{CloseBehavior, Settings, SignInChoice};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State, Url};

use crate::AppState;
use crate::error::{CmdError, CmdResult};
use crate::queue::{ItemStatus, QueueItem, QueueItemView};

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

pub(crate) fn start_url(parsed: &ParsedLink) -> Url {
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

/// The listed folder as flat, parallel arrays in display order (folders before files, each group
/// sorted case-insensitively). Parents always come before their children and every subtree is a
/// contiguous range, so the frontend can build rollups and toggle whole folders with plain loops.
/// Roots (the folder's direct contents) have parent -1.
#[derive(Serialize, Debug, Default, PartialEq, Eq)]
pub struct PlanTree {
    pub total_files: u64,
    pub total_bytes: u64,
    pub listed_ms: u64,
    pub names: Vec<String>,
    pub parents: Vec<i32>,
    /// 1 for a folder, 0 for a file.
    pub dirs: Vec<u8>,
    /// File sizes in bytes; 0 for folders.
    pub sizes: Vec<u64>,
    /// Last-modified time of each file in unix seconds; 0 for folders.
    pub modified: Vec<i64>,
}

struct BNode {
    name: String,
    is_dir: bool,
    size: u64,
    modified: i64,
    children: Vec<usize>,
}

/// Collects listed entries into a tree, then flattens it (see [`PlanTree`]).
#[derive(Default)]
struct TreeBuilder {
    nodes: Vec<BNode>,
    roots: Vec<usize>,
    dirs: HashMap<(Option<usize>, String), usize>,
}

impl TreeBuilder {
    fn push(&mut self, parent: Option<usize>, name: &str, is_dir: bool, size: u64, modified: i64) -> usize {
        let idx = self.nodes.len();
        self.nodes.push(BNode {
            name: name.to_string(),
            is_dir,
            size,
            modified,
            children: Vec::new(),
        });
        match parent {
            Some(p) => self.nodes[p].children.push(idx),
            None => self.roots.push(idx),
        }
        idx
    }

    /// Make sure the folder chain `comps` exists; returns the last folder (None for no components).
    fn ensure_dirs(&mut self, comps: &[&str]) -> Option<usize> {
        let mut parent = None;
        for c in comps {
            let key = (parent, c.to_lowercase());
            let idx = match self.dirs.get(&key) {
                Some(i) => *i,
                None => {
                    let i = self.push(parent, c, true, 0, 0);
                    self.dirs.insert(key, i);
                    i
                }
            };
            parent = Some(idx);
        }
        parent
    }

    fn add_dir(&mut self, comps: &[&str]) {
        self.ensure_dirs(comps);
    }

    fn add_file(&mut self, comps: &[&str], size: u64, modified: i64) {
        let Some((name, dirs)) = comps.split_last() else {
            return;
        };
        let parent = self.ensure_dirs(dirs);
        self.push(parent, name, false, size, modified);
    }

    fn finish(mut self, listed_ms: u64) -> PlanTree {
        let mut out = PlanTree {
            listed_ms,
            ..PlanTree::default()
        };
        let order = |nodes: &[BNode], list: &mut Vec<usize>| {
            list.sort_by_cached_key(|&i| (!nodes[i].is_dir, nodes[i].name.to_lowercase()));
        };
        let mut roots = std::mem::take(&mut self.roots);
        order(&self.nodes, &mut roots);
        // Depth-first, children in order: a stack of (node, new index of its parent).
        let mut stack: Vec<(usize, i32)> = roots.into_iter().rev().map(|r| (r, -1)).collect();
        while let Some((i, parent_new)) = stack.pop() {
            let new_idx = i32::try_from(out.names.len()).unwrap_or(i32::MAX);
            let mut kids = std::mem::take(&mut self.nodes[i].children);
            order(&self.nodes, &mut kids);
            let n = &mut self.nodes[i];
            out.names.push(std::mem::take(&mut n.name));
            out.parents.push(parent_new);
            out.dirs.push(u8::from(n.is_dir));
            out.sizes.push(n.size);
            out.modified.push(n.modified);
            if !n.is_dir {
                out.total_files += 1;
                out.total_bytes += n.size;
            }
            stack.extend(kids.into_iter().rev().map(|k| (k, new_idx)));
        }
        out
    }
}

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
    let mut builder = TreeBuilder::default();
    let (mut total_files, mut total_bytes) = (0u64, 0u64);
    let mut last_emit = Instant::now();
    let mut stream = remote.walk(root);
    while let Some(entry) = stream.next().await {
        match entry? {
            RemoteEntry::File(f) => {
                let comps: Vec<&str> = f.rel_path.components().collect();
                total_files += 1;
                total_bytes += f.size;
                builder.add_file(&comps, f.size, f.modified.unix_timestamp());
            }
            RemoteEntry::Folder { rel_path } => {
                let comps: Vec<&str> = rel_path.components().collect();
                builder.add_dir(&comps);
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
    Ok(builder.finish(u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX)))
}

/// Classify pasted, dropped or clipboard text (see `links`), so the frontend never guesses.
#[tauri::command(rename_all = "snake_case")]
pub fn parse_link(text: String) -> CmdResult<crate::links::ParsedLinkInfo> {
    crate::links::parse_text(&text).map_err(|m| CmdError::new("invalid_link", m))
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
    /// Explicit file list (D20) for selections that would need too many globs.
    #[serde(default)]
    pub files: Option<Vec<RelPath>>,
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
    spec.files = req.files;
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
pub fn get_queue(state: State<'_, AppState>) -> Vec<QueueItemView> {
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
    /// "webview", "edge", "chrome", "paste", or "auto" (the remembered way, else ask).
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
        "webview" | "auto" => state.signin.obtain(&app, &host, &url, true, true).await?,
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

/// The host of the sign-in question the frontend should be showing, if one is open.
#[tauri::command(rename_all = "snake_case")]
pub fn pending_sign_in_choice(state: State<'_, AppState>) -> Option<String> {
    state.signin.pending_choice()
}

/// Answer the sign-in question (D26): `method` is "webview", "edge" or "chrome"; `None` cancels.
/// With `remember` the choice becomes the default and the question is not asked again.
#[tauri::command(rename_all = "snake_case")]
pub fn choose_sign_in(state: State<'_, AppState>, method: Option<String>, remember: bool) -> CmdResult<()> {
    let choice = method.as_deref().and_then(SignInChoice::from_method);
    if let (true, Some(c)) = (remember, choice) {
        let mut s = state.settings();
        s.sign_in_with = Some(c);
        s.save()?;
        *state.settings.write().unwrap_or_else(|p| p.into_inner()) = s;
    }
    state.signin.resolve_choice(choice);
    Ok(())
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

/// Settings > "Remove all AmpSync data". Closes every sign-in window, stops the queue, starts a
/// detached copy of the app that waits for this process to exit, deletes everything (keychain
/// sessions, settings, job state, sign-in profile, WebView cache) and starts AmpSync again. The
/// purge cannot run in this process: the main window's WebView holds its profile folder open.
#[tauri::command(rename_all = "snake_case")]
pub fn purge_everything(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    let helper = crate::purge::spawn_restart_helper()
        .map_err(|e| CmdError::new("error", format!("Could not start the cleanup: {e}")))?;
    // Keep the pipe to the helper open until this process ends; that is its signal to go ahead.
    std::mem::forget(helper);
    state.quitting.store(true, std::sync::atomic::Ordering::SeqCst);
    state.signin.cancel(&app);
    state.queue.pause_for_exit(&app, false);
    app.exit(0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_tree_carries_modified_times_in_display_order() {
        let mut b = TreeBuilder::default();
        b.add_file(&["zeta.txt"], 5, 1_700_000_000);
        b.add_file(&["Docs", "b.pdf"], 7, 1_714_558_830);
        b.add_file(&["Docs", "a.pdf"], 9, 1_600_000_000);
        b.add_dir(&["Empty"]);
        let t = b.finish(3);
        assert_eq!(t.names, ["Docs", "a.pdf", "b.pdf", "Empty", "zeta.txt"]);
        assert_eq!(t.dirs, [1, 0, 0, 1, 0]);
        assert_eq!(t.sizes, [0, 9, 7, 0, 5]);
        // Folders carry 0, files their own time.
        assert_eq!(t.modified, [0, 1_600_000_000, 1_714_558_830, 0, 1_700_000_000]);
        assert_eq!((t.total_files, t.total_bytes), (3, 21));
    }
}
