# AmpSync architecture

AmpSync downloads large SharePoint Online folders to a local disk, file by file, verifying each file and resuming after any interruption. This document is the contract every crate is built against. Change it before changing an interface.

## Decisions

| # | Decision | Why |
|---|----------|-----|
| D1 | Download only. Re-running a job mirrors: unchanged files are skipped, partial files resume, nothing local is deleted unless `--prune`. | Replaces OneDrive zip downloads; two-way sync is out of scope. |
| D2 | Auth uses the user's SharePoint browser session (FedAuth / rtFa cookies). No Entra app registration. | Works in any tenant without IT involvement. |
| D3 | Sign-in methods, tried in order: (1) embedded webview (persistent profile, see D15), (2) installed Edge/Chrome with a throwaway profile read over the DevTools protocol, (3) pasted `Cookie` header. | Conditional Access may block embedded webviews. |
| D4 | Zero config for users. The tenant host is taken from the pasted URL. Sessions live in the OS keychain, keyed by host. Settings are edited in the GUI and saved to `settings.toml` in the per-user config dir. | Non-developers must never edit a file. |
| D5 | No hardcoded tenants, sites, URLs or company names anywhere in code. | Public, cross-org repo. |
| D6 | Files are written to `<name>.ampsync-part`, downloaded in parallel byte ranges, verified (QuickXorHash when available, else size + unchanged eTag), then atomically renamed. | Corruption-proof, resumable. |
| D7 | Job state lives in SQLite in the per-user app data dir: `<data_local_dir>/jobs/<key>/state.db`, where `<key>` is the hex of the first 16 bytes of SHA-256 over the canonical destination path (Windows: case-folded, no `\\?\`). Nothing of AmpSync stays in the download destination once a job is done. | Survives crashes and reboots; lets a rerun resume; keeps shared, zipped and code folders clean. |
| D8 | Throttling: honour `Retry-After` on 429/503 globally (all lanes pause), exponential backoff with jitter otherwise. User-Agent `NONISV|AmpSync|AmpSync/<version>`. | Microsoft guidance. |
| D9 | Expired session pauses the job (never fails it) and emits `Event::AuthExpired`. | 80 GB jobs outlive sessions. |
| D10 | Platforms: Windows x64 and Ubuntu x64. No macOS in v1. No code signing, no auto-update, no telemetry. | Agreed scope. |
| D11 | GUI is Tauri 2 + Svelte 5, with an approved "instrument panel" design (graphite + amber, IBM Plex). | |
| D13 | First launch shows a skippable walkthrough: a dimmed overlay spotlights one control at a time with a short text box (Back / Next / Skip, step counter). It is replayable from Settings. The "seen" flag lives in `settings.toml`. | Non-developer users. |
| D15 | The webview sign-in uses a persistent AmpSync-only browser profile (per-user data dir), separate from the user's browsers. On launch the app silently checks every saved session (hidden webview load of the site); if Microsoft needs a password or MFA, the sign-in window opens right then, before any download starts. Mid-job expiry also tries a silent refresh first and shows the window only if interaction is required. On Windows, WebView2 SSO with the OS work account is enabled, so managed machines sign in with no prompt. Password saving is a Settings toggle, off by default. "Sign out" deletes the profile and the stored session. | Personal machines otherwise hit MFA on every sign-in; prompts should come at launch, not halfway through an 80 GB job. |
| D16 | AmpSync never reads, signs in to, or syncs with the user's existing browser profiles (Edge, Chrome, Teams, …). The webview profile lives only in AmpSync's own data dir. The Edge/Chrome fallback always launches a fresh AmpSync-owned `--user-data-dir` with `--disable-sync` and browser implicit sign-in disabled, and only the SharePoint web session is captured. No code path imports cookies or passwords from a browser profile. | User requirement; browser profiles hold personal and work identities that AmpSync must not touch. |
| D17 | Release builds (installers, portable exe, AppImage, .deb, CLI binaries) run only when a `v*` tag is pushed, and publish a GitHub Release for that tag. Pushes to `main` build nothing. Pull requests run only fmt + clippy + tests on Windows and Ubuntu. `v1.0.0` is tagged after the user has tested on a personal and a work Windows machine. | Avoids burning CI and publishing artifacts on every push. |
| D18 | Probe result (real tenant, 2026-10-22): with cookies, `/_api/v2.0/*` returns 403 and `SP.OAuth.Token/Acquire` returns 400, so no QuickXorHash. Classic REST listing works; `/_layouts/15/download.aspx?UniqueId=` honours Range (206); `GetFileByServerRelativePath/$value` ignores Range. v1 ships `ClassicRemote` only; verification is exact size + eTag unchanged (re-`stat` after download). `V2Remote` stays a documented extension point, not implemented. | Measured, not assumed. |
| D14 | Commits and PRs carry only human authors: no bot or AI-tool co-author trailers or "generated with" lines. | The contributor list shows people. |
| D12 | Contributors list is generated from the GitHub API at build time; repo slug comes from `AMPSYNC_REPO` / `GITHUB_REPOSITORY`, never hardcoded. | Forks show their own contributors; works offline. |
| D20 | `JobSpec.files` is an optional explicit list of files. The picker sends globs when 50 patterns or fewer describe the selection, otherwise the list. `include` / `exclude` still apply on top, and the usual skip and resume rules hold. | Filtered selections (v1.1 search) scatter across the tree. |
| D21 | `Plan.bytes_needed` is the bytes still to download (skipped files and ranges already on disk excluded; equal to `totals.bytes`). `free_space(dest)` (the `fs4` crate) reports the space on the destination's volume, using the nearest existing parent. A write that fails with disk full (`StorageFull`, `ENOSPC`, Windows 112 / 39) pauses the job like `AuthExpired`, emits `Event::DiskFull { path, needed, free }`, and never fails the job or deletes finished or partial work; `resume` retries. | Big jobs fill disks; a full disk must be recoverable, not a pile of failed files. |
| D22 | Parallel downloads are tuned automatically by default (`Settings.auto_tune`, true); `parallel_files` is the fixed value when it is off. AIMD per 10 s of active transfer: start 4 lanes, bounds 2 to 16; +1 lane when throughput rose at least 5% over the window before the last change, else take the last lane back and hold 60 s; halve (minimum 2) on 429/503 and hold 2 min after the `Retry-After` wait; above 2,400 requests per rolling 5 minutes (80% of SharePoint's ~3,000) stop adding lanes and use 32 MiB ranges for files started from then on. Lanes change mid-job without losing or repeating work (`Event::LanesChanged { lanes, reason }`). | The best lane count depends on the link and the tenant; a fixed number is wrong somewhere. |
| D23 | `Settings.notify_on_finish: Option<bool>`; `None` means the user has not been asked yet (the key is absent from `settings.toml`). The desktop asks once, at the first download, and notifies only while the window is unfocused. | Notifications are opt-in. |
| D24 | `Event::RangeDone { rel_path, index, count }` is sent once per range of a file split into ranges; `Event::FileStarted` carries `done_ranges` (ranges already on disk when resuming). Ranges lie on a fixed grid of the file's range size, so indices are stable across resumes. | Drives the fill map for big files. |
| D25 | `Settings.splash: SplashMode` with `on` (default), `reduced`, `off`. The desktop implements the behaviour. | Startup animation is a preference. |

## Layout

```
Cargo.toml                 workspace
crates/
  ampsync-core/            all logic; no UI, no webview
  ampsync-cli/             `ampsync` binary (clap + indicatif)
apps/
  desktop/                 Svelte 5 frontend (Vite)
  desktop/src-tauri/       `ampsync-desktop` crate (Tauri 2), workspace member
spikes/probe/              throwaway probe, NOT a workspace member
```

## `ampsync-core` modules and interfaces

Signatures are normative; bodies and private helpers are free.

### `link` — understand what the user pasted

```rust
pub struct ParsedLink {
    pub host: String,                 // "contoso.sharepoint.com"
    pub kind: LinkKind,
}
pub enum LinkKind {
    Sharing { url: Url },                              // /:f:/s/Site/E...
    Folder { site_url: Url, server_relative_path: String }, // ?id= or direct path
}
pub fn parse(input: &str) -> Result<ParsedLink, Error>;
```

### `auth` — sessions and where they're stored

```rust
pub enum SignInMethod { Webview, Browser { kind: BrowserKind }, Pasted }
pub enum BrowserKind { Edge, Chrome }

pub struct Session {
    pub host: String,
    pub cookies: Vec<SessionCookie>,   // name, value, domain, path, expires
    pub method: SignInMethod,
    pub captured_at: OffsetDateTime,
}

pub trait SessionStore: Send + Sync {
    fn load(&self, host: &str) -> Result<Option<Session>, Error>;
    fn save(&self, session: &Session) -> Result<(), Error>;
    fn delete(&self, host: &str) -> Result<(), Error>;
    fn hosts(&self) -> Result<Vec<String>, Error>;
}
pub struct KeyringStore;        // OS keychain via `keyring` crate
pub struct MemoryStore;         // tests

/// Implemented in core for Browser and Pasted; the Webview method is implemented
/// by the GUI/CLI because it needs a window.
#[async_trait]
pub trait SignIn: Send + Sync {
    async fn sign_in(&self, host: &str, start_url: &Url) -> Result<Session, Error>;
}
// Webview sign-in completion (learned from the probe): do NOT rely on page-load events alone;
// SharePoint keeps navigating client-side after sign-in, so a check on "page finished" can run
// before FedAuth exists and never run again. Poll the webview cookie store every ~750 ms from the
// event-loop thread (never from a WebView2 callback: deadlock) and finish as soon as FedAuth for
// the tenant host and rtFa are both present, then close the window automatically. Also pump the
// event loop briefly after dropping the window so Windows destroys it (else "Not responding").
pub struct BrowserSignIn { pub kind: BrowserKind }
pub struct PastedSignIn { pub cookie_header: String }
```

### `remote` — talking to SharePoint

One trait. v1 implements `ClassicRemote` only (D18). Facts from the probe it must respect:
- Sharing links resolve by following the 302 (cookie jar on) to `…/_layouts/15/onedrive.aspx?id=<server-relative folder path>` (OneDrive personal sites: `/personal/<user>/Documents/...`); team sites look like `/sites/<Site>/<Library>/...`.
- Listing: `GET {site}/_api/web/GetFolderByServerRelativePath(decodedurl='{path}')/Files?$select=Name,Length,TimeLastModified,ETag,UniqueId,ServerRelativeUrl` and `/Folders?$select=Name,ServerRelativeUrl,UniqueId,ItemCount`, header `Accept: application/json;odata=nometadata`. `Length` arrives as a string. Folder paths must be percent-encoded with `'` doubled.
- Download: `GET {site}/_layouts/15/download.aspx?UniqueId={guid}` with `Range: bytes=a-b` → 206 with `Content-Range`. Always check the status is 206 and Content-Range matches before writing; a 200 means the server ignored Range.
- Auth expiry shows up as 401/403 or a redirect to `login.microsoftonline.com`; the client must not follow redirects off the tenant host for API calls.

```rust
pub struct RemoteRoot { pub site_url: Url, pub drive_id: Option<String>, pub item_id: Option<String>,
                        pub server_relative_path: String, pub display_name: String }

pub struct RemoteFile {
    pub rel_path: RelPath,            // relative to the root, '/'-separated
    pub size: u64,
    pub etag: String,
    pub modified: OffsetDateTime,
    pub hash: Option<RemoteHash>,     // QuickXor(base64) | Sha256(hex)
    pub locator: FileLocator,         // opaque to callers; how to download it
}
pub enum RemoteEntry { File(RemoteFile), Folder { rel_path: RelPath } }

#[async_trait]
pub trait Remote: Send + Sync {
    async fn resolve(&self, link: &ParsedLink) -> Result<RemoteRoot, Error>;
    /// Recursive listing, streamed so huge trees start showing immediately.
    fn walk<'a>(&'a self, root: &'a RemoteRoot) -> BoxStream<'a, Result<RemoteEntry, Error>>;
    /// Bytes `range` of a file. Must return exactly that range or an error.
    async fn read_range(&self, file: &RemoteFile, range: Range<u64>) -> Result<BoxStream<'static, Result<Bytes, Error>>, Error>;
    /// Re-fetch metadata of one file (used to confirm the eTag didn't change).
    async fn stat(&self, file: &RemoteFile) -> Result<RemoteFile, Error>;
    /// Swap cookies after a re-sign-in (default no-op). Used by JobHandle::replace_session.
    fn replace_session(&self, session: Session) {}
}
pub struct ClassicRemote;  // /_api/web + /_layouts/15/download.aspx  (v1)
// V2Remote (/_api/v2.x) is not implemented: it needs a bearer token we cannot get from cookies (D18).
pub fn http_client(session: &Session) -> reqwest::Client; // cookie jar, UA, timeouts
```

### `errors`

```rust
#[derive(thiserror::Error, Debug)]
pub enum Error {
    AuthExpired { host: String },                // 401/403 or redirect to login.microsoftonline.com
    Throttled { retry_after: Duration },         // 429/503
    NotFound(String),
    Integrity { rel_path: RelPath, expected: String, actual: String },
    RemoteChanged { rel_path: RelPath },         // eTag changed mid-download
    InvalidLink(String),
    Http { status: u16, context: String },
    Network(String),
    Io { path: PathBuf, source: std::io::Error },
    State(String),
    Cancelled,
}
impl Error { pub fn is_retryable(&self) -> bool; }
```

### `hash`

`QuickXorHash` streaming hasher (`update(&[u8])`, `finalize() -> [u8; 20]`, base64 helper), tested against known vectors.

### `plan` — what will happen before anything downloads

```rust
pub struct JobSpec {
    pub source: String,                 // whatever the user pasted
    pub dest: PathBuf,
    pub include: Vec<String>,           // globs on rel_path; empty = everything
    pub exclude: Vec<String>,
    pub existing: ExistingPolicy,       // SkipUnchanged (default) | Redownload
    pub prune: bool,                    // CLI only; default false
}
pub struct Plan { pub root: RemoteRoot, pub items: Vec<PlannedFile>, pub totals: Totals,
                  pub bytes_needed: u64 }   // D21: bytes still to download, == totals.bytes
pub fn free_space(dest: &Path) -> io::Result<u64>;  // D21: also `ampsync_core::free_space`
pub enum PlannedAction { Download, Resume { have: u64 }, SkipUnchanged }
```

### `state` — SQLite job state

Tables: `job` (spec, root, created/updated), `file` (rel_path PK, size, etag, hash, status, bytes_done, verified_by, error). Opened with WAL. All writes go through one task to avoid lock contention.

### `engine` — the transfer engine

```rust
pub struct Settings {
    pub parallel_files: u8,            // default 6, 1..=16; the fixed lane count when auto_tune is off
    pub auto_tune: bool,               // default true (D22): lanes chosen by the engine, 2..=16
    // application fields: notify_on_finish: Option<bool> (D23), splash: SplashMode (D25)
    pub range_size: u64,               // default 16 MiB (keeps a fast download under SharePoint's ~3000 requests / 5 min per user)
    pub large_file_threshold: u64,     // files above this get parallel ranges; default 64 MiB
    pub bandwidth_limit: Option<u64>,  // bytes/s, shared across lanes
    pub max_retries: u32,              // per range; default 8
}

pub struct Engine<R: Remote> { /* remote, settings */ }
impl<R: Remote + 'static> Engine<R> {
    pub fn new(remote: Arc<R>, settings: Settings) -> Self;
    pub async fn plan(&self, spec: &JobSpec) -> Result<Plan, Error>;
    pub fn start(&self, spec: JobSpec, plan: Plan) -> JobHandle;
}

