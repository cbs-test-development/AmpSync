//! Experiments E1..E7. Each is independent: a failure is recorded and the next one still runs.

use crate::auth::{is_auth_name, Captured};
use crate::http::{Http, Req, Resp};
use crate::jwt;
use crate::names::check_name;
use crate::qxor::QuickXor;
use crate::redact::{redact_name, scrub_text, url_shape};
use crate::urlparse::{encode_path, is_default_english_library, odata_string_arg, share_id, split_site, classify_url, SplitPath, UrlKind, ParsedUrl};
use anyhow::{anyhow, Result};
use futures_util::future::join_all;
use percent_encoding::percent_decode_str;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashSet, VecDeque};
use std::time::{Duration, Instant};
use url::Url;

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pass,
    Fail,
    Skipped,
}

#[derive(Serialize, Clone)]
pub struct ExpResult {
    pub id: &'static str,
    pub name: &'static str,
    pub status: Status,
    pub summary: String,
    pub details: Value,
    pub error: Option<String>,
}

type Out = Result<(Status, String, Value)>;

fn finish(id: &'static str, name: &'static str, r: Out) -> ExpResult {
    match r {
        Ok((status, summary, details)) => ExpResult { id, name, status, summary, details, error: None },
        Err(e) => ExpResult { id, name, status: Status::Fail, summary: "experiment aborted".into(), details: Value::Null, error: Some(scrub_text(&format!("{e:#}"))) },
    }
}

// ---------------------------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------------------------

#[derive(Clone)]
pub enum FolderRef {
    Id { drive: String, id: String },
    RootRel { drive: String, rel: String },
    DefaultRel { rel: String },
}

#[derive(Clone)]
pub struct V2Root {
    pub base: String,
    pub root: FolderRef,
    pub method: &'static str,
}

#[derive(Clone, Default)]
pub struct FileCand {
    pub name: String,
    pub size: u64,
    /// Path relative to the walked root folder (real names; memory only).
    pub rel: String,
    pub drive_id: Option<String>,
    pub item_id: Option<String>,
    pub unique_id: Option<String>,
    pub download_url: Option<String>,
    pub quickxor: Option<String>,
}

#[derive(Default)]
pub struct WalkOut {
    pub largest: Option<FileCand>,
    pub hash_cand: Option<FileCand>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
pub enum Dl {
    DownloadUrlBare,
    DownloadUrlCookies,
    DownloadAspx,
    ClassicValue,
}

impl Dl {
    pub fn name(self) -> &'static str {
        match self {
            Dl::DownloadUrlBare => "v2_downloadUrl_no_cookies",
            Dl::DownloadUrlCookies => "v2_downloadUrl_with_cookies",
            Dl::DownloadAspx => "download_aspx_uniqueid",
            Dl::ClassicValue => "classic_GetFileByServerRelativePath_value",
        }
    }
    pub const ALL: [Dl; 4] = [Dl::DownloadUrlBare, Dl::DownloadUrlCookies, Dl::DownloadAspx, Dl::ClassicValue];
}

pub struct State {
    pub raw_url: String,
    pub host: String,
    pub origin: String,
    pub parsed: ParsedUrl,
    pub cookies: Vec<Captured>,
    pub folder_rel: Option<String>,
    pub split: Option<SplitPath>,
    pub share: Option<(String, String)>, // (drive id, item id)
    pub v2: Option<V2Root>,
    pub classic_ok: bool,
    pub walk: WalkOut,
    pub best_dl: Option<Dl>,
}

impl State {
    pub fn new(raw_url: &str, parsed: ParsedUrl, cookies: Vec<Captured>) -> Self {
        let host = parsed.host.clone();
        State {
            raw_url: raw_url.to_string(),
            origin: format!("{}://{host}", crate::urlparse::scheme()),
            host,
            folder_rel: parsed.folder_server_rel.clone(),
            split: parsed.folder_server_rel.as_deref().and_then(split_site),
            parsed,
            cookies,
            share: None,
            v2: None,
            classic_ok: false,
            walk: WalkOut::default(),
            best_dl: None,
        }
    }
    fn set_folder(&mut self, p: &str) {
        self.folder_rel = Some(p.to_string());
        self.split = split_site(p);
    }
    fn site_base(&self) -> String {
        format!("{}{}", self.origin, self.split.as_ref().map(|s| s.site.as_str()).unwrap_or(""))
    }
}

async fn get_retry(http: &Http, mk: impl Fn() -> Req) -> Result<Resp> {
    let mut attempt = 0;
    loop {
        let r = http.send(mk()).await?;
        if (r.status == 429 || r.status == 503) && attempt < 3 {
            let wait = r.header("retry-after").and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(2).min(20);
            tokio::time::sleep(Duration::from_secs(wait)).await;
            attempt += 1;
            continue;
        }
        return Ok(r);
    }
}

fn host_port(u: &Url) -> String {
    let h = u.host_str().unwrap_or("").to_ascii_lowercase();
    match u.port() {
        Some(p) => format!("{h}:{p}"),
        None => h,
    }
}

fn same_host(url: &str, host: &str) -> bool {
    Url::parse(url).map(|u| host_port(&u).eq_ignore_ascii_case(host)).unwrap_or(false)
}

fn status_err(r: &Resp) -> anyhow::Error {
    anyhow!("HTTP {}", r.status)
}

// ---------------------------------------------------------------------------------------------
// E1 cookies
// ---------------------------------------------------------------------------------------------

pub fn e1_cookies(st: &State) -> ExpResult {
    let now = chrono::Utc::now().timestamp();
    let names: BTreeSet<String> = st.cookies.iter().map(|c| c.name.clone()).collect();
    let find = |n: &str| st.cookies.iter().find(|c| c.name.eq_ignore_ascii_case(n));
    let describe = |c: Option<&Captured>| -> Value {
        match c {
            None => json!({"present": false}),
            Some(c) => json!({
                "present": true,
                "session_cookie": c.session,
                "expires_utc": c.expires_unix.and_then(|e| chrono::DateTime::from_timestamp(e, 0)).map(|d| d.to_rfc3339()),
                "hours_until_expiry": c.expires_unix.map(|e| ((e - now) as f64 / 3600.0 * 10.0).round() / 10.0),
                "http_only": c.http_only,
                "secure": c.secure,
                "domain": c.domain,
                "value_length": c.value.len(),
            }),
        }
    };
    let fed = find("FedAuth");
    let rtfa = find("rtFa");
    let ok = fed.is_some() || rtfa.is_some() || st.cookies.iter().any(|c| is_auth_name(&c.name));
    let summary = format!(
        "{} cookies; FedAuth {}, rtFa {}",
        st.cookies.len(),
        if fed.is_some() { "present" } else { "absent" },
        if rtfa.is_some() { "present" } else { "absent" }
    );
    ExpResult {
        id: "E1",
        name: "cookies",
        status: if ok { Status::Pass } else { Status::Fail },
        summary,
        details: json!({
            "count": st.cookies.len(),
            "names": names,
            "FedAuth": describe(fed),
            "rtFa": describe(rtfa),
            "note": "cookie values are never recorded; value_length is the only trace",
        }),
        error: None,
    }
}

// ---------------------------------------------------------------------------------------------
// E2 resolve
// ---------------------------------------------------------------------------------------------

