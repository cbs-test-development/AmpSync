//! Talking to SharePoint.
//!
//! One trait, [`Remote`]. v1 implements [`ClassicRemote`] only (D18): the classic REST API for
//! listing and `download.aspx` for byte ranges. Facts measured on a real tenant that the
//! implementation must respect:
//!
//! - Sharing links resolve by following the 302 (cookie jar on) to
//!   `.../_layouts/15/onedrive.aspx?id=<server-relative folder path>`. OneDrive personal sites use
//!   `/personal/<user>/Documents/...`; team sites `/sites/<Site>/<Library>/...`.
//! - Listing: `GET {site}/_api/web/GetFolderByServerRelativePath(decodedurl='{path}')/Files?$select=Name,Length,TimeLastModified,ETag,UniqueId,ServerRelativeUrl`
//!   and `/Folders?$select=Name,ServerRelativeUrl,UniqueId,ItemCount`, with header
//!   `Accept: application/json;odata=nometadata`. `Length` arrives as a string. Paths are
//!   percent-encoded with `'` doubled (see [`crate::link::odata_string_arg`]).
//! - Download: `GET {site}/_layouts/15/download.aspx?UniqueId={guid}` with `Range: bytes=a-b`
//!   answers 206 with `Content-Range`. Always check status 206 and that `Content-Range` matches
//!   before writing; a 200 means the server ignored Range (treat as an error).
//! - Auth expiry shows up as 401/403 or a redirect to `login.microsoftonline.com`. API calls must
//!   not follow redirects off the tenant host; map those cases to `Error::AuthExpired`.
//! - 429/503: parse `Retry-After` into `Error::Throttled`.
//! - Every request sends User-Agent `NONISV|AmpSync|AmpSync/<version>` (D8).

use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::{BoxStream, FuturesUnordered};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio::sync::Semaphore;
use url::Url;

use crate::Error;
use crate::auth::Session;
use crate::link::{self, LinkKind, ParsedLink, odata_string_arg, split_site};
use crate::plan::RelPath;

/// The folder a job downloads, after links have been resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteRoot {
    /// The web that owns the folder, e.g. `https://contoso.sharepoint.com/sites/Team`.
    pub site_url: Url,
    /// Graph/v2 drive id; always `None` with `ClassicRemote`.
    pub drive_id: Option<String>,
    /// Graph/v2 item id; always `None` with `ClassicRemote`.
    pub item_id: Option<String>,
    /// Decoded server-relative path of the folder, e.g. `/sites/Team/Shared Documents/Projects`.
    pub server_relative_path: String,
    /// Last path segment, for display and for naming the destination folder.
    pub display_name: String,
}

/// Hash reported by the server for a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "algo", content = "value", rename_all = "snake_case")]
pub enum RemoteHash {
    /// Base64 QuickXorHash (see [`crate::hash`]).
    QuickXor(String),
    /// Lower-case hex SHA-256.
    Sha256(String),
}

