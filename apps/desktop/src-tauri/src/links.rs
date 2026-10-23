//! What the paste, drop and tray entry points hand us: find the link in a piece of text, take off
//! an Outlook / Teams Safe Links wrapper, and classify it with `link::parse`.

use ampsync_core::link::{self, LinkKind};
use serde::Serialize;
use url::Url;

/// Host suffixes of Microsoft's Safe Links rewriter (commercial and US government clouds). The
/// real address is in the `url` query parameter.
const SAFELINKS_SUFFIXES: &[&str] = &[
    ".safelinks.protection.outlook.com",
    ".safelinks.protection.office365.us",
];

/// How many nested Safe Links wrappers are removed (a forwarded mail can wrap twice).
const MAX_UNWRAP: usize = 3;

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct ParsedLinkInfo {
    /// Lower-case tenant host, e.g. `contoso.sharepoint.com`.
    pub host: String,
    /// "sharing" for a `/:f:/s/...` link, "folder" for an address-bar URL or direct path.
    pub kind: &'static str,
    /// The text to put in the link field: trimmed, and with any Safe Links wrapper removed.
    pub link: String,
}

/// The address inside a Safe Links URL, or `None` when `s` is not one.
fn unwrap_safelinks(s: &str) -> Option<String> {
    let url = Url::parse(s).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    if !SAFELINKS_SUFFIXES.iter().any(|x| host.ends_with(x)) {
        return None;
    }
    url.query_pairs()
        .find(|(k, _)| k == "url")
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn strip_wrapping(s: &str) -> &str {
    s.trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '<' || c == '>')
}

/// Try one candidate string.
fn attempt(raw: &str) -> Result<ParsedLinkInfo, String> {
    let mut text = strip_wrapping(raw).to_string();
    for _ in 0..MAX_UNWRAP {
        match unwrap_safelinks(&text) {
            Some(inner) => text = strip_wrapping(&inner).to_string(),
            None => break,
        }
    }
    let parsed = link::parse(&text).map_err(|e| match e {
        ampsync_core::Error::InvalidLink(m) => friendly(&m),
        other => other.to_string(),
    })?;
    Ok(ParsedLinkInfo {
        host: parsed.host,
        kind: match parsed.kind {
            LinkKind::Sharing { .. } => "sharing",
            LinkKind::Folder { .. } => "folder",
        },
        link: text,
    })
}

/// Core's wording is aimed at a field the user typed into; here the text came from a clipboard
/// or a drag, so technical parse failures become one plain sentence.
fn friendly(core: &str) -> String {
    if core.starts_with("not a valid URL")
        || core.starts_with("unsupported scheme")
        || core.starts_with("link has no host")
    {
        return "That is not a SharePoint link.".into();
    }
    let mut chars = core.chars();
    let first = chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>())
        .unwrap_or_default();
    let mut out = format!("{first}{}", chars.as_str());
    if !out.ends_with('.') {
        out.push('.');
    }
    out
}

/// A link inside running text ("Here is the folder: https://...").
fn url_token(line: &str) -> Option<&str> {
    line.split_whitespace()
        .find(|t| {
            let t = t.trim_start_matches(['<', '(', '"', '\'']);
            t.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("http://"))
                || t.get(..8).is_some_and(|p| p.eq_ignore_ascii_case("https://"))
        })
        .map(|t| t.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '>', '"', '\'']))
}

