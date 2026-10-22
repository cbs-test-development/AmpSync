//! Browser sign-in (D3 method 2, D16): launch the installed Edge/Chrome with a brand-new
//! AmpSync-owned profile, wait for the user to sign in, read the SharePoint cookies over the
//! Chrome DevTools Protocol, then close the browser and delete the profile.
//!
//! The user's own browser profiles are never read, signed in or synced: the throwaway profile
//! lives under AmpSync's per-user data directory and the browser is started with sync and
//! implicit sign-in disabled.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use directories::ProjectDirs;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use time::OffsetDateTime;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

use super::{BrowserKind, Session, SessionCookie, SignInMethod};
use crate::Error;

/// How long the user has to finish signing in.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Cookie poll interval.
const POLL_INTERVAL: Duration = Duration::from_millis(750);
/// How long to wait for the browser's debugging endpoint to come up.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(45);

fn label(kind: BrowserKind) -> &'static str {
    match kind {
        BrowserKind::Edge => "Microsoft Edge",
        BrowserKind::Chrome => "Google Chrome",
    }
}

/// Locate the browser executable, or `None` when it is not installed.
pub(crate) fn find_browser(kind: BrowserKind) -> Option<PathBuf> {
    #[allow(unused_mut)]
    let mut candidates: Vec<PathBuf> = Vec::new();
    #[cfg(windows)]
    {
        let rel = match kind {
            BrowserKind::Edge => r"Microsoft\Edge\Application\msedge.exe",
            BrowserKind::Chrome => r"Google\Chrome\Application\chrome.exe",
        };
        for var in ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(var) {
                candidates.push(PathBuf::from(base).join(rel));
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        candidates.push(match kind {
            BrowserKind::Edge => "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge".into(),
            BrowserKind::Chrome => "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into(),
        });
    }
    if let Some(found) = candidates.into_iter().find(|c| c.is_file()) {
        return Some(found);
    }
    let names: &[&str] = match (kind, cfg!(windows)) {
        (BrowserKind::Edge, true) => &["msedge.exe"],
        (BrowserKind::Chrome, true) => &["chrome.exe"],
        (BrowserKind::Edge, false) => &["microsoft-edge", "microsoft-edge-stable"],
        (BrowserKind::Chrome, false) => &[
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
        ],
    };
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|p| p.is_file())
}

fn free_port() -> Result<u16, Error> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|e| Error::State(format!("cannot reserve a local port for the browser: {e}")))?;
    listener
        .local_addr()
        .map(|a| a.port())
        .map_err(|e| Error::State(format!("cannot reserve a local port for the browser: {e}")))
}

/// Parent directory of the throwaway profiles: `<per-user data dir>/browser-signin`.
fn profile_root() -> PathBuf {
    ProjectDirs::from("", "", "AmpSync")
        .map(|d| d.data_local_dir().join("browser-signin"))
        .unwrap_or_else(|| std::env::temp_dir().join("AmpSync-browser-signin"))
}

/// Remove profile directories that a crashed earlier run left behind (older than a day).
fn remove_stale_profiles(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > Duration::from_secs(24 * 3600));
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Owns the browser process and its profile directory. Dropping it kills the browser and removes
/// the profile (on a helper thread, since Windows needs a few retries while handles close).
struct BrowserProcess {
    child: Option<Child>,
    dir: PathBuf,
}

impl BrowserProcess {
    fn has_exited(&mut self) -> bool {
        match self.child.as_mut() {
            Some(c) => !matches!(c.try_wait(), Ok(None)),
            None => true,
        }
    }