/// How to download one file. Opaque to everything except the `Remote` that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "api", rename_all = "snake_case")]
#[non_exhaustive]
pub enum FileLocator {
    /// `{site_url}/_layouts/15/download.aspx?UniqueId={unique_id}`
    Classic {
        site_url: Url,
        unique_id: String,
        /// Decoded server-relative URL of the file, for `stat`.
        server_relative_url: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemoteFile {
    /// Relative to the root, `/`-separated.
    pub rel_path: RelPath,
    pub size: u64,
    /// Opaque version tag; unchanged eTag means unchanged content (D6, D18).
    pub etag: String,
    #[serde(with = "time::serde::rfc3339")]
    pub modified: OffsetDateTime,
    /// `None` with `ClassicRemote` (D18).
    pub hash: Option<RemoteHash>,
    pub locator: FileLocator,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // shape fixed by the architecture contract
pub enum RemoteEntry {
    File(RemoteFile),
    Folder { rel_path: RelPath },
}

#[async_trait]
pub trait Remote: Send + Sync {
    /// Turn a parsed link into a concrete folder. For `LinkKind::Sharing` this follows the
    /// redirect chain (cookies on, off-tenant hops are `AuthExpired`); for `LinkKind::Folder` it
    /// verifies the folder exists (`NotFound` otherwise) and fills `display_name`.
    async fn resolve(&self, link: &ParsedLink) -> Result<RemoteRoot, Error>;

    /// Recursive listing, streamed so huge trees start showing immediately. Yields every folder
    /// (parents before children) and every file under `root`. An error ends the stream after
    /// being yielded.
    fn walk<'a>(&'a self, root: &'a RemoteRoot) -> BoxStream<'a, Result<RemoteEntry, Error>>;

    /// Bytes `range` of a file. Must return exactly that range or an error: callers rely on the
    /// stream yielding `range.end - range.start` bytes in total.
    async fn read_range(
        &self,
        file: &RemoteFile,
        range: Range<u64>,
    ) -> Result<BoxStream<'static, Result<Bytes, Error>>, Error>;

    /// Re-fetch metadata of one file (used to confirm the eTag didn't change). `NotFound` if the
    /// file was deleted.
    async fn stat(&self, file: &RemoteFile) -> Result<RemoteFile, Error>;

    /// Swap the cookies used for all later requests (after a re-sign-in). Called by
    /// `JobHandle::replace_session`. The default does nothing, which suits test doubles.
    fn replace_session(&self, session: Session) {
        let _ = session;
    }
}

/// User-Agent sent with every request (D8).
const USER_AGENT: &str = concat!("NONISV|AmpSync|AmpSync/", env!("CARGO_PKG_VERSION"));
/// `Retry-After` used when a 429/503 carries no usable header.
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(10);
/// Upper bound for a server-supplied `Retry-After`.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60 * 60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const READ_TIMEOUT: Duration = Duration::from_secs(120);
/// Listing requests in flight at once (D8: stay well under SharePoint's throttling limits).
const LISTING_CONCURRENCY: usize = 4;
/// Folders being listed at the same time (each needs a Files and a Folders request).
const FOLDERS_IN_FLIGHT: usize = 8;
const MAX_REDIRECTS: usize = 10;
const SELECT_FILE: &str = "Name,Length,TimeLastModified,ETag,UniqueId,ServerRelativeUrl";
const SELECT_FOLDER: &str = "Name,ServerRelativeUrl,UniqueId,ItemCount";

/// The two clients used for one session: one follows redirects inside the tenant host, one never
/// follows (used to read the redirect chain of sharing links).
#[derive(Clone)]
struct Clients {
    follow: reqwest::Client,
    manual: reqwest::Client,
}

fn build_clients(session: &Session) -> Clients {
    Clients {
        follow: build_client(session, true),
        manual: build_client(session, false),
    }
}

fn build_client(session: &Session, follow: bool) -> reqwest::Client {
    let jar = Arc::new(reqwest::cookie::Jar::default());
    let host = session.host.to_ascii_lowercase();
    if let Ok(base) = Url::parse(&format!("https://{host}/")) {
        let now = OffsetDateTime::now_utc();
        for c in &session.cookies {
            if c.expires.is_some_and(|e| e <= now) {
                continue;
            }
            // Host-only cookie (no Domain attribute): it is never sent to any other host.
            let path = if c.path.is_empty() { "/" } else { c.path.as_str() };
            jar.add_cookie_str(&format!("{}={}; Path={path}", c.name, c.value), &base);
        }
    }
    let policy = if follow {
        reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error("too many redirects");
            }
            let next = attempt.url();
            let downgrade =
                attempt.previous().last().is_some_and(|p| p.scheme() == "https") && next.scheme() != "https";
            let same_host = next.host_str().is_some_and(|h| h.eq_ignore_ascii_case(&host));
            if same_host && !downgrade {
                attempt.follow()
            } else {
                // The 3xx response is handed back; the caller maps it to `AuthExpired`.
                attempt.stop()
            }
        })
    } else {
        reqwest::redirect::Policy::none()
    };
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .cookie_provider(jar)
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .redirect(policy)
        // Real parallelism: several connections instead of one multiplexed HTTP/2 connection.
        .http1_only()
        .build()
        .expect("HTTP client configuration is valid")
}

/// An HTTP client for `session`: cookie jar pre-loaded with the session cookies (host-only, so
/// they are never sent elsewhere), the AmpSync User-Agent, connect timeout 20 s, read timeout
/// 120 s. Redirects are followed only within the tenant host; a redirect elsewhere (for example
/// to `login.microsoftonline.com`) is returned as the 3xx response so the caller can report
/// [`Error::AuthExpired`].
pub fn http_client(session: &Session) -> reqwest::Client {
    build_client(session, true)
}

