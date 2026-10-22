//! Sessions and where they're stored.
//!
//! AmpSync authenticates with the user's SharePoint web session cookies (D2). Sessions are kept
//! in the OS keychain keyed by tenant host (D4). AmpSync never reads cookies or passwords from
//! the user's own browser profiles (D16).

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use url::Url;

use crate::Error;

/// How a session was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SignInMethod {
    /// Embedded webview with AmpSync's own persistent profile (D15).
    Webview,
    /// Installed browser launched with a fresh AmpSync-owned profile (D16).
    Browser { kind: BrowserKind },
    /// The user pasted a `Cookie` header.
    Pasted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserKind {
    Edge,
    Chrome,
}

/// One cookie. `Debug` shows only the name and the value's length: values are credentials and
/// must never reach logs.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    /// `None` for session cookies (no expiry).
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub expires: Option<OffsetDateTime>,
}

impl fmt::Debug for SessionCookie {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionCookie")
            .field("name", &self.name)
            .field("value_len", &self.value.len())
            .field("domain", &self.domain)
            .field("path", &self.path)
            .field("expires", &self.expires)
            .finish()
    }
}

/// Everything needed to talk to one tenant. Serialised to JSON for keychain storage.
/// `Debug` never prints cookie values (see [`SessionCookie`]).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// Tenant host, e.g. `contoso.sharepoint.com`; the keychain key.
    pub host: String,
    pub cookies: Vec<SessionCookie>,
    pub method: SignInMethod,
    #[serde(with = "time::serde::rfc3339")]
    pub captured_at: OffsetDateTime,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("host", &self.host)
            .field("cookies", &self.cookies)
            .field("method", &self.method)
            .field("captured_at", &self.captured_at)
            .finish()
    }
}

