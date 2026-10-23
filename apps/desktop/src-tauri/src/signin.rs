//! Webview sign-in (D15, D16).
//!
//! The sign-in window uses a persistent browser profile that belongs to AmpSync alone. Completion
//! is detected by polling the webview cookie store from an async task, never from a WebView2
//! callback (that deadlocks). The same profile is used for silent refreshes: the SharePoint site
//! is loaded in an invisible window and the cookies are polled for a while.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ampsync_core::Error;
use ampsync_core::auth::{Session, SessionCookie, SignInMethod};
use ampsync_core::settings::Settings;
use tauri::{AppHandle, Emitter, Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use time::OffsetDateTime;

use crate::paths;

const SIGNIN_LABEL: &str = "signin";
const REFRESH_LABEL: &str = "signin-refresh";
const POLL: Duration = Duration::from_millis(750);
const SILENT_TIMEOUT: Duration = Duration::from_secs(20);

/// Result of a cheap authenticated request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Validity {
    Valid,
    /// SharePoint definitely rejects the session (401, or a redirect to the login page).
    Invalid,
    /// SharePoint gave no clear answer (offline, 403 on a restricted site, server error): do not
    /// treat the session as expired.
    Unknown,
}

/// The web (site) a URL belongs to: `/sites/X`, `/teams/X` or `/personal/X` for site and folder
/// URLs, the same for sharing links (`/:f:/s/X/...`, `/:f:/t/X/...`, `/:f:/g/personal/X/...`,
/// `/:f:/r/sites/X/...`), and the tenant root for anything else.
///
/// The session probe goes here rather than to the tenant root: guests and users without access
/// to the root site are refused there even though their session is perfectly good.
pub fn site_url_of(url: &Url) -> Url {
    let segs: Vec<&str> = url
        .path_segments()
        .map(|s| s.filter(|x| !x.is_empty()).collect())
        .unwrap_or_default();
    let is_web = |k: &str| {
        ["sites", "teams", "personal"]
            .iter()
            .any(|w| w.eq_ignore_ascii_case(k))
    };
    let path = match segs.as_slice() {
        [tok, "s", name, ..] if is_sharing_token(tok) => Some(format!("/sites/{name}")),
        [tok, "t", name, ..] if is_sharing_token(tok) => Some(format!("/teams/{name}")),
        [tok, "g" | "r", kind, name, ..] if is_sharing_token(tok) && is_web(kind) => {
            Some(format!("/{kind}/{name}"))
        }
        [kind, name, ..] if is_web(kind) => Some(format!("/{kind}/{name}")),
        _ => None,
    };
    let mut out = url.clone();
    out.set_query(None);
    out.set_fragment(None);
    out.set_path(path.as_deref().unwrap_or("/"));
    out
}

fn is_sharing_token(seg: &str) -> bool {
    let b = seg.as_bytes();
    b.len() == 3 && b[0] == b':' && b[2] == b':' && b[1].is_ascii_alphabetic()
}

/// A WebView2 environment handle. COM reference counting is thread-safe and the handle is only
/// ever *used* on the main thread, inside wry, so moving it between threads is sound.
#[cfg(windows)]
#[derive(Clone)]
struct SendEnv(webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Environment);
#[cfg(windows)]
unsafe impl Send for SendEnv {}
#[cfg(windows)]
unsafe impl Sync for SendEnv {}

pub struct SignInCoordinator {
    /// Serialises everything that may open a window.
    gate: tokio::sync::Mutex<()>,
    interactive_open: AtomicBool,
    #[cfg(windows)]
    env: Mutex<Option<SendEnv>>,
    #[cfg(not(windows))]
    _env: Mutex<()>,
}

impl Default for SignInCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl SignInCoordinator {
    pub fn new() -> Self {
        SignInCoordinator {
            gate: tokio::sync::Mutex::new(()),
            interactive_open: AtomicBool::new(false),
            #[cfg(windows)]
            env: Mutex::new(None),
            #[cfg(not(windows))]
            _env: Mutex::new(()),
        }
    }

    pub fn is_window_open(&self) -> bool {
        self.interactive_open.load(Ordering::SeqCst)
    }

    /// Close any sign-in window (used by Cancel).
    pub fn cancel(&self, app: &AppHandle) {
        for label in [SIGNIN_LABEL, REFRESH_LABEL] {
            if let Some(w) = app.get_webview_window(label) {
                let _ = w.destroy();
            }
        }
    }

