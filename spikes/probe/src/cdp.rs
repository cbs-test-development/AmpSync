//! `--auth edge|chrome`: launch the installed browser with a fresh profile + remote debugging,
//! let the user sign in, then read cookies over the Chrome DevTools Protocol.

use crate::auth::{domain_matches, AuthResult, Captured};
use anyhow::{anyhow, bail, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Browser {
    Edge,
    Chrome,
}

impl Browser {
    pub fn label(self) -> &'static str {
        match self {
            Browser::Edge => "edge",
            Browser::Chrome => "chrome",
        }
    }
}

pub fn find_browser(b: Browser) -> Option<PathBuf> {
    #[allow(unused_mut)]
    let mut cands: Vec<PathBuf> = Vec::new();
    #[cfg(windows)]
    {
        let envs = ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"];
        for e in envs {
            if let Ok(base) = std::env::var(e) {
                let rel = match b {
                    Browser::Edge => r"Microsoft\Edge\Application\msedge.exe",
                    Browser::Chrome => r"Google\Chrome\Application\chrome.exe",
                };
                cands.push(PathBuf::from(&base).join(rel));
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        cands.push(match b {
            Browser::Edge => "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge".into(),
            Browser::Chrome => "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into(),
        });
    }
    for c in &cands {
        if c.is_file() {
            return Some(c.clone());
        }
    }
    let names: &[&str] = match (b, cfg!(windows)) {
        (Browser::Edge, true) => &["msedge.exe"],
        (Browser::Chrome, true) => &["chrome.exe"],
        (Browser::Edge, false) => &["microsoft-edge", "microsoft-edge-stable"],
        (Browser::Chrome, false) => &["google-chrome", "google-chrome-stable", "chromium", "chromium-browser"],
    };
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for n in names {
            let p = dir.join(n);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

fn free_port() -> Result<u16> {
    let l = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(l.local_addr()?.port())
}

struct Cleanup {
    child: Option<Child>,
    dir: PathBuf,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(c) = self.child.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
        for _ in 0..10 {
            if std::fs::remove_dir_all(&self.dir).is_ok() || !self.dir.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
    }
}

async fn cdp_call(ws_url: &str, method: &str, params: Value) -> Result<Value> {
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url).await.map_err(|e| anyhow!("websocket connect failed: {e}"))?;
    let msg = json!({"id": 1, "method": method, "params": params}).to_string();
    ws.send(Message::Text(msg.into())).await?;
    let fut = async {
        while let Some(m) = ws.next().await {
            let m = m?;
            if let Message::Text(t) = m {
                let v: Value = serde_json::from_str(t.as_str())?;
                if v.get("id").and_then(|i| i.as_i64()) == Some(1) {
                    return Ok::<Value, anyhow::Error>(v);
                }
            }
        }
        bail!("websocket closed before response")
    };
    let v = tokio::time::timeout(Duration::from_secs(30), fut).await.map_err(|_| anyhow!("CDP call timed out"))??;
    if let Some(e) = v.get("error") {
        bail!("CDP error: {}", e.get("message").and_then(|m| m.as_str()).unwrap_or("unknown"));
    }
    Ok(v["result"].clone())
}

fn parse_cookies(result: &Value, host: &str) -> Vec<Captured> {
    result["cookies"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or(&[])
        .iter()
        .filter_map(|c| {
            let domain = c["domain"].as_str()?.to_string();
            if !domain_matches(&domain, host) {
                return None;
            }
            let session = c["session"].as_bool().unwrap_or(false);
            let exp = c["expires"].as_f64().unwrap_or(-1.0);
            Some(Captured {
                name: c["name"].as_str()?.to_string(),
                value: c["value"].as_str().unwrap_or("").to_string(),
                domain: Some(domain),
                path: c["path"].as_str().map(|s| s.to_string()),
                secure: c["secure"].as_bool(),
                http_only: c["httpOnly"].as_bool(),
                expires_unix: if session || exp < 0.0 { None } else { Some(exp as i64) },
                session: Some(session || exp < 0.0),
            })
        })
        .collect()
}

pub async fn capture(b: Browser, url: &str, host: &str) -> Result<AuthResult> {
    let exe = find_browser(b).ok_or_else(|| anyhow!("{} executable not found", b.label()))?;
    let dir = tempfile::Builder::new().prefix("ampsync-probe-br-").tempdir()?.keep();
    let port = free_port()?;
    let mut cleanup = Cleanup { child: None, dir: dir.clone() };
    let child = Command::new(&exe)
        .arg(format!("--user-data-dir={}", dir.display()))
        .arg(format!("--remote-debugging-port={port}"))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        // Never sign the throwaway profile in to the browser or sync it with the user's profiles.
        // Edge would otherwise implicitly sign new profiles in with the Windows account.
        .arg("--disable-sync")
        .arg("--disable-features=msImplicitSignin,msEdgeSyncConsent,msSignInWithWindowsAccount")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow!("failed to launch browser: {e}"))?;
    cleanup.child = Some(child);

    println!("A fresh {} window (temporary profile) has opened. Sign in until the SharePoint page is visible,", b.label());
    println!("then come back here and press Enter.");
    tokio::task::spawn_blocking(crate::input::line).await?;

    let http = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(10)).build()?;
    let ver: Value = http.get(format!("http://127.0.0.1:{port}/json/version")).send().await.map_err(|e| anyhow!("cannot reach debugging port: {}", e.without_url()))?.json().await?;
    let browser_ws = ver["webSocketDebuggerUrl"].as_str().ok_or_else(|| anyhow!("no webSocketDebuggerUrl"))?.to_string();
    let browser_version = ver["Browser"].as_str().unwrap_or("?").to_string();

    let mut notes = vec![format!("browser: {browser_version}")];
    let mut cookies = Vec::new();
    match cdp_call(&browser_ws, "Storage.getCookies", json!({})).await {
        Ok(r) => cookies = parse_cookies(&r, host),
        Err(e) => notes.push(format!("Storage.getCookies failed: {e}")),
    }
    if cookies.is_empty() {
        // Fallback: page-level Network.getAllCookies
        let targets: Value = http.get(format!("http://127.0.0.1:{port}/json")).send().await?.json().await?;
        if let Some(ws) = targets.as_array().and_then(|a| a.iter().find(|t| t["type"] == "page")).and_then(|t| t["webSocketDebuggerUrl"].as_str()) {
            match cdp_call(ws, "Network.getAllCookies", json!({})).await {
                Ok(r) => {
                    cookies = parse_cookies(&r, host);
                    notes.push("used Network.getAllCookies fallback".into());
                }
                Err(e) => notes.push(format!("Network.getAllCookies failed: {e}")),
            }
        }
    }
    // Politely close, then Cleanup kills + deletes the profile.
    let _ = cdp_call(&browser_ws, "Browser.close", json!({})).await;
    drop(cleanup);

    if cookies.is_empty() {
        bail!("no cookies for the tenant host were returned by the browser");
    }
    Ok(AuthResult { cookies, notes, sso: "n/a".into() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cdp_cookie_list_and_filters_host() {
        let v = json!({"cookies": [
            {"name":"FedAuth","value":"SECRET","domain":"x.sharepoint.com","path":"/","expires":1900000000.5,"httpOnly":true,"secure":true,"session":false},
            {"name":"rtFa","value":"S2","domain":".sharepoint.com","path":"/","expires":-1,"httpOnly":true,"secure":true,"session":true},
            {"name":"other","value":"S3","domain":"google.com","path":"/","expires":-1,"session":true}
        ]});
        let c = parse_cookies(&v, "x.sharepoint.com");
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].expires_unix, Some(1900000000));
        assert_eq!(c[1].session, Some(true));
        assert_eq!(c[1].expires_unix, None);
    }
}