pub struct JobHandle { /* … */ }
impl JobHandle {
    pub fn events(&self) -> tokio::sync::broadcast::Receiver<Event>;
    pub fn pause(&self);
    pub fn resume(&self);                       // also used after re-sign-in
    pub fn replace_session(&self, s: Session);  // swap cookies, then resume
    pub fn cancel(&self);
    pub async fn wait(self) -> Result<Summary, Error>;
}

pub enum Event {
    Planned { files: u64, bytes: u64, skipped: u64 },
    FileStarted { rel_path: RelPath, size: u64, lane: u8, done_ranges: Vec<u32> }, // D24
    Progress { bytes_done: u64, bytes_total: u64, rate_bps: u64 }, // throttled to ~4 Hz
    LaneProgress { lane: u8, rel_path: RelPath, offset: u64, size: u64 },
    FileVerified { rel_path: RelPath, by: VerifiedBy },             // Hash | SizeAndEtag | SizeAndMtime
    FileSkipped { rel_path: RelPath },
    FileFailed { rel_path: RelPath, error: String, will_retry: bool },
    Throttled { retry_after: Duration },
    AuthExpired { host: String },                                   // job is now paused
    DiskFull { path: String, needed: u64, free: u64 },              // job is now paused (D21)
    RangeDone { rel_path: RelPath, index: u32, count: u32 },        // once per range of a ranged file (D24)
    LanesChanged { lanes: u8, reason: LanesReason },                // probe_up | no_gain | throttled | budget (D22)
    Paused, Resumed,
    Finished(Summary),
}
```

Engine rules:
- Files ≤ `large_file_threshold` are one request each; larger files are split into `range_size` ranges spread across lanes. Total in-flight requests never exceed the lane count (`parallel_files`, or the tuned count with `auto_tune`; see D22). A lane that is taken away finishes its current range or file first.
- Each range is written with positional writes into the preallocated `.ampsync-part`; completed ranges are recorded in `state.db` so a restart only fetches missing ranges.
- After the last byte: verify hash if present, else size + `stat()` eTag unchanged. Then rename over the destination. On mismatch: delete the part file and retry the file once, then `FileFailed`.
- Windows: use `\\?\` long paths; replace characters illegal on Windows (`<>:"|?*` and trailing dots/spaces) with a visible substitute and record the mapping in `state.db`.