/// v1 remote: `/_api/web` listing plus `/_layouts/15/download.aspx` ranges.
///
/// Construct with [`ClassicRemote::new`]. Internally holds swappable `reqwest::Client`s so
/// [`Remote::replace_session`] takes effect for in-flight jobs (requests already running finish on
/// the old cookies; every later request uses the new ones).
///
/// `V2Remote` (`/_api/v2.x`) is not implemented: it needs a bearer token we cannot get from
/// cookies (D18).
pub struct ClassicRemote {
    clients: std::sync::RwLock<Clients>,
    /// Host that cookies are bound to and that redirects must stay on.
    host: String,
    /// The tenant host from the session (differs from `host` only with an origin override).
    public_host: String,
    /// Send every request here instead of to the address in the link (tests, local proxies).
    origin: Option<Url>,
}

impl ClassicRemote {
    /// Build a remote that authenticates with `session`'s cookies.
    pub fn new(session: &Session) -> Self {
        ClassicRemote {
            clients: std::sync::RwLock::new(build_clients(session)),
            host: session.host.to_ascii_lowercase(),
            public_host: session.host.to_ascii_lowercase(),
            origin: None,
        }
    }

    /// Like [`ClassicRemote::new`], but every request goes to `origin` (scheme, host and port)
    /// instead of the address in the link; paths and queries are unchanged and the session cookies
    /// are bound to the origin's host. For integration tests against a local fake server and for
    /// debugging through a local proxy. Not needed in normal use.
    pub fn with_origin(session: &Session, origin: Url) -> Self {
        let host = origin.host_str().unwrap_or_default().to_ascii_lowercase();
        let mut remote = ClassicRemote::new(&Self::rebound(session, &host));
        remote.public_host = session.host.to_ascii_lowercase();
        remote.origin = Some(origin);
        remote
    }

    fn rebound(session: &Session, host: &str) -> Session {
        Session {
            host: host.to_string(),
            ..session.clone()
        }
    }

    /// Apply the origin override to a request address.
    fn target(&self, mut url: Url) -> Url {
        if let Some(o) = &self.origin {
            let _ = url.set_scheme(o.scheme());
            let _ = url.set_host(o.host_str());
            let _ = url.set_port(o.port());
        }
        url
    }

    /// The address as the tenant would see it (undo the origin override), for link parsing.
    fn canonical(&self, url: &Url) -> Url {
        let mut u = url.clone();
        if self.origin.is_some() {
            let _ = u.set_scheme("https");
            let _ = u.set_host(Some(&self.public_host));
            let _ = u.set_port(None);
        }
        u
    }

    fn clients(&self) -> Clients {
        self.clients.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn auth_expired(&self) -> Error {
        Error::AuthExpired {
            host: self.host.clone(),
        }
    }

    fn same_host(&self, url: &Url) -> bool {
        url.host_str().is_some_and(|h| h.eq_ignore_ascii_case(&self.host))
    }

    /// Send a request and map transport errors and non-success statuses to [`Error`].
    async fn send(&self, req: reqwest::RequestBuilder, context: &str) -> Result<reqwest::Response, Error> {
        let resp = req.send().await.map_err(net_err)?;
        self.check(resp, context)
    }

    /// Map an HTTP response to `Ok` (2xx) or the matching [`Error`].
    fn check(&self, resp: reqwest::Response, context: &str) -> Result<reqwest::Response, Error> {
        let status = resp.status().as_u16();
        if resp.status().is_success() {
            // SharePoint answers an expired session with a 200 sign-in page for some endpoints.
            if is_html(resp.headers()) {
                return Err(self.auth_expired());
            }
            return Ok(resp);
        }
        match status {
            401 | 403 => Err(self.auth_expired()),
            429 | 503 => Err(Error::Throttled {
                retry_after: parse_retry_after(resp.headers(), OffsetDateTime::now_utc()),
            }),
            404 => Err(Error::NotFound(context.to_string())),
            300..=399 => {
                let to_login = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|l| resp.url().join(l).ok())
                    .is_some_and(|u| is_login_host(&u));
                if to_login {
                    Err(self.auth_expired())
                } else {
                    Err(Error::Http {
                        status,
                        context: format!("{context}: unexpected redirect"),
                    })
                }
            }
            _ => Err(Error::Http {
                status,
                context: context.to_string(),
            }),
        }
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: Url, context: &str) -> Result<T, Error> {
        let req = self
            .clients()
            .follow
            .get(self.target(url))
            .header(reqwest::header::ACCEPT, "application/json;odata=nometadata");
        let resp = self.send(req, context).await?;
        let body = resp.bytes().await.map_err(net_err)?;
        serde_json::from_slice(&body).map_err(|e| {
            if body.first() == Some(&b'<') {
                self.auth_expired()
            } else {
                Error::Http {
                    status: 200,
                    context: format!("{context}: unreadable response ({e})"),
                }
            }
        })
    }

