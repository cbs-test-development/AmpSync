//! Cookie capture: shared types + `paste` mode + host matching helpers.
//! Cookie VALUES live only in memory; they are never serialised or printed.

use anyhow::{anyhow, Result};

#[derive(Clone)]
pub struct Captured {
    pub name: String,
    pub value: String,
    pub domain: Option<String>,
    pub path: Option<String>,
    pub secure: Option<bool>,
    pub http_only: Option<bool>,
    /// Unix seconds; None for session cookies or unknown.
    pub expires_unix: Option<i64>,
    pub session: Option<bool>,
}

// Deliberately no Debug/Display that would reveal `value`.
impl std::fmt::Debug for Captured {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Captured({})", self.name)
    }
}

pub struct AuthResult {
    pub cookies: Vec<Captured>,
    /// Free-form notes that go into the report (e.g. SSO option status).
    pub notes: Vec<String>,
    pub sso: String,
}

pub fn has_auth_cookie(c: &[Captured]) -> bool {
    c.iter().any(|c| is_auth_name(&c.name))
}

pub fn is_auth_name(n: &str) -> bool {
    n.eq_ignore_ascii_case("FedAuth") || n.eq_ignore_ascii_case("rtFa")
}

/// Does a cookie with `domain` apply to `host`? (`.sharepoint.com` / `sharepoint.com` / exact host)
pub fn domain_matches(domain: &str, host: &str) -> bool {
    let d = domain.trim_start_matches('.').to_ascii_lowercase();
    let h = host.to_ascii_lowercase();
    h == d || h.ends_with(&format!(".{d}"))
}

/// Parse a `Cookie:` header value (`a=b; c=d`), tolerating a leading `Cookie:` and surrounding quotes.
pub fn parse_cookie_header(s: &str) -> Vec<Captured> {
    let mut s = s.trim().trim_matches('"').trim_matches('\'').trim();
    if s.len() >= 7 && s[..7].eq_ignore_ascii_case("cookie:") {
        s = s[7..].trim();
    }
    s.split(';')
        .filter_map(|p| {
            let p = p.trim();
            let (n, v) = p.split_once('=')?;
            if n.is_empty() {
                return None;
            }
            Some(Captured {
                name: n.trim().to_string(),
                value: v.trim().to_string(),
                domain: None,
                path: None,
                secure: None,
                http_only: None,
                expires_unix: None,
                session: None,
            })
        })
        .collect()
}

pub fn paste(cookie_file: Option<&str>) -> Result<AuthResult> {
    let raw = if let Some(p) = cookie_file {
        std::fs::read_to_string(p).map_err(|e| anyhow!("cannot read cookie file: {}", e.kind()))?
    } else if let Ok(v) = std::env::var("AMPSYNC_PROBE_COOKIE") {
        v
    } else {
        println!("Paste the value of the request `Cookie:` header for the SharePoint host (from browser DevTools > Network).");
        println!("Input is hidden. If your terminal truncates long lines, save it to a file and use --cookie-file <path> instead.");
        match rpassword::prompt_password("Cookie header: ") {
            Ok(s) => s,
            Err(_) => {
                let mut l = String::new();
                std::io::stdin().read_line(&mut l)?;
                l
            }
        }
    };
    let cookies = parse_cookie_header(&raw);
    if cookies.is_empty() {
        return Err(anyhow!("no cookies parsed from the pasted value"));
    }
    Ok(AuthResult { cookies, notes: vec!["cookies pasted by user; expiry/flags unknown".into()], sso: "n/a".into() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cookie_header() {
        let c = parse_cookie_header("Cookie: FedAuth=abc==; rtFa=def; x=1");
        assert_eq!(c.len(), 3);
        assert_eq!(c[0].name, "FedAuth");
        assert_eq!(c[0].value, "abc==");
        assert!(has_auth_cookie(&c));
        let c = parse_cookie_header("  \"a=b\"  ");
        assert_eq!(c.len(), 1);
        assert!(!has_auth_cookie(&c));
        assert!(parse_cookie_header("garbage").is_empty());
    }

    #[test]
    fn domain_match() {
        assert!(domain_matches(".sharepoint.com", "x.sharepoint.com"));
        assert!(domain_matches("x.sharepoint.com", "x.sharepoint.com"));
        assert!(!domain_matches("y.sharepoint.com", "x.sharepoint.com"));
        assert!(!domain_matches("evilsharepoint.com", "x.sharepoint.com"));
    }

    #[test]
    fn debug_never_shows_value() {
        let c = parse_cookie_header("FedAuth=SECRETVALUE");
        assert!(!format!("{:?}", c[0]).contains("SECRETVALUE"));
    }
}
