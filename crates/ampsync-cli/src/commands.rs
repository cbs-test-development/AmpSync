//! Subcommand implementations. Each returns the process exit code (see `exit`).

use std::future::Future;
use std::io::{BufRead, IsTerminal, Write};

use ampsync_core::Error;
use ampsync_core::PurgeReport;
use ampsync_core::auth::{KeyringStore, Session, SessionStore, SignInMethod};
use ampsync_core::link::{self, LinkKind, ParsedLink};
use ampsync_core::remote::{ClassicRemote, Remote, RemoteEntry};
use ampsync_core::settings::Settings;
use futures::StreamExt;
use indicatif::{ProgressBar, ProgressDrawTarget};
use time::macros::format_description;
use url::Url;

use crate::cli::{LoginArgs, LogoutArgs, LsArgs, Method, PurgeArgs};
use crate::exit::{self, Failure};
use crate::session_check::{self, SessionStatus, check, host_root, site_url_for};
use crate::signin::{self, Request};
use crate::{paths, tree};

/// Run a command body and turn its failure into a printed message and an exit code.
pub async fn run(body: impl Future<Output = Result<u8, Failure>>) -> u8 {
    match body.await {
        Ok(code) => code,
        Err(f) => {
            eprintln!("error: {}", f.message);
            f.code
        }
    }
}

/// Settings from `settings.toml`; a broken file warns and falls back to the defaults.
pub fn load_settings() -> Settings {
    match Settings::load() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("warning: {}; using default settings.", exit::describe(&e, ""));
            Settings::default()
        }
    }
}

pub fn parse_link(input: &str) -> Result<ParsedLink, Failure> {
    link::parse(input).map_err(|e| Failure::from_core(&e, input))
}

/// The page to open for sign-in: the pasted link itself, or the site that holds the folder.
pub fn start_url(link: &ParsedLink) -> Url {
    match &link.kind {
        LinkKind::Sharing { url } => url.clone(),
        LinkKind::Folder { site_url, .. } => site_url.clone(),
    }
}

pub fn require_session(store: &dyn SessionStore, host: &str, link_text: &str) -> Result<Session, Failure> {
    match store.load(host) {
        Ok(Some(s)) => Ok(s),
        Ok(None) => Err(Failure::error(format!(
            "You're not signed in to {host}. Run `ampsync login \"{link_text}\"` first."
        ))),
        Err(e) => Err(Failure::from_core(&e, link_text)),
    }
}

/// `https://Host/x` or `Host` -> `host`.
pub fn normalize_host(input: &str) -> String {
    let t = input.trim();
    let host = if t.contains("://") {
        Url::parse(t)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_else(|| t.to_string())
    } else {
        t.trim_end_matches('/').to_string()
    };
    host.to_ascii_lowercase()
}

pub fn method_label(m: &SignInMethod) -> &'static str {
    match m {
        SignInMethod::Webview => "webview",
        SignInMethod::Browser { kind } => match kind {
            ampsync_core::auth::BrowserKind::Edge => "edge",
            ampsync_core::auth::BrowserKind::Chrome => "chrome",
        },
        SignInMethod::Pasted => "pasted",
    }
}

/// `ampsync login`: obtain a session (webview, then Edge/Chrome, then paste unless `--method`
/// says otherwise) and save it to the keychain.
pub async fn login(args: LoginArgs) -> u8 {
    run(async {
        let link = parse_link(&args.url)?;
        let settings = load_settings();
        let method = args
            .method
            .or_else(|| args.cookie_file.is_some().then_some(Method::Paste));
        let start = start_url(&link);
        let site = site_url_for(&link);
        let session = signin::obtain(&Request {
            host: &link.host,
            start_url: &start,
            check_url: &site,
            method,
            cookie_file: args.cookie_file.as_deref(),
            settings: &settings,
        })
        .await?;
        match check(&session, &site).await {
            SessionStatus::Valid => {}
            SessionStatus::Expired => {
                return Err(Failure::error(format!(
                    "SharePoint did not accept that sign-in for {}, so nothing was saved. Try again.",
                    link.host
                )));
            }
            SessionStatus::Unknown => {
                eprintln!(
                    "note: couldn't confirm the sign-in works (SharePoint gave no clear answer); saving it anyway."
                );
            }
        }
        KeyringStore::new()
            .save(&session)
            .map_err(|e| Failure::from_core(&e, &args.url))?;
        println!(
            "Signed in to {} ({}).",
            session.host,
            method_label(&session.method)
        );
        Ok(exit::OK)
    })
    .await
}