pub async fn e2_resolve(http: &Http, st: &mut State) -> ExpResult {
    finish("E2", "resolve", e2_inner(http, st).await)
}

async fn e2_inner(http: &Http, st: &mut State) -> Out {
    let mut details = json!({ "url_kind": st.parsed.kind });
    let mut notes: Vec<String> = Vec::new();

    if st.parsed.kind == UrlKind::SharingLink {
        let r = http
            .send(Req::get("E2", "<sharing link> (follow redirects)", st.raw_url.clone()).header("Accept", "text/html,*/*").follow().max_body(256 * 1024).no_store())
            .await;
        match r {
            Ok(r) => {
                let fin = &r.final_url;
                let final_host = host_port(fin);
                details["redirect_chain"] = json!(r.hops);
                details["final_status"] = json!(r.status);
                details["final_shape"] = json!(url_shape(fin));
                details["final_host_same_as_input"] = json!(final_host.eq_ignore_ascii_case(&st.host));
                let fh = final_host.to_ascii_lowercase();
                if fh.contains("login.") || fh.contains("microsoftonline") || fh.contains("live.com") {
                    notes.push("redirect chain left SharePoint (likely sign-in page): cookies were not accepted for this link".into());
                    details["ended_on_signin"] = json!(true);
                } else if let Ok(p) = classify_url(fin.as_str()) {
                    if let Some(path) = p.folder_server_rel {
                        st.set_folder(&path);
                        details["folder_from_redirect"] = json!(true);
                    }
                }
            }
            Err(e) => notes.push(format!("redirect follow failed: {e}")),
        }
    }

    // /shares/{id}/driveItem with redeemSharingLink
    let share_src = if st.parsed.kind == UrlKind::SharingLink {
        Some(st.raw_url.clone())
    } else {
        st.folder_rel.as_ref().map(|p| format!("{}{}", st.origin, encode_path(p)))
    };
    if let Some(src) = share_src {
        let url = format!("{}/_api/v2.0/shares/{}/driveItem", st.origin, share_id(&src));
        match http
            .send(Req::get("E2", "/_api/v2.0/shares/{shareId}/driveItem", url).json().header("Prefer", "redeemSharingLink"))
            .await
        {
            Ok(r) if r.is_success() => {
                let v = r.json()?;
                let drive = v["parentReference"]["driveId"].as_str().map(|s| s.to_string());
                let id = v["id"].as_str().map(|s| s.to_string());
                if let (Some(d), Some(i)) = (drive, id) {
                    st.share = Some((d, i));
                }
                let is_folder = v.get("folder").is_some();
                details["shares_api"] = json!({
                    "status": r.status, "ok": true, "is_folder": is_folder,
                    "has_webUrl": v["webUrl"].is_string(),
                    "has_driveId": st.share.is_some(),
                    "child_count": v["folder"]["childCount"],
                });
                if let Some(w) = v["webUrl"].as_str() {
                    if st.folder_rel.is_none() {
                        if let Ok(p) = classify_url(w) {
                            if let Some(path) = p.folder_server_rel {
                                st.set_folder(&path);
                                details["folder_from_shares_api"] = json!(true);
                            }
                        }
                    }
                }
            }
            Ok(r) => {
                details["shares_api"] = json!({"status": r.status, "ok": false});
                notes.push(format!("shares API returned HTTP {}", r.status));
            }
            Err(e) => {
                details["shares_api"] = json!({"ok": false, "error": scrub_text(&e.to_string())});
            }
        }
    }

    match (&st.folder_rel, &st.split) {
        (Some(p), Some(sp)) => {
            details["folder_depth_segments"] = json!(p.split('/').filter(|s| !s.is_empty()).count());
            details["site_kind"] = json!(sp.site.split('/').nth(1).unwrap_or("root"));
            details["site_name_len"] = json!(sp.site.split('/').nth(2).map(|s| s.chars().count()));
            details["library_name_len"] = json!(sp.library.chars().count());
            details["library_is_default_english_name"] = json!(is_default_english_library(&sp.library));
            details["path_inside_library_depth"] = json!(sp.rel.split('/').filter(|s| !s.is_empty()).count());
            details["notes"] = json!(notes);
            Ok((Status::Pass, format!("resolved folder ({} path segments, library default-name: {})", p.split('/').filter(|s| !s.is_empty()).count(), is_default_english_library(&sp.library)), details))
        }
        _ => {
            details["notes"] = json!(notes);
            Ok((Status::Fail, "could not determine the server-relative folder path".into(), details))
        }
    }
}

// ---------------------------------------------------------------------------------------------
// v2.0 listing helpers
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct Entry {
    pub name: String,
    pub size: u64,
    pub is_folder: bool,
    pub id: Option<String>,
    pub drive_id: Option<String>,
    pub quickxor: Option<String>,
    pub download_url: Option<String>,
    pub unique_id: Option<String>,
}

fn download_key(v: &Value) -> Option<&'static str> {
    if v.get("@content.downloadUrl").is_some() {
        Some("@content.downloadUrl")
    } else if v.get("@microsoft.graph.downloadUrl").is_some() {
        Some("@microsoft.graph.downloadUrl")
    } else {
        None
    }
}

fn parse_v2_entry(v: &Value) -> Entry {
    Entry {
        name: v["name"].as_str().unwrap_or("").to_string(),
        size: v["size"].as_u64().unwrap_or(0),
        is_folder: v.get("folder").is_some(),
        id: v["id"].as_str().map(|s| s.to_string()),
        drive_id: v["parentReference"]["driveId"].as_str().map(|s| s.to_string()),
        quickxor: v["file"]["hashes"]["quickXorHash"].as_str().map(|s| s.to_string()),
        download_url: download_key(v).and_then(|k| v[k].as_str()).map(|s| s.to_string()),
        unique_id: v["sharepointIds"]["listItemUniqueId"].as_str().map(|s| s.to_string()),
    }
}

fn children_url(base: &str, f: &FolderRef, top: u32) -> (String, &'static str) {
    match f {
        FolderRef::Id { drive, id } => (
            format!("{base}/_api/v2.0/drives/{}/items/{}/children?$top={top}", encode_path(drive), encode_path(id)),
            "/_api/v2.0/drives/{driveId}/items/{itemId}/children?$top=N",
        ),
        FolderRef::RootRel { drive, rel } if rel.is_empty() => {
            (format!("{base}/_api/v2.0/drives/{}/root/children?$top={top}", encode_path(drive)), "/_api/v2.0/drives/{driveId}/root/children?$top=N")
        }
        FolderRef::RootRel { drive, rel } => (
            format!("{base}/_api/v2.0/drives/{}/root:/{}:/children?$top={top}", encode_path(drive), encode_path(rel)),
            "/_api/v2.0/drives/{driveId}/root:/{path}:/children?$top=N",
        ),
        FolderRef::DefaultRel { rel } if rel.is_empty() => (format!("{base}/_api/v2.0/drive/root/children?$top={top}"), "/_api/v2.0/drive/root/children?$top=N"),
        FolderRef::DefaultRel { rel } => {
            (format!("{base}/_api/v2.0/drive/root:/{}:/children?$top={top}", encode_path(rel)), "/_api/v2.0/drive/root:/{path}:/children?$top=N")
        }
    }
}