impl Session {
    /// The `Cookie:` request header value (`a=1; b=2`) for all cookies that have not expired
    /// at `now`. Contains credentials: never log it.
    pub fn cookie_header(&self, now: OffsetDateTime) -> String {
        self.cookies
            .iter()
            .filter(|c| c.expires.is_none_or(|e| e > now))
            .map(|c| format!("{}={}", c.name, c.value))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Persistent storage for sessions, keyed by tenant host.
pub trait SessionStore: Send + Sync {
    /// `Ok(None)` when no session is stored for `host`.
    fn load(&self, host: &str) -> Result<Option<Session>, Error>;
    /// Insert or replace the session for `session.host`.
    fn save(&self, session: &Session) -> Result<(), Error>;
    /// Remove the session; not an error if there is none.
    fn delete(&self, host: &str) -> Result<(), Error>;
    /// Hosts that currently have a stored session, sorted.
    fn hosts(&self) -> Result<Vec<String>, Error>;
}

/// OS keychain (Windows Credential Manager, Secret Service on Linux) via the `keyring` crate.
///
/// Each session is one JSON entry (service `AmpSync`, account = host). The keychain cannot be
/// enumerated portably, so an index entry (account `__hosts__`, JSON array) tracks the hosts.
/// Keychain failures map to `Error::State`.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringStore;

impl KeyringStore {
    pub fn new() -> Self {
        KeyringStore
    }
}

#[allow(unused_variables)]
impl SessionStore for KeyringStore {
    fn load(&self, host: &str) -> Result<Option<Session>, Error> {
        todo!()
    }
    fn save(&self, session: &Session) -> Result<(), Error> {
        todo!()
    }
    fn delete(&self, host: &str) -> Result<(), Error> {
        todo!()
    }
    fn hosts(&self) -> Result<Vec<String>, Error> {
        todo!()
    }
}

/// In-memory store for tests and for the `--method paste` flow without a keychain.
#[derive(Debug, Default)]
pub struct MemoryStore {
    sessions: Mutex<BTreeMap<String, Session>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Session>> {
        self.sessions.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl SessionStore for MemoryStore {
    fn load(&self, host: &str) -> Result<Option<Session>, Error> {
        Ok(self.lock().get(host).cloned())
    }
    fn save(&self, session: &Session) -> Result<(), Error> {
        self.lock().insert(session.host.clone(), session.clone());
        Ok(())
    }
    fn delete(&self, host: &str) -> Result<(), Error> {
        self.lock().remove(host);
        Ok(())
    }
    fn hosts(&self) -> Result<Vec<String>, Error> {
        Ok(self.lock().keys().cloned().collect())
    }
}

/// Obtains a [`Session`] interactively or from user-supplied data.
///
/// Implemented in core for `Browser` and `Pasted`; the `Webview` method is implemented by the
/// GUI/CLI because it needs a window. `start_url` is the page to open for sign-in (the link the
/// user pasted).
#[async_trait]
pub trait SignIn: Send + Sync {
    async fn sign_in(&self, host: &str, start_url: &Url) -> Result<Session, Error>;
}

/// Sign in through an installed Edge/Chrome launched with a fresh AmpSync-owned
/// `--user-data-dir`, `--disable-sync` and browser implicit sign-in disabled; the SharePoint
/// cookies are read over the DevTools protocol (D3, D16). Never touches existing browser profiles.
#[derive(Debug, Clone, Copy)]
pub struct BrowserSignIn {
    pub kind: BrowserKind,
}

#[async_trait]
#[allow(unused_variables)]
impl SignIn for BrowserSignIn {
    async fn sign_in(&self, host: &str, start_url: &Url) -> Result<Session, Error> {
        todo!()
    }
}

/// Sign in from a pasted `Cookie` header (`FedAuth=...; rtFa=...`). `sign_in` does no network
/// access: it parses the header into cookies for `host`. Like `Session`, `Debug` must not leak
/// the header, so it is implemented manually.
#[derive(Clone)]
pub struct PastedSignIn {
    pub cookie_header: String,
}

impl fmt::Debug for PastedSignIn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PastedSignIn")
            .field("cookie_header_len", &self.cookie_header.len())
            .finish()
    }
}

#[async_trait]
#[allow(unused_variables)]
impl SignIn for PastedSignIn {
    async fn sign_in(&self, host: &str, start_url: &Url) -> Result<Session, Error> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Duration;

    fn session(host: &str) -> Session {
        Session {
            host: host.into(),
            cookies: vec![
                SessionCookie {
                    name: "FedAuth".into(),
                    value: "SUPERSECRETVALUE".into(),
                    domain: host.into(),
                    path: "/".into(),
                    expires: None,
                },
                SessionCookie {
                    name: "old".into(),
                    value: "x".into(),
                    domain: host.into(),
                    path: "/".into(),
                    expires: Some(OffsetDateTime::UNIX_EPOCH),
                },
            ],
            method: SignInMethod::Browser {
                kind: BrowserKind::Edge,
            },
            captured_at: OffsetDateTime::UNIX_EPOCH + Duration::days(1),
        }
    }

    #[test]
    fn debug_never_shows_cookie_values() {
        let s = session("contoso.sharepoint.com");
        let text = format!("{s:?} {:?}", s.cookies[0]);
        assert!(!text.contains("SUPERSECRETVALUE"), "{text}");
        assert!(text.contains("FedAuth"));
        assert!(text.contains("value_len: 16"));
        let p = PastedSignIn {
            cookie_header: "FedAuth=SUPERSECRETVALUE".into(),
        };
        assert!(!format!("{p:?}").contains("SUPERSECRETVALUE"));
    }

    #[test]
    fn serde_round_trip() {
        let s = session("contoso.sharepoint.com");
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Session>(&json).unwrap(), s);
        assert!(json.contains("\"type\":\"browser\""));
    }

    #[test]
    fn cookie_header_skips_expired() {
        let s = session("contoso.sharepoint.com");
        assert_eq!(
            s.cookie_header(OffsetDateTime::UNIX_EPOCH + Duration::days(2)),
            "FedAuth=SUPERSECRETVALUE"
        );
    }

    #[test]
    fn memory_store_round_trip() {
        let store = MemoryStore::new();
        assert!(store.load("a").unwrap().is_none());
        store.save(&session("b.sharepoint.com")).unwrap();
        store.save(&session("a.sharepoint.com")).unwrap();
        assert_eq!(store.hosts().unwrap(), ["a.sharepoint.com", "b.sharepoint.com"]);
        assert!(store.load("a.sharepoint.com").unwrap().is_some());
        store.delete("a.sharepoint.com").unwrap();
        store.delete("a.sharepoint.com").unwrap();
        assert_eq!(store.hosts().unwrap(), ["b.sharepoint.com"]);
    }
}