/// Find a SharePoint link in pasted or dropped text. `text/uri-list` drops come as lines with
/// `#` comments, so every non-comment line is tried, the first that works wins, and if none does
/// the first line's reason is returned.
pub fn parse_text(text: &str) -> Result<ParsedLinkInfo, String> {
    let mut first_err = None;
    for line in text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let mut result = attempt(line);
        if result.is_err()
            && line.contains(char::is_whitespace)
            && let Some(tok) = url_token(line)
        {
            result = attempt(tok);
        }
        match result {
            Ok(info) => return Ok(info),
            Err(e) => {
                first_err.get_or_insert(e);
            }
        }
    }
    Err(first_err.unwrap_or_else(|| "There is no link to open. Copy a SharePoint folder link first.".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(text: &str) -> ParsedLinkInfo {
        parse_text(text).unwrap_or_else(|e| panic!("{text:?} should parse: {e}"))
    }

    const SHARING: &str = "https://contoso.sharepoint.com/:f:/s/Team/EabcDEF123?e=XyZ";
    const ADDRESS_BAR: &str = "https://contoso.sharepoint.com/sites/Team/Shared%20Documents/Forms/AllItems.aspx?id=%2Fsites%2FTeam%2FShared%20Documents%2FProjects&viewid=abc";

    #[test]
    fn sharing_link() {
        let i = ok(SHARING);
        assert_eq!(i.host, "contoso.sharepoint.com");
        assert_eq!(i.kind, "sharing");
        assert_eq!(i.link, SHARING);
    }

    #[test]
    fn address_bar_url_and_direct_path() {
        let i = ok(&format!("  {ADDRESS_BAR}\r\n"));
        assert_eq!((i.host.as_str(), i.kind), ("contoso.sharepoint.com", "folder"));
        assert_eq!(i.link, ADDRESS_BAR);
        let i = ok("contoso-my.sharepoint.com/personal/jo_contoso_com/Documents/Photos");
        assert_eq!((i.host.as_str(), i.kind), ("contoso-my.sharepoint.com", "folder"));
    }

    fn wrap(inner: &str) -> String {
        let enc = url::form_urlencoded::byte_serialize(inner.as_bytes()).collect::<String>();
        format!(
            "https://nam06.safelinks.protection.outlook.com/?url={enc}&data=05%7C02%7Cjo%40contoso.com&sdata=abc%3D&reserved=0"
        )
    }

    #[test]
    fn safelinks_wrapped_links_are_unwrapped() {
        let i = ok(&wrap(SHARING));
        assert_eq!((i.host.as_str(), i.kind), ("contoso.sharepoint.com", "sharing"));
        assert_eq!(i.link, SHARING);
        // The id= parameter stays encoded once the wrapper is gone.
        let i = ok(&wrap(ADDRESS_BAR));
        assert_eq!(i.kind, "folder");
        assert_eq!(i.link, ADDRESS_BAR);
        // Wrapped twice (a forwarded message).
        assert_eq!(ok(&wrap(&wrap(SHARING))).link, SHARING);
        // Government cloud host.
        let gov = wrap(SHARING).replace(
            "nam06.safelinks.protection.outlook.com",
            "usg01.safelinks.protection.office365.us",
        );
        assert_eq!(ok(&gov).link, SHARING);
    }

    #[test]
    fn safelinks_around_something_that_is_not_sharepoint_is_rejected() {
        let e = parse_text(&wrap("https://example.com/a")).unwrap_err();
        assert!(e.contains("example.com"), "{e}");
        assert!(parse_text("https://nam06.safelinks.protection.outlook.com/?data=x").is_err());
    }

    #[test]
    fn link_inside_running_text_or_a_uri_list() {
        assert_eq!(ok(&format!("Here is the folder: {SHARING}.")).link, SHARING);
        assert_eq!(ok(&format!("see <{SHARING}> thanks")).link, SHARING);
        let list = format!("# dragged from a browser\r\n{SHARING}\r\n");
        assert_eq!(ok(&list).link, SHARING);
        // The first line that works wins.
        assert_eq!(
            ok(&format!("Budget folder\nhttps://example.com/x\n{SHARING}")).link,
            SHARING
        );
    }

    #[test]
    fn junk_gets_a_short_plain_message() {
        assert_eq!(
            parse_text("").unwrap_err(),
            "There is no link to open. Copy a SharePoint folder link first."
        );
        assert_eq!(
            parse_text("# only a comment\n").unwrap_err(),
            "There is no link to open. Copy a SharePoint folder link first."
        );
        assert_eq!(
            parse_text("hello world").unwrap_err(),
            "That is not a SharePoint link."
        );
        assert_eq!(
            parse_text("ftp://contoso.sharepoint.com/a").unwrap_err(),
            "That is not a SharePoint link."
        );
        let e = parse_text("https://example.com/sites/Team/Documents/A").unwrap_err();
        assert!(
            e.contains("not a SharePoint Online address") && e.ends_with('.'),
            "{e}"
        );
        // A site with no library is not a folder.
        let e = parse_text("https://contoso.sharepoint.com/sites/Team").unwrap_err();
        assert!(e.starts_with("The link points to a site"), "{e}");
        assert!(parse_text("https://contoso.sharepoint.com/").is_err());
    }
}
