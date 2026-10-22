//! Embedded-webview sign-in (D3 method 1, D15).
//!
//! A native window (wry + tao) opens the pasted link in a persistent AmpSync-owned browser
//! profile; the cookie store is polled from the event-loop thread every ~750 ms (never from a
//! WebView2 callback: that deadlocks) and the window closes by itself as soon as FedAuth and rtFa
//! for the tenant host exist. After the window is dropped the event loop is pumped for ~500 ms so
//! Windows really destroys it (otherwise it shows as "Not responding").
//!
//! The profile is persistent, so it may still hold cookies from an earlier, now rejected,
//! sign-in. Each candidate session is therefore passed to `validate`; a rejected FedAuth value is
//! remembered and ignored until the page produces a different one.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ampsync_core::auth::{Session, SessionCookie, SignInMethod};
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::run_return::EventLoopExtRunReturn;
use tao::window::WindowBuilder;
use time::OffsetDateTime;
use url::Url;
use wry::{WebView, WebViewBuilder};

const POLL_EVERY: Duration = Duration::from_millis(750);
const PUMP_AFTER_CLOSE: Duration = Duration::from_millis(500);
/// A hidden (silent refresh) window is shown if sign-in has not completed after this long.
const SILENT_PATIENCE: Duration = Duration::from_secs(12);

/// How to run the window.
#[derive(Debug, Clone)]
pub struct Options {
    /// Persistent AmpSync-owned profile directory (D15).
    pub profile_dir: PathBuf,
    /// Windows: single sign-on with the OS work/school account (`Settings::use_windows_account`).
    pub use_windows_account: bool,
    /// Let the browser offer to save passwords (`Settings::save_passwords`).
    pub save_passwords: bool,
    /// Start with the window hidden (silent refresh); it is shown if user interaction is needed.
    pub start_hidden: bool,
    /// A FedAuth value known to be rejected; ignored until a different one appears.
    pub known_bad_fedauth: Option<String>,
}

/// Why no session was produced.
#[derive(Debug)]
pub enum Failure {
    /// The window or webview could not be created (no runtime, no display, blocked): try the
    /// next sign-in method.
    Unavailable(String),
    /// The user closed the window before signing in.
    Closed,
}

/// Whether a window can be shown at all. Always true on Windows.
pub fn display_available() -> bool {
    #[cfg(windows)]
    {
        true
    }
    #[cfg(not(windows))]
    {
        ["DISPLAY", "WAYLAND_DISPLAY"]
            .iter()
            .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()))
    }
}

/// Does a cookie scoped to `domain` apply to `host`? (`.sharepoint.com`, `sharepoint.com`, exact.)
pub fn domain_matches(domain: &str, host: &str) -> bool {
    let d = domain.trim_start_matches('.').to_ascii_lowercase();
    let h = host.to_ascii_lowercase();
    h == d || h.ends_with(&format!(".{d}"))
}

fn cookie_named<'a>(cookies: &'a [SessionCookie], name: &str, host: &str) -> Option<&'a SessionCookie> {
    cookies
        .iter()
        .find(|c| c.name.eq_ignore_ascii_case(name) && domain_matches(&c.domain, host) && !c.value.is_empty())
}

/// Sign-in is complete when both FedAuth and rtFa exist for the tenant host.
pub fn has_auth_cookies(cookies: &[SessionCookie], host: &str) -> bool {
    cookie_named(cookies, "FedAuth", host).is_some() && cookie_named(cookies, "rtFa", host).is_some()
}

fn fedauth_value(cookies: &[SessionCookie], host: &str) -> Option<String> {
    cookie_named(cookies, "FedAuth", host).map(|c| c.value.clone())
}

fn convert(c: &wry::cookie::Cookie<'_>, host: &str) -> SessionCookie {
    let expires = match c.expires() {
        Some(wry::cookie::Expiration::DateTime(dt)) => {
            OffsetDateTime::from_unix_timestamp(dt.unix_timestamp()).ok()
        }
        _ => None,
    };
    SessionCookie {
        name: c.name().to_string(),
        value: c.value().to_string(),
        domain: c.domain().map(str::to_string).unwrap_or_else(|| host.to_string()),
        path: c.path().map(str::to_string).unwrap_or_else(|| "/".to_string()),
        expires,
    }
}

