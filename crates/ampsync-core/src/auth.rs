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

mod browser;

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

/// Keychain service name for every AmpSync entry.
const KEYRING_SERVICE: &str = "AmpSync";
/// Account name of the host index entry. Hosts are DNS names, so this can never collide.
const INDEX_ACCOUNT: &str = "__hosts__";
/// Largest piece of a session stored in one keychain entry. Windows Credential Manager limits a
/// credential blob to 2560 bytes and the keyring crate stores text as UTF-16, so 1000 bytes of
/// UTF-8 (at most 1000 UTF-16 units = 2000 bytes) always fits. A SharePoint session (FedAuth +
/// rtFa) is routinely larger than one blob, so it is split over `host`, `host#1`, `host#2`, ...
const CHUNK_BYTES: usize = 1000;

/// A flat key/value secret store: the seam between the session logic and the OS keychain.
trait SecretBackend {
    fn get(&self, account: &str) -> Result<Option<String>, Error>;
    fn set(&self, account: &str, value: &str) -> Result<(), Error>;
    /// Not an error if the entry does not exist.
    fn remove(&self, account: &str) -> Result<(), Error>;
}

struct OsKeychain;

fn keychain_err(e: keyring::Error) -> Error {
    Error::State(format!("keychain: {e}"))
}

impl OsKeychain {
    fn entry(account: &str) -> Result<keyring::Entry, Error> {
        keyring::Entry::new(KEYRING_SERVICE, account).map_err(keychain_err)
    }
}

impl SecretBackend for OsKeychain {
    fn get(&self, account: &str) -> Result<Option<String>, Error> {
        match Self::entry(account)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(keychain_err(e)),
        }
    }
    fn set(&self, account: &str, value: &str) -> Result<(), Error> {
        Self::entry(account)?.set_password(value).map_err(keychain_err)
    }
    fn remove(&self, account: &str) -> Result<(), Error> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(keychain_err(e)),
        }
    }
}

/// Serialises read-modify-write cycles on the host index within this process.
static KEYCHAIN_LOCK: Mutex<()> = Mutex::new(());

fn chunk_account(host: &str, i: usize) -> String {
    if i == 0 {
        host.to_string()
    } else {
        format!("{host}#{i}")
    }
}

fn split_chunks(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = s;
    while !rest.is_empty() {
        let mut cut = CHUNK_BYTES.min(rest.len());
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        let (head, tail) = rest.split_at(cut);
        out.push(head);
        rest = tail;
    }
    if out.is_empty() {
        out.push("");
    }
    out
}

/// Number of chunks announced by the first entry, and its payload.
fn parse_first_chunk(raw: &str) -> Option<(usize, &str)> {
    if raw.starts_with('{') {
        return Some((1, raw)); // plain JSON, no chunk header
    }
    let (n, payload) = raw.split_once(':')?;
    Some((n.parse().ok().filter(|n| (1..=1024).contains(n))?, payload))
}

fn read_hosts(b: &dyn SecretBackend) -> Result<Vec<String>, Error> {
    match b.get(INDEX_ACCOUNT)? {
        None => Ok(Vec::new()),
        Some(text) => {
            let mut hosts: Vec<String> = serde_json::from_str(&text).unwrap_or_default();
            hosts.sort();
            hosts.dedup();
            Ok(hosts)
        }
    }
}

fn write_hosts(b: &dyn SecretBackend, hosts: &[String]) -> Result<(), Error> {
    if hosts.is_empty() {
        return b.remove(INDEX_ACCOUNT);
    }
    let text = serde_json::to_string(hosts).map_err(|e| Error::State(format!("keychain: {e}")))?;
    b.set(INDEX_ACCOUNT, &text)
}

fn store_load(b: &dyn SecretBackend, host: &str) -> Result<Option<Session>, Error> {
    let host = host.to_ascii_lowercase();
    let Some(first) = b.get(&host)? else {
        return Ok(None);
    };
    let Some((n, payload)) = parse_first_chunk(&first) else {
        return Err(Error::State(format!("keychain entry for {host} is damaged")));
    };
    let mut json = String::from(payload);
    for i in 1..n {
        match b.get(&chunk_account(&host, i))? {
            Some(part) => json.push_str(&part),
            None => return Err(Error::State(format!("keychain entry for {host} is incomplete"))),
        }
    }
    serde_json::from_str(&json)
        .map(Some)
        .map_err(|_| Error::State(format!("keychain entry for {host} is damaged")))
}

