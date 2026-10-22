//! Understand what the user pasted.
//!
//! [`parse`] classifies a SharePoint / OneDrive-for-Business URL without any network access.
//! Sharing links (`/:f:/s/...`) cannot be turned into a folder path offline; they are returned
//! as [`LinkKind::Sharing`] and `Remote::resolve` follows the redirect.

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use url::Url;

use crate::Error;

/// A successfully classified link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedLink {
    /// Lower-case tenant host, e.g. `contoso.sharepoint.com` or `contoso-my.sharepoint.com`.
    /// Sessions are stored per host (D4).
    pub host: String,
    pub kind: LinkKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkKind {
    /// `https://host/:f:/s/Site/E...` (any `:x:` first segment). The full URL, normalised to https.
    /// Resolving it means following the 302 with the cookie jar on.
    Sharing { url: Url },
    /// A folder address: `?id=` / `?RootFolder=` address-bar URLs, or a direct path.
    Folder {
        /// `https://host` plus the web path, e.g. `https://contoso.sharepoint.com/sites/Team`
        /// or `https://contoso-my.sharepoint.com/personal/jo_contoso_com`. Sub-sites
        /// (`/sites/A/B/Library`) are not detected offline; `/sites/A` is assumed to be the web.
        site_url: Url,
        /// Decoded (not percent-encoded) server-relative folder path, no trailing slash,
        /// e.g. `/sites/Team/Shared Documents/Projects`.
        server_relative_path: String,
    },
}

/// Split of a decoded server-relative path into web / library / path inside the library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitPath {
    /// `/sites/Site`, `/teams/X`, `/personal/user_x`, or empty for the root site.
    pub site: String,
    /// Library URL name as it appears in the path (may be localised), e.g. `Shared Documents`.
    pub library: String,
    /// Path inside the library without leading slash (may be empty).
    pub rel: String,
}

/// Host suffixes accepted as SharePoint Online (commercial, GCC High/DoD, China, Germany).
const SHAREPOINT_SUFFIXES: &[&str] = &[
    ".sharepoint.com",
    ".sharepoint.us",
    ".sharepoint.cn",
    ".sharepoint.de",
];

/// Classify the text the user pasted.
///
/// Accepts `https://` URLs (a missing scheme is assumed to be https; `http://` is upgraded) on a
/// SharePoint Online host. Anything else returns [`Error::InvalidLink`].
pub fn parse(input: &str) -> Result<ParsedLink, Error> {
    let trimmed = input
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '<' || c == '>');
    if trimmed.is_empty() {
        return Err(Error::InvalidLink("nothing was pasted".into()));
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let mut url =
        Url::parse(&with_scheme).map_err(|e| Error::InvalidLink(format!("not a valid URL: {e}")))?;
    match url.scheme() {
        "https" => {}
        "http" => {
            url.set_scheme("https")
                .map_err(|_| Error::InvalidLink("cannot use this URL".into()))?;
        }
        other => {
            return Err(Error::InvalidLink(format!(
                "unsupported scheme `{other}`; use an https link"
            )));
        }
    }
    if !matches!(url.port(), None | Some(443)) {
        return Err(Error::InvalidLink("unexpected port number in link".into()));
    }
    let host = url
        .host_str()
        .ok_or_else(|| Error::InvalidLink("link has no host".into()))?
        .to_ascii_lowercase();
    if !is_sharepoint_host(&host) {
        return Err(Error::InvalidLink(format!(
            "`{host}` is not a SharePoint Online address (expected something like contoso.sharepoint.com)"
        )));
    }
    url.set_fragment(None);

    // Sharing links: first path segment is `:f:`, `:x:`, `:b:` ...
    let first_seg = url.path_segments().and_then(|mut s| s.next()).unwrap_or("");
    if is_sharing_token(first_seg) {
        return Ok(ParsedLink {
            host,
            kind: LinkKind::Sharing { url },
        });
    }

    // Address bar: `?id=` (modern UI, also `onedrive.aspx?id=`) or `?RootFolder=` (classic UI).
    let mut path = None;
    for (k, v) in url.query_pairs() {
        if k.eq_ignore_ascii_case("id") || k.eq_ignore_ascii_case("RootFolder") {
            let v = v.trim();
            if v.starts_with('/') {
                path = Some(trim_trailing_slash(v));
                break;
            }
        }
    }
    // Direct path: `/sites/X/Shared Documents/Y`, possibly with a view page suffix.
    let path = match path {
        Some(p) => p,
        None => direct_path(&url),
    };
    if path.is_empty() || path == "/" {
        return Err(Error::InvalidLink("the link does not point to a folder".into()));
    }
    let split = split_site(&path).ok_or_else(|| {
        Error::InvalidLink("the link points to a site, not to a folder in a library".into())
    })?;
    let site_url = Url::parse(&format!("https://{host}{}", encode_path(&split.site)))
        .map_err(|e| Error::InvalidLink(format!("cannot build site address: {e}")))?;
    Ok(ParsedLink {
        host,
        kind: LinkKind::Folder {
            site_url,
            server_relative_path: path,
        },
    })
}

