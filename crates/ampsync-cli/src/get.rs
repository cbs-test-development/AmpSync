//! `ampsync get <url> <dest>`: plan, confirm, download, with progress, re-sign-in and Ctrl+C
//! handling.

use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::Arc;

use ampsync_core::Error;
use ampsync_core::auth::{KeyringStore, Session, SessionStore};
use ampsync_core::autotune::AutoTuneConfig;
use ampsync_core::engine::{Engine, Event, Summary};
use ampsync_core::link::ParsedLink;
use ampsync_core::plan::{ExistingPolicy, JobSpec, Plan, PlannedAction, Totals};
use ampsync_core::remote::ClassicRemote;
use ampsync_core::settings::Settings;
use indicatif::{ProgressBar, ProgressDrawTarget};
use tokio::sync::broadcast::error::RecvError;
use tokio::time::Instant;

use crate::cli::GetArgs;
use crate::commands::{self, load_settings, parse_link, require_session, start_url, stdin_is_terminal};
use crate::exit::{self, Failure};
use crate::human;
use crate::progress::Display;
use crate::session_check::{SessionStatus, check, site_url_for};
use crate::signin::{self, Attempt};
use crate::space::{self, DiskWait};

/// Re-sign-in attempts in a row (without a file completing in between) before giving up.
const MAX_REAUTH: u32 = 3;

/// How the number of simultaneous transfers is chosen, for the summary:
/// `automatic (2 to 16, starting at 4)` or `6 at a time (fixed)`.
pub fn lanes_text(settings: &Settings) -> String {
    if settings.auto_tune {
        let t = AutoTuneConfig::default();
        format!(
            "automatic ({} to {}, starting at {})",
            t.min_lanes, t.max_lanes, t.start_lanes
        )
    } else {
        format!("{} at a time (fixed)", settings.parallel_files)
    }
}