    /// Ask the browser to quit, then make sure it is gone and the profile is deleted.
    async fn shutdown(mut self, cdp: Option<&mut Cdp>) {
        if let Some(cdp) = cdp {
            let _ = cdp.call("Browser.close", json!({})).await;
        }
        for _ in 0..50 {
            if self.has_exited() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        for _ in 0..20 {
            if std::fs::remove_dir_all(&self.dir).is_ok() || !self.dir.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        tracing::warn!("could not delete the temporary browser profile");
    }
}

impl Drop for BrowserProcess {
    fn drop(&mut self) {
        let child = self.child.take();
        let dir = self.dir.clone();
        if child.is_none() && !dir.exists() {
            return;
        }
        std::thread::spawn(move || {
            if let Some(mut c) = child {
                let _ = c.kill();
                let _ = c.wait();
            }
            for _ in 0..20 {
                if std::fs::remove_dir_all(&dir).is_ok() || !dir.exists() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(300));
            }
        });
    }
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Failure of one DevTools call.
#[derive(Debug)]
enum CdpError {
    /// The connection is gone: the user closed the browser.
    Closed,
    Failed(String),
}

/// A minimal DevTools protocol client over one websocket.
struct Cdp {
    ws: Ws,
    next_id: u64,
}

impl Cdp {
    async fn connect(url: &str) -> Result<Self, Error> {
        let (ws, _) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|e| Error::State(format!("cannot connect to the browser's debugging port: {e}")))?;
        Ok(Cdp { ws, next_id: 1 })
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value, CdpError> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({"id": id, "method": method, "params": params}).to_string();
        self.ws
            .send(Message::Text(msg.into()))
            .await
            .map_err(|_| CdpError::Closed)?;
        let wait = async {
            while let Some(frame) = self.ws.next().await {
                let frame = frame.map_err(|_| CdpError::Closed)?;
                if let Message::Text(t) = frame {
                    let v: Value = serde_json::from_str(t.as_str())
                        .map_err(|e| CdpError::Failed(format!("bad DevTools reply: {e}")))?;
                    if v.get("id").and_then(Value::as_u64) == Some(id) {
                        return Ok(v);
                    }
                }
            }
            Err(CdpError::Closed)
        };
        let reply = tokio::time::timeout(Duration::from_secs(30), wait)
            .await
            .map_err(|_| CdpError::Failed("the browser did not answer".into()))??;
        if let Some(err) = reply.get("error") {
            let text = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Err(CdpError::Failed(format!("DevTools error: {text}")));
        }
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    }
}

/// Does a cookie with `domain` apply to `host`? (`.sharepoint.com`, `sharepoint.com` or the host)
pub(crate) fn domain_matches(domain: &str, host: &str) -> bool {
    let d = domain.trim_start_matches('.').to_ascii_lowercase();
    let h = host.to_ascii_lowercase();
    !d.is_empty() && (h == d || h.ends_with(&format!(".{d}")))
}

/// Cookies in a `Storage.getCookies` result that apply to `host`.
pub(crate) fn parse_cookies(result: &Value, host: &str) -> Vec<SessionCookie> {
    result["cookies"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter_map(|c| {
            let domain = c["domain"].as_str()?.to_string();
            if !domain_matches(&domain, host) {
                return None;
            }
            let session = c["session"].as_bool().unwrap_or(false);
            let exp = c["expires"].as_f64().unwrap_or(-1.0);
            let expires = if session || exp <= 0.0 {
                None
            } else {
                OffsetDateTime::from_unix_timestamp(exp as i64).ok()
            };
            Some(SessionCookie {
                name: c["name"].as_str()?.to_string(),
                value: c["value"].as_str().unwrap_or("").to_string(),
                domain,
                path: c["path"].as_str().unwrap_or("/").to_string(),
                expires,
            })
        })
        .collect()
}

/// Sign-in is complete when `FedAuth` (tenant host) and `rtFa` are both present.
pub(crate) fn has_auth_cookies(cookies: &[SessionCookie]) -> bool {
    let has = |n: &str| {
        cookies
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case(n) && !c.value.is_empty())
    };
    has("FedAuth") && has("rtFa")
}

async fn wait_for_debugger(port: u16, proc: &mut BrowserProcess) -> Result<String, Error> {
    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| Error::Network(e.to_string()))?;
    let url = format!("http://127.0.0.1:{port}/json/version");
    let started = Instant::now();
    loop {
        if proc.has_exited() {
            return Err(Error::Cancelled);
        }
        if let Ok(resp) = http.get(&url).send().await
            && let Ok(body) = resp.bytes().await
            && let Ok(v) = serde_json::from_slice::<Value>(&body)
            && let Some(ws) = v["webSocketDebuggerUrl"].as_str()
        {
            return Ok(ws.to_string());
        }
        if started.elapsed() > STARTUP_TIMEOUT {
            return Err(Error::State(
                "the browser did not start in time (its debugging port never opened)".into(),
            ));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

pub(crate) async fn sign_in(kind: BrowserKind, host: &str, start_url: &Url) -> Result<Session, Error> {
    let host = host.to_ascii_lowercase();
    let exe = find_browser(kind).ok_or_else(|| {
        Error::State(format!(
            "{} was not found on this computer; choose another sign-in method",
            label(kind)
        ))
    })?;

    let root = profile_root();
    remove_stale_profiles(&root);
    let nanos = OffsetDateTime::now_utc().unix_timestamp_nanos();
    let dir = root.join(format!("{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    let mut proc = BrowserProcess {
        child: None,
        dir: dir.clone(),
    };
    let port = free_port()?;
    let child = Command::new(&exe)
        .arg(format!("--user-data-dir={}", dir.display()))
        .arg(format!("--remote-debugging-port={port}"))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        // Never sign the throwaway profile in to the browser or sync it with other profiles.
        // Edge would otherwise implicitly sign new profiles in with the Windows account (D16).
        .arg("--disable-sync")
        .arg("--disable-features=msImplicitSignin,msEdgeSyncConsent,msSignInWithWindowsAccount")
        .arg(start_url.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| Error::State(format!("could not start {}: {e}", label(kind))))?;
    proc.child = Some(child);

    let ws_url = wait_for_debugger(port, &mut proc).await?;
    let mut cdp = Cdp::connect(&ws_url).await?;

    let deadline = Instant::now() + SIGN_IN_TIMEOUT;
    let captured = loop {
        match cdp.call("Storage.getCookies", json!({})).await {
            Ok(result) => {
                let cookies = parse_cookies(&result, &host);
                if has_auth_cookies(&cookies) {
                    break cookies;
                }
            }
            Err(CdpError::Closed) => return Err(Error::Cancelled),
            Err(CdpError::Failed(e)) => tracing::debug!("cookie poll failed: {e}"),
        }
        if proc.has_exited() {
            return Err(Error::Cancelled);
        }
        if Instant::now() >= deadline {
            proc.shutdown(Some(&mut cdp)).await;
            return Err(Error::State(
                "sign-in timed out after 10 minutes; try again".into(),
            ));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    };

    proc.shutdown(Some(&mut cdp)).await;
    Ok(Session {
        host,
        cookies: captured,
        method: SignInMethod::Browser { kind },
        captured_at: OffsetDateTime::now_utc(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_match() {
        assert!(domain_matches(".sharepoint.com", "x.sharepoint.com"));
        assert!(domain_matches("x.sharepoint.com", "x.sharepoint.com"));
        assert!(!domain_matches("y.sharepoint.com", "x.sharepoint.com"));
        assert!(!domain_matches("evilsharepoint.com", "x.sharepoint.com"));
        assert!(!domain_matches("", "x.sharepoint.com"));
    }

    #[test]
    fn parses_cdp_cookie_list_and_filters_host() {
        let v = json!({"cookies": [
            {"name":"FedAuth","value":"S1","domain":"x.sharepoint.com","path":"/","expires":1900000000.5,"session":false},
            {"name":"rtFa","value":"S2","domain":".sharepoint.com","path":"/","expires":-1,"session":true},
            {"name":"other","value":"S3","domain":"google.com","path":"/","expires":-1,"session":true}
        ]});
        let c = parse_cookies(&v, "x.sharepoint.com");
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].expires.unwrap().unix_timestamp(), 1_900_000_000);
        assert_eq!(c[1].expires, None);
        assert!(has_auth_cookies(&c));
        assert!(!has_auth_cookies(&c[..1]));
        assert!(!has_auth_cookies(&[]));
    }

    #[test]
    fn chunk_free_edge_cases() {
        let v = json!({"cookies": []});
        assert!(parse_cookies(&v, "x.sharepoint.com").is_empty());
        assert!(parse_cookies(&Value::Null, "x.sharepoint.com").is_empty());
    }
}