    fn api_url(site: &Url, tail: &str) -> Result<Url, Error> {
        let base = site.as_str().trim_end_matches('/');
        Url::parse(&format!("{base}/_api/web/{tail}"))
            .map_err(|e| Error::InvalidLink(format!("cannot build request address: {e}")))
    }

    fn folder_url(site: &Url, server_path: &str, suffix: &str) -> Result<Url, Error> {
        Self::api_url(
            site,
            &format!(
                "GetFolderByServerRelativePath(decodedurl='{}'){suffix}",
                odata_string_arg(server_path)
            ),
        )
    }

    /// Verify the folder exists and build the [`RemoteRoot`].
    async fn root_for(&self, site_url: &Url, server_path: &str) -> Result<RemoteRoot, Error> {
        let url = Self::folder_url(site_url, server_path, "?$select=Name,ServerRelativeUrl")?;
        let info: FolderInfo = self.get_json(url, server_path).await?;
        let last = server_path.rsplit('/').find(|s| !s.is_empty()).unwrap_or("");
        let display_name = info
            .name
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| last.to_string());
        Ok(RemoteRoot {
            site_url: site_url.clone(),
            drive_id: None,
            item_id: None,
            server_relative_path: server_path.trim_end_matches('/').to_string(),
            display_name,
        })
    }

    /// Follow a sharing link's redirect chain by hand until it names a folder path.
    async fn resolve_sharing(&self, start: &Url) -> Result<RemoteRoot, Error> {
        let client = self.clients().manual;
        let mut url = self.target(start.clone());
        for _ in 0..MAX_REDIRECTS {
            let resp = client
                .get(url.clone())
                .header(reqwest::header::ACCEPT, "text/html,application/xhtml+xml")
                .send()
                .await
                .map_err(net_err)?;
            let status = resp.status().as_u16();
            if (300..400).contains(&status) {
                let next = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|l| url.join(l).ok())
                    .ok_or_else(|| Error::Http {
                        status,
                        context: "sharing link redirect without a usable Location".into(),
                    })?;
                if !self.same_host(&next) {
                    return Err(if is_login_host(&next) {
                        self.auth_expired()
                    } else {
                        Error::Http {
                            status,
                            context: "the sharing link redirects to a different site".into(),
                        }
                    });
                }
                if let Ok(ParsedLink {
                    kind:
                        LinkKind::Folder {
                            site_url,
                            server_relative_path,
                        },
                    ..
                }) = link::parse(self.canonical(&next).as_str())
                {
                    return self.root_for(&site_url, &server_relative_path).await;
                }
                url = next;
                continue;
            }
            return match self.check(resp, "sharing link") {
                Ok(_) => Err(Error::InvalidLink(
                    "the sharing link does not lead to a folder".into(),
                )),
                Err(e) => Err(e),
            };
        }
        Err(Error::Http {
            status: 310,
            context: "too many redirects while opening the sharing link".into(),
        })
    }

    /// All pages of one listing endpoint (`/Files` or `/Folders`).
    async fn list_pages<T: serde::de::DeserializeOwned>(
        &self,
        sem: &Semaphore,
        first: Url,
        context: &str,
    ) -> Result<Vec<T>, Error> {
        let mut out = Vec::new();
        let mut next = Some(first);
        while let Some(url) = next.take() {
            let _permit = sem.acquire().await.map_err(|_| Error::Cancelled)?;
            let page: Page<T> = self.get_json(url, context).await?;
            out.extend(page.value);
            if let Some(link) = page.next_link.filter(|l| !l.is_empty()) {
                let u = Url::parse(&link).map_err(|_| Error::Http {
                    status: 200,
                    context: format!("{context}: invalid paging link"),
                })?;
                if !self.same_host(&u) {
                    return Err(Error::Http {
                        status: 200,
                        context: format!("{context}: paging link leaves the site"),
                    });
                }
                next = Some(u);
            }
        }
        Ok(out)
    }

    async fn list_dir(&self, sem: &Semaphore, site: &Url, dir: &DirToList) -> Result<DirListing, Error> {
        let files_url = Self::folder_url(site, &dir.server_path, &format!("/Files?$select={SELECT_FILE}"))?;
        let folders_url = Self::folder_url(
            site,
            &dir.server_path,
            &format!("/Folders?$select={SELECT_FOLDER}"),
        )?;
        let (files, folders) = futures::future::try_join(
            self.list_pages::<FileItem>(sem, files_url, &dir.server_path),
            self.list_pages::<FolderItem>(sem, folders_url, &dir.server_path),
        )
        .await?;
        Ok(DirListing {
            dir: dir.clone(),
            files,
            folders,
        })
    }
}

