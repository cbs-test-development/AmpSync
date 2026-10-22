//! Classification and parsing of SharePoint folder URLs the user may paste.

use base64::Engine;
use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UrlKind {
    /// `https://x.sharepoint.com/:f:/s/Site/E...`
    SharingLink,
    /// Browser address bar: `.../Forms/AllItems.aspx?id=%2Fsites%2F...`
    IdParam,
    /// `https://x.sharepoint.com/sites/Site/Shared Documents/Folder`
    DirectPath,
}

#[derive(Debug, Clone)]
pub struct ParsedUrl {
    pub host: String,
    pub kind: UrlKind,
    /// Decoded server-relative folder path (not known for sharing links until resolved).
    pub folder_server_rel: Option<String>,
}

/// Split of a decoded server-relative path into site / library / path inside the library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitPath {
    /// `/sites/Site`, `/teams/X`, `/personal/user_x` or `` (root site)
    pub site: String,
    /// Library URL name as it appears in the path (may be localised), e.g. `Shared Documents`.
    pub library: String,
    /// Path inside the library without leading slash (may be empty).
    pub rel: String,
}

pub fn classify_url(raw: &str) -> Result<ParsedUrl, String> {
    let url = Url::parse(raw.trim()).map_err(|e| format!("not a valid URL: {e}"))?;
    if url.scheme() != "https" && url.scheme() != "http" {
        return Err("URL must be http(s)".into());
    }
    let mut host = url.host_str().ok_or("URL has no host")?.to_ascii_lowercase();
    if let Some(p) = url.port() {
        host = format!("{host}:{p}"); // only ever non-default in the local self-test
    }

    let first_seg = url.path_segments().and_then(|mut s| s.next()).unwrap_or("");
    if is_sharing_token(first_seg) {
        return Ok(ParsedUrl { host, kind: UrlKind::SharingLink, folder_server_rel: None });
    }

    // ?id= (new UI) or ?RootFolder= (classic UI), case-insensitive key.
    for (k, v) in url.query_pairs() {
        if k.eq_ignore_ascii_case("id") || k.eq_ignore_ascii_case("RootFolder") {
            let v = v.trim().to_string();
            if v.starts_with('/') {
                return Ok(ParsedUrl {
                    host,
                    kind: UrlKind::IdParam,
                    folder_server_rel: Some(trim_trailing_slash(&v)),
                });
            }
        }
    }

    let decoded = percent_decode_str(url.path()).decode_utf8_lossy().to_string();
    let decoded = strip_view_suffix(&decoded);
    if decoded.is_empty() || decoded == "/" {
        return Err("URL has no folder path".into());
    }
    Ok(ParsedUrl { host, kind: UrlKind::DirectPath, folder_server_rel: Some(decoded) })
}

/// Scheme used for requests. Always https, except for the local self-test (`AMPSYNC_PROBE_TEST_HTTP=1`).
pub fn scheme() -> &'static str {
    if std::env::var_os("AMPSYNC_PROBE_TEST_HTTP").is_some() {
        "http"
    } else {
        "https"
    }
}

/// `:f:`, `:x:`, `:b:`, `:w:` ... the first path segment of a sharing link.
fn is_sharing_token(seg: &str) -> bool {
    let b = seg.as_bytes();
    b.len() == 3 && b[0] == b':' && b[2] == b':' && b[1].is_ascii_alphabetic()
}

fn trim_trailing_slash(s: &str) -> String {
    let t = s.trim_end_matches('/');
    if t.is_empty() {
        "/".into()
    } else {
        t.to_string()
    }
}

/// Drop `/Forms/AllItems.aspx` style suffixes (view pages) from a direct path.
fn strip_view_suffix(path: &str) -> String {
    let mut segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if let Some(i) = segs.iter().position(|s| s.eq_ignore_ascii_case("Forms")) {
        segs.truncate(i);
    }
    if let Some(last) = segs.last() {
        if last.to_ascii_lowercase().ends_with(".aspx") {
            segs.pop();
        }
    }
    if segs.is_empty() {
        return String::new();
    }
    format!("/{}", segs.join("/"))
}

/// Split a decoded server-relative path. Sub-sites (`/sites/A/B/Library`) are NOT detected:
/// the probe treats `/sites/A` as the web and `B` as the library (documented limitation).
pub fn split_site(path: &str) -> Option<SplitPath> {
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return None;
    }
    let first = segs[0].to_ascii_lowercase();
    let site_len = if matches!(first.as_str(), "sites" | "teams" | "personal") { 2 } else { 0 };
    if segs.len() < site_len + 1 {
        return None;
    }
    let site = if site_len == 0 { String::new() } else { format!("/{}/{}", segs[0], segs[1]) };
    let library = segs[site_len].to_string();
    let rel = segs[site_len + 1..].join("/");
    Some(SplitPath { site, library, rel })
}

/// English default library URL name. Localised tenants use other names (e.g. `Freigegebene Dokumente`).
pub fn is_default_english_library(name: &str) -> bool {
    name.eq_ignore_ascii_case("Shared Documents") || name.eq_ignore_ascii_case("Documents")
}

/// `u!` + base64url (no padding) of the URL, as used by `/shares/{id}`.
pub fn share_id(url: &str) -> String {
    let b = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(url.as_bytes());
    format!("u!{b}")
}

