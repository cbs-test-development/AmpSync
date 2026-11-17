//! Launch-time session check (D15).
//!
//! For every saved host: one cheap authenticated request. If the session is gone, try a silent
//! refresh in a hidden window with AmpSync's own profile. Only when that fails and a queued job
//! needs the host does the sign-in window open, and it opens before any job starts.
//!
//! With the browser link on, the extension is registered and unpacked first, and the user's own
//! browser is asked before the hidden window (`SignInCoordinator::obtain` does the asking).

use std::sync::atomic::{AtomicBool, Ordering};

use ampsync_core::Error;
use tauri::{AppHandle, Manager, Url};

use crate::AppState;
use crate::signin::{Validity, site_url_of, validate};

/// How long the launch-time check waits for a browser extension to attach before it falls back.
const LINK_WAIT: std::time::Duration = std::time::Duration::from_secs(8);

/// The launch-time check has answered for every saved sign-in (the startup splash waits for it).
static CHECKED: AtomicBool = AtomicBool::new(false);

pub fn checked() -> bool {
    CHECKED.load(Ordering::SeqCst)
}

pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        if app.state::<AppState>().settings().browser_link {
            // Keeps the registration pointing at this exe (a portable exe may have moved) and the
            // unpacked extension at this version.
            match tauri::async_runtime::spawn_blocking(crate::link_setup::prepare).await {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::warn!("browser link set-up failed: {e}"),
                Err(e) => tracing::warn!("browser link set-up failed: {e}"),
            }
        }
        check_all(&app, true).await;
        CHECKED.store(true, Ordering::SeqCst);
        app.state::<AppState>().queue.open_gate();
    });
}

/// `allow_window`: may open the visible sign-in window for hosts that have queued work.
pub async fn check_all(app: &AppHandle, allow_window: bool) {
    let state = app.state::<AppState>();
    let hosts = state.store.hosts().unwrap_or_default();
    let pending = state.queue.pending_hosts();
    let mut waited_for_link = false;
    for host in hosts {
        let Ok(Some(session)) = state.store.load(&host) else {
            continue;
        };
        state.set_status(app, &host, "checking");
        // Probe the site of a queued link where one is known; the tenant root only as a last
        // resort (guests and restricted users are refused there even with a good session).
        let Some(url) = state
            .queue
            .site_hint(&host)
            .or_else(|| Url::parse(&format!("https://{host}/")).ok())
        else {
            continue;
        };
        match validate(&session, &site_url_of(&url)).await {
            Validity::Valid => {
                state.set_status(app, &host, "active");
                continue;
            }
            Validity::Unknown => {
                // Offline, a server hiccup or a site that refuses the probe: keep the session and
                // do not nag; a job will find out itself if it really is gone.
                state.set_status(app, &host, "unknown");
                continue;
            }
            Validity::Invalid => {}
        }
        // The relay retries the app every 5 s, so an open browser attaches within moments of launch.
        if !waited_for_link && state.settings().browser_link && !state.link.is_connected() {
            waited_for_link = true;
            state.link.wait_connected(LINK_WAIT).await;
        }
        let interactive = allow_window && pending.contains(&host);
        match state.signin.obtain(app, &host, &url, true, interactive).await {
            Ok(_) => {}
            Err(Error::AuthExpired { .. } | Error::Cancelled) => state.set_status(app, &host, "expired"),
            Err(e) => {
                tracing::warn!("session check for a saved host failed: {e}");
                state.set_status(app, &host, "expired");
            }
        }
    }
}
