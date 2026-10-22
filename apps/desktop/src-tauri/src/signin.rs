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

pub const USER_AGENT: &str = concat!("NONISV|AmpSync|AmpSync/", env!("CARGO_PKG_VERSION"));

/// Result of a cheap authenticated request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Validity {
    Valid,
    Invalid,
    /// The check itself failed (offline, server error): do not treat the session as expired.
    Unknown,
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
        if !force
            && let Some(s) = state.store.load(host)?
            && validate(&s).await != Validity::Invalid
        {
            return Ok(s);
        }
        let _guard = self.gate.lock().await;
        // Someone else may have signed in while we waited for the gate.
        if let Some(s) = state.store.load(host)?
            && s.captured_at >= began
            && validate(&s).await != Validity::Invalid
        {
            return Ok(s);
        }
        let settings = state.settings();
        let previous = state.store.load(host)?;
        state.set_status(app, host, "checking");
        if let Ok(s) = self
            .run_window(app, host, start_url, &settings, previous.as_ref(), false)
            .await
        {
            state.store.save(&s)?;
            state.set_status(app, host, "active");
            return Ok(s);
        }
        if !interactive {
            state.set_status(app, host, "expired");
            return Err(Error::AuthExpired { host: host.into() });
        }
        self.interactive_open.store(true, Ordering::SeqCst);
        let _ = app.emit(
            "signin-state",
            serde_json::json!({ "host": host, "state": "window_open" }),
        );
        let res = self
            .run_window(app, host, start_url, &settings, previous.as_ref(), true)
            .await;
        self.interactive_open.store(false, Ordering::SeqCst);
        let _ = app.emit(
            "signin-state",
            serde_json::json!({ "host": host, "state": "closed" }),
        );
        match res {
            Ok(s) => {
                state.store.save(&s)?;
                state.set_status(app, host, "active");
                Ok(s)
            }
            Err(e) => {
                state.set_status(app, host, "expired");
                Err(e)
            }
        }
    }

    /// Open a webview on `start_url`, poll its cookie store, return the session once it works.
    async fn run_window(
        &self,
        app: &AppHandle,
        host: &str,
        start_url: &Url,
        settings: &Settings,
        previous: Option<&Session>,
        visible: bool,
    ) -> Result<Session, Error> {
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
            // A cookie left over from an earlier, expired sign-in must not end the wait.
            let same_as_before = previous
                .map(|p| p.cookies.iter().any(|c| c.name == "FedAuth" && c.value == key.0))
                .unwrap_or(false);
            let verdict = validate(&session).await;
            last_checked = Some(key);
            if verdict == Validity::Valid || (verdict == Validity::Unknown && !same_as_before) {
                break Ok(session);
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

/// One cheap authenticated request: `POST /_api/contextinfo` answers 200 to any signed-in user.
pub async fn validate(session: &Session) -> Validity {
    let now = OffsetDateTime::now_utc();
    if let Some(fed) = session.cookies.iter().find(|c| c.name == "FedAuth")
        && fed.expires.is_some_and(|e| e <= now)
    {
        return Validity::Invalid;
    }
    let header = session.cookie_header(now);
    if header.is_empty() {
        return Validity::Invalid;
    }
    let Ok(client) = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(15))
        .build()
    else {
        return Validity::Unknown;
    };
    let url = format!("https://{}/_api/contextinfo", session.host);
    let resp = client
        .post(url)
        .header("Cookie", header)
        .header("Accept", "application/json;odata=nometadata")
        .header("Content-Length", "0")
        .send()
        .await;
    match resp {
        Ok(r) => {
            let s = r.status();
            if s.is_success() {
                Validity::Valid
            } else if s.as_u16() == 401 || s.as_u16() == 403 || s.is_redirection() {
                Validity::Invalid
            } else {
                Validity::Unknown
            }
        }
        Err(_) => Validity::Unknown,
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