/// `ampsync logout <host>`: forget the stored session and the sign-in window's profile (D15).
pub async fn logout(args: LogoutArgs) -> u8 {
    run(async {
        let host = normalize_host(&args.host);
        let store = KeyringStore::new();
        let had = store
            .hosts()
            .map(|h| h.iter().any(|x| x.eq_ignore_ascii_case(&host)))
            .unwrap_or(false);
        store.delete(&host).map_err(|e| Failure::from_core(&e, &host))?;
        if had {
            println!("Signed out of {host}.");
        } else {
            println!("There was no saved sign-in for {host}.");
        }
        match paths::delete_webview_profile() {
            Ok(true) => println!("Deleted the sign-in window's saved browser data."),
            Ok(false) => {}
            Err(e) => eprintln!("warning: couldn't delete the sign-in window's browser data: {e}"),
        }
        Ok(exit::OK)
    })
    .await
}

/// `ampsync purge [--yes]`: delete every sign-in, the settings and all app data. Downloaded files
/// and folders are never touched.
pub async fn purge(args: PurgeArgs) -> u8 {
    run(async {
        if !args.yes {
            if !stdin_is_terminal() {
                return Err(Failure::usage(
                    "this removes all saved sign-ins, settings and app data; run it interactively or pass --yes",
                ));
            }
            eprintln!("This removes every saved sign-in, your settings, and AmpSync's app data");
            eprintln!("(partial-download progress, sign-in window data). Downloaded files are kept.");
            if !confirm("Remove everything?") {
                eprintln!("Nothing was removed.");
                return Ok(exit::OK);
            }
        }
        let report = ampsync_core::purge_all().map_err(|e| Failure::from_core(&e, ""))?;
        for line in render_purge_report(&report) {
            println!("{line}");
        }
        Ok(if report.errors.is_empty() { exit::OK } else { exit::ERROR })
    })
    .await
}

