//! An in-process fake SharePoint: classic REST listing, `download.aspx` byte ranges, sharing-link
//! redirects, and switchable misbehaviour (429, 500, expired session, changing eTags, ignored
//! Range, hanging requests). Every request is logged so tests can assert what was fetched.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use ampsync_core::auth::{Session, SessionCookie, SignInMethod};
use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use percent_encoding::percent_decode_str;
use serde_json::json;
use time::OffsetDateTime;
use url::Url;

pub const LIBRARY: &str = "/sites/Team/Shared Documents";
pub const ROOT: &str = "/sites/Team/Shared Documents/Root";
/// The link tests pass as `JobSpec::source` (the `ClassicRemote` origin override redirects it here).
pub const FOLDER_LINK: &str = "https://contoso.sharepoint.com/sites/Team/Shared%20Documents/Root";
pub const SHARING_LINK: &str = "https://contoso.sharepoint.com/:f:/s/Team/Eabc123?e=xyz";
/// Cookie value the fake accepts initially, and the one it accepts after a simulated expiry.
pub const COOKIE_OLD: &str = "old";
pub const COOKIE_FRESH: &str = "fresh";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Logged {
    Sharing,
    FolderInfo(String),
    List {
        folder: String,
        what: String,
    },
    Stat(String),
    Download {
        key: String,
        start: u64,
        end: u64,
    },
    /// Request refused because the cookie is not accepted.
    Unauthorized,
}

struct FFile {
    data: Vec<u8>,
    etag: String,
    id: String,
}

#[derive(Default)]
struct Inner {
    files: BTreeMap<String, FFile>,
    folders: BTreeSet<String>,
    accepted: HashSet<String>,
    log: Vec<Logged>,
    downloads_served: usize,
    next_id: u64,
    // misbehaviour
    throttle_remaining: u32,
    fail_500_remaining: u32,
    expire_after_downloads: Option<usize>,
    login_redirect: bool,
    ignore_range: bool,
    swap_after_first_range: Option<(String, Vec<u8>, String)>,
    flap_etags: bool,
    hang_after_downloads: Option<usize>,
    page_size: Option<usize>,
}

pub struct Fake {
    pub origin: Url,
    inner: Mutex<Inner>,
}

impl Fake {
    pub async fn start() -> Arc<Fake> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let fake = Arc::new(Fake {
            origin: Url::parse(&format!("http://127.0.0.1:{port}")).unwrap(),
            inner: Mutex::new(Inner::default()),
        });
        fake.lock().accepted.insert(COOKIE_OLD.to_string());
        let app = Router::new().fallback(handle).with_state(fake.clone());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        fake
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    // ----- content -----

    /// Add (or replace) a file by its server-relative URL; parent folders are created.
    pub fn add_file(&self, key: &str, data: Vec<u8>) {
        let mut g = self.lock();
        let mut parent = key.rsplit_once('/').map(|(p, _)| p.to_string());
        while let Some(p) = parent {
            if p.is_empty() {
                break;
            }
            g.folders.insert(p.clone());
            parent = p.rsplit_once('/').map(|(pp, _)| pp.to_string());
        }
        g.next_id += 1;
        let id = format!("00000000-0000-0000-0000-{:012x}", g.next_id);
        let etag = format!("\"{{{id}}},1\"");
        g.files.insert(key.to_string(), FFile { data, etag, id });
    }

    pub fn add_folder(&self, path: &str) {
        self.lock().folders.insert(path.to_string());
    }

    /// New content and a new eTag for an existing file.
    pub fn replace_file(&self, key: &str, data: Vec<u8>, etag: &str) {
        let mut g = self.lock();
        let f = g.files.get_mut(key).expect("file exists");
        f.data = data;
        f.etag = etag.to_string();
    }

    pub fn etag_of(&self, key: &str) -> String {
        self.lock().files[key].etag.clone()
    }

    pub fn session(&self, cookie: &str) -> Session {
        Session {
            host: "contoso.sharepoint.com".into(),
            cookies: vec![SessionCookie {
                name: "FedAuth".into(),
                value: cookie.into(),
                domain: "contoso.sharepoint.com".into(),
                path: "/".into(),
                expires: None,
            }],
            method: SignInMethod::Pasted,
            captured_at: OffsetDateTime::now_utc(),
        }
    }

    // ----- misbehaviour switches -----