/// Lines shown in the "ready to download" confirmation. `disk` is the "Disk space:" line, left
/// out when there is nothing to download; `lanes` is [`lanes_text`].
pub fn plan_lines(
    totals: &Totals,
    resumes: u64,
    name: &str,
    dest: &Path,
    disk: Option<String>,
    lanes: &str,
) -> Vec<String> {
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
    lines.extend(disk);
    lines.push(format!("Transfers:    {lanes}"));
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
///
/// The old session is passed on as "known bad" only when SharePoint definitely rejects it
/// (`bad` is `Some`): the window's profile is persistent, so a FedAuth that is merely
/// unconfirmed may be the only one it will ever produce, and refusing it would wait forever.
fn refresh_session(
    link: &ParsedLink,
    settings: &Settings,
    bad: Option<&Session>,
    store: &dyn SessionStore,
) -> Result<Session, Failure> {
    let start = start_url(link);
    let site = site_url_for(link);
    match signin::webview(&link.host, &start, &site, settings, true, bad) {
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

/// Probe `session`; the flag is true only when SharePoint definitely rejects it.
async fn probe(session: &Session, link: &ParsedLink) -> (SessionStatus, bool) {
    let status = check(session, &site_url_for(link)).await;
    let bad = matches!(status, SessionStatus::Expired);
    (status, bad)
}

enum Stop {
    Finished,
    Interrupted,
    AuthGaveUp(String),
    /// The sign-in is valid but SharePoint keeps refusing the request.
    Refused(String),
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
        // An explicit number means exactly that many, not automatic tuning.
        settings.auto_tune = false;
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
                    let (status, bad) = probe(&session, &link).await;
                    if matches!(status, SessionStatus::Valid) {
                        return Err(Failure::new(
                            format!(
                                "SharePoint refused the request for {host} although the sign-in is still valid. \
                                 Check that you have access to this folder."
                            ),
                            exit::ERROR,
                        ));
                    }
                    eprintln!("Sign-in expired for {host}. Refreshing it...");
                    let old = session.clone();
                    session = refresh_session(&link, &settings, bad.then_some(&old), &store)?;
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
    // Disk space (D21): `bytes_needed` is what is still to download, so a rerun that has most of
    // the files already is judged on the remainder only.
    let volume = space::volume_label(&args.dest);
    let free = if plan.bytes_needed > 0 {
        ampsync_core::free_space(&args.dest)
    } else {
        Ok(0)
    };
    if !args.json {
        let disk = (plan.bytes_needed > 0).then(|| {
            space::summary_line(
                &volume,
                plan.bytes_needed,
                free.as_ref().ok().copied(),
                args.ignore_space,
            )
        });
        let lines = plan_lines(
            &plan.totals,
            resumes,
            &plan.root.server_relative_path,
            &args.dest,
            disk,
            &lanes_text(&settings),
        );
        if args.yes || !tty_in {
            for l in &lines {
                eprintln!("{l}");
            }
        } else {
            for l in &lines {
                println!("{l}");
            }
        }
        if let Err(e) = &free
            && plan.bytes_needed > 0
        {
            eprintln!("warning: couldn't check the free space on {volume}: {e}. Carrying on.");
        }
    }
    space::check_before_start(&volume, plan.bytes_needed, &free, args.ignore_space)?;
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
    let mut lagged = 0u64;
    let mut refused_while_valid = false;
    // Disk full: the job is paused; look at the free space every `RECHECK_EVERY` and resume once
    // there is room (see `DiskWait::ready`). `Enter` on a terminal tries right away.
    let mut disk_wait: Option<DiskWait> = None;
    let mut next_check = Instant::now();
    let mut enter_rx: Option<tokio::sync::mpsc::UnboundedReceiver<()>> = None;
    let stop = loop {
        tokio::select! {
            _ = sig_rx.recv() => break Stop::Interrupted,
            _ = tokio::time::sleep_until(next_check), if disk_wait.is_some() => {
                next_check = Instant::now() + space::RECHECK_EVERY;
                if let Some(wait) = disk_wait
                    && let Ok(free) = ampsync_core::free_space(&args.dest)
                    && wait.ready(free)
                {
                    disk_wait = None;
                    if let Some(d) = &display {
                        d.say(&format!("{} free now; continuing.", human::bytes(free)));
                    }
                    handle.resume();
                }
            }
            _ = enter_pressed(&mut enter_rx), if disk_wait.is_some() => {
                disk_wait = None;
                if let Some(d) = &display {
                    d.say("Trying again now.");
                }
                handle.resume();
            }
            received = events.recv() => {
                let event = match received {
                    Ok(e) => e,
                    Err(RecvError::Lagged(n)) => {
                        // Progress is cumulative, so the bars catch up by themselves; only the
                        // per-file counters can be short until the final summary.
                        lagged += n;
                        if lagged == n {
                            let note = "note: the display fell behind the transfer; file counts may lag until the summary.";
                            match &display {
                                Some(d) => d.say(note),
                                None => eprintln!("{note}"),
                            }
                        }
                        continue;
                    }
                    Err(RecvError::Closed) => break Stop::Finished,
                };
                match &mut display {
                    Some(d) => d.on_event(&event),
                    None => emit_json(&event),
                }
                match event {
                    Event::Finished(_) | Event::Failed { .. } | Event::Cancelled => break Stop::Finished,
                    Event::FileVerified { .. } => reauths = 0,
                    Event::DiskFull { needed, free, .. } => {
                        disk_wait = Some(DiskWait::new(needed, free));
                        next_check = Instant::now() + space::RECHECK_EVERY;
                        // Enter-to-retry needs a person at a terminal; scripts just wait.
                        if enter_rx.is_none() && !args.json && tty_in {
                            enter_rx = Some(spawn_enter_reader());
                        }
                        if let Some(rx) = &mut enter_rx {
                            // Presses from before this moment must not count as a retry.
                            while rx.try_recv().is_ok() {}
                        }
                        if let Some(d) = &display {
                            d.say(&space::disk_full_line(&volume, needed, free, enter_rx.is_some()));
                        }
                    }
                    Event::Resumed => disk_wait = None,
                    Event::AuthExpired { host } => {
                        if reauths >= MAX_REAUTH {
                            break if refused_while_valid {
                                Stop::Refused(host)
                            } else {
                                Stop::AuthGaveUp(host)
                            };
                        }
                        if !can_reauth {
                            break Stop::AuthGaveUp(host);
                        }
                        reauths += 1;
                        let (status, bad) = probe(&session, &link).await;
                        refused_while_valid = matches!(status, SessionStatus::Valid);
                        if refused_while_valid {
                            // The sign-in works, so a new window would hand back the same cookies.
                            // Try once more with what we have; the cap above ends a real refusal.
                            handle.resume();
                            continue;
                        }
                        let old = session.clone();
                        let old_ref = bad.then_some(&old);
                        let renewed = match &display {
                            Some(d) => d.suspend(|| {
                                eprintln!("Sign-in expired for {host}. Refreshing it...");
                                refresh_session(&link, &settings, old_ref, &store)
                            }),
                            None => refresh_session(&link, &settings, old_ref, &store),
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
        Stop::Refused(host) => {
            eprintln!(
                "SharePoint kept refusing access for {host} even though your sign-in is valid. Progress is saved. \
                 Check that you still have access to this folder, then run the same command again to resume."
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

/// One `()` per line typed on stdin, from a thread of its own (a blocking read cannot be
/// cancelled, and the thread simply ends with the process).
fn spawn_enter_reader() -> tokio::sync::mpsc::UnboundedReceiver<()> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        loop {
            line.clear();
            match std::io::stdin().lock().read_line(&mut line) {
                Ok(n) if n > 0 && tx.send(()).is_ok() => {}
                _ => break,
            }
        }
    });
    rx
}

/// Completes when Enter was pressed; never completes without a reader.
async fn enter_pressed(rx: &mut Option<tokio::sync::mpsc::UnboundedReceiver<()>>) {
    if let Some(r) = rx
        && r.recv().await.is_some()
    {
        return;
    }
    std::future::pending::<()>().await;
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
        let l = plan_lines(
            &t,
            2,
            "/sites/T/Docs/P",
            Path::new("out"),
            Some("Disk space:   needs 3.0 MiB, 9.0 GiB free on D:".into()),
            "automatic (2 to 16, starting at 4)",
        );
        let text = l.join("\n");
        assert!(text.contains("3 files, 3.0 MiB (2 resuming)"), "{text}");
        assert!(text.contains("1 file, 1.0 KiB"), "{text}");
        assert!(text.contains("/sites/T/Docs/P"), "{text}");
        assert!(
            text.contains("Disk space:   needs 3.0 MiB, 9.0 GiB free on D:"),
            "{text}"
        );
        assert!(
            text.contains("Transfers:    automatic (2 to 16, starting at 4)"),
            "{text}"
        );
        let none = plan_lines(&t, 0, "x", Path::new("out"), None, "6 at a time (fixed)").join("\n");
        assert!(!none.contains("resuming"));
        assert!(!none.contains("Disk space"), "{none}");
        assert!(none.contains("6 at a time (fixed)"), "{none}");
    }

    #[test]
    fn transfers_line_follows_the_settings() {
        let mut s = Settings::default();
        assert_eq!(lanes_text(&s), "automatic (2 to 16, starting at 4)");
        // `--parallel N` sets this pair.
        s.auto_tune = false;
        s.parallel_files = 3;
        assert_eq!(lanes_text(&s), "3 at a time (fixed)");
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
