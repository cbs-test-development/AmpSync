//! Finish notifications (F5, D23): opt-in, and only while no AmpSync window has the focus.
//!
//! Three things are worth interrupting someone for: a download finished, a download stopped with
//! an error, and a sign-in that needs the person. Everything else (pause, cancel, throttling) is
//! never announced. [`decide`] holds the rules as a pure function; [`send`] gathers the facts
//! and shows the notification.

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

use crate::AppState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    Finished {
        title: String,
        files: u64,
        failed: u64,
    },
    Failed {
        title: String,
    },
    SignInNeeded {
        host: String,
    },
    /// Stopped by the user: they know.
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Job,
    SignIn,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Job => "job",
            Kind::SignIn => "sign_in",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub title: String,
    pub body: String,
    pub kind: Kind,
}

/// `1234567` becomes `1,234,567`.
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn files(n: u64) -> String {
    format!("{} {}", grouped(n), if n == 1 { "file" } else { "files" })
}

/// Whether to notify, and with what. `opt_in` is `Settings.notify_on_finish`: only an explicit
/// yes counts, an unanswered question (`None`) is a no.
pub fn decide(window_focused: bool, opt_in: Option<bool>, trigger: &Trigger) -> Option<Message> {
    if window_focused || opt_in != Some(true) {
        return None;
    }
    match trigger {
        Trigger::Finished {
            title,
            files: n,
            failed,
        } if *failed == 0 => Some(Message {
            title: "Download finished".into(),
            body: format!("{title} is ready, {}", files(*n)),
            kind: Kind::Job,
        }),
        Trigger::Finished { title, failed, .. } => Some(Message {
            title: "Download finished".into(),
            body: format!("{title} finished, but {} could not be saved", files(*failed)),
            kind: Kind::Job,
        }),
        Trigger::Failed { title } => Some(Message {
            title: "Download stopped".into(),
            body: format!("{title} stopped with an error. Open AmpSync to see why."),
            kind: Kind::Job,
        }),
        Trigger::SignInNeeded { host } => Some(Message {
            title: "Sign-in needed".into(),
            body: format!("Sign in to {host} to keep your downloads going."),
            kind: Kind::SignIn,
        }),
        Trigger::Cancelled => None,
    }
}

/// Any of AmpSync's windows (main or sign-in) has the keyboard focus.
fn app_focused(app: &AppHandle) -> bool {
    app.webview_windows()
        .values()
        .any(|w| w.is_focused().unwrap_or(false))
}

pub fn send(app: &AppHandle, trigger: &Trigger) {
    let opt_in = app.state::<AppState>().settings().notify_on_finish;
    let Some(msg) = decide(app_focused(app), opt_in, trigger) else {
        return;
    };
    let shown = app
        .notification()
        .builder()
        .title(msg.title)
        .body(msg.body)
        .extra("kind", msg.kind.as_str())
        .show();
    if let Err(e) = shown {
        tracing::warn!("could not show a notification: {e}");
    }
}

/// A click on a notification brings AmpSync forward: the Transfers view, or the sign-in window
/// when that is what the notification was about. Registered once at startup.
pub fn setup(app: &AppHandle) {
    let handle = app.clone();
    let registered = app.notification().on_action(move |action| {
        let kind = action
            .notification()
            .and_then(|n| n.extra().get("kind"))
            .and_then(|v| v.as_str())
            .unwrap_or("job")
            .to_string();
        crate::tray::show_main(&handle);
        if kind == Kind::SignIn.as_str() {
            handle.state::<AppState>().signin.focus(&handle);
        }
        let _ = handle.emit("notification-click", serde_json::json!({ "kind": kind }));
    });
    if let Err(e) = registered {
        tracing::warn!("notification clicks will not focus the window: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finished() -> Trigger {
        Trigger::Finished {
            title: "RoBat \u{b7} Test Campaign Q3".into(),
            files: 3906,
            failed: 0,
        }
    }
    fn failed() -> Trigger {
        Trigger::Failed { title: "Q3".into() }
    }
    fn sign_in() -> Trigger {
        Trigger::SignInNeeded {
            host: "contoso.sharepoint.com".into(),
        }
    }

    #[test]
    fn notifies_only_when_unfocused_and_opted_in() {
        for trigger in [finished(), failed(), sign_in()] {
            for focused in [true, false] {
                for opt in [None, Some(false), Some(true)] {
                    let got = decide(focused, opt, &trigger);
                    let expected = !focused && opt == Some(true);
                    assert_eq!(
                        got.is_some(),
                        expected,
                        "{trigger:?} focused={focused} opt={opt:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn cancelled_is_never_announced() {
        for focused in [true, false] {
            for opt in [None, Some(false), Some(true)] {
                assert_eq!(decide(focused, opt, &Trigger::Cancelled), None);
            }
        }
    }

    #[test]
    fn finished_message_names_the_job_and_the_file_count() {
        let m = decide(false, Some(true), &finished()).unwrap();
        assert_eq!(m.title, "Download finished");
        assert_eq!(m.body, "RoBat \u{b7} Test Campaign Q3 is ready, 3,906 files");
        assert_eq!(m.kind, Kind::Job);
    }

    #[test]
    fn one_file_is_singular() {
        let t = Trigger::Finished {
            title: "A".into(),
            files: 1,
            failed: 0,
        };
        assert_eq!(decide(false, Some(true), &t).unwrap().body, "A is ready, 1 file");
    }

    #[test]
    fn a_finish_with_failed_files_says_so() {
        let t = Trigger::Finished {
            title: "A".into(),
            files: 10,
            failed: 2,
        };
        let m = decide(false, Some(true), &t).unwrap();
        assert!(m.body.contains("2 files could not be saved"), "{}", m.body);
    }

    #[test]
    fn failed_and_sign_in_messages() {
        let f = decide(false, Some(true), &failed()).unwrap();
        assert_eq!(f.title, "Download stopped");
        assert!(f.body.starts_with("Q3 stopped"));
        let s = decide(false, Some(true), &sign_in()).unwrap();
        assert_eq!(s.title, "Sign-in needed");
        assert!(s.body.contains("contoso.sharepoint.com"));
        assert_eq!(s.kind, Kind::SignIn);
    }

    #[test]
    fn digit_grouping() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1000), "1,000");
        assert_eq!(grouped(1_234_567), "1,234,567");
    }
}