    /// Answer the next `n` download requests with 429 and `Retry-After: 1`.
    pub fn throttle_downloads(&self, n: u32) {
        self.lock().throttle_remaining = n;
    }
    /// Answer the next `n` download requests with 500.
    pub fn fail_downloads(&self, n: u32) {
        self.lock().fail_500_remaining = n;
    }
    /// After `n` downloads were served, reject the old cookie and accept only `COOKIE_FRESH`.
    pub fn expire_session_after(&self, n: usize) {
        self.lock().expire_after_downloads = Some(n);
    }
    /// Unauthorised requests get a redirect to the Microsoft login host instead of 403.
    pub fn redirect_to_login(&self, on: bool) {
        self.lock().login_redirect = on;
    }
    pub fn reject_all_cookies(&self) {
        self.lock().accepted.clear();
    }
    pub fn ignore_range(&self, on: bool) {
        self.lock().ignore_range = on;
    }
    /// After the first ranged download of `key`, swap its content and eTag.
    pub fn swap_after_first_range(&self, key: &str, data: Vec<u8>, etag: &str) {
        self.lock().swap_after_first_range = Some((key.to_string(), data, etag.to_string()));
    }
    /// Every download changes the file's eTag.
    pub fn flap_etags(&self, on: bool) {
        self.lock().flap_etags = on;
    }
    /// Download requests after the first `n` never answer.
    pub fn hang_after(&self, n: usize) {
        self.lock().hang_after_downloads = Some(n);
    }
    /// Return at most `n` files per listing page, with `odata.nextLink`.
    pub fn page_size(&self, n: usize) {
        self.lock().page_size = Some(n);
    }

    // ----- observation -----

    pub fn log(&self) -> Vec<Logged> {
        self.lock().log.clone()
    }
    pub fn clear_log(&self) {
        self.lock().log.clear();
    }
    /// `(file key, start, end exclusive)` of every served download request, in order.
    pub fn downloads(&self) -> Vec<(String, u64, u64)> {
        self.lock()
            .log
            .iter()
            .filter_map(|l| match l {
                Logged::Download { key, start, end } => Some((key.clone(), *start, *end)),
                _ => None,
            })
            .collect()
    }
    pub fn download_count(&self) -> usize {
        self.lock().downloads_served
    }
}

fn plain(status: StatusCode, body: &str) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn json_response(v: serde_json::Value) -> Response {
    Response::builder()
        .status(200)
        .header(
            header::CONTENT_TYPE,
            "application/json;odata=nometadata;streaming=true;charset=utf-8",
        )
        .body(Body::from(v.to_string()))
        .unwrap()
}

fn cookie_values(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|p| p.trim().strip_prefix("FedAuth=").map(str::to_string))
        .collect()
}

fn mtime() -> &'static str {
    "2024-05-01T10:20:30Z"
}

fn file_json(key: &str, f: &FFile) -> serde_json::Value {
    json!({
        "Name": key.rsplit('/').next().unwrap(),
        "Length": f.data.len().to_string(),
        "TimeLastModified": mtime(),
        "ETag": f.etag,
        "UniqueId": f.id,
        "ServerRelativeUrl": key,
    })
}

fn parent_of(key: &str) -> &str {
    key.rsplit_once('/').map(|(p, _)| p).unwrap_or("")
}

async fn handle(State(fake): State<Arc<Fake>>, req: Request) -> Response {
    let raw_path = req.uri().path().to_string();
    let path = percent_decode_str(&raw_path).decode_utf8_lossy().to_string();
    let query: HashMap<String, String> = req
        .uri()
        .query()
        .map(|q| url::form_urlencoded::parse(q.as_bytes()).into_owned().collect())
        .unwrap_or_default();
    let is_download = path.ends_with("/_layouts/15/download.aspx");

    let hang;
    {
        let mut g = fake.lock();
        if is_download
            && let Some(n) = g.expire_after_downloads
            && g.downloads_served >= n
        {
            g.accepted.clear();
            g.accepted.insert(COOKIE_FRESH.to_string());
            g.expire_after_downloads = None;
        }
        let ok = cookie_values(req.headers())
            .iter()
            .any(|c| g.accepted.contains(c));
        if !ok {
            g.log.push(Logged::Unauthorized);
            return if g.login_redirect {
                Response::builder()
                    .status(302)
                    .header(
                        header::LOCATION,
                        "https://login.microsoftonline.com/common/oauth2/authorize",
                    )
                    .body(Body::empty())
                    .unwrap()
            } else {
                plain(StatusCode::FORBIDDEN, "forbidden")
            };
        }
        hang = is_download && g.hang_after_downloads.is_some_and(|n| g.downloads_served >= n);
        if hang {
            g.downloads_served += 1;
        }
    }
    if hang {
        // Never answers; the client drops the connection when the job is cancelled.
        std::future::pending::<()>().await;
    }

    if path.starts_with("/:f:/") {
        fake.lock().log.push(Logged::Sharing);
        let target =
            "/sites/Team/_layouts/15/onedrive.aspx?id=%2Fsites%2FTeam%2FShared%20Documents%2FRoot&ga=1";
        return Response::builder()
            .status(302)
            .header(header::LOCATION, target)
            .body(Body::empty())
            .unwrap();
    }

    if let Some((_, rest)) = path.split_once("/_api/web/GetFolderByServerRelativePath(decodedurl='") {
        let (folder, suffix) = rest.split_once("')").unwrap_or((rest, ""));
        let folder = folder.replace("''", "'");
        return folder_request(&fake, &folder, suffix, &query);
    }
    if let Some((_, rest)) = path.split_once("/_api/web/GetFileById('") {
        let id = rest.split("')").next().unwrap_or("");
        let mut g = fake.lock();
        let found = g.files.iter().find(|(_, f)| f.id == id).map(|(k, _)| k.clone());
        return match found {
            Some(key) => {
                g.log.push(Logged::Stat(key.clone()));
                let f = &g.files[&key];
                json_response(file_json(&key, f))
            }
            None => plain(StatusCode::NOT_FOUND, "no such file"),
        };
    }
    if is_download {
        let id = query.get("UniqueId").cloned().unwrap_or_default();
        return download(&fake, &id, req.headers());
    }
    plain(StatusCode::NOT_FOUND, "unknown route")
}

