//! `--auth webview`: native sign-in window (wry + tao), then read cookies from the webview's
//! cookie store (HttpOnly included).
//!
//! Threading note (Windows / WebView2): `WebView::cookies*` blocks while pumping messages and
//! must NOT be called from inside a WebView2 callback (e.g. the page-load handler) or it
//! deadlocks. We therefore only post a `UserEvent` from the callback and read the cookies from the
//! tao event-loop handler, which is not a WebView2 callback.

use crate::auth::{domain_matches, has_auth_cookie, AuthResult, Captured};
use anyhow::{anyhow, Result};
use std::time::{Duration, Instant};
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::run_return::EventLoopExtRunReturn;
use tao::window::WindowBuilder;
use url::Url;
use wry::{PageLoadEvent, WebView, WebViewBuilder};

#[derive(Debug, Clone)]
enum UserEvent {
    PageLoaded(String),
    EnterPressed,
}

pub fn is_sharepoint_host(h: &str) -> bool {
    let h = h.to_ascii_lowercase();
    [".sharepoint.com", ".sharepoint.us", ".sharepoint.cn", ".sharepoint.de", ".sharepoint-mil.us"].iter().any(|s| h.ends_with(s))
}

fn conv(c: &wry::cookie::Cookie<'_>) -> Captured {
    let (expires_unix, session) = match c.expires() {
        Some(wry::cookie::Expiration::DateTime(dt)) => (Some(dt.unix_timestamp()), Some(false)),
        Some(wry::cookie::Expiration::Session) | None => (None, Some(true)),
    };
    Captured {
        name: c.name().to_string(),
        value: c.value().to_string(),
        domain: c.domain().map(|d| d.to_string()),
        path: c.path().map(|p| p.to_string()),
        secure: c.secure(),
        http_only: c.http_only(),
        expires_unix,
        session,
    }
}

fn fetch_cookies(wv: &WebView, host: &str) -> Vec<Captured> {
    let mut out: Vec<Captured> = Vec::new();
    if let Ok(list) = wv.cookies_for_url(&format!("https://{host}/")) {
        out.extend(list.iter().map(conv));
    }
    if !has_auth_cookie(&out) {
        if let Ok(all) = wv.cookies() {
            for c in all.iter() {
                let cc = conv(c);
                let applies = cc.domain.as_deref().map(|d| domain_matches(d, host)).unwrap_or(false);
                let dup = out.iter().any(|o| o.name == cc.name && o.domain == cc.domain && o.path == cc.path);
                if applies && !dup {
                    out.push(cc);
                }
            }
        }
    }
    out
}

#[cfg(windows)]
fn create_env(
    data_dir: &std::path::Path,
    sso: bool,
) -> Result<webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Environment> {
    use webview2_com::Microsoft::Web::WebView2::Win32::{CreateCoreWebView2EnvironmentWithOptions, ICoreWebView2Environment, ICoreWebView2EnvironmentOptions};
    use webview2_com::{wait_with_pump, CoreWebView2EnvironmentOptions, CreateCoreWebView2EnvironmentCompletedHandler};
    use windows_core::{HSTRING, PCWSTR};

    let (tx, rx) = std::sync::mpsc::channel::<windows_core::Result<ICoreWebView2Environment>>();
    let options = CoreWebView2EnvironmentOptions::default();
    unsafe {
        // Same defaults wry uses, plus the SSO switch wry does not expose.
        options.set_additional_browser_arguments("--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection".to_string());
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
                tx.send(r).map_err(|_| windows_core::Error::from(windows_core::HRESULT(0x8000_ffffu32 as i32)))
            })),
        )?;
    }
    Ok(wait_with_pump(rx)??)
}