fn net_err(e: reqwest::Error) -> Error {
    let text = e.without_url().to_string();
    Error::Network(text)
}

fn is_html(headers: &reqwest::header::HeaderMap) -> bool {
    headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().starts_with("text/html"))
}

/// Microsoft sign-in hosts: a redirect here means the session is no longer accepted.
fn is_login_host(url: &Url) -> bool {
    url.host_str().is_some_and(|h| {
        let h = h.to_ascii_lowercase();
        h.starts_with("login.")
            || h.ends_with(".microsoftonline.com")
            || h.ends_with(".microsoftonline.us")
            || h.ends_with(".microsoftonline.cn")
            || h == "login.live.com"
            || h.ends_with(".windows.net")
    })
}

/// `Retry-After` as a delay: delta-seconds or an HTTP date; 10 s when absent or unreadable.
fn parse_retry_after(headers: &reqwest::header::HeaderMap, now: OffsetDateTime) -> Duration {
    let Some(raw) = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
    else {
        return DEFAULT_RETRY_AFTER;
    };
    if let Ok(secs) = raw.parse::<u64>() {
        return Duration::from_secs(secs).min(MAX_RETRY_AFTER);
    }
    if let Ok(when) = httpdate::parse_http_date(raw) {
        let when = OffsetDateTime::from(when);
        let delta = when - now;
        return if delta.is_negative() {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(delta.as_seconds_f64()).min(MAX_RETRY_AFTER)
        };
    }
    DEFAULT_RETRY_AFTER
}

/// `bytes a-b/total` -> `(a, b)`.
fn parse_content_range(raw: &str) -> Option<(u64, u64)> {
    let rest = raw.trim().strip_prefix("bytes")?.trim_start();
    let (span, _total) = rest.split_once('/')?;
    let (a, b) = span.split_once('-')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

#[derive(Deserialize)]
struct Page<T> {
    value: Vec<T>,
    #[serde(default, rename = "odata.nextLink", alias = "@odata.nextLink")]
    next_link: Option<String>,
}

#[derive(Deserialize)]
struct FolderInfo {
    #[serde(rename = "Name")]
    name: Option<String>,
}

#[derive(Deserialize)]
struct FolderItem {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "ServerRelativeUrl")]
    server_relative_url: Option<String>,
}

#[derive(Deserialize)]
struct FileItem {
    #[serde(rename = "Name")]
    name: String,
    /// A string on real tenants, but accept a number too.
    #[serde(rename = "Length")]
    length: serde_json::Value,
    #[serde(rename = "TimeLastModified")]
    modified: Option<String>,
    #[serde(rename = "ETag")]
    etag: Option<String>,
    #[serde(rename = "UniqueId")]
    unique_id: String,
    #[serde(rename = "ServerRelativeUrl")]
    server_relative_url: Option<String>,
}

