//! Errors as the frontend sees them: a stable `kind` plus a plain-language message.

use ampsync_core::Error;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct CmdError {
    pub kind: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

impl CmdError {
    pub fn new(kind: &'static str, message: impl Into<String>) -> Self {
        CmdError {
            kind,
            message: message.into(),
            host: None,
        }
    }
}

impl From<Error> for CmdError {
    fn from(e: Error) -> Self {
        let mut host = None;
        let kind = match &e {
            Error::AuthExpired { host: h } => {
                host = Some(h.clone());
                "auth_expired"
            }
            Error::Throttled { .. } => "throttled",
            Error::NotFound(_) => "not_found",
            Error::InvalidLink(_) => "invalid_link",
            Error::Network(_) => "network",
            Error::DestinationBusy { .. } => "destination_busy",
            Error::Cancelled => "cancelled",
            _ => "error",
        };
        let message = match &e {
            Error::DestinationBusy { .. } => "Another AmpSync download is already using this folder. Wait for it to finish, or save to a different folder.".to_string(),
            other => other.to_string(),
        };
        CmdError { kind, message, host }
    }
}

impl From<tauri::Error> for CmdError {
    fn from(e: tauri::Error) -> Self {
        CmdError::new("error", e.to_string())
    }
}

pub type CmdResult<T> = Result<T, CmdError>;
