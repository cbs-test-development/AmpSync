//! Build script: Tauri's own step, plus two things that must never break a build:
//!
//! - the contributor list (D12): when `AMPSYNC_REPO` or `GITHUB_REPOSITORY` (`owner/name`) is
//!   set and the network is reachable, fetch the contributors and their avatars from GitHub and
//!   embed them as JSON with data URIs. Otherwise embed an empty list.
//! - build identification: short commit id and target triple for the About page.
//! - the browser link extension (`extensions/browser-link`), embedded as a table of file names and
//!   `include_bytes!` so the app can unpack it for "Load unpacked" without a download (and so the
//!   portable single-file exe still carries it).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use base64::Engine as _;
use serde_json::{Value, json};

fn main() {
    println!("cargo:rerun-if-env-changed=AMPSYNC_REPO");
    println!("cargo:rerun-if-env-changed=GITHUB_REPOSITORY");
    println!("cargo:rerun-if-env-changed=GITHUB_TOKEN");
    println!("cargo:rerun-if-env-changed=AMPSYNC_SKIP_CONTRIBUTORS");

    let slug = std::env::var("AMPSYNC_REPO")
        .ok()
        .or_else(|| std::env::var("GITHUB_REPOSITORY").ok())
        .map(|s| s.trim().to_string())
        .filter(|s| valid_slug(s))
        .unwrap_or_default();
    println!("cargo:rustc-env=AMPSYNC_REPO_SLUG={slug}");

    let items = if slug.is_empty() || std::env::var_os("AMPSYNC_SKIP_CONTRIBUTORS").is_some() {
        Vec::new()
    } else {
        fetch_contributors(&slug).unwrap_or_default()
    };
    let out =
        PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo")).join("contributors.json");
    let doc = json!({ "repo": slug, "items": items });
    std::fs::write(out, doc.to_string()).expect("cannot write contributors.json");

    let build = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=AMPSYNC_BUILD_ID={build}");
    println!(
        "cargo:rustc-env=AMPSYNC_TARGET={}",
        std::env::var("TARGET").unwrap_or_default()
    );

    embed_extension();

    tauri_build::build();
}

/// Write `$OUT_DIR/extension_files.rs`: `EXTENSION_FILES: &[(&str, &[u8])]` with every file of the
/// extension folder except its unit tests and README (neither belongs in a browser profile).
/// A missing folder (a source archive without it) yields an empty table rather than a failed build.
fn embed_extension() {
    let root =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo"))
            .join("..")
            .join("..")
            .join("..")
            .join("extensions")
            .join("browser-link");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    collect_extension_files(&root, "", &mut files);
    files.sort();
    let mut code = String::from("pub static EXTENSION_FILES: &[(&str, &[u8])] = &[\n");
    for (name, path) in &files {
        println!("cargo:rerun-if-changed={}", path.display());
        // `{:?}` escapes backslashes in Windows paths.
        code.push_str(&format!(
            "    ({name:?}, include_bytes!({:?})),\n",
            path.to_string_lossy()
        ));
    }
    code.push_str("];\n");
    let out =
        PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo")).join("extension_files.rs");
    std::fs::write(out, code).expect("cannot write extension_files.rs");
}

fn collect_extension_files(dir: &std::path::Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        match entry.file_type() {
            // Tests and the README are for developers, not for the unpacked extension.
            Ok(t) if t.is_dir() && name != "test" && name != "node_modules" => {
                collect_extension_files(&path, &rel, out);
            }
            Ok(t) if t.is_file() && !(prefix.is_empty() && name.eq_ignore_ascii_case("README.md")) => {
                out.push((rel, path));
            }
            _ => {}
        }
    }
}

fn valid_slug(s: &str) -> bool {
    let mut parts = s.split('/');
    let ok = |p: &str| {
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    matches!((parts.next(), parts.next(), parts.next()), (Some(a), Some(b), None) if ok(a) && ok(b))
}

/// GET through `curl` so the build script needs no TLS stack. The token goes in on stdin.
fn curl(url: &str) -> Option<Vec<u8>> {
    let mut cmd = Command::new("curl");
    cmd.args([
        "-fsSL",
        "--max-time",
        "15",
        "-A",
        "ampsync-build",
        "-H",
        "Accept: application/vnd.github+json",
        "-K",
        "-",
        url,
    ]);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    {
        let mut stdin = child.stdin.take()?;
        if let Ok(token) = std::env::var("GITHUB_TOKEN")
            && !token.is_empty()
            && !token.contains(['"', '\n', '\r'])
        {
            let _ = writeln!(stdin, "header = \"Authorization: Bearer {token}\"");
        }
    }
    let out = child.wait_with_output().ok()?;
    out.status.success().then_some(out.stdout)
}

fn excluded(login: &str, name: &str) -> bool {
    let hay = format!("{login} {name}").to_lowercase();
    hay.contains("claude") || hay.contains("anthropic")
}

fn fetch_contributors(slug: &str) -> Option<Vec<Value>> {
    let body = curl(&format!(
        "https://api.github.com/repos/{slug}/contributors?per_page=100"
    ))?;
    let list: Vec<Value> = serde_json::from_slice(&body).ok()?;
    let mut items = Vec::new();
    for c in list {
        let login = c.get("login")?.as_str()?.to_string();
        if c.get("type").and_then(Value::as_str) == Some("Bot") || login.ends_with("[bot]") {
            continue;
        }
        let name = curl(&format!("https://api.github.com/users/{login}"))
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|u| u.get("name").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default();
        if excluded(&login, &name) {
            continue;
        }
        let avatar = c
            .get("avatar_url")
            .and_then(Value::as_str)
            .map(|u| format!("{u}{}s=64", if u.contains('?') { "&" } else { "?" }))
            .and_then(|u| curl(&u))
            .map(|bytes| {
                let mime = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
                    "image/png"
                } else if bytes.starts_with(&[0xFF, 0xD8]) {
                    "image/jpeg"
                } else if bytes.starts_with(b"GIF8") {
                    "image/gif"
                } else {
                    "image/webp"
                };
                format!(
                    "data:{mime};base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(bytes)
                )
            });
        items.push(json!({
            "login": login,
            "name": name,
            "avatar": avatar,
            "contributions": c.get("contributions").and_then(Value::as_u64).unwrap_or(0),
            "url": c.get("html_url").and_then(Value::as_str).unwrap_or(""),
        }));
    }
    Some(items)
}
