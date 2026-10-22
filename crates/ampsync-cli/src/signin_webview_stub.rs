//! Stand-in used when the `webview` feature is off (headless builds): the embedded window is
//! never available, so sign-in falls back to Edge/Chrome or a pasted cookie.

use std::path::PathBuf;

use ampsync_core::auth::Session;
use url::Url;

#[derive(Debug, Clone)]
#[allow(dead_code)] // fields are only read by the real implementation
pub struct Options {
    pub profile_dir: PathBuf,
    pub use_windows_account: bool,
    pub save_passwords: bool,
    pub start_hidden: bool,
    pub known_bad_fedauth: Option<String>,
}

#[derive(Debug)]
pub enum Failure {
    Unavailable(String),
    #[allow(dead_code)] // only the real implementation produces it
    Closed,
}

pub fn display_available() -> bool {
    false
}

pub fn sign_in(
    _host: &str,
    _start_url: &Url,
    _opts: &Options,
    _validate: &mut dyn FnMut(&Session) -> bool,
) -> Result<Session, Failure> {
    Err(Failure::Unavailable(
        "this build of ampsync has no embedded sign-in window".into(),
    ))
}