    pub fn focus(&self, app: &AppHandle) {
        if let Some(w) = app.get_webview_window(SIGNIN_LABEL) {
            let _ = w.show();
            let _ = w.set_focus();
        }
    }

    /// Return a session that works, trying in order: the stored one, a silent refresh in the
    /// hidden window, then (if `interactive`) the visible sign-in window.
    ///
    /// `force` skips the stored session (used after the server said it expired).
    pub async fn obtain(
        &self,
        app: &AppHandle,
        host: &str,
        start_url: &Url,
        force: bool,
        interactive: bool,
    ) -> Result<Session, Error> {
        let began = OffsetDateTime::now_utc();
        let state = app.state::<crate::AppState>();
        let site = site_url_of(start_url);
        if !force
            && let Some(s) = state.store.load(host)?
            && validate(&s, &site).await != Validity::Invalid
        {
            return Ok(s);
        }
        let _guard = self.gate.lock().await;
        // Someone else may have signed in while we waited for the gate.
        if let Some(s) = state.store.load(host)?
            && s.captured_at >= began
            && validate(&s, &site).await != Validity::Invalid
        {
            return Ok(s);
        }
        let settings = state.settings();
        state.set_status(app, host, "checking");
        if let Ok((s, verdict)) = self
            .run_window(app, host, start_url, &site, &settings, false)
            .await
        {
            state.store.save(&s)?;
            state.set_status(app, host, status_name(verdict));
            return Ok(s);
        }
        if !interactive {
            state.set_status(app, host, "expired");
            return Err(Error::AuthExpired { host: host.into() });
        }
        self.interactive_open.store(true, Ordering::SeqCst);
        crate::notify::send(app, &crate::notify::Trigger::SignInNeeded { host: host.into() });
        let _ = app.emit(
            "signin-state",
            serde_json::json!({ "host": host, "state": "window_open" }),
        );
        let res = self
            .run_window(app, host, start_url, &site, &settings, true)
            .await;
        self.interactive_open.store(false, Ordering::SeqCst);
        let _ = app.emit(
            "signin-state",
            serde_json::json!({ "host": host, "state": "closed" }),
        );
        match res {
            Ok((s, verdict)) => {
                state.store.save(&s)?;
                state.set_status(app, host, status_name(verdict));
                Ok(s)
            }
            Err(e) => {
                state.set_status(app, host, "expired");
                Err(e)
            }
        }
    }