fn fetch_cookies(wv: &WebView, host: &str) -> Vec<SessionCookie> {
    let mut out: Vec<SessionCookie> = Vec::new();
    if let Ok(list) = wv.cookies_for_url(&format!("https://{host}/")) {
        out.extend(list.iter().map(|c| convert(c, host)));
    }
    if !has_auth_cookies(&out, host)
        && let Ok(all) = wv.cookies()
    {
        for c in all.iter() {
            let cc = convert(c, host);
            let dup = out
                .iter()
                .any(|o| o.name == cc.name && o.domain == cc.domain && o.path == cc.path);
            if domain_matches(&cc.domain, host) && !dup {
                out.push(cc);
            }
        }
    }
    out
}

#[cfg(windows)]
fn create_env(
    data_dir: &std::path::Path,
    sso: bool,
) -> Result<webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Environment, String> {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        CreateCoreWebView2EnvironmentWithOptions, ICoreWebView2Environment, ICoreWebView2EnvironmentOptions,
    };
    use webview2_com::{
        CoreWebView2EnvironmentOptions, CreateCoreWebView2EnvironmentCompletedHandler, wait_with_pump,
    };
    use windows_core::{HSTRING, PCWSTR};

    let (tx, rx) = std::sync::mpsc::channel::<windows_core::Result<ICoreWebView2Environment>>();
    let options = CoreWebView2EnvironmentOptions::default();
    unsafe {
        // Same defaults wry uses, plus the SSO switch wry does not expose.
        options.set_additional_browser_arguments(
            "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection".to_string(),
        );
        options.set_allow_single_sign_on_using_os_primary_account(sso);
        let dir = HSTRING::from(data_dir.as_os_str());
        CreateCoreWebView2EnvironmentWithOptions(
            PCWSTR::null(),
            &dir,
            &ICoreWebView2EnvironmentOptions::from(options),
            &CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(move |err, env| {
                let r = (|| {
                    err?;
                    env.ok_or_else(|| windows_core::Error::from(windows_core::HRESULT(0x8000_4003u32 as i32)))
                })();
                tx.send(r)
                    .map_err(|_| windows_core::Error::from(windows_core::HRESULT(0x8000_ffffu32 as i32)))
            })),
        )
        .map_err(|e| e.to_string())?;
    }
    wait_with_pump(rx)
        .map_err(|e| format!("{e:?}"))?
        .map_err(|e| e.to_string())
}

/// Windows: apply `IsPasswordAutosaveEnabled` to the live webview.
#[cfg(windows)]
fn apply_password_setting(webview: &WebView, save_passwords: bool) {
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings4;
    use windows_core::Interface;
    use wry::WebViewExtWindows;

    let core = webview.webview();
    unsafe {
        if let Ok(settings) = core.Settings()
            && let Ok(s4) = settings.cast::<ICoreWebView2Settings4>()
        {
            let _ = s4.SetIsPasswordAutosaveEnabled(save_passwords);
        }
    }
}

enum Outcome {
    Signed(Session),
    Closed,
}

