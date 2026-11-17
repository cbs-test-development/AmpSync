//! Native-messaging host mode: the browser starts `ampsync-desktop <origin>` and talks to it over
//! stdio; this relay passes those messages to the running app over its pipe / socket and back.
//!
//! Like `--purge` (see `purge.rs`) this runs at the very start of `run()`, before Tauri and the
//! single-instance plugin exist, so a browser starting the host never opens a window or wakes the
//! running app's window. The relay never touches the keychain and never logs: the messages carry
//! cookie values.
//!
//! The extension sends `hello` once, when it connects. The relay keeps it and replays it to the app
//! on every (re)connection, because the app may start after the browser did.

use std::ffi::OsString;
use std::io;
use std::time::Duration;

use ampsync_core::auth::BrowserKind;
use ampsync_core::browser_link::{
    self, FromExtension, MAX_MESSAGE, ToExtension, read_native_frame, validate_session, write_native_frame,
};
use time::OffsetDateTime;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

const ORIGIN_PREFIX: &str = "chrome-extension://";
const RETRY: Duration = Duration::from_secs(5);

type Rd = Box<dyn AsyncRead + Unpin + Send>;
type Wr = Box<dyn AsyncWrite + Unpin + Send>;

/// The browser's origin argument when this process was started as a native host. Only the first
/// argument counts (Chrome and Edge pass `--parent-window=N` after it on Windows).
pub fn host_origin<I: IntoIterator<Item = OsString>>(args: I) -> Option<String> {
    let first = args.into_iter().nth(1)?.into_string().ok()?;
    first.starts_with(ORIGIN_PREFIX).then_some(first)
}

/// What to do with one message that came from the extension.
#[derive(Debug, PartialEq, Eq)]
enum FromExt {
    /// Keep (to replay later) and forward.
    Hello(Vec<u8>),
    Forward(Vec<u8>),
    Drop,
}

/// Validate a frame from the extension. The forwarded bytes are the original message (compact
/// JSON), not a re-encoding of the parsed type, so fields a newer extension adds survive.
fn classify_from_extension(frame: &[u8]) -> FromExt {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(frame) else {
        return FromExt::Drop;
    };
    let Ok(msg) = serde_json::from_value::<FromExtension>(value.clone()) else {
        return FromExt::Drop;
    };
    let Ok(bytes) = serde_json::to_vec(&value) else {
        return FromExt::Drop;
    };
    if bytes.len() + 1 > MAX_MESSAGE {
        return FromExt::Drop;
    }
    match msg {
        FromExtension::Hello { .. } => FromExt::Hello(bytes),
        FromExtension::Session { host, cookies, .. } => {
            // The browser kind does not matter here; the app checks again with the real one.
            match validate_session(&host, &cookies, BrowserKind::Edge, OffsetDateTime::now_utc()) {
                Ok(_) => FromExt::Forward(bytes),
                Err(_) => FromExt::Drop,
            }
        }
        FromExtension::Error { .. } | FromExtension::Pong { .. } => FromExt::Forward(bytes),
        FromExtension::Unknown => FromExt::Drop,
    }
}

/// A line from the app, as the bytes to send the extension; `None` for anything the app may not
/// say (the `app` message belongs to the relay alone).
fn classify_from_app(line: &[u8]) -> Option<Vec<u8>> {
    match serde_json::from_slice::<ToExtension>(line).ok()? {
        ToExtension::Refresh { .. } | ToExtension::Ping { .. } => Some(line.to_vec()),
        ToExtension::App { .. } => None,
    }
}

/// Send the extension a frame. `false` when the browser has gone away.
fn to_extension(payload: &[u8]) -> bool {
    let mut out = io::stdout().lock();
    write_native_frame(&mut out, payload).is_ok()
}

fn app_running(running: bool) -> bool {
    match browser_link::encode_line(&ToExtension::App { running }) {
        // The frame has no trailing newline.
        Ok(mut line) => {
            line.pop();
            to_extension(&line)
        }
        Err(_) => true,
    }
}

