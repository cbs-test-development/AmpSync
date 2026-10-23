//! Redaction helpers. Everything that ends up in the report or on the terminal passes through here.
//! Rules: never cookie values; file/folder names become `<ext, size>`; path segments become
//! `<seg:len>` except a small allow-list of structural SharePoint tokens; URLs in free text are
//! reduced to their host.

use url::Url;

/// Structural path tokens that carry no tenant-specific information.
const KEEP_SEGMENTS: &[&str] = &[
    "sites", "teams", "personal", "_layouts", "_api", "forms", "_forms", "v2.0", "web", "drives", "drive", "items", "root:", "shares", "_vti_bin",
];

fn is_sharing_token(seg: &str) -> bool {
    let b = seg.as_bytes();
    b.len() == 3 && b[0] == b':' && b[2] == b':'
}

fn keep_segment(seg: &str, prev: Option<&str>) -> bool {
    let l = seg.to_ascii_lowercase();
    if KEEP_SEGMENTS.contains(&l.as_str()) || is_sharing_token(seg) {
        return true;
    }
    // `/:f:/s/...` : the r/s/t/g flavour letter directly after a sharing token
    if prev.map(is_sharing_token).unwrap_or(false) && matches!(l.as_str(), "r" | "s" | "t" | "g") {
        return true;
    }
    if l == "15" && prev.map(|p| p.eq_ignore_ascii_case("_layouts")).unwrap_or(false) {
        return true;
    }
    // well-known page names
    matches!(l.as_str(), "allitems.aspx" | "download.aspx" | "onedrive.aspx" | "authenticate.aspx" | "default.aspx" | "start.aspx")
}

/// `/sites/Team/Shared Documents/A/b.pdf` -> `/sites/<seg:4>/<seg:16>/<seg:1>/<seg:5>`
pub fn path_shape(path: &str) -> String {
    let mut out = String::new();
    let mut prev: Option<&str> = None;
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        out.push('/');
        // The segment right after sites/teams/personal is a site name: always redact.
        let after_site = prev.map(|p| matches!(p.to_ascii_lowercase().as_str(), "sites" | "teams" | "personal")).unwrap_or(false);
        if !after_site && keep_segment(seg, prev) {
            out.push_str(seg);
        } else {
            out.push_str(&format!("<seg:{}>", seg.chars().count()));
        }
        prev = Some(seg);
    }
    if out.is_empty() {
        "/".into()
    } else {
        out
    }
}

/// Host + path shape + query keys only (`?id,e`).
pub fn url_shape(url: &Url) -> String {
    let mut s = format!("{}{}", url.host_str().unwrap_or("?"), path_shape(url.path()));
    let keys: Vec<String> = url.query_pairs().map(|(k, _)| k.to_string()).collect();
    if !keys.is_empty() {
        s.push('?');
        s.push_str(&keys.join(","));
    }
    s
}

/// File/folder name -> `<.ext, 1234 B>`; extension kept only if short and alphanumeric.
pub fn redact_name(name: &str, size: Option<u64>) -> String {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e)
        .filter(|e| !e.is_empty() && e.len() <= 8 && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .map(|e| format!(".{}", e.to_ascii_lowercase()))
        .unwrap_or_else(|| "-".to_string());
    match size {
        Some(sz) => format!("<{ext}, {sz} B>"),
        None => format!("<{ext}>"),
    }
}

/// Reduce every URL inside free text (errors, messages) to `scheme://host/<redacted>` and blank
/// the values of `FedAuth=` / `rtFa=` style assignments.
pub fn scrub_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(i) = find_url_start(rest) {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | ')' | '(' | ',' | '`'))
            .unwrap_or(tail.len());
        let (u, after) = tail.split_at(end);
        out.push_str(&host_only(u));
        rest = after;
    }
    out.push_str(rest);
    scrub_cookie_assignments(&out)
}

fn find_url_start(s: &str) -> Option<usize> {
    match (s.find("https://"), s.find("http://")) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn host_only(u: &str) -> String {
    let scheme_end = u.find("://").map(|i| i + 3).unwrap_or(0);
    let after = &u[scheme_end..];
    let host_end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let host = &after[..host_end];
    if host_end == after.len() {
        format!("{}{}", &u[..scheme_end], host)
    } else {
        format!("{}{}/<redacted>", &u[..scheme_end], host)
    }
}

fn scrub_cookie_assignments(s: &str) -> String {
    let mut out = s.to_string();
    for name in ["FedAuth", "rtFa", "SPOIDCRL", "tempauth", "access_token", "ESTSAUTH", "ESTSAUTHPERSISTENT"] {
        let pat = format!("{name}=");
        let mut search_from = 0;
        while let Some(rel) = out[search_from..].find(&pat) {
            let start = search_from + rel + pat.len();
            let end = out[start..].find(|c: char| c == ';' || c.is_whitespace() || c == '&').map(|e| start + e).unwrap_or(out.len());
            out.replace_range(start..end, "<redacted>");
            search_from = start + "<redacted>".len();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_hide_names_keep_structure() {
        assert_eq!(
            path_shape("/sites/Team/Shared Documents/Projects/a.pdf"),
            "/sites/<seg:4>/<seg:16>/<seg:8>/<seg:5>"
        );
        assert_eq!(path_shape("/:f:/s/Team/Eabc"), "/:f:/s/<seg:4>/<seg:4>");
        assert_eq!(path_shape("/_api/v2.0/drives/b!abc/items/01XYZ/children"), "/_api/v2.0/drives/<seg:5>/items/<seg:5>/<seg:8>");
        assert_eq!(path_shape("/"), "/");
        assert_eq!(path_shape("/sites/s/t/r"), "/sites/<seg:1>/<seg:1>/<seg:1>");
        assert_eq!(path_shape("/_layouts/15/download.aspx"), "/_layouts/15/download.aspx");
    }

    #[test]
    fn url_shape_keeps_only_query_keys() {
        let u = Url::parse("https://c.sharepoint.com/sites/T/Docs/Forms/AllItems.aspx?id=%2Fsites%2FT%2FSecret&e=xyz").unwrap();
        let s = url_shape(&u);
        assert_eq!(s, "c.sharepoint.com/sites/<seg:1>/<seg:4>/Forms/AllItems.aspx?id,e");
        assert!(!s.contains("Secret"));
    }

    #[test]
    fn names_become_ext_and_size() {
        assert_eq!(redact_name("Quarterly Report.PDF", Some(1234)), "<.pdf, 1234 B>");
        assert_eq!(redact_name("noext", Some(0)), "<-, 0 B>");
        assert_eq!(redact_name("weird.ex t!", None), "<->");
        assert!(!redact_name("secret.docx", Some(1)).contains("secret"));
    }

    #[test]
    fn scrub_urls_and_cookies() {
        let s = scrub_text("error sending request for url (https://c.sharepoint.com/sites/Secret/a%20b?id=1): timed out");
        assert_eq!(s, "error sending request for url (https://c.sharepoint.com/<redacted>): timed out");
        let s = scrub_text("Cookie: FedAuth=77u/abc==; rtFa=zzzz; other=1");
        assert!(!s.contains("77u") && !s.contains("zzzz"));
        assert!(s.contains("other=1"));
        assert_eq!(scrub_text("see http://127.0.0.1:9222/json now"), "see http://127.0.0.1:9222/<redacted> now");
        assert_eq!(scrub_text("no urls here"), "no urls here");
        assert_eq!(scrub_text("host https://a.b"), "host https://a.b");
    }
}
