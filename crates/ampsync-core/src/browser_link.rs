//! Browser link: protocol, validation and file locations shared by the desktop app and the
//! native-messaging relay (see `docs/plans/browser-link.md`).
//!
//! Everything here is pure: no registry writes, no sockets. The desktop crate does the I/O.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::Error;
use crate::auth::{BrowserKind, Session, SessionCookie, SignInMethod};
use crate::link::{SHAREPOINT_SUFFIXES, is_sharepoint_host};

/// Chrome/Edge extension id derived from the public key in the extension manifest.
pub const EXTENSION_ID: &str = "foohiaphdliphaogfjlahdmbjemmaocp";
/// Native messaging host name.
pub const HOST_NAME: &str = "io.github.ampsync.browserlink";
/// The only origin allowed to start the native host (browsers pass it as argv[1]).
pub const ALLOWED_ORIGIN: &str = "chrome-extension://foohiaphdliphaogfjlahdmbjemmaocp/";
/// Highest protocol version this build understands.
pub const PROTOCOL_VERSION: u32 = 1;
/// Most cookies accepted in one `session` message.
pub const MAX_COOKIES: usize = 64;
/// Longest accepted cookie value, in bytes.
pub const MAX_COOKIE_VALUE: usize = 16 * 1024;
/// Largest message AmpSync itself sends or accepts per line.
pub const MAX_MESSAGE: usize = 256 * 1024;
/// Largest native-messaging frame accepted from (or written to) the browser.
pub const MAX_INCOMING: usize = 1024 * 1024;

const HOST_DESCRIPTION: &str = "AmpSync browser link";

// --- messages ---------------------------------------------------------------------------------

/// A cookie as the extension sends it. `Debug` hides the value.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    #[serde(default = "default_path")]
    pub path: String,
    /// Unix seconds; `None` for a session cookie.
    #[serde(default)]
    pub expires: Option<i64>,
}

fn default_path() -> String {
    "/".to_string()
}

impl std::fmt::Debug for WireCookie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WireCookie")
            .field("name", &self.name)
            .field("value_len", &self.value.len())
            .field("domain", &self.domain)
            .field("path", &self.path)
            .field("expires", &self.expires)
            .finish()
    }
}

/// Why the extension sent a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionReason {
    Changed,
    Requested,
    Initial,
    #[serde(other)]
    Other,
}

/// Why the extension could not deliver a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    SigninRequired,
    Timeout,
    NoPermission,
    BadRequest,
    #[serde(other)]
    Other,
}

/// Extension to AmpSync. An unknown `type` parses as [`FromExtension::Unknown`] (ignore it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromExtension {
    Hello {
        ext_version: String,
        browser: BrowserKind,
        protocol: u32,
    },
    Session {
        #[serde(default)]
        id: Option<String>,
        host: String,
        reason: SessionReason,
        cookies: Vec<WireCookie>,
    },
    Error {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        host: Option<String>,
        code: ErrorCode,
    },
    Pong {
        id: String,
    },
    #[serde(other)]
    Unknown,
}

/// AmpSync (or the relay) to the extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToExtension {
    Refresh {
        id: String,
        host: String,
        start_url: String,
        interactive: bool,
    },
    Ping {
        id: String,
    },
    /// Sent by the relay only.
    App {
        running: bool,
    },
}

// --- validation -------------------------------------------------------------------------------