    /// Open a webview on `start_url`, poll its cookie store, return the session once SharePoint
    /// does not reject it. Only a definite rejection keeps the window waiting; an unclear answer
    /// (`Unknown`) accepts the session, because a persistent profile may never produce another.
    async fn run_window(
        &self,
        app: &AppHandle,
        host: &str,
        start_url: &Url,
        site: &Url,
        settings: &Settings,
        visible: bool,
    ) -> Result<(Session, Validity), Error> {
        let label = if visible { SIGNIN_LABEL } else { REFRESH_LABEL };
        if let Some(old) = app.get_webview_window(label) {
            let _ = old.destroy();
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        let profile = paths::webview_profile();
        std::fs::create_dir_all(&profile).map_err(|e| Error::io(&profile, e))?;

        let mut builder = WebviewWindowBuilder::new(app, label, WebviewUrl::External(start_url.clone()))
            .title("Sign in to SharePoint - AmpSync")
            .inner_size(1000.0, 780.0)
            .center()
            .visible(visible)
            .focused(visible)
            .data_directory(profile.clone());
        if !visible {
            builder = builder.skip_taskbar(true);
        }
        #[cfg(windows)]
        {
            let env = self
                .windows_env(app, &profile, settings.use_windows_account)
                .await?;
            builder = builder.with_environment(env);
        }
        let window = builder
            .build()
            .map_err(|e| Error::State(format!("cannot open the sign-in window: {e}")))?;
        apply_password_setting(&window, settings.save_passwords);

        let cookie_url =
            Url::parse(&format!("https://{host}/")).map_err(|e| Error::InvalidLink(e.to_string()))?;
        let started = Instant::now();
        let mut last_checked: Option<(String, String)> = None;
        let result = loop {
            tokio::time::sleep(POLL).await;
            if app.get_webview_window(label).is_none() {
                break Err(Error::Cancelled);
            }
            if !visible && started.elapsed() > SILENT_TIMEOUT {
                break Err(Error::AuthExpired { host: host.into() });
            }
            let w = window.clone();
            let url = cookie_url.clone();
            let cookies = match tokio::task::spawn_blocking(move || w.cookies_for_url(url)).await {
                Ok(Ok(c)) => c,
                _ => continue,
            };
            let find = |name: &str| {
                cookies
                    .iter()
                    .find(|c| c.name() == name)
                    .map(|c| c.value().to_string())
            };
            let (Some(fed), Some(rt)) = (find("FedAuth"), find("rtFa")) else {
                continue;
            };
            let key = (fed, rt);
            if last_checked.as_ref() == Some(&key) {
                continue;
            }
            let session = Session {
                host: host.to_string(),
                cookies: cookies.iter().map(|c| convert(c, host)).collect(),
                method: SignInMethod::Webview,
                captured_at: OffsetDateTime::now_utc(),
            };
            // A cookie left over from an earlier, expired sign-in is rejected by the probe and
            // does not end the wait; the same cookie is not probed again until it changes.
            let verdict = validate(&session, site).await;
            last_checked = Some(key);
            if verdict != Validity::Invalid {
                break Ok((session, verdict));
            }
        };
        let _ = window.destroy();
        tokio::time::sleep(Duration::from_millis(200)).await;
        result
    }

    /// Sign-out: remove this host's cookies from the profile; clear the whole profile when no
    /// other host remains.
    pub async fn clear_profile(&self, app: &AppHandle, host: &str, last_host: bool) -> Result<(), Error> {
        let _guard = self.gate.lock().await;
        let profile = paths::webview_profile();
        if !profile.exists() {
            return Ok(());
        }
        let builder = WebviewWindowBuilder::new(
            app,
            "signin-clear",
            WebviewUrl::External(Url::parse("about:blank").expect("static url")),
        )
        .visible(false)
        .skip_taskbar(true)
        .data_directory(profile.clone());
        #[cfg(windows)]
        let builder = {
            let sso = app.state::<crate::AppState>().settings().use_windows_account;
            let env = self.windows_env(app, &profile, sso).await?;
            builder.with_environment(env)
        };
        let window = builder
            .build()
            .map_err(|e| Error::State(format!("cannot open the profile: {e}")))?;
        let w = window.clone();
        let host = host.to_string();
        let r = tokio::task::spawn_blocking(move || {
            if last_host {
                w.clear_all_browsing_data()
            } else {
                for c in w.cookies()? {
                    let d = c
                        .domain()
                        .unwrap_or("")
                        .trim_start_matches('.')
                        .to_ascii_lowercase();
                    // Only the tenant's own cookies; shared parent-domain cookies stay.
                    if d == host {
                        let _ = w.delete_cookie(c);
                    }
                }
                Ok(())
            }
        })
        .await;
        let _ = window.destroy();
        match r {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(Error::State(e.to_string())),
            Err(e) => Err(Error::State(e.to_string())),
        }
    }

    #[cfg(windows)]
    async fn windows_env(
        &self,
        app: &AppHandle,
        dir: &std::path::Path,
        sso: bool,
    ) -> Result<webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Environment, Error> {
        if let Some(SendEnv(env)) = self.env.lock().unwrap_or_else(|p| p.into_inner()).clone() {
            // WebView2 allows one set of environment options per profile per process, so a
            // changed "use Windows account" setting takes effect after AmpSync restarts.
            return Ok(env);
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        let dir = dir.to_path_buf();
        app.run_on_main_thread(move || {
            let r = create_env(&dir, sso).map(SendEnv).map_err(|e| e.to_string());
            let _ = tx.send(r);
        })
        .map_err(|e| Error::State(e.to_string()))?;
        let env = rx
            .await
            .map_err(|_| Error::State("webview environment was not created".into()))?
            .map_err(|e| Error::State(format!("cannot start the sign-in browser: {e}")))?;
        *self.env.lock().unwrap_or_else(|p| p.into_inner()) = Some(env.clone());
        Ok(env.0)
    }
}

fn convert(c: &tauri::webview::cookie::Cookie<'_>, host: &str) -> SessionCookie {
    SessionCookie {
        name: c.name().to_string(),
        value: c.value().to_string(),
        domain: c
            .domain()
            .map(|d| d.to_string())
            .unwrap_or_else(|| host.to_string()),
        path: c.path().unwrap_or("/").to_string(),
        expires: c.expires_datetime(),
    }
}

/// The status string the frontend shows for a probe result that was accepted.
fn status_name(v: Validity) -> &'static str {
    match v {
        Validity::Valid => "active",
        _ => "unknown",
    }
}

/// Probe `session` against `site` (core's `check_session`): only a definite rejection is
/// `Invalid`.
pub async fn validate(session: &Session, site: &Url) -> Validity {
    let now = OffsetDateTime::now_utc();
    if let Some(fed) = session.cookies.iter().find(|c| c.name == "FedAuth")
        && fed.expires.is_some_and(|e| e <= now)
    {
        return Validity::Invalid;
    }
    match ampsync_core::check_session(session, site).await {
        ampsync_core::SessionStatus::Valid => Validity::Valid,
        ampsync_core::SessionStatus::Expired => Validity::Invalid,
        ampsync_core::SessionStatus::Unknown => Validity::Unknown,
    }
}

#[cfg(windows)]
fn create_env(
    data_dir: &std::path::Path,
    sso: bool,
) -> windows_core::Result<webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Environment> {
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
        // The same defaults wry uses, plus the single sign-on switch Tauri does not expose.
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
        )?;
    }
    match wait_with_pump(rx) {
        Ok(r) => r,
        Err(e) => Err(windows_core::Error::new(
            windows_core::HRESULT(0x8000_4005u32 as i32),
            format!("waiting for WebView2 failed: {e:?}"),
        )),
    }
}

