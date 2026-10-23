//! System tray icon and what the window's close button does (setting `close_behavior`).

use std::sync::atomic::Ordering;

use ampsync_core::settings::CloseBehavior;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WindowEvent};

use crate::AppState;

pub(crate) const TRAY_ID: &str = "main";

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show AmpSync", true, None::<&str>)?;
    let clip = MenuItem::with_id(
        app,
        "clipboard",
        "Download link from clipboard",
        true,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", "Quit (pause downloads)", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &clip, &quit])?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("AmpSync")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_main(app),
            "clipboard" => link_from_clipboard(app),
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
    // Any show counts as "the window has appeared" for the startup fallback timer.
    crate::splash::mark_shown();
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// The tray's "Download link from clipboard": the one place the app reads the clipboard itself,
/// and only because the user chose that menu item. The frontend parses the text and says so
/// inline when it is not a SharePoint folder link.
fn link_from_clipboard(app: &AppHandle) {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    // Anything that is not text (an image, an empty clipboard) reads as no text.
    let text = app.clipboard().read_text().unwrap_or_default();
    show_main(app);
    let _ = app.emit(
        "incoming-link",
        serde_json::json!({ "text": text, "source": "tray" }),
    );
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
    if window.label() != "main" {
        return;
    }
    if let WindowEvent::Focused(true) = event {
        crate::progress::on_focus(window.app_handle());
        return;
    }
    let WindowEvent::CloseRequested { api, .. } = event else {
        return;
    };
    let app = window.app_handle();
    let state = app.state::<AppState>();
    if state.quitting.load(Ordering::SeqCst) {
        return;
    }
    let behavior = state.settings().close_behavior;
    let active = state.queue.has_active();
    if !active && behavior == CloseBehavior::Pause {
        // Nothing running and the user chose "pause": just quit.
        state.quitting.store(true, Ordering::SeqCst);
        return;
    }
    // Until the user picks a remembered choice (behavior Ask), every close asks, idle or not.
    // A remembered "pause and quit" still warns while a download is running.
    api.prevent_close();
    match behavior {
        CloseBehavior::Ask | CloseBehavior::Pause => {
            let _ = app.emit("close-requested", state.queue.has_active());
            let _ = window.show();
            let _ = window.set_focus();
        }
        other => apply_close(app, other),
    }
}