/// One page. Returns (raw items, nextLink).
async fn v2_page(http: &Http, exp: &'static str, url: String, template: &str) -> Result<(Vec<Value>, Option<String>)> {
    let r = get_retry(http, || Req::get(exp, template, url.clone()).json()).await?;
    if !r.is_success() {
        return Err(status_err(&r));
    }
    let v = r.json()?;
    let items = v["value"].as_array().cloned().ok_or_else(|| anyhow!("no `value` array in response"))?;
    Ok((items, v["@odata.nextLink"].as_str().map(|s| s.to_string())))
}

#[derive(Default, Serialize)]
struct Presence {
    items_seen: usize,
    files_seen: usize,
    quick_xor_hash: bool,
    sha1_hash: bool,
    sha256_hash: bool,
    etag: bool,
    ctag: bool,
    size: bool,
    last_modified: bool,
    download_url: Option<&'static str>,
    sharepoint_ids: bool,
    parent_reference_drive_id: bool,
    item_keys: BTreeSet<String>,
    file_hash_keys: BTreeSet<String>,
}

impl Presence {
    fn observe(&mut self, v: &Value) {
        self.items_seen += 1;
        if let Some(o) = v.as_object() {
            self.item_keys.extend(o.keys().cloned());
        }
        self.etag |= v.get("eTag").is_some();
        self.ctag |= v.get("cTag").is_some();
        self.size |= v.get("size").is_some();
        self.last_modified |= v.get("lastModifiedDateTime").is_some();
        self.sharepoint_ids |= v.get("sharepointIds").is_some();
        self.parent_reference_drive_id |= v["parentReference"]["driveId"].is_string();
        if let Some(k) = download_key(v) {
            self.download_url = Some(k);
        }
        if v.get("file").is_some() {
            self.files_seen += 1;
            if let Some(h) = v["file"]["hashes"].as_object() {
                self.file_hash_keys.extend(h.keys().cloned());
                self.quick_xor_hash |= h.contains_key("quickXorHash");
                self.sha1_hash |= h.contains_key("sha1Hash");
                self.sha256_hash |= h.contains_key("sha256Hash");
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// E3 v2.0 listing
// ---------------------------------------------------------------------------------------------

pub async fn e3_v2_listing(http: &Http, st: &mut State) -> ExpResult {
    finish("E3", "v2.0 listing", e3_inner(http, st).await)
}

async fn e3_inner(http: &Http, st: &mut State) -> Out {
    let split = st.split.clone().ok_or_else(|| anyhow!("folder path unknown (E2 failed)"))?;
    let base = st.site_base();
    let mut attempts: Vec<Value> = Vec::new();
    let mut chosen: Option<(V2Root, Vec<Value>)> = None;

    // A: item id from the shares API
    if let Some((drive, id)) = st.share.clone() {
        let root = FolderRef::Id { drive, id };
        let (url, tpl) = children_url(&base, &root, 200);
        match v2_page(http, "E3", url, tpl).await {
            Ok((items, _)) => {
                attempts.push(json!({"method": "shares_item_id", "ok": true}));
                chosen = Some((V2Root { base: base.clone(), root, method: "shares_item_id" }, items));
            }
            Err(e) => attempts.push(json!({"method": "shares_item_id", "ok": false, "error": e.to_string()})),
        }
    }

    // B: discover the library drive by webUrl, then root:/path:
    if chosen.is_none() {
        let url = format!("{base}/_api/v2.0/drives");
        match get_retry(http, || Req::get("E3", "/_api/v2.0/drives", url.clone()).json()).await {
            Ok(r) if r.is_success() => {
                let v = r.json()?;
                let want = format!("{}/{}", split.site, split.library).to_lowercase();
                let drive_id = v["value"].as_array().and_then(|a| {
                    a.iter().find_map(|d| {
                        let w = Url::parse(d["webUrl"].as_str()?).ok()?;
                        let p = percent_decode_str(w.path()).decode_utf8_lossy().trim_end_matches('/').to_lowercase();
                        (p == want).then(|| d["id"].as_str().map(|s| s.to_string())).flatten()
                    })
                });
                attempts.push(json!({"method": "drives_discovery", "ok": drive_id.is_some(), "drives_returned": v["value"].as_array().map(|a| a.len())}));
                if let Some(drive) = drive_id {
                    let root = FolderRef::RootRel { drive, rel: split.rel.clone() };
                    let (url, tpl) = children_url(&base, &root, 200);
                    match v2_page(http, "E3", url, tpl).await {
                        Ok((items, _)) => {
                            attempts.push(json!({"method": "drives_root_path", "ok": true}));
                            chosen = Some((V2Root { base: base.clone(), root, method: "drives_discovery_root_path" }, items));
                        }
                        Err(e) => attempts.push(json!({"method": "drives_root_path", "ok": false, "error": e.to_string()})),
                    }
                }
            }
            Ok(r) => attempts.push(json!({"method": "drives_discovery", "ok": false, "status": r.status})),
            Err(e) => attempts.push(json!({"method": "drives_discovery", "ok": false, "error": scrub_text(&e.to_string())})),
        }
    }

    // C: default drive
    if chosen.is_none() {
        let root = FolderRef::DefaultRel { rel: split.rel.clone() };
        let (url, tpl) = children_url(&base, &root, 200);
        match v2_page(http, "E3", url, tpl).await {
            Ok((items, _)) => {
                attempts.push(json!({"method": "default_drive_root_path", "ok": true}));
                chosen = Some((V2Root { base: base.clone(), root, method: "default_drive_root_path" }, items));
            }
            Err(e) => attempts.push(json!({"method": "default_drive_root_path", "ok": false, "error": e.to_string()})),
        }
    }

    let Some((v2, items)) = chosen else {
        return Ok((Status::Fail, "v2.0 listing does not work with cookies only".into(), json!({"attempts": attempts})));
    };

    let mut presence = Presence::default();
    let (mut files, mut folders) = (0, 0);
    for it in &items {
        presence.observe(it);
        if it.get("folder").is_some() {
            folders += 1;
        } else {
            files += 1;
        }
    }

    // Paging probe with a tiny page size.
    let (url, tpl) = children_url(&v2.base, &v2.root, 2);
    let mut paging = json!({"tested": true});
    match v2_page(http, "E3", url, tpl).await {
        Ok((first, Some(next))) => {
            paging["next_link_present"] = json!(true);
            paging["first_page_items"] = json!(first.len());
            let same = same_host(&next, &st.host);
            paging["next_link_same_host"] = json!(same);
            if same {
                match v2_page(http, "E3", next, "<@odata.nextLink>").await {
                    Ok((second, _)) => paging["second_page_ok"] = json!({"ok": true, "items": second.len()}),
                    Err(e) => paging["second_page_ok"] = json!({"ok": false, "error": e.to_string()}),
                }
            }
        }
        Ok((first, None)) => {
            paging["next_link_present"] = json!(false);
            paging["first_page_items"] = json!(first.len());
            paging["note"] = json!("folder has <= 2 items, paging not observable");
        }
        Err(e) => paging["error"] = json!(e.to_string()),
    }

    let summary = format!(
        "works via {}; page1 {} files/{} folders; quickXorHash {}, downloadUrl {}",
        v2.method,
        files,
        folders,
        if presence.quick_xor_hash { "yes" } else { "no" },
        presence.download_url.unwrap_or("no")
    );
    let details = json!({"listing_method": v2.method, "cookies_only": true, "attempts": attempts, "first_page": {"files": files, "folders": folders}, "fields": presence, "paging": paging});
    st.v2 = Some(v2);
    Ok((Status::Pass, summary, details))
}

// ---------------------------------------------------------------------------------------------
// E4 classic REST
// ---------------------------------------------------------------------------------------------

fn classic_files_url(base: &str, folder: &str) -> String {
    format!(
        "{base}/_api/web/GetFolderByServerRelativePath(decodedurl='{}')/Files?$select=Name,Length,TimeLastModified,ETag,UniqueId,ServerRelativeUrl",
        odata_string_arg(folder)
    )
}
fn classic_folders_url(base: &str, folder: &str) -> String {
    format!("{base}/_api/web/GetFolderByServerRelativePath(decodedurl='{}')/Folders?$select=Name,ServerRelativeUrl,UniqueId,ItemCount", odata_string_arg(folder))
}
const CLASSIC_FILES_TPL: &str = "/_api/web/GetFolderByServerRelativePath(decodedurl='{path}')/Files?$select=Name,Length,TimeLastModified,ETag,UniqueId,ServerRelativeUrl";
const CLASSIC_FOLDERS_TPL: &str = "/_api/web/GetFolderByServerRelativePath(decodedurl='{path}')/Folders?$select=Name,ServerRelativeUrl,UniqueId,ItemCount";

pub async fn e4_classic(http: &Http, st: &mut State) -> ExpResult {
    finish("E4", "classic REST listing", e4_inner(http, st).await)
}

async fn e4_inner(http: &Http, st: &mut State) -> Out {
    let folder = st.folder_rel.clone().ok_or_else(|| anyhow!("folder path unknown (E2 failed)"))?;
    let base = st.site_base();
    let mut details = json!({});

    let files_url = classic_files_url(&base, &folder);
    let r = get_retry(http, || Req::get("E4", CLASSIC_FILES_TPL, files_url.clone()).nometadata()).await?;
    let files_ok = r.is_success();
    details["files"] = json!({"status": r.status, "ok": files_ok});
    if files_ok {
        let v = r.json()?;
        let arr = v["value"].as_array().cloned().unwrap_or_default();
        let keys: BTreeSet<String> = arr.iter().filter_map(|i| i.as_object()).flat_map(|o| o.keys().cloned()).collect();
        details["files"]["count"] = json!(arr.len());
        details["files"]["fields_present"] = json!(keys);
        details["files"]["length_is_string"] = json!(arr.first().map(|f| f["Length"].is_string()));
    }

    let folders_url = classic_folders_url(&base, &folder);
    let r = get_retry(http, || Req::get("E4", CLASSIC_FOLDERS_TPL, folders_url.clone()).nometadata()).await?;
    let folders_ok = r.is_success();
    details["folders"] = json!({"status": r.status, "ok": folders_ok});
    if folders_ok {
        let v = r.json()?;
        let arr = v["value"].as_array().cloned().unwrap_or_default();
        let keys: BTreeSet<String> = arr.iter().filter_map(|i| i.as_object()).flat_map(|o| o.keys().cloned()).collect();
        details["folders"]["count"] = json!(arr.len());
        details["folders"]["fields_present"] = json!(keys);
    }
    st.classic_ok = files_ok && folders_ok;
    let status = if st.classic_ok { Status::Pass } else { Status::Fail };
    Ok((status, format!("Files {} / Folders {}", if files_ok { "ok" } else { "failed" }, if folders_ok { "ok" } else { "failed" }), details))
}

// ---------------------------------------------------------------------------------------------
// E5 recursive count
// ---------------------------------------------------------------------------------------------

enum Node {
    V2(FolderRef),
    Classic(String),
}

pub async fn e5_walk(http: &Http, st: &mut State) -> ExpResult {
    finish("E5", "recursive count", e5_inner(http, st).await)
}

async fn list_node(http: &Http, st: &State, node: &Node, deadline: Instant) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    match node {
        Node::V2(f) => {
            let v2 = st.v2.as_ref().ok_or_else(|| anyhow!("no v2 root"))?;
            let (mut url, mut tpl) = children_url(&v2.base, f, 200);
            let mut tpl_s = tpl.to_string();
            let default_drive = match f {
                FolderRef::Id { drive, .. } | FolderRef::RootRel { drive, .. } => Some(drive.clone()),
                FolderRef::DefaultRel { .. } => None,
            };
            loop {
                let (items, next) = v2_page(http, "E5", url, &tpl_s).await?;
                for it in &items {
                    let mut e = parse_v2_entry(it);
                    if e.drive_id.is_none() {
                        e.drive_id = default_drive.clone();
                    }
                    out.push(e);
                }
                match next {
                    Some(n) if Instant::now() < deadline && same_host(&n, &st.host) => {
                        url = n;
                        tpl = "<@odata.nextLink>";
                        tpl_s = tpl.to_string();
                    }
                    _ => break,
                }
            }
        }
        Node::Classic(path) => {
            let base = st.site_base();
            let u = classic_files_url(&base, path);
            let r = get_retry(http, || Req::get("E5", CLASSIC_FILES_TPL, u.clone()).nometadata()).await?;
            if !r.is_success() {
                return Err(status_err(&r));
            }
            for f in r.json()?["value"].as_array().cloned().unwrap_or_default() {
                out.push(Entry {
                    name: f["Name"].as_str().unwrap_or("").to_string(),
                    size: f["Length"].as_str().and_then(|s| s.parse().ok()).or_else(|| f["Length"].as_u64()).unwrap_or(0),
                    is_folder: false,
                    unique_id: f["UniqueId"].as_str().map(|s| s.to_string()),
                    ..Default::default()
                });
            }
            let u = classic_folders_url(&base, path);
            let r = get_retry(http, || Req::get("E5", CLASSIC_FOLDERS_TPL, u.clone()).nometadata()).await?;
            if !r.is_success() {
                return Err(status_err(&r));
            }
            for f in r.json()?["value"].as_array().cloned().unwrap_or_default() {
                out.push(Entry {
                    name: f["Name"].as_str().unwrap_or("").to_string(),
                    is_folder: true,
                    id: f["ServerRelativeUrl"].as_str().map(|s| s.to_string()), // reused as the child path
                    ..Default::default()
                });
            }
        }
    }
    Ok(out)
}

async fn e5_inner(http: &Http, st: &mut State) -> Out {
    let started = Instant::now();
    let deadline = started + Duration::from_secs(60);
    let log_start = http.snapshot_len();
    let (method, root) = if let Some(v2) = &st.v2 {
        ("v2.0", Node::V2(v2.root.clone()))
    } else if st.classic_ok {
        ("classic", Node::Classic(st.folder_rel.clone().ok_or_else(|| anyhow!("no folder"))?))
    } else {
        return Ok((Status::Skipped, "no working listing method (E3 and E4 both failed)".into(), Value::Null));
    };
    let is_library_root = st.split.as_ref().map(|s| s.rel.is_empty()).unwrap_or(false);

    let mut queue: VecDeque<(Node, usize, String)> = VecDeque::new();
    queue.push_back((root, 0, String::new()));
    let (mut files, mut folders, mut bytes) = (0u64, 0u64, 0u64);
    let (mut max_depth, mut longest, mut over260, mut over200) = (0usize, 0usize, 0u64, 0u64);
    let (mut illegal, mut reserved, mut trailing, mut case_collisions, mut any_issue) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut complete = true;
    let mut limit_reason = None;
    let mut walk = WalkOut::default();
    let mut folder_errors = 0u64;

    'outer: while let Some((node, depth, prefix)) = queue.pop_front() {
        if Instant::now() >= deadline {
            complete = false;
            limit_reason = Some("60 s time limit");
            break;
        }
        let entries = match list_node(http, st, &node, deadline).await {
            Ok(e) => e,
            Err(_) => {
                folder_errors += 1;
                continue;
            }
        };
        let mut seen_lower: HashSet<String> = HashSet::new();
        for e in entries {
            if method == "classic" && e.is_folder && depth == 0 && is_library_root && e.name == "Forms" {
                continue;
            }
            if files + folders >= 2000 {
                complete = false;
                limit_reason = Some("2000 item limit");
                break 'outer;
            }
            let rel = if prefix.is_empty() { e.name.clone() } else { format!("{prefix}/{}", e.name) };
            let plen = rel.chars().count();
            longest = longest.max(plen);
            if plen > 260 {
                over260 += 1;
            }
            if plen > 200 {
                over200 += 1;
            }
            let iss = check_name(&e.name);
            illegal += iss.illegal_chars as u64;
            reserved += iss.reserved as u64;
            trailing += iss.trailing_dot_space as u64;
            any_issue += iss.any() as u64;
            if !seen_lower.insert(e.name.to_lowercase()) {
                case_collisions += 1;
            }
            max_depth = max_depth.max(depth + 1);
            if e.is_folder {
                folders += 1;
                let child = match (&node, &e) {
                    (Node::V2(_), e) => match (&e.drive_id, &e.id) {
                        (Some(d), Some(i)) => Some(Node::V2(FolderRef::Id { drive: d.clone(), id: i.clone() })),
                        _ => None,
                    },
                    (Node::Classic(_), e) => e.id.clone().map(Node::Classic),
                };
                if let Some(c) = child {
                    queue.push_back((c, depth + 1, rel));
                } else {
                    folder_errors += 1;
                }
            } else {
                files += 1;
                bytes += e.size;
                let cand = FileCand {
                    name: e.name.clone(),
                    size: e.size,
                    rel,
                    drive_id: e.drive_id.clone(),
                    item_id: e.id.clone(),
                    unique_id: e.unique_id.clone(),
                    download_url: e.download_url.clone(),
                    quickxor: e.quickxor.clone(),
                };
                if walk.largest.as_ref().map(|l| e.size > l.size).unwrap_or(true) {
                    walk.largest = Some(cand.clone());
                }
                if cand.quickxor.is_some() && e.size > 0 && e.size < 20 * 1024 * 1024 && walk.hash_cand.as_ref().map(|l| e.size > l.size).unwrap_or(true) {
                    walk.hash_cand = Some(cand);
                }
            }
        }
    }

    // Throttling statistics from the request log.
    let (mut reqs, mut throttled, mut retry_afters) = (0usize, 0usize, Vec::<String>::new());
    for r in http.log.lock().unwrap().iter().skip(log_start).filter(|r| r.exp == "E5") {
        reqs += 1;
        if matches!(r.status, Some(429) | Some(503)) {
            throttled += 1;
            if let Some(ra) = r.headers.get("retry-after") {
                retry_afters.push(ra.clone());
            }
        }
    }

    let largest_desc = walk.largest.as_ref().map(|l| redact_name(&l.name, Some(l.size)));
    let hash_desc = walk.hash_cand.as_ref().map(|l| redact_name(&l.name, Some(l.size)));
    let details = json!({
        "method": method,
        "complete": complete,
        "stopped_because": limit_reason,
        "elapsed_s": (started.elapsed().as_secs_f64() * 10.0).round() / 10.0,
        "files": files, "folders": folders, "total_bytes": bytes,
        "max_depth": max_depth,
        "longest_relative_path_chars": longest,
        "paths_over_260_chars": over260,
        "paths_over_200_chars": over200,
        "names_with_illegal_windows_chars": illegal,
        "names_reserved_device": reserved,
        "names_trailing_dot_or_space": trailing,
        "names_with_any_windows_issue": any_issue,
        "case_insensitive_name_collisions": case_collisions,
        "folders_failed_to_list": folder_errors,
        "requests_made": reqs,
        "throttle_responses_429_503": throttled,
        "retry_after_values": retry_afters,
        "largest_file": largest_desc,
        "hash_test_candidate": hash_desc,
    });
    st.walk = walk;
    let status = if files + folders > 0 || complete { Status::Pass } else { Status::Fail };
    Ok((
        status,
        format!("{files} files, {folders} folders, {:.2} GB, depth {max_depth}, {reqs} requests{}", bytes as f64 / 1e9, if complete { "" } else { " (limit hit)" }),
        details,
    ))
}

// ---------------------------------------------------------------------------------------------
// E6 range download
// ---------------------------------------------------------------------------------------------

struct DlTarget {
    url: String,
    template: &'static str,
    cookies: bool,
}

fn server_rel(st: &State, cand: &FileCand) -> String {
    format!("{}/{}", st.folder_rel.clone().unwrap_or_default().trim_end_matches('/'), cand.rel)
}

async fn dl_target(http: &Http, st: &State, cand: &mut FileCand, m: Dl) -> Result<DlTarget> {
    let base = st.site_base();
    match m {
        Dl::DownloadUrlBare | Dl::DownloadUrlCookies => {
            if cand.download_url.is_none() {
                if let (Some(d), Some(i)) = (&cand.drive_id, &cand.item_id) {
                    let url = format!("{base}/_api/v2.0/drives/{}/items/{}", encode_path(d), encode_path(i));
                    let r = http.send(Req::get("E6", "/_api/v2.0/drives/{driveId}/items/{itemId}", url).json()).await?;
                    if r.is_success() {
                        let v = r.json()?;
                        cand.download_url = download_key(&v).and_then(|k| v[k].as_str()).map(|s| s.to_string());
                    }
                }
            }
            let url = cand.download_url.clone().ok_or_else(|| anyhow!("no downloadUrl available for this file"))?;
            Ok(DlTarget { url, template: "<v2.0 downloadUrl>", cookies: m == Dl::DownloadUrlCookies })
        }
        Dl::DownloadAspx => {
            if cand.unique_id.is_none() {
                let url = format!("{base}/_api/web/GetFileByServerRelativePath(decodedurl='{}')?$select=UniqueId", odata_string_arg(&server_rel(st, cand)));
                let r = http.send(Req::get("E6", "/_api/web/GetFileByServerRelativePath(decodedurl='{path}')?$select=UniqueId", url).nometadata()).await?;
                if r.is_success() {
                    cand.unique_id = r.json()?["UniqueId"].as_str().map(|s| s.to_string());
                }
            }
            let uid = cand.unique_id.clone().ok_or_else(|| anyhow!("no UniqueId available for this file"))?;
            Ok(DlTarget { url: format!("{base}/_layouts/15/download.aspx?UniqueId={}", encode_path(&uid)), template: "/_layouts/15/download.aspx?UniqueId={guid}", cookies: true })
        }
        Dl::ClassicValue => Ok(DlTarget {
            url: format!("{base}/_api/web/GetFileByServerRelativePath(decodedurl='{}')/$value", odata_string_arg(&server_rel(st, cand))),
            template: "/_api/web/GetFileByServerRelativePath(decodedurl='{path}')/$value",
            cookies: true,
        }),
    }
}

fn mk_dl_req(t: &DlTarget) -> Req {
    let r = Req::get("E6", t.template, t.url.clone()).follow();
    if t.cookies {
        r
    } else {
        r.no_cookies()
    }
}

#[derive(Serialize, Default, Clone)]
struct Probe {
    range: String,
    status: Option<u16>,
    content_range: Option<String>,
    content_length: Option<String>,
    accept_ranges: Option<String>,
    received: u64,
    expected: u64,
    server_ignored_range: bool,
    ok: bool,
    error: Option<String>,
}

async fn range_probe(http: &Http, t: &DlTarget, start: u64, end: u64) -> Probe {
    let expected = end - start + 1;
    let mut p = Probe { range: format!("bytes={start}-{end}"), expected, ..Default::default() };
    match http.send(mk_dl_req(t).range(start, end).max_body(expected as usize + 1).no_store()).await {
        Ok(r) => {
            p.status = Some(r.status);
            p.content_range = r.header("content-range");
            p.content_length = r.header("content-length");
            p.accept_ranges = r.header("accept-ranges");
            p.received = r.received;
            p.server_ignored_range = r.status == 200;
            p.ok = r.status == 206 && r.received == expected;
        }
        Err(e) => p.error = Some(scrub_text(&e.to_string())),
    }
    p
}

pub async fn e6_range(http: &Http, st: &mut State) -> ExpResult {
    finish("E6", "range download", e6_inner(http, st).await)
}

async fn e6_inner(http: &Http, st: &mut State) -> Out {
    let Some(mut cand) = st.walk.largest.clone() else {
        return Ok((Status::Skipped, "no file found by the recursive walk".into(), Value::Null));
    };
    if cand.size == 0 {
        return Ok((Status::Skipped, "largest file has size 0".into(), Value::Null));
    }
    let size = cand.size;
    let mb = 1024 * 1024u64;
    let head = (0, size.min(mb) - 1);
    let tail = (size.saturating_sub(mb), size - 1);
    let mut methods = Vec::new();
    let mut best: Option<(Dl, DlTarget)> = None;
    for m in Dl::ALL {
        match dl_target(http, st, &mut cand, m).await {
            Ok(t) => {
                let h = range_probe(http, &t, head.0, head.1).await;
                let tl = range_probe(http, &t, tail.0, tail.1).await;
                let ok = h.ok && tl.ok;
                methods.push(json!({"method": m.name(), "range_ok": ok, "head": h, "tail": tl}));
                if ok && best.is_none() {
                    best = Some((m, t));
                }
            }
            Err(e) => methods.push(json!({"method": m.name(), "range_ok": false, "unavailable": scrub_text(&e.to_string())})),
        }
    }
    // keep refreshed ids/urls for E7
    if let Some(l) = st.walk.largest.as_mut() {
        *l = cand.clone();
    }

    let mut details = json!({"file": redact_name(&cand.name, Some(size)), "methods": methods});
    let Some((best_m, t)) = best else {
        return Ok((Status::Fail, "no method honoured Range requests (206 with exact length)".into(), details));
    };
    st.best_dl = Some(best_m);

    // Throughput: up to 64 MiB, 4 parallel ranges.
    let total = size.min(64 * mb);
    let parts = 4u64;
    let part = total.div_ceil(parts);
    let started = Instant::now();
    let futs = (0..parts).filter(|i| i * part < total).map(|i| {
        let s = i * part;
        let e = ((i + 1) * part).min(total) - 1;
        let t = &t;
        async move {
            match http.send(mk_dl_req(t).range(s, e).max_body((e - s + 1) as usize + 1).no_store()).await {
                Ok(r) => (r.status, r.received, e - s + 1),
                Err(_) => (0, 0, e - s + 1),
            }
        }
    });
    let res = join_all(futs).await;
    let secs = started.elapsed().as_secs_f64().max(0.001);
    let got: u64 = res.iter().map(|r| r.1).sum();
    let all_206 = res.iter().all(|r| r.0 == 206 && r.1 == r.2);
    details["throughput"] = json!({
        "method": best_m.name(),
        "parallel_ranges": res.len(),
        "bytes": got,
        "seconds": (secs * 100.0).round() / 100.0,
        "MB_per_s": ((got as f64 / 1e6 / secs) * 100.0).round() / 100.0,
        "all_ranges_ok": all_206,
        "file_smaller_than_64MiB": size < 64 * mb,
    });
    Ok((Status::Pass, format!("Range works via {}; {:.1} MB/s with 4 parallel ranges", best_m.name(), got as f64 / 1e6 / secs), details))
}

// ---------------------------------------------------------------------------------------------
// E7 hash check
// ---------------------------------------------------------------------------------------------

pub async fn e7_hash(http: &Http, st: &mut State) -> ExpResult {
    finish("E7", "hash check", e7_inner(http, st).await)
}

async fn e7_inner(http: &Http, st: &mut State) -> Out {
    let Some(mut cand) = st.walk.hash_cand.clone() else {
        return Ok((Status::Skipped, "no file < 20 MiB with a quickXorHash in the listing".into(), Value::Null));
    };
    let listed = cand.quickxor.clone().unwrap_or_default();
    let mut order: Vec<Dl> = Vec::new();
    if let Some(b) = st.best_dl {
        order.push(b);
    }
    order.extend(Dl::ALL.iter().copied().filter(|m| Some(*m) != st.best_dl));
    let mut tried = Vec::new();
    for m in order {
        let t = match dl_target(http, st, &mut cand, m).await {
            Ok(t) => t,
            Err(e) => {
                tried.push(json!({"method": m.name(), "error": scrub_text(&e.to_string())}));
                continue;
            }
        };
        let req = if t.cookies { Req::get("E7", t.template, t.url.clone()) } else { Req::get("E7", t.template, t.url.clone()).no_cookies() };
        let started = Instant::now();
        match http.send(req.follow().max_body(cand.size as usize + 1)).await {
            Ok(r) if r.is_success() && !r.over_limit => {
                let mut h = QuickXor::new();
                h.update(&r.body);
                let computed = h.finalize_base64();
                let matches = computed == listed;
                let details = json!({
                    "file": redact_name(&cand.name, Some(cand.size)),
                    "method": m.name(),
                    "listed_size": cand.size,
                    "downloaded_bytes": r.body.len(),
                    "size_matches": r.body.len() as u64 == cand.size,
                    "match": matches,
                    "download_ms": started.elapsed().as_millis() as u64,
                    "failed_methods_before": tried,
                });
                return Ok((if matches { Status::Pass } else { Status::Fail }, format!("quickXorHash match = {matches} (via {})", m.name()), details));
            }
            Ok(r) => tried.push(json!({"method": m.name(), "status": r.status, "over_limit": r.over_limit})),
            Err(e) => tried.push(json!({"method": m.name(), "error": scrub_text(&e.to_string())})),
        }
    }
    Ok((Status::Fail, "could not download the file with any method".into(), json!({"tried": tried})))
}

// ---------------------------------------------------------------------------------------------
// E8 hidden bearer token
// ---------------------------------------------------------------------------------------------
//
// Real-tenant observation this probe is checking: with FedAuth/rtFa cookies only, the v2.x
// drive API (`/_api/v2.0/shares/...`, `/_api/v2.0/drives`, `/_api/v2.0/drive/root:/...`) returns
// 403, while the classic `/_api/web/GetFolderByServerRelativePath(...)/Files` listing works (no
// hash) and `/_layouts/15/download.aspx?UniqueId=` honours Range. QuickXorHash is only exposed by
// the v2.x API. Hypothesis: the SharePoint/OneDrive web UI turns its cookie session into a bearer
// token for that same v2.x API. This tries, in order, stopping at the first step that yields a
// usable token:
//   1. POST {site}/_api/contextinfo (cookies, Accept: application/json;odata=nometadata, empty
//      body) -> FormDigestValue. This is the standard SharePoint REST "request digest" call used
//      before any write; see Microsoft's SharePoint REST documentation ("getcontextwebinformation"
//      / `_api/contextinfo`).
//   2. POST {site}/_api/SP.OAuth.Token/Acquire with X-RequestDigest, trying up to 3 well-evidenced
//      body/Content-Type variants (resource = the tenant host; resource = the tenant host with any
//      `-my` suffix dropped, e.g. contoso-my.sharepoint.com -> contoso.sharepoint.com, since that is
//      the OneDrive-for-Business "MySite" host pattern; and a plain `application/json` variant
//      instead of `application/json;odata=verbose`). This endpoint is NOT publicly documented by
//      Microsoft; it is reverse-engineered from the SharePoint/OneDrive modern web client's own
//      network traffic (the same internal surface used by `SP.OAuth.NativeClient`, below) and is
//      recorded here as an untested hypothesis, not a confirmed API - every attempt and its
//      outcome is kept in the report either way.
//   3. As a 4th, non-token-value discovery attempt (bringing step 2's total to the requested
//      max of 4): GET {site}/_api/SP.OAuth.NativeClient with no auth. SharePoint Online responds
//      to this well-known (if undocumented) endpoint with a 302 to
//      login.microsoftonline.com/.../oauth2/authorize?client_id=...&resource=..., which is how
//      several community tools (e.g. PnP-style device-code flows) discover the first-party
//      client_id/resource pair for this tenant; it does not itself hand back a bearer token
//      through the cookie session, so it is only recorded as evidence, never used as `token`.
//   4. If a token was obtained: decode its JWT payload (no signature check; see `jwt.rs`) and
//      keep only aud / scp / roles / minutes-to-expiry / token length - never the token itself.
//      Then call `GET {site}/_api/v2.1/drive/root:/{path}:/children?$top=50&select=...` (and the
//      `v2.0` equivalent), with the bearer token, both without and with cookies, plus a bare
//      `GET {site}/_api/v{ver}/drives` as an extra access-check. Record status and whether
//      `file.hashes.quickXorHash` / `@content.downloadUrl` are present.
//   5. If a quickXorHash is present on some file < 20 MiB, download it (download URL, or
//      download.aspx by UniqueId) and compare with the existing `qxor` module.

pub async fn e8_hidden_token(http: &Http, st: &mut State) -> ExpResult {
    finish("E8", "hidden bearer token", e8_inner(http, st).await)
}

/// `contoso-my.sharepoint.com` -> `contoso.sharepoint.com`; any other host is returned unchanged.
fn root_host(host: &str) -> String {
    if let Some(dot) = host.find('.') {
        let (label, rest) = host.split_at(dot);
        if let Some(stripped) = label.strip_suffix("-my") {
            return format!("{stripped}{rest}");
        }
    }
    host.to_string()
}

/// Up to 3 `SP.OAuth.Token/Acquire` body/header variants (the 4th attempt, `SP.OAuth.NativeClient`,
/// is handled separately since it is a GET with no body).
fn token_acquire_variants(host: &str) -> Vec<(&'static str, String, &'static str)> {
    let root = root_host(host);
    let mut v = vec![("resource_host_verbose", format!("https://{host}"), "application/json;odata=verbose")];
    if root != host {
        v.push(("resource_hostroot_verbose", format!("https://{root}"), "application/json;odata=verbose"));
    }
    v.push(("resource_host_plain_json", format!("https://{host}"), "application/json"));
    v.truncate(3);
    v
}

async fn get_form_digest(http: &Http, base: &str) -> Result<String> {
    let url = format!("{base}/_api/contextinfo");
    let r = http.send(Req::post("E8", "/_api/contextinfo", url, Vec::new()).nometadata()).await?;
    if !r.is_success() {
        return Err(status_err(&r));
    }
    let v = r.json()?;
    v.get("FormDigestValue")
        .and_then(|d| d.as_str())
        .or_else(|| v["d"]["GetContextWebInformation"]["FormDigestValue"].as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("no FormDigestValue in response"))
}

fn extract_access_token(v: &Value) -> Option<String> {
    v.get("access_token")
        .and_then(|t| t.as_str())
        .or_else(|| v["d"].get("access_token").and_then(|t| t.as_str()))
        .or_else(|| v["d"]["Acquire"].get("access_token").and_then(|t| t.as_str()))
        .map(|s| s.to_string())
}

/// `/_api/{ver}/drive/root(:/{path}:)/children?$top=N&$select=...`, scoped to the library's
/// default drive (no drive-id discovery needed - that is the point of this fallback shape).
fn default_children_url(base: &str, rel: &str, top: u32, ver: &str) -> (String, String) {
    const SELECT: &str = "name,size,file,eTag,lastModifiedDateTime";
    if rel.is_empty() {
        (format!("{base}/_api/{ver}/drive/root/children?$top={top}&$select={SELECT}"), format!("/_api/{ver}/drive/root/children?$top=N&$select=..."))
    } else {
        (
            format!("{base}/_api/{ver}/drive/root:/{}:/children?$top={top}&$select={SELECT}", encode_path(rel)),
            format!("/_api/{ver}/drive/root:/{{path}}:/children?$top=N&$select=..."),
        )
    }
}

async fn e8_inner(http: &Http, st: &mut State) -> Out {
    let split = st.split.clone().ok_or_else(|| anyhow!("folder path unknown (E2 failed)"))?;
    let base = st.site_base();
    let mut attempts: Vec<Value> = Vec::new();

    // Step 1: form digest (needed to call SP.OAuth.Token/Acquire, which is a POST).
    let digest = match get_form_digest(http, &base).await {
        Ok(d) => {
            attempts.push(json!({"step": "contextinfo", "ok": true}));
            Some(d)
        }
        Err(e) => {
            attempts.push(json!({"step": "contextinfo", "ok": false, "error": scrub_text(&e.to_string())}));
            None
        }
    };

    // Step 2: up to 3 SP.OAuth.Token/Acquire variants, stopping at the first access_token.
    let mut token: Option<String> = None;
    let mut token_source: Option<&'static str> = None;
    if let Some(digest) = &digest {
        for (label, resource, content_type) in token_acquire_variants(&st.host) {
            let url = format!("{base}/_api/SP.OAuth.Token/Acquire");
            let body = serde_json::to_vec(&json!({"resource": resource})).unwrap_or_default();
            let req = Req::post("E8", "/_api/SP.OAuth.Token/Acquire", url, body).header("X-RequestDigest", digest).header("Content-Type", content_type).header("Accept", content_type);
            match http.send(req).await {
                Ok(r) if r.is_success() => {
                    let found = r.json().ok().and_then(|v| extract_access_token(&v));
                    attempts.push(json!({"step": "token_acquire", "variant": label, "status": r.status, "found_token": found.is_some()}));
                    if let Some(t) = found {
                        token = Some(t);
                        token_source = Some(label);
                        break;
                    }
                }
                Ok(r) => attempts.push(json!({"step": "token_acquire", "variant": label, "status": r.status, "found_token": false})),
                Err(e) => attempts.push(json!({"step": "token_acquire", "variant": label, "found_token": false, "error": scrub_text(&e.to_string())})),
            }
        }
    } else {
        attempts.push(json!({"step": "token_acquire", "skipped": true, "reason": "no form digest"}));
    }

    // Step 2d (4th attempt overall): SP.OAuth.NativeClient discovery, only if still no token.
    if token.is_none() {
        let url = format!("{base}/_api/SP.OAuth.NativeClient");
        match http.send(Req::get("E8", "/_api/SP.OAuth.NativeClient", url).no_store()).await {
            Ok(r) => {
                let redirect_host = r.header("location").and_then(|l| Url::parse(&l).ok()).map(|u| host_port(&u));
                attempts.push(json!({
                    "step": "nativeclient_discovery",
                    "status": r.status,
                    "is_redirect": matches!(r.status, 301 | 302 | 303 | 307 | 308),
                    "redirect_host_same_as_tenant_auth": redirect_host.map(|h| h.contains("login.microsoftonline") || h.contains("microsoftonline")),
                    "note": "discovery only; does not itself hand back a bearer token from the cookie session",
                }));
            }
            Err(e) => attempts.push(json!({"step": "nativeclient_discovery", "error": scrub_text(&e.to_string())})),
        }
    }

    let Some(token) = token else {
        return Ok((Status::Fail, "no bearer token obtained from the cookie session".into(), json!({"attempts": attempts})));
    };

    let claims = match jwt::decode_claims_redacted(&token) {
        Ok(c) => c,
        Err(e) => json!({"decode_error": scrub_text(&e.to_string())}),
    };

    // Step 3: v2.1 then v2.0, bearer-only then bearer+cookies, stopping at the first that lists.
    let mut v2_attempts: Vec<Value> = Vec::new();
    let mut listing: Option<(&'static str, Vec<Value>)> = None;
    for ver in ["v2.1", "v2.0"] {
        let drives_url = format!("{base}/_api/{ver}/drives");
        let drives_tpl = format!("/_api/{ver}/drives");
        match http.send(Req::get("E8", &drives_tpl, drives_url).json().header("Authorization", &format!("Bearer {token}")).no_cookies()).await {
            Ok(r) => v2_attempts.push(json!({"step": format!("{ver}_drives_bearer_only"), "status": r.status, "ok": r.is_success()})),
            Err(e) => v2_attempts.push(json!({"step": format!("{ver}_drives_bearer_only"), "error": scrub_text(&e.to_string())})),
        }

        if listing.is_some() {
            continue;
        }
        for use_cookies in [false, true] {
            let (url, tpl) = default_children_url(&base, &split.rel, 50, ver);
            let label: &'static str = match (ver, use_cookies) {
                ("v2.1", false) => "v2.1_children_bearer_only",
                ("v2.1", true) => "v2.1_children_bearer_and_cookies",
                ("v2.0", false) => "v2.0_children_bearer_only",
                _ => "v2.0_children_bearer_and_cookies",
            };
            let mut req = Req::get("E8", &tpl, url).json().header("Authorization", &format!("Bearer {token}"));
            if !use_cookies {
                req = req.no_cookies();
            }
            match http.send(req).await {
                Ok(r) if r.is_success() => match r.json() {
                    Ok(v) => {
                        let items = v["value"].as_array().cloned().unwrap_or_default();
                        v2_attempts.push(json!({"step": label, "status": r.status, "ok": true, "items": items.len()}));
                        listing = Some((label, items));
                        break;
                    }
                    Err(_) => v2_attempts.push(json!({"step": label, "status": r.status, "ok": false, "note": "non-json response"})),
                },
                Ok(r) => v2_attempts.push(json!({"step": label, "status": r.status, "ok": false})),
                Err(e) => v2_attempts.push(json!({"step": label, "ok": false, "error": scrub_text(&e.to_string())})),
            }
        }
    }

    let Some((listing_method, items)) = listing else {
        let details = json!({"token": {"source": token_source, "claims": claims}, "attempts": attempts, "v2_attempts": v2_attempts});
        return Ok((Status::Fail, "obtained a bearer token but v2.0/v2.1 listing still failed".into(), details));
    };

    let mut presence = Presence::default();
    for it in &items {
        presence.observe(it);
    }
    let cand = items.iter().map(parse_v2_entry).find(|e| !e.is_folder && e.quickxor.is_some() && e.size > 0 && e.size < 20 * 1024 * 1024);

    // Step 4: verify one small file's quickXorHash, if the listing exposed one.
    let mut verify = json!({"attempted": false});
    if let Some(c) = &cand {
        let dl = if let Some(u) = c.download_url.clone() {
            http.send(Req::get("E8", "<v2 downloadUrl>", u).max_body(c.size as usize + 1)).await
        } else if let Some(uid) = &c.unique_id {
            let url = format!("{base}/_layouts/15/download.aspx?UniqueId={}", encode_path(uid));
            http.send(Req::get("E8", "/_layouts/15/download.aspx?UniqueId={guid}", url).follow().max_body(c.size as usize + 1)).await
        } else {
            Err(anyhow!("no downloadUrl or UniqueId on the verification candidate"))
        };
        verify = match dl {
            Ok(r) if r.is_success() => {
                let mut h = QuickXor::new();
                h.update(&r.body);
                let computed = h.finalize_base64();
                let listed = c.quickxor.clone().unwrap_or_default();
                json!({
                    "attempted": true,
                    "file": redact_name(&c.name, Some(c.size)),
                    "downloaded_bytes": r.body.len(),
                    "size_matches": r.body.len() as u64 == c.size,
                    "match": computed == listed,
                })
            }
            Ok(r) => json!({"attempted": true, "ok": false, "status": r.status}),
            Err(e) => json!({"attempted": true, "ok": false, "error": scrub_text(&e.to_string())}),
        };
    }

    let status = match (cand.is_some(), verify.get("match").and_then(|m| m.as_bool())) {
        (true, Some(true)) => Status::Pass,
        (true, _) => Status::Fail,
        (false, _) => Status::Pass, // token + v2.x listing proven; just nothing small enough to verify with
    };
    let summary = format!(
        "bearer token via {} ({listing_method}); quickXorHash {}, downloadUrl {}, verify match = {}",
        token_source.unwrap_or("?"),
        if presence.quick_xor_hash { "yes" } else { "no" },
        presence.download_url.unwrap_or("no"),
        verify.get("match").and_then(|m| m.as_bool()).map(|b| b.to_string()).unwrap_or_else(|| "n/a".into()),
    );
    let details = json!({
        "token": {"source": token_source, "claims": claims},
        "attempts": attempts,
        "v2_attempts": v2_attempts,
        "listing_method": listing_method,
        "fields": presence,
        "verify": verify,
    });
    Ok((status, summary, details))
}
