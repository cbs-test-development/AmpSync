//! Is a stored session still accepted by SharePoint? The probe itself lives in core
//! (`check_session`); this module decides *where* to probe and how to word the answer.
//!
//! The probe goes to the site that holds the pasted link, never the tenant root: guests and
//! users without access to the root site get a 403 there even though their session is fine, and
//! core only reports `Expired` for a definite 401 or a redirect to the login page.

use ampsync_core::auth::Session;
use ampsync_core::link::{LinkKind, ParsedLink};
use url::Url;

pub use ampsync_core::SessionStatus;

pub fn label(status: &SessionStatus) -> &'static str {
    match status {
        SessionStatus::Valid => "valid",
        SessionStatus::Expired => "expired",
        SessionStatus::Unknown => "unknown",
    }
}

/// `https://host/`: the last resort when no link is known.
pub fn host_root(host: &str) -> Url {
    Url::parse(&format!("https://{host}/")).expect("host came from a parsed link or the keychain")
}

/// The web (site) that a pasted link belongs to. Folder links carry it; sharing links name it in
/// their path (`/:f:/s/Team/...`, `/:f:/t/...`, `/:f:/g/personal/...`, `/:f:/r/sites/Team/...`).
/// Anything unrecognised falls back to the tenant root.
pub fn site_url_for(link: &ParsedLink) -> Url {
    match &link.kind {
        LinkKind::Folder { site_url, .. } => site_url.clone(),
        LinkKind::Sharing { url } => {
            let segs: Vec<&str> = url
                .path_segments()
                .map(|s| s.filter(|x| !x.is_empty()).collect())
                .unwrap_or_default();
            // segs[0] is the `:f:` token.
            let path = match segs.as_slice() {
                [_, "s", name, ..] => Some(format!("/sites/{name}")),
                [_, "t", name, ..] => Some(format!("/teams/{name}")),
                [_, "g" | "r", kind, name, ..]
                    if ["sites", "teams", "personal"]
                        .iter()
                        .any(|k| k.eq_ignore_ascii_case(kind)) =>
                {
                    Some(format!("/{kind}/{name}"))
                }
                _ => None,
            };
            path.and_then(|p| Url::parse(&format!("https://{}{p}", link.host)).ok())
                .unwrap_or_else(|| host_root(&link.host))
        }
    }
}

/// Probe `session` against `site`.
pub async fn check(session: &Session, site: &Url) -> SessionStatus {
    ampsync_core::check_session(session, site).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ampsync_core::link;

    fn site(input: &str) -> String {
        site_url_for(&link::parse(input).unwrap()).to_string()
    }

    #[test]
    fn folder_links_use_their_site() {
        assert_eq!(
            site("https://contoso.sharepoint.com/sites/Team/Shared%20Documents/Projects"),
            "https://contoso.sharepoint.com/sites/Team"
        );
    }

    #[test]
    fn sharing_links_name_their_site() {
        assert_eq!(
            site("https://contoso.sharepoint.com/:f:/s/Team/EabcDEF123?e=x"),
            "https://contoso.sharepoint.com/sites/Team"
        );
        assert_eq!(
            site("https://contoso.sharepoint.com/:f:/t/Crew/Eabc"),
            "https://contoso.sharepoint.com/teams/Crew"
        );
        assert_eq!(
            site("https://contoso-my.sharepoint.com/:f:/g/personal/jo_contoso_com/Eabc"),
            "https://contoso-my.sharepoint.com/personal/jo_contoso_com"
        );
        assert_eq!(
            site("https://contoso.sharepoint.com/:f:/r/sites/Team/Shared%20Documents/x?csf=1"),
            "https://contoso.sharepoint.com/sites/Team"
        );
    }

    #[test]
    fn unrecognised_sharing_links_fall_back_to_the_root() {
        assert_eq!(
            site("https://contoso.sharepoint.com/:f:/x"),
            "https://contoso.sharepoint.com/"
        );
    }

    #[test]
    fn labels() {
        assert_eq!(label(&SessionStatus::Valid), "valid");
        assert_eq!(label(&SessionStatus::Expired), "expired");
        assert_eq!(label(&SessionStatus::Unknown), "unknown");
    }
}
