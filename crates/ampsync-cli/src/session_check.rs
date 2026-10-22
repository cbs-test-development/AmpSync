//! Is a stored session still accepted by SharePoint? One cheap authenticated GET.

use std::time::Duration;

use ampsync_core::auth::Session;
use ampsync_core::remote::http_client;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStatus {
    Valid,
    Expired,
    /// Could not tell (offline, throttled, unexpected answer).
    Unknown(String),
}

impl SessionStatus {
    pub fn label(&self) -> &'static str {
        match self {
            SessionStatus::Valid => "valid",
            SessionStatus::Expired => "expired",
            SessionStatus::Unknown(_) => "unknown",
        }
    }
}

/// Interpret the answer to the probe request. `location` is the `Location` header, if any.
pub fn classify(status: u16, location: Option<&str>, tenant: &str) -> SessionStatus {
    match status {
        200..=299 => SessionStatus::Valid,
        401 | 403 => SessionStatus::Expired,
        300..=399 => {
            let host = location
                .and_then(|l| url::Url::parse(l).ok())
                .and_then(|u| u.host_str().map(str::to_ascii_lowercase));
            match host {
                // A relative or same-tenant redirect is SharePoint routing us around, not a login.
                None => SessionStatus::Valid,
                Some(h) if h == tenant.to_ascii_lowercase() => SessionStatus::Valid,
                Some(_) => SessionStatus::Expired,
            }
        }
        other => SessionStatus::Unknown(format!("HTTP {other}")),
    }
}

/// Ask the tenant for the root web's title with the session's cookies.
pub async fn check(session: &Session) -> SessionStatus {
    let client = http_client(session);
    let url = format!("https://{}/_api/web/title", session.host);
    let request = client
        .get(url)
        .header("Accept", "application/json;odata=nometadata")
        .send();
    match tokio::time::timeout(Duration::from_secs(15), request).await {
        Err(_) => SessionStatus::Unknown("timed out".into()),
        Ok(Err(e)) => SessionStatus::Unknown(e.to_string()),
        Ok(Ok(resp)) => {
            let location = resp
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            classify(resp.status().as_u16(), location.as_deref(), &session.host)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = "contoso.sharepoint.com";

    #[test]
    fn statuses() {
        assert_eq!(classify(200, None, T), SessionStatus::Valid);
        assert_eq!(classify(401, None, T), SessionStatus::Expired);
        assert_eq!(classify(403, None, T), SessionStatus::Expired);
        assert!(matches!(classify(503, None, T), SessionStatus::Unknown(_)));
        assert!(matches!(classify(404, None, T), SessionStatus::Unknown(_)));
    }

    #[test]
    fn redirects() {
        let login = "https://login.microsoftonline.com/common/oauth2/authorize?x=1";
        assert_eq!(classify(302, Some(login), T), SessionStatus::Expired);
        assert_eq!(
            classify(302, Some("https://contoso.sharepoint.com/_layouts/15/x"), T),
            SessionStatus::Valid
        );
        assert_eq!(classify(301, Some("/relative"), T), SessionStatus::Valid);
        assert_eq!(classify(302, None, T), SessionStatus::Valid);
    }

    #[test]
    fn labels() {
        assert_eq!(SessionStatus::Valid.label(), "valid");
        assert_eq!(SessionStatus::Expired.label(), "expired");
        assert_eq!(SessionStatus::Unknown("x".into()).label(), "unknown");
    }
}