impl FileItem {
    fn into_remote(self, rel_path: RelPath, site: &Url, dir_server_path: &str) -> Result<RemoteFile, Error> {
        let size = match &self.length {
            serde_json::Value::String(s) => s.trim().parse::<u64>().ok(),
            serde_json::Value::Number(n) => n.as_u64(),
            _ => None,
        }
        .ok_or_else(|| Error::Http {
            status: 200,
            context: format!("listing of {dir_server_path}: unreadable file size"),
        })?;
        let modified = self
            .modified
            .as_deref()
            .and_then(|s| OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok())
            .unwrap_or_else(OffsetDateTime::now_utc);
        let unique_id = self
            .unique_id
            .trim_matches(|c| c == '{' || c == '}')
            .to_ascii_lowercase();
        let server_relative_url = self
            .server_relative_url
            .unwrap_or_else(|| format!("{}/{}", dir_server_path.trim_end_matches('/'), self.name));
        Ok(RemoteFile {
            rel_path,
            size,
            etag: self.etag.unwrap_or_default(),
            modified,
            hash: None,
            locator: FileLocator::Classic {
                site_url: site.clone(),
                unique_id,
                server_relative_url,
            },
        })
    }
}

#[derive(Clone)]
struct DirToList {
    /// `None` for the root.
    rel: Option<RelPath>,
    server_path: String,
}

struct DirListing {
    dir: DirToList,
    files: Vec<FileItem>,
    folders: Vec<FolderItem>,
}

struct WalkState<'a> {
    remote: &'a ClassicRemote,
    root: &'a RemoteRoot,
    sem: Arc<Semaphore>,
    queue: VecDeque<DirToList>,
    in_flight: FuturesUnordered<BoxFuture<'a, Result<DirListing, Error>>>,
    out: VecDeque<RemoteEntry>,
    /// The library root holds a system folder `Forms` that is not user content.
    library_root: String,
    done: bool,
}

impl<'a> WalkState<'a> {
    fn new(remote: &'a ClassicRemote, root: &'a RemoteRoot) -> Self {
        let library_root = split_site(&root.server_relative_path)
            .map(|s| {
                let site = s.site.trim_end_matches('/');
                format!("{site}/{}", s.library)
            })
            .unwrap_or_default();
        let mut queue = VecDeque::new();
        queue.push_back(DirToList {
            rel: None,
            server_path: root.server_relative_path.clone(),
        });
        WalkState {
            remote,
            root,
            sem: Arc::new(Semaphore::new(LISTING_CONCURRENCY)),
            queue,
            in_flight: FuturesUnordered::new(),
            out: VecDeque::new(),
            library_root,
            done: false,
        }
    }

    fn absorb(&mut self, listing: DirListing) -> Result<(), Error> {
        let DirListing { dir, files, folders } = listing;
        let at_library_root = dir
            .server_path
            .trim_end_matches('/')
            .eq_ignore_ascii_case(&self.library_root);
        for f in folders {
            if at_library_root && f.name.eq_ignore_ascii_case("Forms") {
                continue;
            }
            let Some(rel) = child_rel(dir.rel.as_ref(), &f.name) else {
                tracing::warn!("skipping folder with an unusable name");
                continue;
            };
            let server_path = f
                .server_relative_url
                .unwrap_or_else(|| format!("{}/{}", dir.server_path.trim_end_matches('/'), f.name));
            self.out.push_back(RemoteEntry::Folder {
                rel_path: rel.clone(),
            });
            self.queue.push_back(DirToList {
                rel: Some(rel),
                server_path,
            });
        }
        for f in files {
            let Some(rel) = child_rel(dir.rel.as_ref(), &f.name) else {
                tracing::warn!("skipping file with an unusable name");
                continue;
            };
            let file = f.into_remote(rel, &self.root.site_url, &dir.server_path)?;
            self.out.push_back(RemoteEntry::File(file));
        }
        Ok(())
    }
}

fn child_rel(parent: Option<&RelPath>, name: &str) -> Option<RelPath> {
    match parent {
        Some(p) => p.join(name).ok(),
        None => RelPath::new(name).ok(),
    }
}

#[async_trait]
impl Remote for ClassicRemote {
    async fn resolve(&self, link: &ParsedLink) -> Result<RemoteRoot, Error> {
        match &link.kind {
            LinkKind::Sharing { url } => self.resolve_sharing(url).await,
            LinkKind::Folder {
                site_url,
                server_relative_path,
            } => self.root_for(site_url, server_relative_path).await,
        }
    }

