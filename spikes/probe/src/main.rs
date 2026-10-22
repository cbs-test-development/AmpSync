//! ampsync-probe: throwaway probe that tests which SharePoint access approaches work with
//! browser-session cookies only (no Entra app registration). See README.md.
//! Never stores cookies or file contents; the report is redacted (see redact.rs).

mod auth;
mod cdp;
mod experiments;
mod http;
mod names;
mod qxor;
#[cfg(test)]
mod qxor_vectors;
mod redact;
mod urlparse;
mod webview;

use anyhow::{anyhow, Result};
use experiments::{ExpResult, State, Status};
use serde::Serialize;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Copy, PartialEq, Eq)]
enum AuthMode {
    Webview,
    Edge,
    Chrome,
    Paste,
}

impl AuthMode {
    fn label(self) -> &'static str {
        match self {
            AuthMode::Webview => "webview",
            AuthMode::Edge => "edge",
            AuthMode::Chrome => "chrome",
            AuthMode::Paste => "paste",
        }
    }
}

struct Args {
    url: String,
    auth: AuthMode,
    out: String,
    no_sso: bool,
    ascii: bool,
    cookie_file: Option<String>,
}

const USAGE: &str = "usage: ampsync-probe <sharepoint-folder-url> [--auth webview|edge|chrome|paste] [--out probe-report.json]\n\
                     \x20      [--no-sso] [--ascii] [--cookie-file <path>  (paste mode)]";

fn parse_args() -> Result<Args> {
    let mut it = std::env::args().skip(1);
    let mut url = None;
    let mut auth = AuthMode::Webview;
    let mut out = "probe-report.json".to_string();
    let (mut no_sso, mut ascii, mut cookie_file) = (false, false, None);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--auth" => {
                auth = match it.next().ok_or_else(|| anyhow!("--auth needs a value"))?.as_str() {
                    "webview" => AuthMode::Webview,
                    "edge" => AuthMode::Edge,
                    "chrome" => AuthMode::Chrome,
                    "paste" => AuthMode::Paste,
                    o => return Err(anyhow!("unknown auth mode `{o}`")),
                }
            }
            "--out" => out = it.next().ok_or_else(|| anyhow!("--out needs a value"))?,
            "--cookie-file" => cookie_file = Some(it.next().ok_or_else(|| anyhow!("--cookie-file needs a value"))?),
            "--no-sso" => no_sso = true,
            "--ascii" => ascii = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            s if s.starts_with("--") => return Err(anyhow!("unknown option `{s}`")),
            s => {
                if url.replace(s.to_string()).is_some() {
                    return Err(anyhow!("more than one URL given (quote the URL?)"));
                }
            }
        }
    }
    Ok(Args { url: url.ok_or_else(|| anyhow!("missing <sharepoint-folder-url>"))?, auth, out, no_sso, ascii, cookie_file })
}

fn os_version() -> String {
    #[cfg(windows)]
    {
        if let Ok(o) = std::process::Command::new("cmd").args(["/C", "ver"]).output() {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/etc/os-release") {
            if let Some(l) = s.lines().find(|l| l.starts_with("PRETTY_NAME=")) {
                return l["PRETTY_NAME=".len()..].trim_matches('"').to_string();
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(o) = std::process::Command::new("sw_vers").arg("-productVersion").output() {
            return String::from_utf8_lossy(&o.stdout).trim().to_string();
        }
    }
    "unknown".into()
}

#[derive(Serialize)]
struct Report {
    probe_version: &'static str,
    os: &'static str,
    arch: &'static str,
    os_version: String,
    auth_mode: &'static str,
    sso_option: String,
    auth_notes: Vec<String>,
    started_utc: String,
    finished_utc: String,
    tenant_host: String,
    url_kind: urlparse::UrlKind,
    auth_error: Option<String>,
    experiments: Vec<ExpResult>,
    http_calls: Vec<http::HttpRec>,
    redaction: &'static str,
}

const REDACTION_NOTE: &str = "No cookie values, file/folder/site names or full URLs are recorded. Names appear as <ext, size>; path segments as <seg:length>; URLs reduced to host.";

fn read_line(prompt: &str) -> String {
    use std::io::Write;
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    let _ = std::io::stdin().read_line(&mut s);
    s.trim().to_string()
}

/// Started with no arguments (e.g. double-clicked): ask for everything instead of exiting.
fn interactive_args() -> Args {
    println!("ampsync-probe {VERSION}\n");
    println!("This checks which ways of downloading from your SharePoint work. It only reads;");
    println!("it never stores your sign-in or any file contents.\n");
    let url = loop {
        let u = read_line("Paste a SharePoint folder link, then press Enter:\n> ");
        let u = u.trim_matches('"').to_string();
        match urlparse::classify_url(&u) {
            Ok(_) => break u,
            Err(e) => println!("That doesn't look like a SharePoint folder link ({e}). Try again.\n"),
        }
    };
    println!("\nHow should it sign in?");
    println!("  1  AmpSync window (default)");
    println!("  2  Microsoft Edge (temporary profile)");
    println!("  3  Google Chrome (temporary profile)");
    println!("  4  Paste a Cookie header from browser DevTools");
    let auth = match read_line("Choose 1-4 [1]: ").as_str() {
        "2" => AuthMode::Edge,
        "3" => AuthMode::Chrome,
        "4" => AuthMode::Paste,
        _ => AuthMode::Webview,
    };
    Args { url, auth, out: format!("probe-{}.json", auth.label()), no_sso: false, ascii: false, cookie_file: None }
}

fn main() {
    let interactive = std::env::args().len() == 1;
    let args = if interactive {
        interactive_args()
    } else {
        match parse_args() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("error: {e}\n{USAGE}");
                std::process::exit(2);
            }
        }
    };
    let code = match run(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {}", redact::scrub_text(&format!("{e:#}")));
            1
        }
    };
    if interactive {
        // Keep the console window open so the summary can be read.
        read_line("\nPress Enter to close.");
    }
    std::process::exit(code);
}