/// The relay's connection to the app: lines from it arrive on `lines`.
struct Conn {
    wr: Wr,
    lines: mpsc::UnboundedReceiver<Vec<u8>>,
}

impl Conn {
    async fn send(&mut self, payload: &[u8]) -> bool {
        let mut line = Vec::with_capacity(payload.len() + 1);
        line.extend_from_slice(payload);
        line.push(b'\n');
        self.wr.write_all(&line).await.is_ok() && self.wr.flush().await.is_ok()
    }
}

#[cfg(windows)]
async fn connect_app() -> io::Result<(Rd, Wr)> {
    use tokio::net::windows::named_pipe::ClientOptions;
    const ERROR_PIPE_BUSY: i32 = 231;
    let name = crate::link_ipc::pipe_name()?;
    let mut busy = 0;
    let client = loop {
        match ClientOptions::new().open(&name) {
            Ok(c) => break c,
            // Every instance is serving someone for a moment: try again shortly.
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && busy < 10 => {
                busy += 1;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => return Err(e),
        }
    };
    let (r, w) = tokio::io::split(client);
    Ok((Box::new(r), Box::new(w)))
}

#[cfg(unix)]
async fn connect_app() -> io::Result<(Rd, Wr)> {
    let stream = tokio::net::UnixStream::connect(crate::link_ipc::socket_path()).await?;
    let (r, w) = tokio::io::split(stream);
    Ok((Box::new(r), Box::new(w)))
}

/// Read NDJSON lines (bounded) in their own task; the channel closes when the app goes away.
fn spawn_line_reader(rd: Rd) -> mpsc::UnboundedReceiver<Vec<u8>> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut rd = BufReader::new(rd);
        loop {
            let mut buf = Vec::new();
            match (&mut rd).take(MAX_MESSAGE as u64 + 1).read_until(b'\n', &mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            // No newline: the stream ended mid-line, or the line was over the limit.
            if buf.pop() != Some(b'\n') {
                break;
            }
            if tx.send(buf).is_err() {
                break;
            }
        }
    });
    rx
}

async fn next_line(conn: &mut Option<Conn>) -> Option<Vec<u8>> {
    match conn {
        Some(c) => c.lines.recv().await,
        None => std::future::pending().await,
    }
}

/// Connect to the app and tell the extension; replays `hello`. `None` when the app is not there.
async fn try_connect(hello: &Option<Vec<u8>>) -> Option<Conn> {
    let (rd, wr) = connect_app().await.ok()?;
    let mut conn = Conn {
        wr,
        lines: spawn_line_reader(rd),
    };
    if !app_running(true) {
        return None;
    }
    if let Some(h) = hello
        && !conn.send(h).await
    {
        return None;
    }
    Some(conn)
}

async fn relay() -> i32 {
    // Blocking stdin reads stay on their own thread; the channel closing means stdin ended.
    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        while let Ok(Some(frame)) = read_native_frame(&mut stdin) {
            if frame_tx.send(frame).is_err() {
                break;
            }
        }
    });

    let mut hello: Option<Vec<u8>> = None;
    let mut conn: Option<Conn> = try_connect(&hello).await;
    let mut retry = tokio::time::interval(RETRY);
    retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    retry.tick().await; // the first tick is immediate
    loop {
        tokio::select! {
            frame = frame_rx.recv() => {
                let Some(frame) = frame else { return 0 };
                match classify_from_extension(&frame) {
                    FromExt::Hello(bytes) => {
                        let sent = match conn.as_mut() {
                            Some(c) => c.send(&bytes).await,
                            None => false,
                        };
                        hello = Some(bytes);
                        if !sent {
                            conn = None;
                            if !app_running(false) {
                                return 0;
                            }
                        }
                    }
                    FromExt::Forward(bytes) => {
                        // Nothing is queued for a missing app: the startup check asks again.
                        if let Some(c) = conn.as_mut()
                            && !c.send(&bytes).await
                        {
                            conn = None;
                            if !app_running(false) {
                                return 0;
                            }
                        }
                    }
                    FromExt::Drop => {}
                }
            }
            line = next_line(&mut conn) => {
                match line {
                    Some(l) => {
                        if let Some(payload) = classify_from_app(&l)
                            && !to_extension(&payload)
                        {
                            return 0;
                        }
                    }
                    None => {
                        conn = None;
                        if !app_running(false) {
                            return 0;
                        }
                    }
                }
            }
            _ = retry.tick(), if conn.is_none() => {
                conn = try_connect(&hello).await;
            }
        }
    }
}

