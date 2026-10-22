//! Browser sign-in (D3 method 2, D16): launch the installed Edge/Chrome with a brand-new
//! AmpSync-owned profile, wait for the user to sign in, read the SharePoint cookies over the
//! Chrome DevTools Protocol, then close the browser and delete the profile.
//!
//! The protocol runs over `--remote-debugging-pipe`, not a TCP port, so no local socket is ever
//! opened and no other process on the machine can attach to the browser: messages are
//! NUL-terminated JSON over an anonymous pipe pair that only the child inherits.
//! - Unix: the browser reads fd 3 and writes fd 4 (set up with `dup2` between fork and exec).
//! - Windows: Chromium takes the two inherited handles from `--remote-debugging-io-pipes=<read
//!   handle>,<write handle>`; the child ends are marked inheritable (and the parent ends are not),
//!   and `std::process::Command` always creates the child with handle inheritance on.
//!
//! Closing our end makes the browser exit, so a crash of AmpSync never leaves it running with an
//! open debugging channel.
//!
//! The user's own browser profiles are never read, signed in or synced: the throwaway profile
//! lives under AmpSync's per-user data directory and the browser is started with sync and
//! implicit sign-in disabled.

use std::io::{PipeReader, PipeWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use directories::ProjectDirs;
use serde_json::{Value, json};
use time::OffsetDateTime;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use url::Url;

use super::{BrowserKind, Session, SessionCookie, SignInMethod};
use crate::Error;

/// How long the user has to finish signing in.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Cookie poll interval.
const POLL_INTERVAL: Duration = Duration::from_millis(750);
/// How long to wait for the browser to start answering on the debugging pipe.
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

/// Failure of one DevTools call.
#[derive(Debug)]
enum CdpError {
    /// The channel is gone: the user closed the browser.
    Closed,
    Failed(String),
}

/// Largest single DevTools message accepted from the browser.
const MAX_MESSAGE: usize = 64 * 1024 * 1024;

/// A minimal DevTools protocol client over the debugging pipe pair.
struct Cdp {
    writer: PipeWriter,
    replies: UnboundedReceiver<Value>,
    next_id: u64,
}

impl Cdp {
    /// `reader` receives the browser's messages, `writer` sends ours (NUL-terminated JSON).
    fn new(reader: PipeReader, writer: PipeWriter) -> Self {
        let (tx, replies) = unbounded_channel();
        std::thread::spawn(move || read_messages(reader, tx));
        Cdp {
            writer,
            replies,
            next_id: 1,
        }
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value, CdpError> {
        self.call_within(method, params, Duration::from_secs(30)).await
    }

    async fn call_within(
        &mut self,
        method: &str,
        params: Value,
        within: Duration,
    ) -> Result<Value, CdpError> {
        let id = self.next_id;
        self.next_id += 1;
        let mut msg = json!({"id": id, "method": method, "params": params})
            .to_string()
            .into_bytes();
        msg.push(0);
        // Requests are tiny, so this never blocks for long even though the write is synchronous.
        self.writer
            .write_all(&msg)
            .and_then(|()| self.writer.flush())
            .map_err(|_| CdpError::Closed)?;
        let replies = &mut self.replies;
        let wait = async {
            // Events (no `id`) and replies to earlier, timed-out calls are skipped.
            while let Some(v) = replies.recv().await {
                if v.get("id").and_then(Value::as_u64) == Some(id) {
                    return Ok(v);
                }
            }
            Err(CdpError::Closed)
        };
        let reply = tokio::time::timeout(within, wait)
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

/// Reader thread: split the pipe into NUL-terminated JSON messages until it closes.
fn read_messages(mut reader: PipeReader, tx: UnboundedSender<Value>) {
    let (mut buf, mut chunk) = (Vec::new(), [0u8; 16 * 1024]);
    loop {
        let n = match reader.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        for &b in &chunk[..n] {
            if b != 0 {
                if buf.len() < MAX_MESSAGE {
                    buf.push(b);
                }
                continue;
            }
            if let Ok(v) = serde_json::from_slice::<Value>(&buf)
                && tx.send(v).is_err()
            {
                return;
            }
            buf.clear();
        }
    }
}

/// Child-side pipe ends that must stay open until the child has been spawned, then be dropped so
/// that the pipes report end-of-file when the browser exits.
struct ChildEnds(#[allow(dead_code)] Box<dyn std::any::Any + Send>);

/// Hand the child ends of the two pipes to `cmd` (flag, fds or handles).
#[cfg(unix)]
fn attach_pipes(
    cmd: &mut Command,
    child_read: PipeReader,
    child_write: PipeWriter,
) -> std::io::Result<ChildEnds> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::process::CommandExt;

    /// A duplicate of `fd` numbered 10 or higher, so the `dup2` calls below can never overwrite
    /// one source with the other target.
    fn high(fd: RawFd) -> std::io::Result<OwnedFd> {
        // SAFETY: plain fcntl on a descriptor we own; the result is wrapped immediately.
        let n = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10) };
        if n < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            // SAFETY: `n` is a fresh descriptor that nothing else owns.
            Ok(unsafe { OwnedFd::from_raw_fd(n) })
        }
    }
    let read_fd = high(child_read.as_raw_fd())?;
    let write_fd = high(child_write.as_raw_fd())?;
    drop((child_read, child_write));
    let (r, w) = (read_fd.as_raw_fd(), write_fd.as_raw_fd());
    cmd.arg("--remote-debugging-pipe");
    // SAFETY: the closure only calls async-signal-safe `dup2` on descriptors that are open in
    // the child; `dup2` also clears close-on-exec on fds 3 and 4, which is what we want.
    unsafe {
        cmd.pre_exec(move || {
            if libc::dup2(r, 3) < 0 || libc::dup2(w, 4) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(ChildEnds(Box::new((read_fd, write_fd))))
}

#[cfg(windows)]
fn attach_pipes(
    cmd: &mut Command,
    child_read: PipeReader,
    child_write: PipeWriter,
    parent_read: &PipeReader,
    parent_write: &PipeWriter,
) -> std::io::Result<ChildEnds> {
    use std::os::windows::io::AsRawHandle;

    const HANDLE_FLAG_INHERIT: u32 = 1;
    unsafe extern "system" {
        fn SetHandleInformation(handle: *mut std::ffi::c_void, mask: u32, flags: u32) -> i32;
    }
    fn inheritable(handle: *mut std::ffi::c_void, on: bool) -> std::io::Result<()> {
        // SAFETY: `handle` is a live pipe handle owned by the caller for the whole call.
        let ok = unsafe {
            SetHandleInformation(
                handle,
                HANDLE_FLAG_INHERIT,
                if on { HANDLE_FLAG_INHERIT } else { 0 },
            )
        };
        if ok == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    // Only the child's ends are inherited, never ours.
    inheritable(parent_read.as_raw_handle(), false)?;
    inheritable(parent_write.as_raw_handle(), false)?;
    inheritable(child_read.as_raw_handle(), true)?;
    inheritable(child_write.as_raw_handle(), true)?;
    cmd.arg("--remote-debugging-pipe").arg(format!(
        "--remote-debugging-io-pipes={},{}",
        child_read.as_raw_handle() as usize,
        child_write.as_raw_handle() as usize
    ));
    Ok(ChildEnds(Box::new((child_read, child_write))))
}

/// Start the browser with the throwaway `profile` and a DevTools pipe; returns the process and
/// the connected client.
fn spawn_browser(
    exe: &Path,
    profile: &Path,
    start_url: &str,
    headless: bool,
) -> std::io::Result<(Child, Cdp)> {
    // browser -> us, and us -> browser.
    let (parent_read, child_write) = std::io::pipe()?;
    let (child_read, parent_write) = std::io::pipe()?;
    let mut cmd = Command::new(exe);
    cmd.arg(format!("--user-data-dir={}", profile.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        // Never sign the throwaway profile in to the browser or sync it with other profiles.
        // Edge would otherwise implicitly sign new profiles in with the Windows account (D16).
        .arg("--disable-sync")
        .arg("--disable-features=msImplicitSignin,msEdgeSyncConsent,msSignInWithWindowsAccount");
    if headless {
        cmd.arg("--headless=new");
    }
    cmd.arg(start_url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    let ends = attach_pipes(&mut cmd, child_read, child_write)?;
    #[cfg(windows)]
    let ends = attach_pipes(&mut cmd, child_read, child_write, &parent_read, &parent_write)?;
    let child = cmd.spawn();
    drop(ends);
    Ok((child?, Cdp::new(parent_read, parent_write)))
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

/// Wait until the browser answers on the pipe (its first reply can take a few seconds).
async fn wait_for_browser(cdp: &mut Cdp, proc: &mut BrowserProcess) -> Result<(), Error> {
    let started = Instant::now();
    loop {
        if proc.has_exited() {
            return Err(Error::Cancelled);
        }
        match cdp
            .call_within("Browser.getVersion", json!({}), Duration::from_secs(5))
            .await
        {
            Ok(_) => return Ok(()),
            Err(CdpError::Closed) => return Err(Error::Cancelled),
            Err(CdpError::Failed(_)) => {}
        }
        if started.elapsed() > STARTUP_TIMEOUT {
            return Err(Error::State(
                "the browser did not start in time (it never answered on its debugging pipe)".into(),
            ));
        }
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
    let (child, mut cdp) = spawn_browser(&exe, &dir, start_url.as_str(), false)
        .map_err(|e| Error::State(format!("could not start {}: {e}", label(kind))))?;
    proc.child = Some(child);
    wait_for_browser(&mut cdp, &mut proc).await?;

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
    fn messages_are_nul_terminated_json_split_anywhere() {
        let (r, mut w) = std::io::pipe().unwrap();
        let (tx, mut rx) = unbounded_channel();
        let t = std::thread::spawn(move || read_messages(r, tx));
        // Two messages, the second delivered in awkward pieces, plus one that is not JSON.
        w.write_all(b"{\"id\":1}\0not json\0{\"id\"").unwrap();
        w.flush().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        w.write_all(b":2,\"result\":{\"a\":\"\xc3\xa9\"}}\0").unwrap();
        drop(w);
        t.join().unwrap();
        assert_eq!(rx.try_recv().unwrap()["id"], 1);
        let second = rx.try_recv().unwrap();
        assert_eq!(second["id"], 2);
        assert_eq!(second["result"]["a"], "\u{e9}");
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn call_matches_replies_by_id_and_skips_events() {
        // A fake browser on the other end of two pipes.
        let (browser_in, our_writer) = std::io::pipe().unwrap();
        let (our_reader, mut browser_out) = std::io::pipe().unwrap();
        let server = std::thread::spawn(move || {
            let mut input = browser_in;
            let mut got = Vec::new();
            let mut byte = [0u8; 1];
            while input.read(&mut byte).unwrap_or(0) == 1 {
                if byte[0] != 0 {
                    got.push(byte[0]);
                    continue;
                }
                let req: Value = serde_json::from_slice(&got).unwrap();
                got.clear();
                let id = req["id"].as_u64().unwrap();
                let reply = if req["method"] == "Boom" {
                    json!({"id": id, "error": {"message": "nope"}})
                } else {
                    json!({"id": id, "result": {"echo": req["method"]}})
                };
                let mut out = json!({"method": "Some.event", "params": {}})
                    .to_string()
                    .into_bytes();
                out.push(0);
                out.extend(json!({"id": id + 100, "result": {}}).to_string().into_bytes());
                out.push(0);
                out.extend(reply.to_string().into_bytes());
                out.push(0);
                browser_out.write_all(&out).unwrap();
            }
        });
        let mut cdp = Cdp::new(our_reader, our_writer);
        let r = cdp.call("Storage.getCookies", json!({})).await.unwrap();
        assert_eq!(r["echo"], "Storage.getCookies");
        match cdp.call("Boom", json!({})).await {
            Err(CdpError::Failed(m)) => assert!(m.contains("nope")),
            other => panic!("{other:?}"),
        }
        drop(cdp); // closes the request pipe; the fake browser stops
        server.join().unwrap();
    }

    #[tokio::test]
    async fn call_reports_a_closed_channel_when_the_browser_goes_away() {
        let (browser_in, our_writer) = std::io::pipe().unwrap();
        let (our_reader, browser_out) = std::io::pipe().unwrap();
        let mut cdp = Cdp::new(our_reader, our_writer);
        drop(browser_out); // the browser exits: our reader sees EOF
        match cdp.call("Anything", json!({})).await {
            Err(CdpError::Closed) => {}
            other => panic!("{other:?}"),
        }
        drop(browser_in);
    }

    /// Needs Edge or Chrome installed: starts a headless instance with a throwaway profile and
    /// talks to it over the debugging pipe exactly as `sign_in` does. Run with
    /// `cargo test -p ampsync-core --lib real_browser -- --ignored`.
    #[tokio::test]
    #[ignore = "launches the installed Edge/Chrome"]
    async fn real_browser_answers_over_the_debugging_pipe() {
        let exe = find_browser(BrowserKind::Edge)
            .or_else(|| find_browser(BrowserKind::Chrome))
            .expect("Edge or Chrome is installed");
        let dir = std::env::temp_dir().join(format!("ampsync-pipe-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (child, mut cdp) = spawn_browser(&exe, &dir, "about:blank", true).unwrap();
        let mut proc = BrowserProcess {
            child: Some(child),
            dir: dir.clone(),
        };
        wait_for_browser(&mut cdp, &mut proc).await.unwrap();
        let version = cdp.call("Browser.getVersion", json!({})).await.unwrap();
        assert!(
            version["product"].as_str().is_some_and(|p| !p.is_empty()),
            "{version}"
        );
        let cookies = cdp.call("Storage.getCookies", json!({})).await.unwrap();
        assert!(cookies["cookies"].is_array(), "{cookies}");
        proc.shutdown(Some(&mut cdp)).await;
        assert!(!dir.exists(), "profile removed");
    }

    #[test]
    fn chunk_free_edge_cases() {
        let v = json!({"cookies": []});
        assert!(parse_cookies(&v, "x.sharepoint.com").is_empty());
        assert!(parse_cookies(&Value::Null, "x.sharepoint.com").is_empty());
    }
}