fn folder_request(fake: &Fake, folder: &str, suffix: &str, query: &HashMap<String, String>) -> Response {
    let mut g = fake.lock();
    if !g.folders.contains(folder) {
        return plain(StatusCode::NOT_FOUND, "no such folder");
    }
    match suffix {
        "" => {
            g.log.push(Logged::FolderInfo(folder.to_string()));
            json_response(json!({"Name": folder.rsplit('/').next().unwrap(), "ServerRelativeUrl": folder}))
        }
        "/Files" => {
            g.log.push(Logged::List {
                folder: folder.to_string(),
                what: "files".into(),
            });
            let all: Vec<serde_json::Value> = g
                .files
                .iter()
                .filter(|(k, _)| parent_of(k) == folder)
                .map(|(k, f)| file_json(k, f))
                .collect();
            let skip: usize = query.get("skip").and_then(|s| s.parse().ok()).unwrap_or(0);
            let (page, next) = match g.page_size {
                Some(n) if all.len() > skip + n => (all[skip..skip + n].to_vec(), Some(skip + n)),
                _ => (all[skip.min(all.len())..].to_vec(), None),
            };
            let mut body = json!({ "value": page });
            if let Some(next) = next {
                let enc = percent_encoding::utf8_percent_encode(folder, percent_encoding::NON_ALPHANUMERIC);
                body["odata.nextLink"] = json!(format!(
                    "{}/sites/Team/_api/web/GetFolderByServerRelativePath(decodedurl='{enc}')/Files?skip={next}",
                    fake.origin.as_str().trim_end_matches('/')
                ));
            }
            json_response(body)
        }
        "/Folders" => {
            g.log.push(Logged::List {
                folder: folder.to_string(),
                what: "folders".into(),
            });
            let items: Vec<serde_json::Value> = g
                .folders
                .iter()
                .filter(|p| parent_of(p) == folder)
                .map(|p| json!({"Name": p.rsplit('/').next().unwrap(), "ServerRelativeUrl": p, "UniqueId": "f", "ItemCount": 0}))
                .collect();
            json_response(json!({ "value": items }))
        }
        _ => plain(StatusCode::NOT_FOUND, "unknown listing"),
    }
}

fn download(fake: &Fake, id: &str, headers: &HeaderMap) -> Response {
    let mut g = fake.lock();
    let Some(key) = g.files.iter().find(|(_, f)| f.id == id).map(|(k, _)| k.clone()) else {
        return plain(StatusCode::NOT_FOUND, "no such file");
    };
    if g.throttle_remaining > 0 {
        g.throttle_remaining -= 1;
        return Response::builder()
            .status(429)
            .header(header::RETRY_AFTER, "1")
            .body(Body::empty())
            .unwrap();
    }
    if g.fail_500_remaining > 0 {
        g.fail_500_remaining -= 1;
        return plain(StatusCode::INTERNAL_SERVER_ERROR, "boom");
    }
    g.downloads_served += 1;
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| v.split_once('-'))
        .and_then(|(a, b)| Some((a.parse::<u64>().ok()?, b.parse::<u64>().ok()?)));

    let flap = g.flap_etags;
    let swap = g
        .swap_after_first_range
        .take_if(|(k, _, _)| *k == key && range.is_some());
    let (data, total) = {
        let f = &g.files[&key];
        (f.data.clone(), f.data.len() as u64)
    };
    let response = match range {
        Some((a, b)) if !g.ignore_range => {
            let b = b.min(total.saturating_sub(1));
            g.log.push(Logged::Download {
                key: key.clone(),
                start: a,
                end: b + 1,
            });
            Response::builder()
                .status(206)
                .header(header::CONTENT_RANGE, format!("bytes {a}-{b}/{total}"))
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .body(Body::from(data[a as usize..=b as usize].to_vec()))
                .unwrap()
        }
        _ => {
            g.log.push(Logged::Download {
                key: key.clone(),
                start: 0,
                end: total,
            });
            Response::builder()
                .status(200)
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .body(Body::from(data))
                .unwrap()
        }
    };
    if let Some((_, new_data, new_etag)) = swap {
        let f = g.files.get_mut(&key).unwrap();
        f.data = new_data;
        f.etag = new_etag;
    }
    if flap {
        let n = g.downloads_served;
        let f = g.files.get_mut(&key).unwrap();
        f.etag = format!("\"flap-{n}\"");
    }
    response
}
