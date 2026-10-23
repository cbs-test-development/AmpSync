//! Update check (D10, D17): at most once a day, look at the latest GitHub release of this
//! repository and tell the user. Nothing is downloaded or installed.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use time::OffsetDateTime;

use crate::AppState;
use crate::paths;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateInfo {
    pub version: String,
    pub url: String,
}

#[derive(Serialize, Deserialize, Default)]
struct Cache {
    checked_at: i64,
    latest: Option<UpdateInfo>,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

fn parse_version(s: &str) -> Option<Vec<u64>> {
    let core = s.trim().trim_start_matches(['v', 'V']);
    let core = core.split(['-', '+']).next()?;
    core.split('.').map(|p| p.parse().ok()).collect()
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

fn read_cache() -> Cache {
    std::fs::read_to_string(paths::update_file())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_cache(c: &Cache) {
    let path = paths::update_file();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(t) = serde_json::to_string(c) {
        let _ = std::fs::write(path, t);
    }
}

pub fn spawn(app: AppHandle) {
    if crate::REPO_SLUG.is_empty() {
        return;
    }
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(8)).await;
        loop {
            check_once(&app).await;
            tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
        }
    });
}

async fn check_once(app: &AppHandle) {
    let state = app.state::<AppState>();
    if !state.settings().check_updates {
        return;
    }
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let mut cache = read_cache();
    let current = env!("CARGO_PKG_VERSION");
    if now - cache.checked_at < 24 * 3600 {
        publish(app, cache.latest.filter(|u| is_newer(&u.version, current)));
        return;
    }
    let url = format!(
        "https://api.github.com/repos/{}/releases/latest",
        crate::REPO_SLUG
    );
    let Ok(client) = reqwest::Client::builder()
        .user_agent(format!("AmpSync/{current}"))
        .timeout(Duration::from_secs(15))
        .build()
    else {
        return;
    };
    let Ok(resp) = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
    else {
        return;
    };
    cache.checked_at = now;
    if resp.status().is_success()
        && let Ok(rel) = resp.json::<Release>().await
        && !rel.draft
        && !rel.prerelease
    {
        cache.latest = Some(UpdateInfo {
            version: rel.tag_name.trim_start_matches(['v', 'V']).to_string(),
            url: rel.html_url,
        });
    }
    write_cache(&cache);
    publish(app, cache.latest.filter(|u| is_newer(&u.version, current)));
}

fn publish(app: &AppHandle, info: Option<UpdateInfo>) {
    let state = app.state::<AppState>();
    *state.update.lock().unwrap_or_else(|p| p.into_inner()) = info.clone();
    let _ = app.emit("update-available", info);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0-rc.1", "0.1.0"));
        assert!(!is_newer("garbage", "0.1.0"));
    }
}
