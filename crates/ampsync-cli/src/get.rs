//! `ampsync get <url> <dest>`: plan, confirm, download, with progress, re-sign-in and Ctrl+C
//! handling.

use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::Arc;

use ampsync_core::Error;
use ampsync_core::auth::{KeyringStore, Session, SessionStore};
use ampsync_core::engine::{Engine, Event, Summary};
use ampsync_core::link::ParsedLink;
use ampsync_core::plan::{ExistingPolicy, JobSpec, Plan, PlannedAction, Totals};
use ampsync_core::remote::ClassicRemote;
use ampsync_core::settings::Settings;
use indicatif::{ProgressBar, ProgressDrawTarget};
use tokio::sync::broadcast::error::RecvError;

use crate::cli::GetArgs;
use crate::commands::{self, load_settings, parse_link, require_session, start_url, stdin_is_terminal};
use crate::exit::{self, Failure};
use crate::human;
use crate::progress::Display;
use crate::signin::{self, Attempt};

/// Re-sign-in attempts in a row (without a file completing in between) before giving up.
const MAX_REAUTH: u32 = 3;

/// Lines shown in the "ready to download" confirmation.
pub fn plan_lines(totals: &Totals, resumes: u64, name: &str, dest: &Path) -> Vec<String> {
    let mut lines = vec![
        format!("Source:       {name}"),
        format!("Destination:  {}", dest.display()),
    ];
    let mut todo = format!(
        "{}, {}",
        human::count(totals.files, "file", "files"),
        human::bytes(totals.bytes)
    );
    if resumes > 0 {
        todo.push_str(&format!(" ({resumes} resuming)"));
    }
    lines.push(format!("To download:  {todo}"));
    lines.push(format!(
        "Skipped:      {}, {} (already up to date)",
        human::count(totals.skipped, "file", "files"),
        human::bytes(totals.skipped_bytes)
    ));
    lines
}

