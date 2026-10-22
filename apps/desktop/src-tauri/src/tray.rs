//! System tray icon and what the window's close button does (setting `close_behavior`).

use std::sync::atomic::Ordering;

use ampsync_core::settings::CloseBehavior;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WindowEvent};

use crate::AppState;

const TRAY_ID: &str = "main";

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show AmpSync", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit (pause downloads)", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("AmpSync")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_main(app),
            "quit" => quit_now(app, true),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

pub fn update_tooltip(app: &AppHandle, fraction: Option<f64>) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let text = match fraction {
            Some(f) => format!("AmpSync - {:.0}%", (f * 100.0).clamp(0.0, 100.0)),
            None => "AmpSync".to_string(),
        };
        let _ = tray.set_tooltip(Some(text));
    }
}

pub fn quit_now(app: &AppHandle, mark_paused: bool) {
    let state = app.state::<AppState>();
    state.quitting.store(true, Ordering::SeqCst);
    state.queue.pause_for_exit(app, mark_paused);
    app.exit(0);
}

/// Carry out a chosen close behaviour.
pub fn apply_close(app: &AppHandle, behavior: CloseBehavior) {
    match behavior {
        CloseBehavior::Tray => {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.hide();
            }
        }
        CloseBehavior::Pause | CloseBehavior::Ask => quit_now(app, true),
    }
}

/// Called for every window event of the main window.
pub fn on_window_event(window: &tauri::Window, event: &WindowEvent) {
    let WindowEvent::CloseRequested { api, .. } = event else {
        return;
    };
    if window.label() != "main" {
        return;
    }
    let app = window.app_handle();
    let state = app.state::<AppState>();
    if state.quitting.load(Ordering::SeqCst) {
        return;
    }
    if !state.queue.has_active() {
        // Nothing running: just quit.
        state.quitting.store(true, Ordering::SeqCst);
        return;
    }
    api.prevent_close();
    match state.settings().close_behavior {
        CloseBehavior::Ask => {
            let _ = app.emit("close-requested", ());
            let _ = window.show();
            let _ = window.set_focus();
        }
        other => apply_close(app, other),
    }
}