### Scaffold notes (as built)

The code in `crates/ampsync-core` is now the source of truth for exact types. Additions beyond this doc: `ClassicRemote::new(&Session)`, `MemoryStore::new()`, one flat `settings::Settings` (engine + app fields, re-exported as `engine::Settings`), `PlannedFile`, `Totals`, `Summary`, `FileFailure`, `state::{FileRecord, FileStatus}`. `Event` serializes with a snake_case `type` tag and durations as `*_ms`. `link::parse` rejects site-only URLs with no library.

### After the first review (as built)

- `download.aspx` answers 206 with `Content-Type: text/html` on real tenants. Download responses never infer auth expiry from content type; only 401/403 or a login redirect do.
- `auth::check_session(&Session, &site_url) -> SessionStatus { Valid, Expired, Unknown }` and `Remote::probe()`. Only `Expired` rejects a session; `Unknown` (e.g. 403 on a restricted site) keeps it. Probes go to the link's own site, not the tenant root.
- On `AuthExpired` for one file the engine probes the session: still `Valid` ⇒ that file fails with 403 and the job continues; otherwise the job pauses.
- Listing, resolve and stat go through `remote::RetryPolicy` (honours `Retry-After`, bounded backoff).
- `<data_local_dir>/jobs/<key>/lock` is held (`File::try_lock`) from plan until the job ends; a second process gets `Error::DestinationBusy`. Before rename, recorded ranges must cover the whole file.
- Plan-time skips are reported only as counts on `Event::Planned { skipped, skipped_bytes }`.
- The Edge/Chrome fallback talks CDP over `--remote-debugging-pipe` / `--remote-debugging-io-pipes`; no TCP port is opened.

