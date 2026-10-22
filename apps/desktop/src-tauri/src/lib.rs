//! AmpSync desktop app: Tauri commands over `ampsync-core`, a one-at-a-time queue, the webview
//! sign-in flow, tray, update check and contributor list.

mod commands;
mod contributors;
mod error;
mod paths;
mod queue;
mod signin;
mod startup;
mod tray;
mod update;

use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};

use ampsync_core::auth::{KeyringStore, SessionStore};
use ampsync_core::settings::Settings;
use tauri::{AppHandle, Emitter, Manager};

/// `owner/name` of the repository this build belongs to (empty when built without one, D12).
pub const REPO_SLUG: &str = env!("AMPSYNC_REPO_SLUG");

pub struct AppState {
    pub settings: RwLock<Settings>,
    pub store: Arc<dyn SessionStore>,
    pub signin: signin::SignInCoordinator,
    pub queue: queue::Queue,
    pub update: Mutex<Option<update::UpdateInfo>>,
    pub quitting: AtomicBool,
    statuses: Mutex<BTreeMap<String, &'static str>>,
}

impl AppState {
    fn new(settings: Settings) -> Self {
        AppState {
            settings: RwLock::new(settings),
            store: Arc::new(KeyringStore::new()),
            signin: signin::SignInCoordinator::new(),
            queue: queue::Queue::new(),
            update: Mutex::new(None),
            quitting: AtomicBool::new(false),
            statuses: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn settings(&self) -> Settings {
        self.settings.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn status_of(&self, host: &str) -> &'static str {
        self.statuses
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(host)
            .copied()
            .unwrap_or("unknown")
    }

    pub fn set_status(&self, app: &AppHandle, host: &str, status: &'static str) {
        self.statuses
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(host.to_string(), status);
        let _ = app.emit("sessions-changed", ());
    }

    pub fn forget_status(&self, app: &AppHandle, host: &str) {
        self.statuses
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(host);
        let _ = app.emit("sessions-changed", ());
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let settings = Settings::load().unwrap_or_else(|e| {
        tracing::warn!("settings could not be read, using defaults: {e}");
        Settings::default()
    });

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tray::show_main(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::new(settings))
        .invoke_handler(tauri::generate_handler![
            commands::resolve,
            commands::plan,
            commands::enqueue,
            commands::get_queue,
            commands::pause,
            commands::resume,
            commands::cancel,
            commands::retry,
            commands::clear_finished,
            commands::sessions,
            commands::check_sessions,
            commands::sign_in,
            commands::cancel_sign_in,
            commands::sign_out,
            commands::get_settings,
            commands::set_settings,
            commands::mark_tour_seen,
            commands::app_info,
            commands::contributors,
            commands::get_update,
            commands::open_external,
            commands::close_decision,
            commands::known_hosts,
        ])
        .on_window_event(tray::on_window_event)
        .setup(|app| {
            let handle = app.handle().clone();
            tray::setup(&handle)?;
            let state = app.state::<AppState>();
            state.queue.restore(&paths::queue_file());
            queue::spawn_runner(handle.clone());
            startup::spawn(handle.clone());
            update::spawn(handle);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running AmpSync");
}