fn store_save(b: &dyn SecretBackend, session: &Session) -> Result<(), Error> {
    let host = session.host.to_ascii_lowercase();
    let json = serde_json::to_string(session).map_err(|e| Error::State(format!("keychain: {e}")))?;
    let chunks = split_chunks(&json);
    let previous = b
        .get(&host)?
        .and_then(|raw| parse_first_chunk(&raw).map(|(n, _)| n))
        .unwrap_or(0);
    // Write the tail first and the header entry last, so a reader never sees a header that
    // promises chunks which are not there yet.
    for (i, chunk) in chunks.iter().enumerate().skip(1) {
        b.set(&chunk_account(&host, i), chunk)?;
    }
    b.set(&host, &format!("{}:{}", chunks.len(), chunks[0]))?;
    for i in chunks.len()..previous {
        b.remove(&chunk_account(&host, i))?;
    }
    let mut hosts = read_hosts(b)?;
    if !hosts.contains(&host) {
        hosts.push(host);
        hosts.sort();
        write_hosts(b, &hosts)?;
    }
    Ok(())
}

fn store_delete(b: &dyn SecretBackend, host: &str) -> Result<(), Error> {
    let host = host.to_ascii_lowercase();
    let n = b
        .get(&host)?
        .and_then(|raw| parse_first_chunk(&raw).map(|(n, _)| n))
        .unwrap_or(1);
    for i in (1..n).rev() {
        b.remove(&chunk_account(&host, i))?;
    }
    b.remove(&host)?;
    let mut hosts = read_hosts(b)?;
    let before = hosts.len();
    hosts.retain(|h| *h != host);
    if hosts.len() != before {
        write_hosts(b, &hosts)?;
    }
    Ok(())
}

fn locked<T>(f: impl FnOnce() -> T) -> T {
    let _guard = KEYCHAIN_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    f()
}