fn is_sharepoint_host(host: &str) -> bool {
    SHAREPOINT_SUFFIXES
        .iter()
        .any(|s| host.len() > s.len() && host.ends_with(s))
}

/// `:f:`, `:x:`, `:b:`, `:w:` ... the first path segment of a sharing link.
fn is_sharing_token(seg: &str) -> bool {
    let b = seg.as_bytes();
    b.len() == 3 && b[0] == b':' && b[2] == b':' && b[1].is_ascii_alphabetic()
}

fn trim_trailing_slash(s: &str) -> String {
    let t = s.trim_end_matches('/');
    if t.is_empty() { "/".into() } else { t.to_string() }
}

/// Decoded path of `url` with view pages (`/Forms/AllItems.aspx`, `/_layouts/...`, `x.aspx`) removed.
fn direct_path(url: &Url) -> String {
    let decoded = percent_decode_str(url.path()).decode_utf8_lossy().to_string();
    let mut segs: Vec<&str> = decoded.split('/').filter(|s| !s.is_empty()).collect();
    if let Some(i) = segs
        .iter()
        .position(|s| s.eq_ignore_ascii_case("Forms") || s.to_ascii_lowercase().starts_with("_layouts"))
    {
        segs.truncate(i);
    }
    if segs
        .last()
        .is_some_and(|l| l.to_ascii_lowercase().ends_with(".aspx"))
    {
        segs.pop();
    }
    if segs.is_empty() {
        String::new()
    } else {
        format!("/{}", segs.join("/"))
    }
}

/// Split a decoded server-relative path. Sub-sites (`/sites/A/B/Library`) are NOT detected:
/// `/sites/A` is treated as the web and `B` as the library.
/// Returns `None` when there is no library segment.
pub fn split_site(path: &str) -> Option<SplitPath> {
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return None;
    }
    let first = segs[0].to_ascii_lowercase();
    let site_len = if matches!(first.as_str(), "sites" | "teams" | "personal") {
        2
    } else {
        0
    };
    if segs.len() < site_len + 1 {
        return None;
    }
    let site = if site_len == 0 {
        String::new()
    } else {
        format!("/{}/{}", segs[0], segs[1])
    };
    let library = segs[site_len].to_string();
    let rel = segs[site_len + 1..].join("/");
    Some(SplitPath { site, library, rel })
}

/// Characters escaped inside a URL *path* (everything except unreserved characters and `/`).
const PATH_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~')
    .remove(b'/');

/// Percent-encode a server-relative path for use in a URL path. `/` is kept.
pub fn encode_path(p: &str) -> String {
    utf8_percent_encode(p, PATH_SET).to_string()
}

