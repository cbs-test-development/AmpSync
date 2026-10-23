//! Startup splash support (F7, D25): what the splash needs to know, and making sure the main
//! window always appears.
//!
//! The main window is created hidden (`visible: false`). The page's splash shows itself, then asks
//! for the window with [`show_window`] once its first frame is ready, so there is no white flash.
//! If the page never asks (a script failure, a broken bundle), a timer shows the window anyway.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ampsync_core::settings::SplashMode;
use serde::Serialize;
use tauri::{AppHandle, State};

use crate::AppState;
use crate::contributors::{self, Credit};
use crate::queue::ItemStatus;

/// The window is shown by this time even if the page never asked for it.
const FALLBACK: Duration = Duration::from_secs(5);

static SHOWN: AtomicBool = AtomicBool::new(false);

#[derive(Serialize)]
pub struct SplashInfo {
    pub mode: SplashMode,
    pub version: &'static str,
    /// Saved sign-ins being checked.
    pub sites: usize,
    /// Downloads waiting to start, and paused ones.
    pub queued: usize,
    pub paused: usize,
    /// The launch-time sign-in check has answered.
    pub checked: bool,
    pub credit: Option<Credit>,
}

#[tauri::command(rename_all = "snake_case")]
pub fn splash_info(state: State<'_, AppState>) -> SplashInfo {
    let items = state.queue.snapshot();
    let count = |s: ItemStatus| items.iter().filter(|i| i.status == s).count();
    SplashInfo {
        mode: state.settings().splash,
        version: env!("CARGO_PKG_VERSION"),
        sites: state.store.hosts().map_or(0, |h| h.len()),
        queued: count(ItemStatus::Queued),
        paused: count(ItemStatus::Paused),
        checked: crate::startup::checked(),
        credit: contributors::credit(&contributors::load()),
    }
}

/// Show the main window the first time this is called; later calls do nothing, so a window the
/// user has hidden to the tray stays hidden.
fn show_once(app: &AppHandle) {
    if SHOWN.swap(true, Ordering::SeqCst) {
        return;
    }
    crate::tray::show_main(app);
}

#[tauri::command(rename_all = "snake_case")]
pub fn show_window(app: AppHandle) {
    show_once(&app);
}

/// Start the fallback timer. Call once from `setup`.
pub fn spawn_fallback(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FALLBACK).await;
        show_once(&app);
    });
}

/// A second launch (the single-instance callback) shows the window itself, which counts.
pub fn mark_shown() {
    SHOWN.store(true, Ordering::SeqCst);
}