/// Run as the native host; returns the process exit code. Exits when the browser closes stdin.
pub fn run(origin: &str) -> i32 {
    if !browser_link::is_allowed_origin(origin) {
        return 1;
    }
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(_) => return 1,
    };
    rt.block_on(relay())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn host_mode_is_the_first_argument_only() {
        assert_eq!(
            host_origin(args(&["x", "chrome-extension://abc/", "--parent-window=1"])),
            Some("chrome-extension://abc/".to_string())
        );
        assert_eq!(host_origin(args(&["x"])), None);
        assert_eq!(host_origin(args(&["x", "--purge"])), None);
        assert_eq!(host_origin(args(&["x", "file", "chrome-extension://abc/"])), None);
    }

    fn session_json(host: &str, with_rtfa: bool) -> Vec<u8> {
        let mut cookies = vec![serde_json::json!(
            {"name":"FedAuth","value":"v","domain":host,"path":"/","expires":null})];
        if with_rtfa {
            cookies.push(serde_json::json!(
                {"name":"rtFa","value":"v","domain":".sharepoint.com","path":"/","expires":null}));
        }
        serde_json::to_vec(&serde_json::json!(
            {"type":"session","id":null,"host":host,"reason":"changed","cookies":cookies}))
        .unwrap()
    }

    #[test]
    fn valid_messages_are_forwarded_and_junk_is_dropped() {
        let hello = br#"{"type":"hello","ext_version":"1.0.0","browser":"edge","protocol":1}"#;
        assert!(matches!(classify_from_extension(hello), FromExt::Hello(_)));
        assert!(matches!(
            classify_from_extension(br#"{"type":"pong","id":"p1"}"#),
            FromExt::Forward(_)
        ));
        assert!(matches!(
            classify_from_extension(br#"{"type":"error","id":"r1","code":"timeout"}"#),
            FromExt::Forward(_)
        ));
        assert_eq!(classify_from_extension(br#"{"type":"telemetry"}"#), FromExt::Drop);
        assert_eq!(classify_from_extension(b"not json"), FromExt::Drop);
        assert_eq!(classify_from_extension(br#"{"type":"hello"}"#), FromExt::Drop);
    }

    #[test]
    fn sessions_are_validated_before_forwarding() {
        assert!(matches!(
            classify_from_extension(&session_json("contoso.sharepoint.com", true)),
            FromExt::Forward(_)
        ));
        // No rtFa, and a host that is not SharePoint.
        assert_eq!(
            classify_from_extension(&session_json("contoso.sharepoint.com", false)),
            FromExt::Drop
        );
        assert_eq!(
            classify_from_extension(&session_json("evil.example.com", true)),
            FromExt::Drop
        );
    }

    #[test]
    fn the_original_message_is_forwarded_with_unknown_fields() {
        let msg = br#"{"type":"pong","id":"p1","future":{"a":1}}"#;
        match classify_from_extension(msg) {
            FromExt::Forward(b) => {
                let v: serde_json::Value = serde_json::from_slice(&b).unwrap();
                assert_eq!(v["future"]["a"], 1);
                assert!(!b.contains(&b'\n'));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_app_may_only_send_refresh_and_ping() {
        assert!(classify_from_app(br#"{"type":"ping","id":"p"}"#).is_some());
        assert!(
            classify_from_app(br#"{"type":"refresh","id":"r","host":"a.sharepoint.com","start_url":"https://a.sharepoint.com/","interactive":false}"#)
                .is_some()
        );
        assert!(classify_from_app(br#"{"type":"app","running":true}"#).is_none());
        assert!(classify_from_app(b"junk").is_none());
    }
}