/// Characters that must be escaped inside a URL *path* segment (everything except unreserved + `/`).
const PATH_SET: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~').remove(b'/');

pub fn encode_path(p: &str) -> String {
    utf8_percent_encode(p, PATH_SET).to_string()
}

/// Escape for use inside `decodedurl='...'`: single quotes are doubled, then everything is
/// percent-encoded (including `/` is NOT escaped; SharePoint accepts it).
pub fn odata_string_arg(p: &str) -> String {
    encode_path(&p.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sharing_link() {
        let p = classify_url("https://contoso.sharepoint.com/:f:/s/Team/EabcDEF123?e=XyZ").unwrap();
        assert_eq!(p.kind, UrlKind::SharingLink);
        assert_eq!(p.host, "contoso.sharepoint.com");
        assert!(p.folder_server_rel.is_none());
        let p = classify_url("https://contoso.sharepoint.com/:x:/r/sites/Team/Shared%20Documents/a.xlsx").unwrap();
        assert_eq!(p.kind, UrlKind::SharingLink);
    }

    #[test]
    fn id_param_is_percent_decoded() {
        let u = "https://contoso.sharepoint.com/sites/Team/Shared%20Documents/Forms/AllItems.aspx?id=%2Fsites%2FTeam%2FShared%20Documents%2FProjects%2FBig%20%26%20Small&viewid=abc";
        let p = classify_url(u).unwrap();
        assert_eq!(p.kind, UrlKind::IdParam);
        assert_eq!(p.folder_server_rel.as_deref(), Some("/sites/Team/Shared Documents/Projects/Big & Small"));
    }

    #[test]
    fn root_folder_param_and_trailing_slash() {
        let u = "https://c.sharepoint.com/sites/T/Docs/Forms/AllItems.aspx?RootFolder=%2Fsites%2FT%2FDocs%2FX%2F";
        let p = classify_url(u).unwrap();
        assert_eq!(p.folder_server_rel.as_deref(), Some("/sites/T/Docs/X"));
    }

    #[test]
    fn direct_path_decoded() {
        let p = classify_url("https://c.sharepoint.com/sites/Team/Shared%20Documents/My%20Folder%20%231").unwrap();
        assert_eq!(p.kind, UrlKind::DirectPath);
        assert_eq!(p.folder_server_rel.as_deref(), Some("/sites/Team/Shared Documents/My Folder #1"));
    }

    #[test]
    fn direct_path_view_page_without_id_is_library_root() {
        let p = classify_url("https://c.sharepoint.com/sites/Team/Shared%20Documents/Forms/AllItems.aspx").unwrap();
        assert_eq!(p.folder_server_rel.as_deref(), Some("/sites/Team/Shared Documents"));
    }

    #[test]
    fn rejects_garbage() {
        assert!(classify_url("not a url").is_err());
        assert!(classify_url("ftp://x.sharepoint.com/a").is_err());
        assert!(classify_url("https://x.sharepoint.com/").is_err());
    }

    #[test]
    fn split_default_and_localised_libraries() {
        let s = split_site("/sites/Team/Shared Documents/A/B").unwrap();
        assert_eq!(s, SplitPath { site: "/sites/Team".into(), library: "Shared Documents".into(), rel: "A/B".into() });
        assert!(is_default_english_library(&s.library));

        let s = split_site("/sites/Team/Freigegebene Dokumente/Ordner").unwrap();
        assert_eq!(s.library, "Freigegebene Dokumente");
        assert_eq!(s.rel, "Ordner");
        assert!(!is_default_english_library(&s.library));

        let s = split_site("/sites/Team/Documents").unwrap();
        assert_eq!(s.rel, "");

        let s = split_site("/personal/jo_contoso_com/Documents/x").unwrap();
        assert_eq!(s.site, "/personal/jo_contoso_com");

        let s = split_site("/Shared Documents/x").unwrap();
        assert_eq!(s.site, "");
        assert_eq!(s.library, "Shared Documents");
        assert!(split_site("/sites/Team").is_none());
    }

    #[test]
    fn share_id_encoding() {
        // "https://a/b?c=d~e" -> base64url without padding, prefixed by u!
        let id = share_id("https://a/b?c=d~e");
        assert!(id.starts_with("u!"));
        assert!(!id.contains('=') && !id.contains('+') && !id.contains('/'));
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(&id[2..]).unwrap();
        assert_eq!(raw, b"https://a/b?c=d~e");
        // known value: base64url("https://example.com") = aHR0cHM6Ly9leGFtcGxlLmNvbQ
        assert_eq!(share_id("https://example.com"), "u!aHR0cHM6Ly9leGFtcGxlLmNvbQ");
        // '?' and '>' produce '/' and '+' in std base64 -> must be '_' and '-' here
        assert_eq!(share_id("???>>>"), "u!Pz8_Pj4-");
    }

    #[test]
    fn path_encoding() {
        assert_eq!(encode_path("/a b/c&d#e%f"), "/a%20b/c%26d%23e%25f");
        assert_eq!(odata_string_arg("/it's/x y"), "/it%27%27s/x%20y");
        assert_eq!(encode_path("/ü"), "/%C3%BC");
    }
}
