//! The contributor list is generated at build time (`build.rs`, D12) and embedded.

const EMBEDDED: &str = include_str!(concat!(env!("OUT_DIR"), "/contributors.json"));

/// `{ "repo": "owner/name", "items": [ { login, name, avatar, contributions, url } ] }`
pub fn load() -> serde_json::Value {
    serde_json::from_str(EMBEDDED).unwrap_or_else(|_| serde_json::json!({ "repo": "", "items": [] }))
}