/// Open the sign-in window and block until a validated session exists or the window is closed.
///
/// Must run on the main thread of a process that is not already inside another tao event loop.
/// `validate` is called (from the event-loop thread) with each candidate session and returns
/// false to reject it.
pub fn sign_in(
    host: &str,
    start_url: &Url,
    opts: &Options,
    validate: &mut dyn FnMut(&Session) -> bool,
) -> Result<Session, Failure> {
    std::fs::create_dir_all(&opts.profile_dir).map_err(|e| {
        Failure::Unavailable(format!(
            "cannot create the sign-in profile folder {}: {e}",
            opts.profile_dir.display()
        ))
    })?;
    if !display_available() {
        return Err(Failure::Unavailable("no display is available".into()));
    }

    let mut builder = EventLoopBuilder::new();
    #[cfg(windows)]
    {
        use tao::platform::windows::EventLoopBuilderExtWindows;
        builder.with_any_thread(true);
    }
    #[cfg(not(windows))]
    {
        use tao::platform::unix::EventLoopBuilderExtUnix;
        builder.with_any_thread(true);
    }
    let mut event_loop = builder.build();

    let window = WindowBuilder::new()
        .with_title("AmpSync - sign in to SharePoint")
        .with_inner_size(LogicalSize::new(1000.0, 800.0))
        .with_visible(!opts.start_hidden)
        .build(&event_loop)
        .map_err(|e| Failure::Unavailable(format!("window creation failed: {e}")))?;

    #[cfg(not(windows))]
    let mut web_context = wry::WebContext::new(Some(opts.profile_dir.as_path()));

    #[cfg(windows)]
    let builder = {
        use wry::WebViewBuilderExtWindows;
        let env = create_env(&opts.profile_dir, opts.use_windows_account)
            .map_err(|e| Failure::Unavailable(format!("WebView2 is not available: {e}")))?;
        WebViewBuilder::new().with_environment(env)
    };
    #[cfg(not(windows))]
    let builder = WebViewBuilder::new_with_web_context(&mut web_context);
    let builder = builder.with_url(start_url.as_str());

    #[cfg(windows)]
    let webview = builder
        .build(&window)
        .map_err(|e| Failure::Unavailable(format!("webview creation failed: {e}")))?;
    #[cfg(not(windows))]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window
            .default_vbox()
            .ok_or_else(|| Failure::Unavailable("no GTK container for the webview".into()))?;
        builder
            .build_gtk(vbox)
            .map_err(|e| Failure::Unavailable(format!("webview creation failed: {e}")))?
    };

    #[cfg(windows)]
    apply_password_setting(&webview, opts.save_passwords);
    #[cfg(not(windows))]
    let _ = opts.save_passwords; // WebKitGTK has no password manager of its own.

    let started = Instant::now();
    let mut shown = !opts.start_hidden;
    let mut rejected: HashSet<String> = opts.known_bad_fedauth.iter().cloned().collect();
    let mut outcome: Option<Outcome> = None;

    event_loop.run_return(|event, _, cf| {
        *cf = ControlFlow::WaitUntil(Instant::now() + POLL_EVERY);
        match event {
            Event::NewEvents(StartCause::ResumeTimeReached { .. } | StartCause::Init) => {
                let cookies = fetch_cookies(&webview, host);
                if has_auth_cookies(&cookies, host) {
                    let value = fedauth_value(&cookies, host).unwrap_or_default();
                    if !rejected.contains(&value) {
                        let session = Session {
                            host: host.to_string(),
                            cookies,
                            method: SignInMethod::Webview,
                            captured_at: OffsetDateTime::now_utc(),
                        };
                        if validate(&session) {
                            outcome = Some(Outcome::Signed(session));
                            *cf = ControlFlow::Exit;
                        } else {
                            rejected.insert(value);
                        }
                    }
                }
                if !shown && !matches!(*cf, ControlFlow::Exit) && started.elapsed() >= SILENT_PATIENCE {
                    window.set_visible(true);
                    window.set_focus();
                    shown = true;
                }
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                outcome = Some(Outcome::Closed);
                *cf = ControlFlow::Exit;
            }
            _ => {}
        }
        if matches!(*cf, ControlFlow::Exit) {
            window.set_visible(false);
        }
    });

    drop(webview);
    drop(window);
    // Keep pumping briefly so Windows actually destroys the window.
    let until = Instant::now() + PUMP_AFTER_CLOSE;
    event_loop.run_return(|_, _, cf| {
        *cf = if Instant::now() >= until {
            ControlFlow::Exit
        } else {
            ControlFlow::WaitUntil(until)
        };
    });

    match outcome {
        Some(Outcome::Signed(s)) => Ok(s),
        _ => Err(Failure::Closed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie(name: &str, domain: &str, value: &str) -> SessionCookie {
        SessionCookie {
            name: name.into(),
            value: value.into(),
            domain: domain.into(),
            path: "/".into(),
            expires: None,
        }
    }

    #[test]
    fn domain_rules() {
        assert!(domain_matches(".sharepoint.com", "contoso.sharepoint.com"));
        assert!(domain_matches("contoso.sharepoint.com", "contoso.sharepoint.com"));
        assert!(domain_matches("CONTOSO.sharepoint.com", "contoso.SharePoint.com"));
        assert!(!domain_matches("other.sharepoint.com", "contoso.sharepoint.com"));
        assert!(!domain_matches("tsoso.sharepoint.com", "contoso.sharepoint.com"));
        assert!(!domain_matches(
            "login.microsoftonline.com",
            "contoso.sharepoint.com"
        ));
    }

    #[test]
    fn needs_both_cookies_for_the_tenant() {
        let host = "contoso.sharepoint.com";
        let fed = cookie("FedAuth", host, "a");
        let rt = cookie("rtFa", ".sharepoint.com", "b");
        assert!(has_auth_cookies(&[fed.clone(), rt.clone()], host));
        assert!(!has_auth_cookies(std::slice::from_ref(&fed), host));
        assert!(!has_auth_cookies(std::slice::from_ref(&rt), host));
        assert!(!has_auth_cookies(
            &[cookie("FedAuth", "other.sharepoint.com", "a"), rt.clone()],
            host
        ));
        assert!(!has_auth_cookies(&[cookie("FedAuth", host, ""), rt], host));
        assert_eq!(fedauth_value(&[fed], host).as_deref(), Some("a"));
    }
}