/// Escape a server-relative path for use inside `decodedurl='...'` in REST calls: single quotes
/// are doubled, then the string is percent-encoded (`/` stays, SharePoint accepts it).
pub fn odata_string_arg(p: &str) -> String {
    encode_path(&p.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(input: &str) -> (String, String, String) {
        match parse(input).unwrap() {
            ParsedLink {
                host,
                kind:
                    LinkKind::Folder {
                        site_url,
                        server_relative_path,
                    },
            } => (host, site_url.to_string(), server_relative_path),
            other => panic!("expected folder, got {other:?}"),
        }
    }

    fn invalid(input: &str) {
        match parse(input) {
            Err(Error::InvalidLink(_)) => {}
            other => panic!("expected InvalidLink for {input:?}, got {other:?}"),
        }
    }

    #[test]
    fn sharing_link_team_site() {
        let p = parse("https://contoso.sharepoint.com/:f:/s/Team/EabcDEF123?e=XyZ").unwrap();
        assert_eq!(p.host, "contoso.sharepoint.com");
        match p.kind {
            LinkKind::Sharing { url } => {
                assert_eq!(
                    url.as_str(),
                    "https://contoso.sharepoint.com/:f:/s/Team/EabcDEF123?e=XyZ"
                )
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn sharing_link_personal_and_other_tokens() {
        let p = parse("https://fabrikam-my.sharepoint.com/:f:/g/personal/jo_fabrikam_com/Eabc?e=1").unwrap();
        assert_eq!(p.host, "fabrikam-my.sharepoint.com");
        assert!(matches!(p.kind, LinkKind::Sharing { .. }));
        let p = parse("https://contoso.sharepoint.com/:x:/r/sites/Team/Shared%20Documents/a.xlsx").unwrap();
        assert!(matches!(p.kind, LinkKind::Sharing { .. }));
    }

    #[test]
    fn id_param_is_percent_decoded() {
        let u = "https://contoso.sharepoint.com/sites/Team/Shared%20Documents/Forms/AllItems.aspx?id=%2Fsites%2FTeam%2FShared%20Documents%2FProjects%2FBig%20%26%20Small&viewid=abc";
        let (host, site, path) = folder(u);
        assert_eq!(host, "contoso.sharepoint.com");
        assert_eq!(site, "https://contoso.sharepoint.com/sites/Team");
        assert_eq!(path, "/sites/Team/Shared Documents/Projects/Big & Small");
    }

    #[test]
    fn onedrive_aspx_id_param() {
        let u = "https://contoso-my.sharepoint.com/personal/jo_contoso_com/_layouts/15/onedrive.aspx?id=%2Fpersonal%2Fjo_contoso_com%2FDocuments%2FBackups&ga=1";
        let (host, site, path) = folder(u);
        assert_eq!(host, "contoso-my.sharepoint.com");
        assert_eq!(site, "https://contoso-my.sharepoint.com/personal/jo_contoso_com");
        assert_eq!(path, "/personal/jo_contoso_com/Documents/Backups");
    }

    #[test]
    fn all_items_id_param_uppercase_key() {
        let u = "https://contoso.sharepoint.com/sites/T/Docs/Forms/AllItems.aspx?ID=%2Fsites%2FT%2FDocs%2FX";
        assert_eq!(folder(u).2, "/sites/T/Docs/X");
    }

    #[test]
    fn root_folder_param_and_trailing_slash() {
        let u = "https://contoso.sharepoint.com/sites/T/Docs/Forms/AllItems.aspx?RootFolder=%2Fsites%2FT%2FDocs%2FX%2F";
        let (_, site, path) = folder(u);
        assert_eq!(path, "/sites/T/Docs/X");
        assert_eq!(site, "https://contoso.sharepoint.com/sites/T");
    }

    #[test]
    fn direct_path_decoded() {
        let (_, site, path) =
            folder("https://contoso.sharepoint.com/sites/Team/Shared%20Documents/My%20Folder%20%231");
        assert_eq!(path, "/sites/Team/Shared Documents/My Folder #1");
        assert_eq!(site, "https://contoso.sharepoint.com/sites/Team");
    }

    #[test]
    fn direct_path_in_personal_site() {
        let (host, site, path) =
            folder("https://fabrikam-my.sharepoint.com/personal/jo_fabrikam_com/Documents/Photos/");
        assert_eq!(host, "fabrikam-my.sharepoint.com");
        assert_eq!(
            site,
            "https://fabrikam-my.sharepoint.com/personal/jo_fabrikam_com"
        );
        assert_eq!(path, "/personal/jo_fabrikam_com/Documents/Photos");
    }

    #[test]
    fn direct_path_view_page_is_library_root() {
        let (_, _, path) =
            folder("https://contoso.sharepoint.com/sites/Team/Shared%20Documents/Forms/AllItems.aspx");
        assert_eq!(path, "/sites/Team/Shared Documents");
    }

    #[test]
    fn site_path_with_non_ascii_gets_encoded_site_url() {
        let (_, site, path) = folder("https://contoso.sharepoint.com/sites/Équipe/Documents/x");
        assert_eq!(site, "https://contoso.sharepoint.com/sites/%C3%89quipe");
        assert_eq!(path, "/sites/Équipe/Documents/x");
    }

    #[test]
    fn root_site_library() {
        let (_, site, path) = folder("https://contoso.sharepoint.com/Shared%20Documents/A");
        assert_eq!(site, "https://contoso.sharepoint.com/");
        assert_eq!(path, "/Shared Documents/A");
    }

    #[test]
    fn scheme_handling_and_whitespace() {
        let (host, _, _) = folder("  contoso.sharepoint.com/sites/Team/Documents/A \n");
        assert_eq!(host, "contoso.sharepoint.com");
        let p = parse("http://contoso.sharepoint.com/:f:/s/Team/Eabc").unwrap();
        match p.kind {
            LinkKind::Sharing { url } => assert_eq!(url.scheme(), "https"),
            other => panic!("{other:?}"),
        }
        let (host, _, _) = folder("HTTPS://CONTOSO.SharePoint.com/sites/Team/Documents/A");
        assert_eq!(host, "contoso.sharepoint.com");
    }

    #[test]
    fn rejects_non_sharepoint_hosts() {
        invalid("https://example.com/sites/Team/Documents/A");
        invalid("https://contoso.sharepoint.com.evil.example/sites/Team/Documents/A");
        invalid("https://sharepoint.com/sites/Team/Documents/A");
        invalid("https://notsharepoint.com/:f:/s/Team/Eabc");
        invalid("https://contoso.onedrive.com/x");
    }

    #[test]
    fn rejects_garbage() {
        invalid("");
        invalid("   ");
        invalid("not a url");
        invalid("ftp://contoso.sharepoint.com/a");
        invalid("https://contoso.sharepoint.com/");
        invalid("https://contoso.sharepoint.com:8443/sites/T/Docs/A");
        // a site with no library is not a folder
        invalid("https://contoso.sharepoint.com/sites/Team");
        invalid("https://contoso-my.sharepoint.com/personal/jo_contoso_com/_layouts/15/onedrive.aspx");
    }

    #[test]
    fn split_default_and_localised_libraries() {
        let s = split_site("/sites/Team/Shared Documents/A/B").unwrap();
        assert_eq!(
            s,
            SplitPath {
                site: "/sites/Team".into(),
                library: "Shared Documents".into(),
                rel: "A/B".into()
            }
        );
        let s = split_site("/sites/Team/Freigegebene Dokumente/Ordner").unwrap();
        assert_eq!(s.library, "Freigegebene Dokumente");
        assert_eq!(s.rel, "Ordner");
        assert_eq!(split_site("/sites/Team/Documents").unwrap().rel, "");
        assert_eq!(
            split_site("/personal/jo_contoso_com/Documents/x").unwrap().site,
            "/personal/jo_contoso_com"
        );
        let s = split_site("/Shared Documents/x").unwrap();
        assert_eq!((s.site.as_str(), s.library.as_str()), ("", "Shared Documents"));
        assert!(split_site("/sites/Team").is_none());
    }

    #[test]
    fn path_encoding() {
        assert_eq!(encode_path("/a b/c&d#e%f"), "/a%20b/c%26d%23e%25f");
        assert_eq!(odata_string_arg("/it's/x y"), "/it%27%27s/x%20y");
        assert_eq!(encode_path("/ü"), "/%C3%BC");
    }
}