### Job state outside the destination (as built)

- Module `appdirs`: `data_dir()` (`ProjectDirs::from("", "", "AmpSync").data_local_dir()`, Windows `%LOCALAPPDATA%\AmpSync\data`; the env var `AMPSYNC_DATA_DIR` replaces it), `config_dir()`, `webview_cache_dir()`, `job_key(dest)`, `job_dir(data_dir, dest)`. A job directory holds `state.db`, `lock` and `dest.txt` (the destination path, for humans).
- `StateDb::open(dest)` resolves the job directory; `StateDb::open_in(Some(data_dir), dest)` and `StateDb::open_at(state_dir)` are for callers and tests that must not touch the real app data. `Tuning::data_dir` does the same for the engine; the integration tests set it to a temporary directory.
- Migration: a `<dest>/.ampsync/state.db` from older builds is moved to the job directory on first open and the old folder is removed when empty.
- Part files stay next to their target (`<name>.ampsync-part`, same volume, atomic rename). When a job ends without being cancelled, no part file is left in the folders it covers, except the part file of a failed download with recorded ranges (resumable; the next successful run removes it).
- No state record for a file: plan treats a local file as `SkipUnchanged` when its size equals the remote size and its mtime equals the remote `modified` (within 2 s; the engine sets the mtime after the rename) and records it as `VerifiedBy::SizeAndMtime`. A copied or moved destination therefore skips everything.
- `purge::purge_all() -> Result<PurgeReport, Error>` (for uninstall and "sign out of everything") deletes every keychain session and the host index, the config dir, the data dir (job state, `signin-profile`, `queue.json`, `update-check.json`) and the desktop WebView cache `<local data>/io.github.ampsync.desktop`. Best effort, symlinks are never followed, download destinations are never touched. `purge_with(&PurgeTargets, &dyn SessionStore)` takes injected locations for tests.