/// Lines printed when the job is over.
pub fn summary_lines(s: &Summary) -> Vec<String> {
    let mut lines = vec![format!(
        "Done in {}: {} downloaded ({}), {} skipped, {} failed.",
        human::duration(s.elapsed),
        s.downloaded,
        human::bytes(s.bytes),
        s.skipped,
        s.failed
    )];
    for f in s.failures.iter().take(10) {
        lines.push(format!("  failed: {}: {}", f.rel_path, f.error));
    }
    if s.failures.len() > 10 {
        lines.push(format!("  ... and {} more", s.failures.len() - 10));
    }
    if s.failed > 0 {
        lines.push("Run the same command again to retry the failed files.".to_string());
    }
    lines
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

/// Renew an expired session through the sign-in window (silently first, D15) and store it.
/// Blocks while the window is up.
fn refresh_session(
    link: &ParsedLink,
    settings: &Settings,
    old: &Session,
    store: &dyn SessionStore,
) -> Result<Session, Failure> {
    let start = start_url(link);
    match signin::webview(&link.host, &start, settings, true, Some(old)) {
        Attempt::Got(s) => {
            if let Err(e) = store.save(&s) {
                eprintln!(
                    "warning: couldn't save the renewed sign-in: {}",
                    exit::describe(&e, "")
                );
            }
            Ok(s)
        }
        Attempt::Unavailable(why) => Err(Failure::new(
            format!("Couldn't open the sign-in window: {why}"),
            exit::AUTH_EXPIRED,
        )),
        Attempt::Cancelled => Err(Failure::new("Sign-in was cancelled.", exit::AUTH_EXPIRED)),
    }
}

enum Stop {
    Finished,
    Interrupted,
    AuthGaveUp(String),
}

/// `ampsync get <url> <dest> ...`: plan, confirm unless `--yes`, download. Exit codes: 0 ok,
/// 3 auth expired (state kept; rerun resumes), 4 some files failed.
pub async fn get(args: GetArgs) -> u8 {
    commands::run(get_inner(args)).await
}

async fn get_inner(args: GetArgs) -> Result<u8, Failure> {
    let link = parse_link(&args.url)?;
    let mut settings = load_settings();
    if let Some(p) = args.parallel {
        settings.parallel_files = p;
    }
    if let Some(l) = args.limit {
        settings.bandwidth_limit = Some(l);
    }
    let settings = settings.sanitized();

    let store = KeyringStore::new();
    let mut session = require_session(&store, &link.host, &args.url)?;

    let mut spec = JobSpec::new(args.url.clone(), args.dest.clone());
    spec.include = args.include.clone();
    spec.exclude = args.exclude.clone();
    spec.prune = args.prune;
    if args.redownload {
        spec.existing = ExistingPolicy::Redownload;
    }

    let tty_in = stdin_is_terminal();
    let can_reauth = !args.json && tty_in && crate::signin_webview::display_available();

    // Plan (list the remote tree and compare with the destination), renewing the sign-in once or
    // twice if it has already expired.
    let (engine, plan): (Engine<ClassicRemote>, Plan) = {
        let mut attempts = 0;
        loop {
            let engine = Engine::new(Arc::new(ClassicRemote::new(&session)), settings.clone());
            let spinner = ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr());
            if !args.json {
                spinner.enable_steady_tick(std::time::Duration::from_millis(120));
                spinner.set_message("Listing the folder and comparing with what you already have...");
            }
            let planned = engine.plan(&spec).await;
            spinner.finish_and_clear();
            match planned {
                Ok(plan) => break (engine, plan),
                Err(Error::AuthExpired { host }) if can_reauth && attempts < 2 => {
                    attempts += 1;
                    eprintln!("Sign-in expired for {host}. Refreshing it...");
                    session = refresh_session(&link, &settings, &session, &store)?;
                }
                Err(e) => return Err(Failure::from_core(&e, &args.url)),
            }
        }
    };

    let resumes = plan
        .items
        .iter()
        .filter(|i| matches!(i.action, PlannedAction::Resume { .. }))
        .count() as u64;
    if !args.json {
        let lines = plan_lines(&plan.totals, resumes, &plan.root.server_relative_path, &args.dest);
        if args.yes || !tty_in {
            for l in &lines {
                eprintln!("{l}");
            }
        } else {
            for l in &lines {
                println!("{l}");
            }
        }
    }
    if plan.totals.files == 0 && !spec.prune {
        if !args.json {
            eprintln!("Nothing to download: everything is already up to date.");
        }
        return Ok(exit::OK);
    }
    if !args.json && !args.yes && tty_in && !confirm("Start the download?") {
        eprintln!("Nothing was downloaded.");
        return Ok(exit::OK);
    }

    let totals = plan.totals;
    let handle = engine.start(spec, plan);
    let mut events = handle.events();
    let mut display = (!args.json).then(|| Display::new(totals.files, totals.bytes));
    if args.json {
        let planned = Event::Planned {
            files: totals.files,
            bytes: totals.bytes,
            skipped: totals.skipped,
        };
        emit_json(&planned);
    }

    // Ctrl+C: first press stops cleanly (progress is kept), second quits at once.
    let (sig_tx, mut sig_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    tokio::spawn(async move {
        while tokio::signal::ctrl_c().await.is_ok() {
            if sig_tx.send(()).is_err() {
                break;
            }
        }
    });

    let mut reauths = 0u32;
    let stop = loop {
        tokio::select! {
            _ = sig_rx.recv() => break Stop::Interrupted,
            received = events.recv() => {
                let event = match received {
                    Ok(e) => e,
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break Stop::Finished,
                };
                match &mut display {
                    Some(d) => d.on_event(&event),
                    None if !matches!(event, Event::Planned { .. }) => emit_json(&event),
                    None => {}
                }
                match event {
                    Event::Finished(_) | Event::Failed { .. } | Event::Cancelled => break Stop::Finished,
                    Event::FileVerified { .. } => reauths = 0,
                    Event::AuthExpired { host } => {
                        if !can_reauth || reauths >= MAX_REAUTH {
                            break Stop::AuthGaveUp(host);
                        }
                        reauths += 1;
                        let renewed = match &display {
                            Some(d) => d.suspend(|| {
                                eprintln!("Sign-in expired for {host}. Refreshing it...");
                                refresh_session(&link, &settings, &session, &store)
                            }),
                            None => refresh_session(&link, &settings, &session, &store),
                        };
                        match renewed {
                            Ok(s) => {
                                session = s.clone();
                                handle.replace_session(s);
                            }
                            Err(f) => {
                                eprintln!("{}", f.message);
                                break Stop::AuthGaveUp(host);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    };
    if let Some(d) = &display {
        d.finish();
    }

    if !matches!(stop, Stop::Finished) {
        handle.cancel();
    }
    let result = tokio::select! {
        r = handle.wait() => r,
        _ = sig_rx.recv() => {
            eprintln!("Quitting now. Run the same command again to resume.");
            std::process::exit(i32::from(exit::ERROR));
        }
    };

    match stop {
        Stop::Interrupted => {
            eprintln!("Stopped. Progress is saved; run the same command again to resume.");
            Ok(exit::ERROR)
        }
        Stop::AuthGaveUp(host) => {
            eprintln!(
                "Sign-in expired for {host}. Progress is saved. Run `ampsync login \"{}\"`, then run the same command again to resume.",
                args.url
            );
            Ok(exit::AUTH_EXPIRED)
        }
        Stop::Finished => match result {
            Ok(summary) => {
                if !args.json {
                    for l in summary_lines(&summary) {
                        println!("{l}");
                    }
                }
                Ok(exit::for_summary(&summary))
            }
            Err(e) => Err(Failure::from_core(&e, &args.url)),
        },
    }
}

fn emit_json(event: &Event) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", human::event_json_line(event));
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use ampsync_core::engine::FileFailure;
    use ampsync_core::plan::RelPath;

    #[test]
    fn plan_summary_text() {
        let t = Totals {
            files: 3,
            bytes: 3 * 1024 * 1024,
            skipped: 1,
            skipped_bytes: 1024,
        };
        let l = plan_lines(&t, 2, "/sites/T/Docs/P", Path::new("out"));
        let text = l.join("\n");
        assert!(text.contains("3 files, 3.0 MiB (2 resuming)"), "{text}");
        assert!(text.contains("1 file, 1.0 KiB"), "{text}");
        assert!(text.contains("/sites/T/Docs/P"), "{text}");
        let none = plan_lines(&t, 0, "x", Path::new("out")).join("\n");
        assert!(!none.contains("resuming"));
    }

    #[test]
    fn job_summary_text() {
        let mut s = Summary {
            downloaded: 5,
            skipped: 2,
            failed: 0,
            bytes: 2048,
            elapsed: Duration::from_secs(65),
            failures: vec![],
        };
        let ok = summary_lines(&s);
        assert_eq!(
            ok,
            ["Done in 1m 05s: 5 downloaded (2.0 KiB), 2 skipped, 0 failed."]
        );
        s.failed = 12;
        s.failures = (0..12)
            .map(|i| FileFailure {
                rel_path: RelPath::new(format!("f{i}")).unwrap(),
                error: "boom".into(),
            })
            .collect();
        let l = summary_lines(&s);
        assert!(l.iter().any(|x| x.contains("failed: f0: boom")));
        assert!(l.iter().any(|x| x.contains("and 2 more")));
        assert!(l.last().unwrap().contains("again"));
    }
}
