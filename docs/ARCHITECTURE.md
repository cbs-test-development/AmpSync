# AmpSync architecture

AmpSync downloads large SharePoint Online folders to a local disk, file by file, verifying each file and resuming after any interruption. This document is the contract every crate is built against. Change it before changing an interface.

## Decisions

| # | Decision | Why |
|---|----------|-----|
| D1 | Download only. Re-running a job mirrors: unchanged files are skipped, partial files resume, nothing local is deleted unless `--prune`. | Replaces OneDrive zip downloads; two-way sync is out of scope. |
| D2 | Auth uses the user's SharePoint browser session (FedAuth / rtFa cookies). No Entra app registration. | Works in any tenant without IT involvement. |
| D3 | Sign-in methods, tried in order: (1) embedded webview, (2) installed Edge/Chrome with a throwaway profile read over the DevTools protocol, (3) pasted `Cookie` header. | Conditional Access may block embedded webviews. |
| D4 | Zero config for users. The tenant host is taken from the pasted URL. Sessions live in the OS keychain, keyed by host. Settings are edited in the GUI and saved to `settings.toml` in the per-user config dir. | Non-developers must never edit a file. |
| D5 | No hardcoded tenants, sites, URLs or company names anywhere in code. | Public, cross-org repo. |
| D6 | Files are written to `<name>.ampsync-part`, downloaded in parallel byte ranges, verified (QuickXorHash when available, else size + unchanged eTag), then atomically renamed. | Corruption-proof, resumable. |
| D7 | Job state lives in SQLite (`<dest>/.ampsync/state.db`). | Survives crashes and reboots; lets a rerun resume. |
| D8 | Throttling: honour `Retry-After` on 429/503 globally (all lanes pause), exponential backoff with jitter otherwise. User-Agent `NONISV|AmpSync|AmpSync/<version>`. | Microsoft guidance. |
| D9 | Expired session pauses the job (never fails it) and emits `Event::AuthExpired`. | 80 GB jobs outlive sessions. |
| D10 | Platforms: Windows x64 and Ubuntu x64. No macOS in v1. No code signing, no auto-update, no telemetry. | Agreed scope. |
| D11 | GUI is Tauri 2 + Svelte 5, following `design/prototype.html` (approved). | |
| D13 | First launch shows a skippable walkthrough: a dimmed overlay spotlights one control at a time with a short text box (Back / Next / Skip, step counter). It is replayable from Settings. The "seen" flag lives in `settings.toml`. | Non-developer users. |
| D14 | Commits and PRs never credit Claude (no co-author trailers or "generated with" lines). | Public contributor list must show humans only. |
| D12 | Contributors list is generated from the GitHub API at build time; repo slug comes from `AMPSYNC_REPO` / `GITHUB_REPOSITORY`, never hardcoded. | Forks show their own contributors; works offline. |

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
design/prototype.html      approved visual spec
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
pub struct BrowserSignIn { pub kind: BrowserKind }
pub struct PastedSignIn { pub cookie_header: String }
```

### `remote` — talking to SharePoint

One trait, two implementations. Which one is the default is decided by the probe results (`spikes/probe`).

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
}
pub struct V2Remote;       // /_api/v2.0 (OneDrive-style API hosted on SharePoint)
pub struct ClassicRemote;  // /_api/web + /_layouts/15/download.aspx
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
pub struct Plan { pub root: RemoteRoot, pub items: Vec<PlannedFile>, pub totals: Totals }
pub enum PlannedAction { Download, Resume { have: u64 }, SkipUnchanged }
```

### `state` — SQLite job state

Tables: `job` (spec, root, created/updated), `file` (rel_path PK, size, etag, hash, status, bytes_done, verified_by, error). Opened with WAL. All writes go through one task to avoid lock contention.

### `engine` — the transfer engine

```rust
pub struct Settings {
    pub parallel_files: u8,            // default 4, 1..=16
    pub range_size: u64,               // default 8 MiB
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
    FileStarted { rel_path: RelPath, size: u64, lane: u8 },
    Progress { bytes_done: u64, bytes_total: u64, rate_bps: u64 }, // throttled to ~4 Hz
    LaneProgress { lane: u8, rel_path: RelPath, offset: u64, size: u64 },
    FileVerified { rel_path: RelPath, by: VerifiedBy },             // Hash | SizeAndEtag
    FileSkipped { rel_path: RelPath },
    FileFailed { rel_path: RelPath, error: String, will_retry: bool },
    Throttled { retry_after: Duration },
    AuthExpired { host: String },                                   // job is now paused
    Paused, Resumed,
    Finished(Summary),
}
```

Engine rules:
- Files ≤ `large_file_threshold` are one request each; larger files are split into `range_size` ranges spread across lanes. Total in-flight requests never exceed `parallel_files`.
- Each range is written with positional writes into the preallocated `.ampsync-part`; completed ranges are recorded in `state.db` so a restart only fetches missing ranges.
- After the last byte: verify hash if present, else size + `stat()` eTag unchanged. Then rename over the destination. On mismatch: delete the part file and retry the file once, then `FileFailed`.
- Windows: use `\\?\` long paths; replace characters illegal on Windows (`<>:"|?*` and trailing dots/spaces) with a visible substitute and record the mapping in `state.db`.

## `ampsync-cli`

```
ampsync login <url> [--method webview|edge|chrome|paste]
ampsync logout <host>
ampsync sessions
ampsync ls <url> [--depth N]
ampsync get <url> <dest> [--include GLOB]... [--exclude GLOB]... [--parallel N]
                         [--limit RATE] [--redownload] [--prune] [--yes] [--json]
```
Exit codes: 0 ok, 1 error, 2 usage, 3 auth expired (rerun resumes), 4 some files failed. `--json` emits one `Event` per line for scripting. The CLI's webview sign-in uses `wry`/`tao` directly behind a `webview` cargo feature (on by default) so headless builds can opt out.

## `ampsync-desktop`

Tauri 2 commands mirror the engine (`resolve`, `plan`, `enqueue`, `pause`, `resume`, `cancel`, `sign_in`, `sign_out`, `sessions`, `get_settings`, `set_settings`, `contributors`). Engine events are forwarded to the frontend as Tauri events. A queue runs jobs one at a time. The frontend reproduces `design/prototype.html`: views Transfers, New download, Sign-ins, Settings, About, plus the first-run walkthrough (steps defined as data: target selector, view to show, title, body), skippable at any step and replayable from Settings.

## Testing

- Unit tests in every core module.
- Integration tests run the engine against a mock SharePoint server (`wiremock` or an `axum` fake) that supports both remote APIs, Range requests, 429 with `Retry-After`, auth expiry mid-job, eTag change mid-job, and corrupted bytes.
- CI: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test` on `windows-latest` and `ubuntu-24.04`.