fn confirm(question: &str) -> bool {
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Human-readable lines for a purge result.
pub fn render_purge_report(r: &PurgeReport) -> Vec<String> {
    let mut lines = vec![format!("Removed {} saved sign-in entries.", r.keychain_entries)];
    if r.removed.is_empty() {
        lines.push("No AmpSync folders were found.".to_string());
    } else {
        lines.push("Removed folders:".to_string());
        lines.extend(r.removed.iter().map(|p| format!("  {}", p.display())));
    }
    if !r.errors.is_empty() {
        lines.push("Some things could not be removed (close AmpSync and try again):".to_string());
        lines.extend(r.errors.iter().map(|e| format!("  {e}")));
    }
    lines
}

/// One row of the `sessions` table.
pub type Row = [String; 4];

/// Left-aligned columns separated by two spaces, with a header.
pub fn render_table(header: [&str; 4], rows: &[Row]) -> Vec<String> {
    let mut widths = header.map(|h| h.chars().count());
    for r in rows {
        for (w, c) in widths.iter_mut().zip(r) {
            *w = (*w).max(c.chars().count());
        }
    }
    let line = |cells: [&str; 4]| {
        cells
            .iter()
            .zip(widths)
            .map(|(c, w)| format!("{c:<w$}"))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    let mut out = vec![line(header)];
    out.extend(rows.iter().map(|r| line([&r[0], &r[1], &r[2], &r[3]])));
    out
}

fn captured_text(s: &Session) -> String {
    s.captured_at
        .to_offset(time::UtcOffset::UTC)
        .format(format_description!("[year]-[month]-[day] [hour]:[minute] UTC"))
        .unwrap_or_else(|_| "unknown".into())
}

/// `ampsync sessions`: host, method, captured time, and whether the server still accepts it.
pub async fn sessions() -> u8 {
    run(async {
        let store = KeyringStore::new();
        let hosts = store.hosts().map_err(|e| Failure::from_core(&e, ""))?;
        if hosts.is_empty() {
            println!("No saved sign-ins. Run `ampsync login <link>` to add one.");
            return Ok(exit::OK);
        }
        let mut loaded = Vec::new();
        for h in &hosts {
            match store.load(h) {
                Ok(Some(s)) => loaded.push(s),
                Ok(None) => {}
                Err(e) => eprintln!(
                    "warning: couldn't read the sign-in for {h}: {}",
                    exit::describe(&e, "")
                ),
            }
        }
        let statuses = futures::future::join_all(loaded.iter().map(|s| {
            let root = host_root(&s.host);
            async move { check(s, &root).await }
        }))
        .await;
        let rows: Vec<Row> = loaded
            .iter()
            .zip(&statuses)
            .map(|(s, st)| {
                [
                    s.host.clone(),
                    method_label(&s.method).to_string(),
                    captured_text(s),
                    session_check::label(st).to_string(),
                ]
            })
            .collect();
        for l in render_table(["HOST", "METHOD", "SIGNED IN", "STATUS"], &rows) {
            println!("{l}");
        }
        if statuses.iter().any(|s| matches!(s, SessionStatus::Expired)) {
            eprintln!("Expired sign-ins can be renewed with `ampsync login <link>`.");
        }
        Ok(exit::OK)
    })
    .await
}

/// `ampsync ls <url> [--depth N]`.
pub async fn ls(args: LsArgs) -> u8 {
    run(async {
        let link = parse_link(&args.url)?;
        let store = KeyringStore::new();
        let session = require_session(&store, &link.host, &args.url)?;
        let remote = ClassicRemote::new(&session);
        let fail = |e: Error| Failure::from_core(&e, &args.url);

        let spinner = ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr());
        spinner.enable_steady_tick(std::time::Duration::from_millis(120));
        spinner.set_message("Listing...");
        let root = remote.resolve(&link).await.map_err(fail)?;

        let mut items: Vec<tree::Item> = Vec::new();
        let mut stream = remote.walk(&root);
        while let Some(entry) = stream.next().await {
            match entry.map_err(fail)? {
                RemoteEntry::File(f) => items.push(tree::Item {
                    path: f.rel_path.to_string(),
                    size: Some(f.size),
                }),
                RemoteEntry::Folder { rel_path } => items.push(tree::Item {
                    path: rel_path.to_string(),
                    size: None,
                }),
            }
            if items.len().is_multiple_of(200) {
                spinner.set_message(format!("Listing... {} items", items.len()));
            }
        }
        spinner.finish_and_clear();

        let (lines, totals) = tree::render(&root.display_name, &items, args.depth.unwrap_or(1));
        for l in lines {
            println!("{l}");
        }
        println!("{}", tree::totals_line(&totals));
        Ok(exit::OK)
    })
    .await
}

pub fn stdin_is_terminal() -> bool {
    std::io::stdin().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ampsync_core::auth::BrowserKind;

    #[test]
    fn hosts_are_normalised() {
        assert_eq!(normalize_host("Contoso.SharePoint.com"), "contoso.sharepoint.com");
        assert_eq!(
            normalize_host("  contoso.sharepoint.com/ "),
            "contoso.sharepoint.com"
        );
        assert_eq!(
            normalize_host("https://Contoso.sharepoint.com/sites/a?x=1"),
            "contoso.sharepoint.com"
        );
    }

    #[test]
    fn purge_report_lists_folders_and_errors() {
        let report = PurgeReport {
            removed: vec!["/data/AmpSync".into()],
            keychain_entries: 3,
            errors: vec!["/x: busy".into()],
        };
        let text = render_purge_report(&report).join(
            "
",
        );
        assert!(text.contains("Removed 3 saved sign-in entries."));
        assert!(text.contains("AmpSync"));
        assert!(text.contains("/x: busy"));
        let empty = render_purge_report(&PurgeReport::default()).join(
            "
",
        );
        assert!(empty.contains("No AmpSync folders were found."));
    }

    #[test]
    fn method_labels() {
        assert_eq!(method_label(&SignInMethod::Webview), "webview");
        assert_eq!(method_label(&SignInMethod::Pasted), "pasted");
        assert_eq!(
            method_label(&SignInMethod::Browser {
                kind: BrowserKind::Chrome
            }),
            "chrome"
        );
        assert_eq!(
            method_label(&SignInMethod::Browser {
                kind: BrowserKind::Edge
            }),
            "edge"
        );
    }

    #[test]
    fn table_aligns_columns() {
        let rows = vec![
            [
                "a.sharepoint.com".into(),
                "webview".into(),
                "2026-10-22 10:00 UTC".into(),
                "valid".into(),
            ],
            [
                "bb.sharepoint.com".into(),
                "pasted".into(),
                "2026-10-21 09:30 UTC".into(),
                "expired".into(),
            ],
        ];
        let t = render_table(["HOST", "METHOD", "SIGNED IN", "STATUS"], &rows);
        assert_eq!(t.len(), 3);
        assert!(t[0].starts_with("HOST "));
        let col = |l: &str| {
            l.find("webview")
                .or_else(|| l.find("pasted"))
                .or_else(|| l.find("METHOD"))
        };
        assert_eq!(col(&t[0]), col(&t[1]));
        assert_eq!(col(&t[1]), col(&t[2]));
        assert!(!t[1].ends_with(' '));
    }
}