pub fn capture(url: &str, host: &str, sso: bool) -> Result<AuthResult> {
    let mut notes = Vec::new();
    let data_dir = tempfile::Builder::new().prefix("ampsync-probe-wv-").tempdir()?;

    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let window = WindowBuilder::new()
        .with_title("AmpSync probe - sign in to SharePoint")
        .with_inner_size(LogicalSize::new(1100.0, 820.0))
        .build(&event_loop)
        .map_err(|e| anyhow!("window creation failed: {e}"))?;

    let proxy_pl = proxy.clone();
    let builder = WebViewBuilder::new().with_url(url).with_on_page_load_handler(move |ev, u| {
        if matches!(ev, PageLoadEvent::Finished) {
            let _ = proxy_pl.send_event(UserEvent::PageLoaded(u));
        }
    });

    #[cfg(windows)]
    let (builder, sso_status) = {
        use wry::WebViewBuilderExtWindows;
        let env = create_env(data_dir.path(), sso)?;
        (
            builder.with_environment(env),
            if sso {
                "enabled (AllowSingleSignOnUsingOSPrimaryAccount=true via custom WebView2 environment created with webview2-com; wry 0.57 itself does not expose it)".to_string()
            } else {
                "disabled by --no-sso".to_string()
            },
        )
    };
    #[cfg(not(windows))]
    let (builder, sso_status) = {
        let _ = sso;
        (builder.with_incognito(true), "not available (Windows/WebView2 only)".to_string())
    };

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let webview = builder.build(&window).map_err(|e| anyhow!("webview creation failed: {e}"))?;
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window.default_vbox().ok_or_else(|| anyhow!("no default vbox"))?;
        builder.build_gtk(vbox).map_err(|e| anyhow!("webview creation failed: {e}"))?
    };

    println!("A sign-in window has opened. Sign in (MFA etc.) until the SharePoint page is visible.");
    println!("The probe continues automatically once SharePoint cookies are present; otherwise press Enter here.");

    let mut collected: Option<Vec<Captured>> = None;
    let mut on_tenant = false;
    let host_s = host.to_string();

    event_loop.run_return(|event, _, cf| {
        // Short tick so a pressed Enter (polled below) is noticed quickly.
        *cf = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(400));
        if matches!(event, Event::NewEvents(_)) && crate::input::try_line().is_some() {
            let _ = proxy.send_event(UserEvent::EnterPressed);
        }
        let finish = |force: bool, collected: &mut Option<Vec<Captured>>| -> bool {
            let c = fetch_cookies(&webview, &host_s);
            if force || has_auth_cookie(&c) {
                *collected = Some(c);
                true
            } else {
                false
            }
        };
        match event {
            Event::UserEvent(UserEvent::PageLoaded(u)) => {
                let h = Url::parse(&u).ok().and_then(|x| x.host_str().map(|s| s.to_ascii_lowercase()));
                if let Some(h) = h {
                    if h == host_s || is_sharepoint_host(&h) {
                        on_tenant = true;
                        if finish(false, &mut collected) {
                            *cf = ControlFlow::Exit;
                        }
                    }
                }
            }
            Event::UserEvent(UserEvent::EnterPressed) => {
                finish(true, &mut collected);
                *cf = ControlFlow::Exit;
            }
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) if on_tenant && collected.is_none() => {
                if finish(false, &mut collected) {
                    *cf = ControlFlow::Exit;
                }
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                if collected.is_none() {
                    finish(true, &mut collected);
                }
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
    // Keep pumping briefly so Windows actually destroys the window. Without this the hidden
    // (or still visible) window stops processing messages and shows as "Not responding".
    let until = Instant::now() + Duration::from_millis(500);
    event_loop.run_return(|_, _, cf| {
        *cf = if Instant::now() >= until { ControlFlow::Exit } else { ControlFlow::WaitUntil(until) };
    });
    // WebView2 child processes may keep the temp profile locked for a moment.
    let path = data_dir.path().to_path_buf();
    let mut removed = false;
    for _ in 0..10 {
        if std::fs::remove_dir_all(&path).is_ok() || !path.exists() {
            removed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    if !removed {
        notes.push("temporary webview profile could not be deleted (still locked); delete the ampsync-probe-wv-* folder in your temp directory".into());
    }
    std::mem::forget(data_dir); // already deleted (or reported); avoid a second delete attempt

    Ok(AuthResult { cookies: collected.unwrap_or_default(), notes, sso: sso_status })
}