/// Check a `session` message and turn it into an [`Session`] for the keychain.
///
/// `host` must be a SharePoint tenant host (lower-cased in the result). Only cookies whose domain
/// (leading dot removed) is `host` or the bare suffix domain (`sharepoint.com`, which carries
/// `rtFa`) are kept. `FedAuth` and `rtFa` are required.
pub fn validate_session(
    host: &str,
    cookies: &[WireCookie],
    kind: BrowserKind,
    now: OffsetDateTime,
) -> Result<Session, Error> {
    let host = host.trim().to_ascii_lowercase();
    let host_ok = host
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.');
    if !host_ok || !is_sharepoint_host(&host) {
        return Err(Error::InvalidLink(format!(
            "`{host}` is not a SharePoint Online address"
        )));
    }
    let bare = SHAREPOINT_SUFFIXES
        .iter()
        .find(|s| host.ends_with(**s))
        .map(|s| s.trim_start_matches('.'))
        .unwrap_or("");
    if cookies.len() > MAX_COOKIES {
        return Err(Error::State(format!(
            "the browser sent too many cookies ({})",
            cookies.len()
        )));
    }
    let mut kept = Vec::new();
    for c in cookies {
        let domain = c.domain.trim_start_matches('.').to_ascii_lowercase();
        if domain != host && domain != bare {
            continue;
        }
        if c.value.len() > MAX_COOKIE_VALUE {
            return Err(Error::State(format!("cookie {} is too large", c.name)));
        }
        let expires = match c.expires {
            None => None,
            Some(secs) => Some(
                OffsetDateTime::from_unix_timestamp(secs)
                    .map_err(|_| Error::State(format!("cookie {} has an invalid expiry", c.name)))?,
            ),
        };
        kept.push(SessionCookie {
            name: c.name.clone(),
            value: c.value.clone(),
            domain,
            path: c.path.clone(),
            expires,
        });
    }
    for needed in ["FedAuth", "rtFa"] {
        if !kept.iter().any(|c| c.name == needed && !c.value.is_empty()) {
            return Err(Error::State(format!(
                "the browser is not signed in to {host} (no {needed} cookie)"
            )));
        }
    }
    Ok(Session {
        host,
        cookies: kept,
        method: SignInMethod::BrowserLink { kind },
        captured_at: now,
    })
}

// --- framing ----------------------------------------------------------------------------------

/// Read one native-messaging frame (4-byte little-endian length, then the payload).
/// `Ok(None)` on a clean end of input; an error for a truncated frame or one larger than
/// [`MAX_INCOMING`].
pub fn read_native_frame<R: Read>(r: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        match r.read(&mut len[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let n = u32::from_le_bytes(len) as usize;
    if n > MAX_INCOMING {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "native message is too large",
        ));
    }
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

/// Write one native-messaging frame and flush. Errors if `payload` exceeds 1 MiB.
pub fn write_native_frame<W: Write>(w: &mut W, payload: &[u8]) -> io::Result<()> {
    if payload.len() > MAX_INCOMING {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "native message is too large",
        ));
    }
    w.write_all(&(payload.len() as u32).to_le_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

/// One NDJSON line: the JSON text of `msg` plus `\n`. Errors if it exceeds [`MAX_MESSAGE`].
/// Decode a received line with `serde_json::from_slice`.
pub fn encode_line<T: Serialize>(msg: &T) -> io::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(msg).map_err(io::Error::other)?;
    if bytes.len() + 1 > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "message is too large",
        ));
    }
    bytes.push(b'\n');
    Ok(bytes)
}

// --- places -----------------------------------------------------------------------------------

/// Windows pipe name for a user: `\\.\pipe\ampsync-link-<first 16 hex of sha256(identity)>`.
/// The desktop passes the user's SID string as `user_identity`.
pub fn pipe_name_for(user_identity: &str) -> String {
    let digest = Sha256::digest(user_identity.as_bytes());
    let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!(r"\\.\pipe\ampsync-link-{hex}")
}

/// Linux socket: `$XDG_RUNTIME_DIR/ampsync-link.sock`, else `<data dir>/link.sock`.
pub fn socket_path(runtime_dir: Option<&Path>, data_dir: &Path) -> PathBuf {
    match runtime_dir {
        Some(d) => d.join("ampsync-link.sock"),
        None => data_dir.join("link.sock"),
    }
}

/// Folder the unpacked extension is extracted to: `<data dir>/browser-link`.
pub fn extension_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("browser-link")
}

/// Folder holding the native host manifest: `<data dir>/native-host`.
pub fn native_host_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("native-host")
}

