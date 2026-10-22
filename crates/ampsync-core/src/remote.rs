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

use std::ops::Range;

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use url::Url;

use crate::Error;
use crate::auth::Session;
use crate::link::ParsedLink;
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

/// v1 remote: `/_api/web` listing plus `/_layouts/15/download.aspx` ranges.
///
/// Construct with [`ClassicRemote::new`]. Internally holds a swappable `reqwest::Client` so
/// [`Remote::replace_session`] takes effect for in-flight jobs.
///
/// `V2Remote` (`/_api/v2.x`) is not implemented: it needs a bearer token we cannot get from
/// cookies (D18).
#[allow(dead_code)] // stub: fields are used once the implementation lands
pub struct ClassicRemote {
    client: std::sync::RwLock<reqwest::Client>,
    host: String,
}

impl ClassicRemote {
    /// Build a remote that authenticates with `session`'s cookies.
    #[allow(unused_variables)]
    pub fn new(session: &Session) -> Self {
        todo!("build the client with http_client(session); remember session.host")
    }
}

#[async_trait]
#[allow(unused_variables)]
impl Remote for ClassicRemote {
    async fn resolve(&self, link: &ParsedLink) -> Result<RemoteRoot, Error> {
        todo!()
    }

    fn walk<'a>(&'a self, root: &'a RemoteRoot) -> BoxStream<'a, Result<RemoteEntry, Error>> {
        todo!()
    }

    async fn read_range(
        &self,
        file: &RemoteFile,
        range: Range<u64>,
    ) -> Result<BoxStream<'static, Result<Bytes, Error>>, Error> {
        todo!()
    }

    async fn stat(&self, file: &RemoteFile) -> Result<RemoteFile, Error> {
        todo!()
    }

    fn replace_session(&self, session: Session) {
        todo!()
    }
}

/// An HTTP client for `session`: cookie jar pre-loaded with the session cookies, the AmpSync
/// User-Agent, connect/read timeouts, redirects disabled (callers follow tenant-local redirects
/// themselves so they can detect a bounce to the login host).
#[allow(unused_variables)]
pub fn http_client(session: &Session) -> reqwest::Client {
    todo!()
}