/// Password saving in the sign-in window follows the Settings toggle (default off).
#[cfg(windows)]
fn apply_password_setting(window: &WebviewWindow, save: bool) {
    let _ = window.with_webview(move |wv| unsafe {
        use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings4;
        use windows_core::Interface;
        if let Ok(core) = wv.controller().CoreWebView2()
            && let Ok(settings) = core.Settings()
            && let Ok(s4) = settings.cast::<ICoreWebView2Settings4>()
        {
            let _ = s4.SetIsPasswordAutosaveEnabled(save);
            let _ = s4.SetIsGeneralAutofillEnabled(save);
        }
    });
}

#[cfg(not(windows))]
fn apply_password_setting(_window: &WebviewWindow, _save: bool) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(u: &str) -> String {
        site_url_of(&Url::parse(u).unwrap()).to_string()
    }

    #[test]
    fn site_of_folder_and_site_urls() {
        assert_eq!(
            site("https://contoso.sharepoint.com/sites/Team"),
            "https://contoso.sharepoint.com/sites/Team"
        );
        assert_eq!(
            site("https://contoso-my.sharepoint.com/personal/jo_contoso_com/Documents/x?id=1"),
            "https://contoso-my.sharepoint.com/personal/jo_contoso_com"
        );
    }

    #[test]
    fn site_of_sharing_links() {
        assert_eq!(
            site("https://contoso.sharepoint.com/:f:/s/Team/Eabc?e=x"),
            "https://contoso.sharepoint.com/sites/Team"
        );
        assert_eq!(
            site("https://contoso.sharepoint.com/:f:/t/Crew/Eabc"),
            "https://contoso.sharepoint.com/teams/Crew"
        );
        assert_eq!(
            site("https://contoso-my.sharepoint.com/:f:/g/personal/jo_contoso_com/Eabc"),
            "https://contoso-my.sharepoint.com/personal/jo_contoso_com"
        );
        assert_eq!(
            site("https://contoso.sharepoint.com/:f:/r/sites/Team/Shared%20Documents/x"),
            "https://contoso.sharepoint.com/sites/Team"
        );
    }

    #[test]
    fn site_falls_back_to_the_root() {
        assert_eq!(
            site("https://contoso.sharepoint.com/"),
            "https://contoso.sharepoint.com/"
        );
        assert_eq!(
            site("https://contoso.sharepoint.com/:f:/x"),
            "https://contoso.sharepoint.com/"
        );
    }

    #[test]
    fn accepted_probe_results_map_to_status_names() {
        assert_eq!(status_name(Validity::Valid), "active");
        assert_eq!(status_name(Validity::Unknown), "unknown");
    }
}