/// `<data dir>/native-host/<HOST_NAME>.json`.
pub fn host_manifest_path(data_dir: &Path) -> PathBuf {
    native_host_dir(data_dir).join(format!("{HOST_NAME}.json"))
}

/// `<data dir>/signin-stats.json`.
pub fn signin_stats_path(data_dir: &Path) -> PathBuf {
    data_dir.join("signin-stats.json")
}

/// Exactly the allowed origin (the browser passes it as the host's first argument).
pub fn is_allowed_origin(arg: &str) -> bool {
    arg == ALLOWED_ORIGIN
}

/// Pretty JSON of the native messaging host manifest pointing at `exe_path`.
pub fn host_manifest_json(exe_path: &Path) -> String {
    let value = serde_json::json!({
        "name": HOST_NAME,
        "description": HOST_DESCRIPTION,
        "path": exe_path.to_string_lossy(),
        "type": "stdio",
        "allowed_origins": [ALLOWED_ORIGIN],
    });
    serde_json::to_string_pretty(&value).unwrap_or_default()
}

/// A per-user Windows registry key (under `HKEY_CURRENT_USER`) whose default value is the path of
/// the host manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryTarget {
    pub browser: BrowserKind,
    /// Key path below HKCU, without a leading backslash.
    pub subkey: String,
}

/// Registry keys to write on Windows (Edge and Chrome).
pub fn registry_targets() -> Vec<RegistryTarget> {
    vec![
        RegistryTarget {
            browser: BrowserKind::Edge,
            subkey: format!(r"Software\Microsoft\Edge\NativeMessagingHosts\{HOST_NAME}"),
        },
        RegistryTarget {
            browser: BrowserKind::Chrome,
            subkey: format!(r"Software\Google\Chrome\NativeMessagingHosts\{HOST_NAME}"),
        },
    ]
}

/// A manifest file to write on Linux.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestTarget {
    pub browser: BrowserKind,
    pub path: PathBuf,
}