impl SessionStore for KeyringStore {
    fn load(&self, host: &str) -> Result<Option<Session>, Error> {
        locked(|| store_load(&OsKeychain, host))
    }
    fn save(&self, session: &Session) -> Result<(), Error> {
        locked(|| store_save(&OsKeychain, session))
    }
    fn delete(&self, host: &str) -> Result<(), Error> {
        locked(|| store_delete(&OsKeychain, host))
    }
    fn hosts(&self) -> Result<Vec<String>, Error> {
        locked(|| read_hosts(&OsKeychain))
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

/// Whether the server still accepts a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    /// The tenant answered a cheap authenticated request normally.
    Valid,
    /// 401, or the request was sent to the Microsoft sign-in page: the user must sign in again.
    Expired,
    /// Could not tell (403, other statuses, network trouble).
    Unknown,
}

/// Ask `site_url` (the web that owns the content, e.g. `https://contoso.sharepoint.com/sites/Team`)
/// whether `session` is still accepted: `GET {site_url}/_api/web?$select=Title`.
/// 2xx is `Valid`; 401 or a redirect to a Microsoft sign-in host is `Expired`; 403, any other
/// status and network errors are `Unknown` (a 403 can also mean "this one file is off limits",
/// so it proves nothing about the session).
pub async fn check_session(session: &Session, site_url: &Url) -> SessionStatus {
    let Some(url) = probe_url(site_url) else {
        return SessionStatus::Unknown;
    };
    let req = crate::remote::http_client(session)
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json;odata=nometadata");
    match tokio::time::timeout(std::time::Duration::from_secs(20), req.send()).await {
        Ok(Ok(resp)) => classify_response(&resp),
        _ => SessionStatus::Unknown,
    }
}

/// `{site}/_api/web?$select=Title`
pub(crate) fn probe_url(site_url: &Url) -> Option<Url> {
    let base = site_url.as_str().trim_end_matches('/');
    Url::parse(&format!("{base}/_api/web?$select=Title")).ok()
}

pub(crate) fn classify_response(resp: &reqwest::Response) -> SessionStatus {
    let header = |name| resp.headers().get(name).and_then(|v| v.to_str().ok());
    let location = header(reqwest::header::LOCATION).and_then(|l| resp.url().join(l).ok());
    classify_probe(
        resp.status().as_u16(),
        header(reqwest::header::CONTENT_TYPE),
        location.as_ref(),
    )
}

/// Pure decision behind [`check_session`].
pub(crate) fn classify_probe(
    status: u16,
    content_type: Option<&str>,
    location: Option<&Url>,
) -> SessionStatus {
    match status {
        // A 200 sign-in page instead of JSON is how some tenants say "not signed in".
        200 if content_type.is_some_and(|c| c.to_ascii_lowercase().starts_with("text/html")) => {
            SessionStatus::Expired
        }
        200..=299 => SessionStatus::Valid,
        401 => SessionStatus::Expired,
        300..=399 if location.is_some_and(crate::remote::is_login_host) => SessionStatus::Expired,
        _ => SessionStatus::Unknown,
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
impl SignIn for BrowserSignIn {
    async fn sign_in(&self, host: &str, start_url: &Url) -> Result<Session, Error> {
        browser::sign_in(self.kind, host, start_url).await
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

/// Cookies from a `Cookie:` header value (`a=b; c=d`). Tolerates a leading `Cookie:`, surrounding
/// quotes, and the header being wrapped over several lines. Entries without `=` are ignored.
pub(crate) fn parse_cookie_header(header: &str, host: &str) -> Vec<SessionCookie> {
    let mut text = header.trim().trim_matches(['"', '\'']).trim();
    if text.len() >= 7 && text.is_char_boundary(7) && text[..7].eq_ignore_ascii_case("cookie:") {
        text = text[7..].trim();
    }
    text.split(';')
        .filter_map(|part| {
            let (name, value) = part.trim().split_once('=')?;
            let name = name.trim();
            if name.is_empty() {
                return None;
            }
            Some(SessionCookie {
                name: name.to_string(),
                value: value.trim().to_string(),
                domain: host.to_string(),
                path: "/".to_string(),
                expires: None,
            })
        })
        .collect()
}

#[async_trait]
impl SignIn for PastedSignIn {
    async fn sign_in(&self, host: &str, _start_url: &Url) -> Result<Session, Error> {
        let host = host.to_ascii_lowercase();
        let cookies = parse_cookie_header(&self.cookie_header, &host);
        if cookies.is_empty() {
            return Err(Error::State(
                "no cookies found in the pasted text; paste the value of the request `Cookie:` header \
                 (name=value pairs separated by `;`)"
                    .into(),
            ));
        }
        Ok(Session {
            host,
            cookies,
            method: SignInMethod::Pasted,
            captured_at: OffsetDateTime::now_utc(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_classification() {
        let login = Url::parse("https://login.microsoftonline.com/x").unwrap();
        let same = Url::parse("https://contoso.sharepoint.com/other").unwrap();
        let json = Some("application/json;odata=nometadata");
        assert_eq!(classify_probe(200, json, None), SessionStatus::Valid);
        assert_eq!(classify_probe(204, None, None), SessionStatus::Valid);
        assert_eq!(
            classify_probe(200, Some("text/html"), None),
            SessionStatus::Expired
        );
        assert_eq!(classify_probe(401, None, None), SessionStatus::Expired);
        assert_eq!(classify_probe(302, None, Some(&login)), SessionStatus::Expired);
        assert_eq!(classify_probe(302, None, Some(&same)), SessionStatus::Unknown);
        assert_eq!(classify_probe(403, None, None), SessionStatus::Unknown);
        assert_eq!(classify_probe(500, None, None), SessionStatus::Unknown);
        let site = Url::parse("https://contoso.sharepoint.com/sites/Team").unwrap();
        assert_eq!(
            probe_url(&site).unwrap().as_str(),
            "https://contoso.sharepoint.com/sites/Team/_api/web?$select=Title"
        );
    }
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

    /// In-memory stand-in for the OS keychain that enforces a per-entry size limit like Windows.
    #[derive(Default)]
    struct MapBackend {
        map: std::cell::RefCell<BTreeMap<String, String>>,
    }

    impl SecretBackend for MapBackend {
        fn get(&self, account: &str) -> Result<Option<String>, Error> {
            Ok(self.map.borrow().get(account).cloned())
        }
        fn set(&self, account: &str, value: &str) -> Result<(), Error> {
            assert!(
                value.len() <= 1100,
                "entry of {} bytes would not fit a credential",
                value.len()
            );
            self.map.borrow_mut().insert(account.into(), value.into());
            Ok(())
        }
        fn remove(&self, account: &str) -> Result<(), Error> {
            self.map.borrow_mut().remove(account);
            Ok(())
        }
    }

    fn big_session(host: &str, cookie_len: usize) -> Session {
        let mut s = session(host);
        s.cookies[0].value = "x".repeat(cookie_len);
        s.cookies.push(SessionCookie {
            name: "rtFa".into(),
            value: "\u{e9}".repeat(cookie_len / 2),
            domain: host.into(),
            path: "/".into(),
            expires: None,
        });
        s
    }

    #[test]
    fn keychain_logic_round_trips_small_and_chunked_sessions() {
        let b = MapBackend::default();
        assert!(store_load(&b, "a.sharepoint.com").unwrap().is_none());
        let small = session("a.sharepoint.com");
        store_save(&b, &small).unwrap();
        assert_eq!(store_load(&b, "A.SharePoint.com").unwrap().unwrap(), small);
        assert_eq!(b.map.borrow().len(), 2, "one entry plus the index");

        let big = big_session("b.sharepoint.com", 3500);
        store_save(&b, &big).unwrap();
        assert_eq!(store_load(&b, "b.sharepoint.com").unwrap().unwrap(), big);
        assert!(b.map.borrow().contains_key("b.sharepoint.com#1"));
        assert_eq!(read_hosts(&b).unwrap(), ["a.sharepoint.com", "b.sharepoint.com"]);

        // Shrinking a session removes the stale chunks.
        store_save(&b, &session("b.sharepoint.com")).unwrap();
        assert!(!b.map.borrow().contains_key("b.sharepoint.com#1"));
        assert_eq!(read_hosts(&b).unwrap().len(), 2);

        store_delete(&b, "b.sharepoint.com").unwrap();
        store_delete(&b, "b.sharepoint.com").unwrap();
        assert!(store_load(&b, "b.sharepoint.com").unwrap().is_none());
        assert_eq!(read_hosts(&b).unwrap(), ["a.sharepoint.com"]);
        store_delete(&b, "a.sharepoint.com").unwrap();
        assert!(
            b.map.borrow().is_empty(),
            "index entry removed with the last host"
        );
    }

    #[test]
    fn damaged_entries_are_reported_not_panicked() {
        let b = MapBackend::default();
        b.set("a.sharepoint.com", "garbage").unwrap();
        assert!(matches!(store_load(&b, "a.sharepoint.com"), Err(Error::State(_))));
        b.set("a.sharepoint.com", "3:{\"host\"").unwrap();
        assert!(matches!(store_load(&b, "a.sharepoint.com"), Err(Error::State(_))));
    }

    #[test]
    fn chunking_respects_char_boundaries() {
        let text = "\u{20ac}".repeat(700); // 3 bytes each
        let parts = split_chunks(&text);
        assert!(parts.iter().all(|p| p.len() <= CHUNK_BYTES));
        assert_eq!(parts.concat(), text);
        assert_eq!(split_chunks(""), [""]);
    }

    /// Talks to the real OS keychain; run with `cargo test -- --ignored` on a desktop session.
    #[test]
    #[ignore = "uses the real OS keychain"]
    fn os_keychain_round_trip() {
        let host = format!("ampsync-test-{}.sharepoint.com", std::process::id());
        let store = KeyringStore::new();
        let s = big_session(&host, 3000);
        store.save(&s).unwrap();
        assert_eq!(store.load(&host).unwrap().unwrap(), s);
        assert!(store.hosts().unwrap().contains(&host));
        store.delete(&host).unwrap();
        assert!(store.load(&host).unwrap().is_none());
        assert!(!store.hosts().unwrap().contains(&host));
    }

    #[tokio::test]
    async fn pasted_header_becomes_a_session() {
        let url = Url::parse("https://contoso.sharepoint.com/").unwrap();
        let p = PastedSignIn {
            cookie_header: "Cookie: FedAuth=abc==; rtFa=def; \n x=1 ;novalue; =bad".into(),
        };
        let s = p.sign_in("Contoso.SharePoint.com", &url).await.unwrap();
        assert_eq!(s.host, "contoso.sharepoint.com");
        assert_eq!(s.method, SignInMethod::Pasted);
        let names: Vec<_> = s
            .cookies
            .iter()
            .map(|c| (c.name.as_str(), c.value.as_str()))
            .collect();
        assert_eq!(names, [("FedAuth", "abc=="), ("rtFa", "def"), ("x", "1")]);
        assert!(
            s.cookies
                .iter()
                .all(|c| c.domain == "contoso.sharepoint.com" && c.path == "/")
        );

        let quoted = PastedSignIn {
            cookie_header: "  \"FedAuth=1\"  ".into(),
        };
        assert_eq!(quoted.sign_in("h", &url).await.unwrap().cookies.len(), 1);
        for bad in ["", "   ", "garbage", "Cookie:"] {
            let p = PastedSignIn {
                cookie_header: bad.into(),
            };
            assert!(
                matches!(p.sign_in("h", &url).await, Err(Error::State(_))),
                "{bad:?}"
            );
        }
    }
}