fn run(args: Args) -> Result<()> {
    let started = chrono::Utc::now();
    let parsed = urlparse::classify_url(&args.url).map_err(|e| anyhow!(e))?;
    println!("ampsync-probe {VERSION} | auth: {} | host: {} | url kind: {:?}", args.auth.label(), parsed.host, parsed.kind);

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;

    // --- auth (webview must run on the main thread, before/outside the tokio runtime) ---
    let auth_res = match args.auth {
        AuthMode::Webview => webview::capture(&args.url, &parsed.host, !args.no_sso),
        AuthMode::Edge => rt.block_on(cdp::capture(cdp::Browser::Edge, &args.url, &parsed.host)),
        AuthMode::Chrome => rt.block_on(cdp::capture(cdp::Browser::Chrome, &args.url, &parsed.host)),
        AuthMode::Paste => auth::paste(args.cookie_file.as_deref()),
    };

    let mut report = Report {
        probe_version: VERSION,
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        os_version: os_version(),
        auth_mode: args.auth.label(),
        sso_option: "n/a".into(),
        auth_notes: vec![],
        started_utc: started.to_rfc3339(),
        finished_utc: String::new(),
        tenant_host: parsed.host.clone(),
        url_kind: parsed.kind,
        auth_error: None,
        experiments: vec![],
        http_calls: vec![],
        redaction: REDACTION_NOTE,
    };

    match auth_res {
        Err(e) => {
            report.auth_error = Some(redact::scrub_text(&format!("{e:#}")));
            report.finished_utc = chrono::Utc::now().to_rfc3339();
            write_report(&report, &args.out)?;
            print_summary(&report, args.ascii);
            return Err(anyhow!("authentication step failed: {e}"));
        }
        Ok(a) => {
            report.sso_option = a.sso.clone();
            report.auth_notes = a.notes.iter().map(|n| redact::scrub_text(n)).collect();
            if a.cookies.is_empty() {
                report.auth_error = Some("no cookies captured for the tenant host".into());
                report.finished_utc = chrono::Utc::now().to_rfc3339();
                write_report(&report, &args.out)?;
                print_summary(&report, args.ascii);
                return Err(anyhow!("authentication step failed: no cookies captured"));
            }
            let http = http::Http::new(&a.cookies, &parsed.host)?;
            let mut st = State::new(&args.url, parsed, a.cookies);
            let exps = rt.block_on(async {
                let mut v = Vec::new();
                v.push(experiments::e1_cookies(&st));
                v.push(experiments::e2_resolve(&http, &mut st).await);
                v.push(experiments::e3_v2_listing(&http, &mut st).await);
                v.push(experiments::e4_classic(&http, &mut st).await);
                v.push(experiments::e5_walk(&http, &mut st).await);
                v.push(experiments::e6_range(&http, &mut st).await);
                v.push(experiments::e7_hash(&http, &mut st).await);
                v
            });
            report.experiments = exps;
            report.http_calls = http.log.lock().unwrap().clone();
        }
    }
    report.finished_utc = chrono::Utc::now().to_rfc3339();
    write_report(&report, &args.out)?;
    print_summary(&report, args.ascii);
    let shown = std::fs::canonicalize(&args.out).map(|p| p.display().to_string()).unwrap_or(args.out.clone());
    println!("\nReport written to {} (redacted; safe to send). Cookies/file contents were never stored.", shown.trim_start_matches(r"\\?\"));
    Ok(())
}

fn write_report(r: &Report, path: &str) -> Result<()> {
    std::fs::write(path, serde_json::to_string_pretty(r)?)?;
    Ok(())
}

fn print_summary(r: &Report, ascii: bool) {
    println!("\n=== ampsync-probe summary ===");
    println!("OS: {} {} ({}) | auth: {} | SSO option: {}", r.os, r.os_version, r.arch, r.auth_mode, r.sso_option);
    if let Some(e) = &r.auth_error {
        println!("{} AUTH   {e}", if ascii { "FAIL" } else { "❌" });
    }
    for e in &r.experiments {
        let mark = match (e.status, ascii) {
            (Status::Pass, false) => "✅",
            (Status::Fail, false) => "❌",
            (Status::Skipped, false) => "⏭ ",
            (Status::Pass, true) => "PASS",
            (Status::Fail, true) => "FAIL",
            (Status::Skipped, true) => "SKIP",
        };
        println!("{mark} {} {:<18} {}", e.id, e.name, e.summary);
        if let Some(err) = &e.error {
            println!("      error: {err}");
        }
    }
}
