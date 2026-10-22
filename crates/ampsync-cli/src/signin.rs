//! Obtaining a session: webview, then Edge, then Chrome, then a pasted cookie (D3).
//!
//! The chain only moves on when a method cannot start or is blocked. A user closing the
//! sign-in window is a decision, not a failure, so it ends the attempt.

use std::io::IsTerminal;
use std::path::Path;

use ampsync_core::auth::{BrowserKind, BrowserSignIn, PastedSignIn, Session, SignIn};
use ampsync_core::settings::Settings;
use url::Url;

use crate::cli::Method;
use crate::exit::Failure;
use crate::session_check::{SessionStatus, check};
use crate::{paths, signin_webview};

/// What the user asked for.
pub struct Request<'a> {
    pub host: &'a str,
    pub start_url: &'a Url,
    /// Site that holds the link; a fresh session is probed here.
    pub check_url: &'a Url,
    pub method: Option<Method>,
    pub cookie_file: Option<&'a Path>,
    pub settings: &'a Settings,
}

/// The outcome of trying one method.
pub enum Attempt {
    Got(Session),
    /// Could not start or was blocked; the next method may work.
    Unavailable(String),
    /// The user closed the window.
    Cancelled,
}

/// Methods to try, in order. An explicit `--method` is the only one tried.
pub fn chain(method: Option<Method>) -> Vec<Method> {
    match method {
        Some(m) => vec![m],
        None => vec![Method::Webview, Method::Edge, Method::Chrome, Method::Paste],
    }
}

fn label(m: Method) -> &'static str {
    match m {
        Method::Webview => "the sign-in window",
        Method::Edge => "Microsoft Edge",
        Method::Chrome => "Google Chrome",
        Method::Paste => "a pasted cookie",
    }
}

fn stdin_is_terminal() -> bool {
    std::io::stdin().is_terminal()
}

/// Sign in with the embedded window. `hidden` starts it invisible so an already-valid profile
/// refreshes silently; the window appears if interaction turns out to be needed. `known_bad` is
/// a session that `check_session` definitely reported as expired (never merely "unknown"), so its
/// stale cookies are not handed straight back. Each candidate is probed against `check_url`;
/// only a definite `Expired` rejects it.
///
/// Blocks the calling thread for as long as the window is open.
pub fn webview(
    host: &str,
    start_url: &Url,
    check_url: &Url,
    settings: &Settings,
    hidden: bool,
    known_bad: Option<&Session>,
) -> Attempt {
    let Some(profile_dir) = paths::webview_profile_dir() else {
        return Attempt::Unavailable("no per-user data folder is available for the sign-in profile".into());
    };
    let known_bad_fedauth = known_bad.and_then(|s| {
        s.cookies
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case("FedAuth"))
            .map(|c| c.value.clone())
    });
    let opts = signin_webview::Options {
        profile_dir,
        use_windows_account: settings.use_windows_account,
        save_passwords: settings.save_passwords,
        start_hidden: hidden,
        known_bad_fedauth,
    };
    // The window runs on this thread; the check runs on a runtime worker.
    let rt = tokio::runtime::Handle::current();
    let check_url = check_url.clone();
    let mut validate = |candidate: &Session| {
        let s = candidate.clone();
        let site = check_url.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        rt.spawn(async move {
            let _ = tx.send(check(&s, &site).await);
        });
        // Only a definite "expired" rejects; an unreachable server must not trap the user here.
        !matches!(rx.recv(), Ok(SessionStatus::Expired))
    };
    match signin_webview::sign_in(host, start_url, &opts, &mut validate) {
        Ok(s) => Attempt::Got(s),
        Err(signin_webview::Failure::Unavailable(why)) => Attempt::Unavailable(why),
        Err(signin_webview::Failure::Closed) => Attempt::Cancelled,
    }
}

async fn browser(kind: BrowserKind, host: &str, start_url: &Url) -> Attempt {
    match (BrowserSignIn { kind }).sign_in(host, start_url).await {
        Ok(s) => Attempt::Got(s),
        Err(ampsync_core::Error::Cancelled) => Attempt::Cancelled,
        Err(e) => Attempt::Unavailable(crate::exit::describe(&e, "")),
    }
}

async fn paste(host: &str, start_url: &Url, cookie_file: Option<&Path>) -> Result<Session, Failure> {
    let header = match cookie_file {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|e| Failure::error(format!("Can't read {}: {e}", path.display())))?,
        None => {
            if !stdin_is_terminal() {
                return Err(Failure::error(
                    "Can't ask for a pasted cookie without a terminal. Use --cookie-file <FILE> instead.",
                ));
            }
            eprintln!("Open the SharePoint site in your browser, sign in, then copy the value of the");
            eprintln!(
                "`Cookie` request header (browser developer tools, Network tab). It stays hidden here."
            );
            rpassword::prompt_password("Cookie header: ")
                .map_err(|e| Failure::error(format!("Couldn't read the pasted text: {e}")))?
        }
    };
    if header.trim().is_empty() {
        return Err(Failure::usage("Nothing was pasted."));
    }
    PastedSignIn {
        cookie_header: header.trim().to_string(),
    }
    .sign_in(host, start_url)
    .await
    .map_err(|e| Failure::from_core(&e, start_url.as_str()))
}

/// Run the chain for `req`.
pub async fn obtain(req: &Request<'_>) -> Result<Session, Failure> {
    let methods = chain(req.method);
    let explicit = req.method.is_some();
    let mut reasons: Vec<String> = Vec::new();
    for (i, method) in methods.iter().copied().enumerate() {
        let attempt = match method {
            Method::Webview => {
                eprintln!("Opening the sign-in window for {}...", req.host);
                webview(req.host, req.start_url, req.check_url, req.settings, false, None)
            }
            Method::Edge => {
                eprintln!("Opening Microsoft Edge with a temporary profile...");
                browser(BrowserKind::Edge, req.host, req.start_url).await
            }
            Method::Chrome => {
                eprintln!("Opening Google Chrome with a temporary profile...");
                browser(BrowserKind::Chrome, req.host, req.start_url).await
            }
            Method::Paste => match paste(req.host, req.start_url, req.cookie_file).await {
                Ok(s) => Attempt::Got(s),
                // Pasting is the last resort; its errors are the final answer.
                Err(f) => return Err(f),
            },
        };
        match attempt {
            Attempt::Got(s) => return Ok(s),
            Attempt::Cancelled => {
                return Err(Failure::error(format!(
                    "Sign-in was cancelled before it finished. If the window showed a message that \
                     sign-in is blocked, try `ampsync login {} --method edge` or `--method paste`.",
                    req.start_url
                )));
            }
            Attempt::Unavailable(why) => {
                if explicit {
                    return Err(Failure::error(format!("Couldn't use {}: {why}", label(method))));
                }
                reasons.push(format!("{}: {why}", label(method)));
                if let Some(next) = methods.get(i + 1) {
                    eprintln!("Couldn't use {} ({why}). Trying {}.", label(method), label(*next));
                }
            }
        }
    }
    Err(Failure::error(format!(
        "No sign-in method worked. {}",
        reasons.join("; ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_chain_order() {
        assert_eq!(
            chain(None),
            [Method::Webview, Method::Edge, Method::Chrome, Method::Paste]
        );
    }

    #[test]
    fn explicit_method_is_alone() {
        assert_eq!(chain(Some(Method::Chrome)), [Method::Chrome]);
        assert_eq!(chain(Some(Method::Paste)), [Method::Paste]);
    }
}