/// Linux manifest destinations below `config_home` (normally `~/.config`).
pub fn linux_manifest_targets(config_home: &Path) -> Vec<ManifestTarget> {
    let file = format!("{HOST_NAME}.json");
    vec![
        ManifestTarget {
            browser: BrowserKind::Edge,
            path: config_home
                .join("microsoft-edge")
                .join("NativeMessagingHosts")
                .join(&file),
        },
        ManifestTarget {
            browser: BrowserKind::Chrome,
            path: config_home
                .join("google-chrome")
                .join("NativeMessagingHosts")
                .join(&file),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Duration;

    fn now() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::days(20000)
    }

    fn cookie(name: &str, domain: &str) -> WireCookie {
        WireCookie {
            name: name.into(),
            value: "v".into(),
            domain: domain.into(),
            path: "/".into(),
            expires: Some(1763300000),
        }
    }

    fn good() -> Vec<WireCookie> {
        vec![
            cookie("FedAuth", "contoso.sharepoint.com"),
            cookie("rtFa", ".sharepoint.com"),
        ]
    }

    #[test]
    fn parses_every_message_type() {
        let m: FromExtension =
            serde_json::from_str(r#"{"type":"hello","ext_version":"1.0.0","browser":"edge","protocol":1}"#)
                .unwrap();
        assert_eq!(
            m,
            FromExtension::Hello {
                ext_version: "1.0.0".into(),
                browser: BrowserKind::Edge,
                protocol: 1
            }
        );
        let m: FromExtension = serde_json::from_str(
            r#"{"type":"session","id":null,"host":"contoso.sharepoint.com","reason":"changed",
                "cookies":[{"name":"FedAuth","value":"x","domain":"contoso.sharepoint.com","path":"/","expires":1763300000},
                           {"name":"rtFa","value":"y","domain":".sharepoint.com","path":"/","expires":null}]}"#,
        )
        .unwrap();
        match m {
            FromExtension::Session {
                id,
                host,
                reason,
                cookies,
            } => {
                assert_eq!(id, None);
                assert_eq!(host, "contoso.sharepoint.com");
                assert_eq!(reason, SessionReason::Changed);
                assert_eq!(cookies.len(), 2);
                assert_eq!(cookies[0].expires, Some(1763300000));
                assert_eq!(cookies[1].expires, None);
            }
            other => panic!("{other:?}"),
        }
        let m: FromExtension = serde_json::from_str(
            r#"{"type":"error","id":"r1","host":"contoso.sharepoint.com","code":"signin_required"}"#,
        )
        .unwrap();
        assert_eq!(
            m,
            FromExtension::Error {
                id: Some("r1".into()),
                host: Some("contoso.sharepoint.com".into()),
                code: ErrorCode::SigninRequired
            }
        );
        let m: FromExtension = serde_json::from_str(r#"{"type":"pong","id":"p7"}"#).unwrap();
        assert_eq!(m, FromExtension::Pong { id: "p7".into() });
        for (text, code) in [
            ("timeout", ErrorCode::Timeout),
            ("no_permission", ErrorCode::NoPermission),
            ("bad_request", ErrorCode::BadRequest),
            ("something_new", ErrorCode::Other),
        ] {
            let m: FromExtension =
                serde_json::from_str(&format!(r#"{{"type":"error","id":"a","code":"{text}"}}"#)).unwrap();
            assert!(
                matches!(m, FromExtension::Error { code: c, .. } if c == code),
                "{text}"
            );
        }
    }

    #[test]
    fn unknown_types_and_reasons_are_tolerated() {
        let m: FromExtension = serde_json::from_str(r#"{"type":"telemetry","x":1}"#).unwrap();
        assert_eq!(m, FromExtension::Unknown);
        let m: FromExtension = serde_json::from_str(
            r#"{"type":"session","host":"a.sharepoint.com","reason":"future","cookies":[]}"#,
        )
        .unwrap();
        assert!(matches!(
            m,
            FromExtension::Session {
                reason: SessionReason::Other,
                id: None,
                ..
            }
        ));
    }

    #[test]
    fn to_extension_serialises_as_specified() {
        let v = serde_json::to_value(ToExtension::Refresh {
            id: "r42".into(),
            host: "contoso.sharepoint.com".into(),
            start_url: "https://contoso.sharepoint.com/sites/X".into(),
            interactive: false,
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({"type":"refresh","id":"r42","host":"contoso.sharepoint.com",
                "start_url":"https://contoso.sharepoint.com/sites/X","interactive":false})
        );
        assert_eq!(
            serde_json::to_string(&ToExtension::Ping { id: "p7".into() }).unwrap(),
            r#"{"type":"ping","id":"p7"}"#
        );
        assert_eq!(
            serde_json::to_string(&ToExtension::App { running: true }).unwrap(),
            r#"{"type":"app","running":true}"#
        );
    }

    #[test]
    fn debug_hides_cookie_values() {
        let mut c = cookie("FedAuth", "a.sharepoint.com");
        c.value = "SECRETVALUE".into();
        assert!(!format!("{c:?}").contains("SECRETVALUE"));
    }

    #[test]
    fn valid_session_keeps_host_and_suffix_cookies() {
        let mut cookies = good();
        cookies.push(cookie("junk", "login.microsoftonline.com"));
        cookies.push(cookie("other", "fabrikam.sharepoint.com"));
        cookies.push(cookie("evil", "evilsharepoint.com"));
        let s = validate_session("Contoso.SharePoint.com", &cookies, BrowserKind::Edge, now()).unwrap();
        assert_eq!(s.host, "contoso.sharepoint.com");
        assert_eq!(
            s.method,
            SignInMethod::BrowserLink {
                kind: BrowserKind::Edge
            }
        );
        assert_eq!(s.captured_at, now());
        let names: Vec<_> = s.cookies.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["FedAuth", "rtFa"]);
        assert_eq!(s.cookies[1].domain, "sharepoint.com");
        assert_eq!(
            s.cookies[0].expires,
            Some(OffsetDateTime::from_unix_timestamp(1763300000).unwrap())
        );
    }

    #[test]
    fn session_cookies_have_no_expiry() {
        let mut cookies = good();
        cookies[0].expires = None;
        let s = validate_session("contoso.sharepoint.com", &cookies, BrowserKind::Chrome, now()).unwrap();
        assert_eq!(s.cookies[0].expires, None);
    }

    #[test]
    fn lookalike_hosts_are_rejected() {
        for host in [
            "evilsharepoint.com",
            "contoso.sharepoint.com.evil.net",
            "sharepoint.com",
            "contoso.sharepoint.com/evil",
            "",
        ] {
            let cookies = vec![cookie("FedAuth", host), cookie("rtFa", host)];
            assert!(
                validate_session(host, &cookies, BrowserKind::Edge, now()).is_err(),
                "{host}"
            );
        }
    }

    #[test]
    fn foreign_domains_do_not_count() {
        let cookies = vec![
            cookie("FedAuth", "contoso.sharepoint.com"),
            cookie("rtFa", "evilsharepoint.com"),
        ];
        assert!(validate_session("contoso.sharepoint.com", &cookies, BrowserKind::Edge, now()).is_err());
        let cookies = vec![
            cookie("FedAuth", "fabrikam.sharepoint.com"),
            cookie("rtFa", ".sharepoint.com"),
        ];
        assert!(validate_session("contoso.sharepoint.com", &cookies, BrowserKind::Edge, now()).is_err());
    }

    #[test]
    fn missing_fedauth_or_rtfa_is_rejected() {
        let only_fed = vec![cookie("FedAuth", "contoso.sharepoint.com")];
        assert!(validate_session("contoso.sharepoint.com", &only_fed, BrowserKind::Edge, now()).is_err());
        let only_rt = vec![cookie("rtFa", "sharepoint.com")];
        assert!(validate_session("contoso.sharepoint.com", &only_rt, BrowserKind::Edge, now()).is_err());
        assert!(validate_session("contoso.sharepoint.com", &[], BrowserKind::Edge, now()).is_err());
    }

    #[test]
    fn caps_are_enforced() {
        let mut big = good();
        big[0].value = "x".repeat(MAX_COOKIE_VALUE + 1);
        assert!(validate_session("contoso.sharepoint.com", &big, BrowserKind::Edge, now()).is_err());
        let mut ok = good();
        ok[0].value = "x".repeat(MAX_COOKIE_VALUE);
        assert!(validate_session("contoso.sharepoint.com", &ok, BrowserKind::Edge, now()).is_ok());
        let mut many = good();
        while many.len() <= MAX_COOKIES {
            many.push(cookie("extra", "contoso.sharepoint.com"));
        }
        assert!(validate_session("contoso.sharepoint.com", &many, BrowserKind::Edge, now()).is_err());
    }

    #[test]
    fn native_frames_round_trip() {
        let mut buf = Vec::new();
        write_native_frame(&mut buf, b"{\"type\":\"ping\",\"id\":\"1\"}").unwrap();
        write_native_frame(&mut buf, b"").unwrap();
        assert_eq!(&buf[..4], &24u32.to_le_bytes());
        let mut r = io::Cursor::new(buf);
        assert_eq!(
            read_native_frame(&mut r).unwrap().unwrap(),
            b"{\"type\":\"ping\",\"id\":\"1\"}"
        );
        assert_eq!(read_native_frame(&mut r).unwrap().unwrap(), b"");
        assert_eq!(read_native_frame(&mut r).unwrap(), None);
    }

    #[test]
    fn native_frames_reject_oversize_and_truncation() {
        let big = vec![b'a'; MAX_INCOMING + 1];
        assert!(write_native_frame(&mut Vec::new(), &big).is_err());
        let mut w = Vec::new();
        write_native_frame(&mut w, &vec![b'a'; MAX_INCOMING]).unwrap();
        assert_eq!(
            read_native_frame(&mut io::Cursor::new(w)).unwrap().unwrap().len(),
            MAX_INCOMING
        );

        let header = ((MAX_INCOMING + 1) as u32).to_le_bytes().to_vec();
        assert!(read_native_frame(&mut io::Cursor::new(header)).is_err());
        // Truncated header and truncated payload.
        assert!(read_native_frame(&mut io::Cursor::new(vec![1u8, 0])).is_err());
        let mut short = 5u32.to_le_bytes().to_vec();
        short.extend_from_slice(b"ab");
        assert!(read_native_frame(&mut io::Cursor::new(short)).is_err());
    }

    #[test]
    fn ndjson_lines() {
        let line = encode_line(&ToExtension::Ping { id: "p".into() }).unwrap();
        assert_eq!(line, b"{\"type\":\"ping\",\"id\":\"p\"}\n");
        let back: ToExtension = serde_json::from_slice(&line[..line.len() - 1]).unwrap();
        assert_eq!(back, ToExtension::Ping { id: "p".into() });
        let huge = ToExtension::Ping {
            id: "x".repeat(MAX_MESSAGE),
        };
        assert!(encode_line(&huge).is_err());
    }

    #[test]
    fn manifest_json_contents() {
        let text = host_manifest_json(Path::new("/opt/ampsync/ampsync-desktop"));
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["name"], HOST_NAME);
        assert_eq!(v["type"], "stdio");
        assert_eq!(v["path"], "/opt/ampsync/ampsync-desktop");
        assert_eq!(v["allowed_origins"], serde_json::json!([ALLOWED_ORIGIN]));
        assert!(v["description"].is_string());
        assert!(text.contains('\n'), "pretty printed");
    }

    #[test]
    fn pipe_name_is_deterministic_and_per_user() {
        let a = pipe_name_for("S-1-5-21-1-2-3-1001");
        assert_eq!(a, pipe_name_for("S-1-5-21-1-2-3-1001"));
        assert_ne!(a, pipe_name_for("S-1-5-21-1-2-3-1002"));
        let hex = a.strip_prefix(r"\\.\pipe\ampsync-link-").unwrap();
        assert_eq!(hex.len(), 16);
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    fn places() {
        let data = Path::new("/data");
        assert_eq!(
            socket_path(Some(Path::new("/run/user/1000")), data),
            Path::new("/run/user/1000/ampsync-link.sock")
        );
        assert_eq!(socket_path(None, data), Path::new("/data/link.sock"));
        assert_eq!(extension_dir(data), Path::new("/data/browser-link"));
        assert_eq!(native_host_dir(data), Path::new("/data/native-host"));
        assert_eq!(
            host_manifest_path(data),
            Path::new("/data/native-host").join("io.github.ampsync.browserlink.json")
        );
        assert_eq!(signin_stats_path(data), Path::new("/data/signin-stats.json"));
        assert!(is_allowed_origin(
            "chrome-extension://foohiaphdliphaogfjlahdmbjemmaocp/"
        ));
        assert!(!is_allowed_origin(
            "chrome-extension://foohiaphdliphaogfjlahdmbjemmaocp"
        ));
        assert!(!is_allowed_origin("chrome-extension://other/"));
        assert!(ALLOWED_ORIGIN.contains(EXTENSION_ID));
    }

    #[test]
    fn registration_targets() {
        let keys = registry_targets();
        assert_eq!(keys.len(), 2);
        assert_eq!(
            keys[0].subkey,
            r"Software\Microsoft\Edge\NativeMessagingHosts\io.github.ampsync.browserlink"
        );
        assert_eq!(
            keys[1].subkey,
            r"Software\Google\Chrome\NativeMessagingHosts\io.github.ampsync.browserlink"
        );
        let files = linux_manifest_targets(Path::new("/home/u/.config"));
        assert_eq!(
            files[0].path,
            Path::new(
                "/home/u/.config/microsoft-edge/NativeMessagingHosts/io.github.ampsync.browserlink.json"
            )
        );
        assert_eq!(
            files[1].path,
            Path::new(
                "/home/u/.config/google-chrome/NativeMessagingHosts/io.github.ampsync.browserlink.json"
            )
        );
        assert_eq!(files[1].browser, BrowserKind::Chrome);
    }
}