    fn walk<'a>(&'a self, root: &'a RemoteRoot) -> BoxStream<'a, Result<RemoteEntry, Error>> {
        futures::stream::unfold(WalkState::new(self, root), |mut st| async move {
            loop {
                if let Some(entry) = st.out.pop_front() {
                    return Some((Ok(entry), st));
                }
                if st.done {
                    return None;
                }
                while st.in_flight.len() < FOLDERS_IN_FLIGHT {
                    let Some(dir) = st.queue.pop_front() else { break };
                    let remote = st.remote;
                    let sem = st.sem.clone();
                    let site = st.root.site_url.clone();
                    st.in_flight
                        .push(Box::pin(async move { remote.list_dir(&sem, &site, &dir).await }));
                }
                match st.in_flight.next().await {
                    None => return None,
                    Some(Ok(listing)) => {
                        if let Err(e) = st.absorb(listing) {
                            st.done = true;
                            return Some((Err(e), st));
                        }
                    }
                    Some(Err(e)) => {
                        st.done = true;
                        return Some((Err(e), st));
                    }
                }
            }
        })
        .boxed()
    }

    async fn read_range(
        &self,
        file: &RemoteFile,
        range: Range<u64>,
    ) -> Result<BoxStream<'static, Result<Bytes, Error>>, Error> {
        if range.start >= range.end {
            return Ok(futures::stream::empty().boxed());
        }
        let FileLocator::Classic {
            site_url, unique_id, ..
        } = &file.locator;
        let base = site_url.as_str().trim_end_matches('/');
        let url = Url::parse(&format!(
            "{base}/_layouts/15/download.aspx?UniqueId={}",
            utf8_percent_encode(unique_id, NON_ALPHANUMERIC)
        ))
        .map_err(|e| Error::InvalidLink(format!("cannot build download address: {e}")))?;
        let last = range.end - 1;
        let req = self
            .clients()
            .follow
            .get(self.target(url))
            .header(reqwest::header::RANGE, format!("bytes={}-{last}", range.start))
            .header(reqwest::header::ACCEPT_ENCODING, "identity");
        let resp = self.send(req, file.rel_path.as_str()).await?;
        let status = resp.status().as_u16();
        if status != 206 {
            // 200 means the server ignored Range: its body is the whole file, not our range.
            return Err(Error::Http {
                status,
                context: format!("{}: the server ignored the Range request", file.rel_path),
            });
        }
        let content_range = resp
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_content_range);
        if content_range != Some((range.start, last)) {
            return Err(Error::Http {
                status,
                context: format!(
                    "{}: the server answered with a different byte range",
                    file.rel_path
                ),
            });
        }
        Ok(exact_body(resp, range.end - range.start))
    }

    async fn stat(&self, file: &RemoteFile) -> Result<RemoteFile, Error> {
        let FileLocator::Classic {
            site_url,
            unique_id,
            server_relative_url,
        } = &file.locator;
        let url = Self::api_url(
            site_url,
            &format!("GetFileById('{unique_id}')?$select={SELECT_FILE}"),
        )?;
        let item: FileItem = self.get_json(url, file.rel_path.as_str()).await?;
        let dir = server_relative_url
            .rsplit_once('/')
            .map(|(d, _)| d.to_string())
            .unwrap_or_default();
        let mut fresh = item.into_remote(file.rel_path.clone(), site_url, &dir)?;
        fresh.locator = file.locator.clone();
        Ok(fresh)
    }

    fn replace_session(&self, session: Session) {
        let session = if self.origin.is_some() {
            Self::rebound(&session, &self.host)
        } else {
            session
        };
        *self.clients.write().unwrap_or_else(|p| p.into_inner()) = build_clients(&session);
    }
}