## `ampsync-cli`

```
ampsync login <url> [--method webview|edge|chrome|paste]
ampsync logout <host>
ampsync sessions
ampsync ls <url> [--depth N]
ampsync get <url> <dest> [--include GLOB]... [--exclude GLOB]... [--parallel N]
                         [--limit RATE] [--redownload] [--prune] [--yes] [--json]
ampsync purge [--yes]          # delete all sign-ins, settings and app data; downloads are kept
```
Exit codes: 0 ok, 1 error, 2 usage, 3 auth expired (rerun resumes), 4 some files failed. `--json` emits one `Event` per line for scripting. The CLI's webview sign-in uses `wry`/`tao` directly behind a `webview` cargo feature (on by default) so headless builds can opt out.

## `ampsync-desktop`

Tauri 2 commands mirror the engine (`resolve`, `plan`, `enqueue`, `pause`, `resume`, `cancel`, `sign_in`, `sign_out`, `sessions`, `get_settings`, `set_settings`, `contributors`). Engine events are forwarded to the frontend as Tauri events. A queue runs jobs one at a time. The frontend has the views Transfers, New download, Sign-ins, Settings, About, plus the first-run walkthrough (steps defined as data: target selector, view to show, title, body), skippable at any step and replayable from Settings.

## Testing

- Unit tests in every core module.
- Integration tests run the engine against a mock SharePoint server (`wiremock` or an `axum` fake) that supports both remote APIs, Range requests, 429 with `Retry-After`, auth expiry mid-job, eTag change mid-job, and corrupted bytes.
- CI on pull requests only: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test` on `windows-latest` and `ubuntu-24.04`.
- Release workflow on `v*` tags only (D17).