/// The response body as a stream that errors unless it delivers exactly `expected` bytes.
fn exact_body(resp: reqwest::Response, expected: u64) -> BoxStream<'static, Result<Bytes, Error>> {
    let inner = Box::pin(resp.bytes_stream());
    futures::stream::unfold(
        (inner, 0u64, false),
        move |(mut inner, mut got, finished)| async move {
            if finished {
                return None;
            }
            match inner.next().await {
                Some(Ok(chunk)) => {
                    got += chunk.len() as u64;
                    if got > expected {
                        let e = Error::Network("the server sent more data than requested".into());
                        Some((Err(e), (inner, got, true)))
                    } else {
                        Some((Ok(chunk), (inner, got, false)))
                    }
                }
                Some(Err(e)) => Some((Err(net_err(e)), (inner, got, true))),
                None if got == expected => None,
                None => {
                    let e = Error::Network(format!("the connection ended after {got} of {expected} bytes"));
                    Some((Err(e), (inner, got, true)))
                }
            }
        },
    )
    .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};

    fn headers(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, HeaderValue::from_str(v).unwrap());
        h
    }

    #[test]
    fn retry_after_seconds_date_and_fallbacks() {
        let now = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
        assert_eq!(parse_retry_after(&headers("7"), now), Duration::from_secs(7));
        assert_eq!(parse_retry_after(&headers(" 0 "), now), Duration::ZERO);
        assert_eq!(parse_retry_after(&HeaderMap::new(), now), DEFAULT_RETRY_AFTER);
        assert_eq!(parse_retry_after(&headers("soon"), now), DEFAULT_RETRY_AFTER);
        assert_eq!(parse_retry_after(&headers("999999"), now), MAX_RETRY_AFTER);
        let at = httpdate::fmt_http_date((now + time::Duration::seconds(30)).into());
        let d = parse_retry_after(&headers(&at), now);
        assert!((29..=30).contains(&d.as_secs()), "{d:?}");
        let past = httpdate::fmt_http_date((now - time::Duration::seconds(30)).into());
        assert_eq!(parse_retry_after(&headers(&past), now), Duration::ZERO);
    }

    #[test]
    fn content_range_parsing() {
        assert_eq!(parse_content_range("bytes 0-99/1000"), Some((0, 99)));
        assert_eq!(parse_content_range("bytes 5-9/*"), Some((5, 9)));
        assert_eq!(parse_content_range("bytes */1000"), None);
        assert_eq!(parse_content_range("items 0-9/10"), None);
        assert_eq!(parse_content_range("bytes a-b/10"), None);
    }

    #[test]
    fn login_hosts() {
        let u = |s: &str| Url::parse(s).unwrap();
        assert!(is_login_host(&u("https://login.microsoftonline.com/x")));
        assert!(is_login_host(&u("https://login.live.com/")));
        assert!(is_login_host(&u("https://sts.windows.net/")));
        assert!(!is_login_host(&u("https://contoso.sharepoint.com/")));
    }

    #[test]
    fn file_item_accepts_string_and_number_lengths() {
        let json = r#"{"value":[
            {"Name":"a.txt","Length":"12","TimeLastModified":"2024-05-01T10:20:30Z","ETag":"\"{ABC},3\"","UniqueId":"{AbC-1}","ServerRelativeUrl":"/sites/T/D/a.txt"},
            {"Name":"b.txt","Length":7,"UniqueId":"u2"}]}"#;
        let page: Page<FileItem> = serde_json::from_str(json).unwrap();
        let site = Url::parse("https://contoso.sharepoint.com/sites/T").unwrap();
        let mut it = page.value.into_iter();
        let a = it
            .next()
            .unwrap()
            .into_remote(RelPath::new("a.txt").unwrap(), &site, "/sites/T/D")
            .unwrap();
        assert_eq!(a.size, 12);
        assert_eq!(a.etag, "\"{ABC},3\"");
        assert_eq!(a.modified.unix_timestamp(), 1_714_558_830);
        let FileLocator::Classic { unique_id, .. } = &a.locator;
        assert_eq!(unique_id, "abc-1");
        let b = it
            .next()
            .unwrap()
            .into_remote(RelPath::new("b.txt").unwrap(), &site, "/sites/T/D")
            .unwrap();
        assert_eq!(b.size, 7);
        let FileLocator::Classic {
            server_relative_url, ..
        } = &b.locator;
        assert_eq!(server_relative_url, "/sites/T/D/b.txt");
    }

    #[test]
    fn next_link_in_both_spellings() {
        let a: Page<FileItem> =
            serde_json::from_str(r#"{"value":[],"odata.nextLink":"https://x/y"}"#).unwrap();
        assert_eq!(a.next_link.as_deref(), Some("https://x/y"));
        let b: Page<FileItem> =
            serde_json::from_str(r#"{"value":[],"@odata.nextLink":"https://x/z"}"#).unwrap();
        assert_eq!(b.next_link.as_deref(), Some("https://x/z"));
    }

    #[test]
    fn user_agent_has_the_required_shape() {
        assert!(USER_AGENT.starts_with("NONISV|AmpSync|AmpSync/"));
    }
}
