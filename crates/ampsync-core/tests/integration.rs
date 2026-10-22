//! End-to-end tests: the real `ClassicRemote` and `Engine` against an in-process fake SharePoint.

mod fake;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ampsync_core::Error;
use ampsync_core::engine::{Engine, Event, Settings, Summary, Tuning, VerifiedBy};
use ampsync_core::link;
use ampsync_core::names;
use ampsync_core::plan::{ExistingPolicy, JobSpec, PlannedAction, RelPath};
use ampsync_core::remote::{ClassicRemote, Remote, RemoteEntry, RemoteFile, RemoteRoot};
use ampsync_core::state::{FileStatus, StateDb};
use fake::*;
use futures::StreamExt;
use tokio::sync::broadcast;

const KIB: usize = 1024;

/// Deterministic, non-repetitive test data.
fn data(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| {
            ((i as u32)
                .wrapping_mul(2_654_435_761)
                .wrapping_add(u32::from(seed) * 7919)
                >> 13) as u8
        })
        .collect()
}

fn key(rel: &str) -> String {
    format!("{ROOT}/{rel}")
}

fn settings() -> Settings {
    Settings {
        parallel_files: 4,
        range_size: 256 * 1024,
        large_file_threshold: 256 * 1024,
        max_retries: 3,
        ..Settings::default()
    }
}

fn tuning() -> Tuning {
    Tuning {
        backoff_base: Duration::from_millis(5),
        backoff_max: Duration::from_millis(40),
        progress_interval: Duration::from_millis(20),
        rename_attempts: 3,
        rename_delay: Duration::from_millis(5),
        write_buffer: 64 * 1024,
    }
}

fn remote(fake: &Fake, cookie: &str) -> Arc<ClassicRemote> {
    Arc::new(ClassicRemote::with_origin(
        &fake.session(cookie),
        fake.origin.clone(),
    ))
}

fn engine(fake: &Fake) -> Engine<ClassicRemote> {
    Engine::with_tuning(remote(fake, COOKIE_OLD), settings(), tuning())
}

fn engine_with(fake: &Fake, s: Settings) -> Engine<ClassicRemote> {
    Engine::with_tuning(remote(fake, COOKIE_OLD), s, tuning())
}

async fn guarded<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(60), f)
        .await
        .expect("test timed out")
}

fn rel(s: &str) -> RelPath {
    RelPath::new(s).unwrap()
}

fn terminal_events(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, Event::Finished(_) | Event::Failed { .. } | Event::Cancelled))
        .count()
}

fn drain(rx: &mut broadcast::Receiver<Event>, into: &mut Vec<Event>) {
    loop {
        match rx.try_recv() {
            Ok(e) => into.push(e),
            Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }
}

/// Plan and run a job to completion; returns the summary and every event.
async fn run(engine: &Engine<ClassicRemote>, spec: JobSpec) -> (Summary, Vec<Event>) {
    let plan = engine.plan(&spec).await.unwrap();
    let handle = engine.start(spec, plan);
    let mut rx = handle.events();
    let summary = handle.wait().await.unwrap();
    let mut events = Vec::new();
    drain(&mut rx, &mut events);
    assert_eq!(
        terminal_events(&events),
        1,
        "exactly one terminal event: {events:?}"
    );
    assert!(matches!(events.last(), Some(Event::Finished(_))));
    (summary, events)
}

/// Read events until `pred` matches (collecting everything seen).
async fn until(rx: &mut broadcast::Receiver<Event>, seen: &mut Vec<Event>, pred: impl Fn(&Event) -> bool) {
    loop {
        match rx.recv().await {
            Ok(e) => {
                let hit = pred(&e);
                seen.push(e);
                if hit {
                    return;
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(e) => panic!("event stream ended: {e}"),
        }
    }
}

fn spec(dest: &Path) -> JobSpec {
    JobSpec::new(FOLDER_LINK, dest)
}

fn read(dest: &Path, rel_path: &str) -> Vec<u8> {
    std::fs::read(rel_path.split('/').fold(dest.to_path_buf(), |p, c| p.join(c))).unwrap()
}

fn exists(dest: &Path, rel_path: &str) -> bool {
    rel_path
        .split('/')
        .fold(dest.to_path_buf(), |p, c| p.join(c))
        .exists()
}

fn no_part_files(dest: &Path) -> bool {
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, found);
            } else if p.to_string_lossy().ends_with(".ampsync-part") {
                found.push(p);
            }
        }
    }
    let mut found = Vec::new();
    walk(dest, &mut found);
    found.is_empty()
}

async fn root_of(remote: &ClassicRemote, link_text: &str) -> RemoteRoot {
    remote.resolve(&link::parse(link_text).unwrap()).await.unwrap()
}

async fn list_files(remote: &ClassicRemote, root: &RemoteRoot) -> Vec<RemoteFile> {
    remote
        .walk(root)
        .filter_map(|e| async move {
            match e.unwrap() {
                RemoteEntry::File(f) => Some(f),
                RemoteEntry::Folder { .. } => None,
            }
        })
        .collect()
        .await
}

// ------------------------------------------------------------------------------------------
// ClassicRemote
// ------------------------------------------------------------------------------------------

#[tokio::test]
async fn resolves_sharing_and_folder_links() {
    guarded(async {
        let fake = Fake::start().await;
        fake.add_file(&key("a.txt"), vec![1]);
        let r = remote(&fake, COOKIE_OLD);

        let shared = root_of(&r, SHARING_LINK).await;
        assert_eq!(shared.server_relative_path, ROOT);
        assert_eq!(shared.display_name, "Root");
        assert_eq!(shared.site_url.path(), "/sites/Team");
        assert_eq!(shared.site_url.host_str(), Some("contoso.sharepoint.com"));
        assert!(fake.log().contains(&Logged::Sharing));

        let direct = root_of(&r, FOLDER_LINK).await;
        assert_eq!(direct, shared);

        let missing =
            link::parse("https://contoso.sharepoint.com/sites/Team/Shared%20Documents/Nope").unwrap();
        assert!(matches!(r.resolve(&missing).await, Err(Error::NotFound(_))));
    })
    .await;
}

#[tokio::test]
async fn walk_lists_everything_with_paging_and_skips_forms() {
    guarded(async {
        let fake = Fake::start().await;
        for (i, name) in ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"].iter().enumerate() {
            fake.add_file(&key(name), data(i as u8, 10 + i));
        }
        fake.add_file(&key("sub/x.bin"), data(9, 100));
        fake.add_file(&key("sub/deep/y.bin"), data(8, 50));
        fake.add_folder(&key("empty"));
        fake.add_folder(&format!("{LIBRARY}/Forms"));
        fake.add_file(&format!("{LIBRARY}/Forms/template.dotx"), vec![0]);
        fake.page_size(2);
        let r = remote(&fake, COOKIE_OLD);

        let root = root_of(&r, FOLDER_LINK).await;
        let entries: Vec<RemoteEntry> = r.walk(&root).map(|e| e.unwrap()).collect().await;
        let pos = |p: &str| {
            entries
                .iter()
                .position(|e| match e {
                    RemoteEntry::File(f) => f.rel_path.as_str() == p,
                    RemoteEntry::Folder { rel_path } => rel_path.as_str() == p,
                })
                .unwrap_or_else(|| panic!("{p} missing"))
        };
        assert!(pos("sub") < pos("sub/x.bin"));
        assert!(pos("sub") < pos("sub/deep"));
        assert!(pos("sub/deep") < pos("sub/deep/y.bin"));
        pos("empty");
        assert_eq!(
            entries.len(),
            5 + 2 + 3,
            "5 files at the top, 2 in sub, 3 folders"
        );
        let sizes: Vec<u64> = list_files(&r, &root).await.iter().map(|f| f.size).collect();
        assert!(sizes.contains(&14) && sizes.contains(&100) && sizes.contains(&50));

        // The library root hides its system `Forms` folder.
        let lib = root_of(&r, "https://contoso.sharepoint.com/sites/Team/Shared%20Documents").await;
        let entries: Vec<RemoteEntry> = r.walk(&lib).map(|e| e.unwrap()).collect().await;
        assert!(entries.iter().all(|e| match e {
            RemoteEntry::File(f) => !f.rel_path.as_str().starts_with("Forms"),
            RemoteEntry::Folder { rel_path } => rel_path.as_str() != "Forms",
        }));
        assert!(
            entries
                .iter()
                .any(|e| matches!(e, RemoteEntry::Folder { rel_path } if rel_path.as_str() == "Root"))
        );
    })
    .await;
}

#[tokio::test]
async fn read_range_returns_exact_bytes_and_stat_tracks_etag() {
    guarded(async {
        let fake = Fake::start().await;
        let content = data(3, 1000);
        fake.add_file(&key("f.bin"), content.clone());
        let r = remote(&fake, COOKIE_OLD);
        let root = root_of(&r, FOLDER_LINK).await;
        let file = list_files(&r, &root).await.pop().unwrap();

        for range in [0..1, 100..200, 900..1000, 0..1000] {
            let chunks: Vec<_> = r.read_range(&file, range.clone()).await.unwrap().collect().await;
            let got: Vec<u8> = chunks.into_iter().flat_map(|c| c.unwrap().to_vec()).collect();
            assert_eq!(got, content[range.start as usize..range.end as usize]);
        }
        fake.clear_log();
        assert_eq!(r.read_range(&file, 5..5).await.unwrap().count().await, 0);
        assert!(fake.log().is_empty(), "empty range makes no request");

        let same = r.stat(&file).await.unwrap();
        assert_eq!((same.etag.as_str(), same.size), (file.etag.as_str(), 1000));
        fake.replace_file(&key("f.bin"), data(4, 1200), "\"v2\"");
        let changed = r.stat(&file).await.unwrap();
        assert_eq!((changed.etag.as_str(), changed.size), ("\"v2\"", 1200));
    })
    .await;
}

#[tokio::test]
async fn ignored_range_is_an_error_and_never_yields_bytes() {
    guarded(async {
        let fake = Fake::start().await;
        fake.add_file(&key("f.bin"), data(3, 1000));
        let r = remote(&fake, COOKIE_OLD);
        let root = root_of(&r, FOLDER_LINK).await;
        let file = list_files(&r, &root).await.pop().unwrap();
        fake.ignore_range(true);
        match r.read_range(&file, 100..200).await {
            Err(Error::Http { status: 200, .. }) => {}
            Err(e) => panic!("unexpected error {e}"),
            Ok(_) => panic!("a 200 answer must not be accepted"),
        }
    })
    .await;
}

#[tokio::test]
async fn expired_sessions_map_to_auth_expired() {
    guarded(async {
        let fake = Fake::start().await;
        fake.add_file(&key("f.bin"), data(3, 100));
        let r = remote(&fake, COOKIE_OLD);
        let root = root_of(&r, FOLDER_LINK).await;
        let file = list_files(&r, &root).await.pop().unwrap();

        for redirect in [false, true] {
            fake.redirect_to_login(redirect);
            fake.reject_all_cookies();
            assert!(
                matches!(
                    r.resolve(&link::parse(FOLDER_LINK).unwrap()).await,
                    Err(Error::AuthExpired { .. })
                ),
                "resolve, redirect={redirect}"
            );
            assert!(
                matches!(r.read_range(&file, 0..10).await, Err(Error::AuthExpired { .. })),
                "range, redirect={redirect}"
            );
            assert!(
                matches!(r.stat(&file).await, Err(Error::AuthExpired { .. })),
                "stat, redirect={redirect}"
            );
            let first = r.walk(&root).next().await.unwrap();
            assert!(
                matches!(first, Err(Error::AuthExpired { .. })),
                "walk, redirect={redirect}"
            );
        }
    })
    .await;
}

#[tokio::test]
async fn throttling_maps_to_throttled_with_retry_after() {
    guarded(async {
        let fake = Fake::start().await;
        fake.add_file(&key("f.bin"), data(3, 100));
        let r = remote(&fake, COOKIE_OLD);
        let root = root_of(&r, FOLDER_LINK).await;
        let file = list_files(&r, &root).await.pop().unwrap();
        fake.throttle_downloads(1);
        match r.read_range(&file, 0..10).await {
            Err(Error::Throttled { retry_after }) => assert_eq!(retry_after, Duration::from_secs(1)),
            Err(e) => panic!("unexpected error {e}"),
            Ok(_) => panic!("expected throttling"),
        }
        assert!(r.read_range(&file, 0..10).await.is_ok());
    })
    .await;
}

// ------------------------------------------------------------------------------------------
// Engine
// ------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn downloads_a_tree_and_verifies_every_file() {
    guarded(async {
        let fake = Fake::start().await;
        let big = data(7, 4 * 256 * KIB + 17);
        let small = data(1, 10);
        let medium = data(2, 100 * KIB);
        fake.add_file(&key("a.txt"), small.clone());
        fake.add_file(&key("sub/b.bin"), medium.clone());
        fake.add_file(&key("sub/deep/big.bin"), big.clone());
        fake.add_file(&key("empty.txt"), Vec::new());
        let dest = tempfile::tempdir().unwrap();

        let (summary, events) = run(&engine(&fake), spec(dest.path())).await;

        assert_eq!((summary.downloaded, summary.skipped, summary.failed), (4, 0, 0));
        assert_eq!(summary.bytes, (10 + 100 * KIB + big.len()) as u64);
        assert_eq!(read(dest.path(), "a.txt"), small);
        assert_eq!(read(dest.path(), "sub/b.bin"), medium);
        assert_eq!(read(dest.path(), "sub/deep/big.bin"), big);
        assert_eq!(read(dest.path(), "empty.txt"), Vec::<u8>::new());
        assert!(no_part_files(dest.path()));

        // The remote modification time is applied.
        let mtime = std::fs::metadata(dest.path().join("a.txt"))
            .unwrap()
            .modified()
            .unwrap();
        let secs = mtime.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(secs, 1_714_558_830);

        // Large file: five ranges, none longer than range_size, no overlap.
        let mut ranges: Vec<(u64, u64)> = fake
            .downloads()
            .into_iter()
            .filter(|(k, ..)| k.ends_with("big.bin"))
            .map(|(_, s, e)| (s, e))
            .collect();
        ranges.sort();
        let step = 256 * KIB as u64;
        assert_eq!(
            ranges,
            vec![
                (0, step),
                (step, 2 * step),
                (2 * step, 3 * step),
                (3 * step, 4 * step),
                (4 * step, 4 * step + 17)
            ]
        );

        // Events.
        assert!(matches!(
            events[0],
            Event::Planned {
                files: 4,
                skipped: 0,
                ..
            }
        ));
        let started = events
            .iter()
            .filter(|e| matches!(e, Event::FileStarted { .. }))
            .count();
        assert_eq!(started, 4);
        let verified: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Event::FileVerified { by, .. } => Some(*by),
                _ => None,
            })
            .collect();
        assert_eq!(verified.len(), 4);
        assert!(verified.iter().all(|b| *b == VerifiedBy::SizeAndEtag));
        let last_progress = events
            .iter()
            .rev()
            .find_map(|e| match e {
                Event::Progress {
                    bytes_done,
                    bytes_total,
                    ..
                } => Some((*bytes_done, *bytes_total)),
                _ => None,
            })
            .unwrap();
        assert_eq!(last_progress.0, last_progress.1);
        assert_eq!(last_progress.0, summary.bytes);

        // State database.
        let db = StateDb::open(dest.path()).unwrap();
        for f in db.files().unwrap() {
            assert_eq!(f.status, FileStatus::Verified, "{}", f.rel_path);
            assert_eq!(f.verified_by, Some(VerifiedBy::SizeAndEtag));
            assert!(db.completed_ranges(&f.rel_path).unwrap().is_empty());
        }
        assert_eq!(db.files().unwrap().len(), 4);
        assert!(db.load_job().unwrap().is_some());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn include_and_exclude_globs_select_files() {
    guarded(async {
        let fake = Fake::start().await;
        for (k, seed) in [("a.txt", 1), ("b.PDF", 2), ("sub/c.pdf", 3), ("sub/d.txt", 4)] {
            fake.add_file(&key(k), data(seed, 20));
        }
        let dest = tempfile::tempdir().unwrap();
        let e = engine(&fake);
        let paths = |plan: &ampsync_core::plan::Plan| {
            let mut v: Vec<String> = plan.items.iter().map(|i| i.file.rel_path.to_string()).collect();
            v.sort();
            v
        };

        let mut s = spec(dest.path());
        s.include = vec!["*.pdf".into()];
        assert_eq!(paths(&e.plan(&s).await.unwrap()), ["b.PDF", "sub/c.pdf"]);

        let mut s = spec(dest.path());
        s.exclude = vec!["sub/**".into()];
        assert_eq!(paths(&e.plan(&s).await.unwrap()), ["a.txt", "b.PDF"]);

        let mut s = spec(dest.path());
        s.include = vec!["sub/*".into()];
        s.exclude = vec!["*.txt".into()];
        let (summary, _) = run(&e, s).await;
        assert_eq!(summary.downloaded, 1);
        assert!(exists(dest.path(), "sub/c.pdf"));
        assert!(!exists(dest.path(), "a.txt") && !exists(dest.path(), "sub/d.txt"));

        let mut s = spec(dest.path());
        s.include = vec!["[".into()];
        assert!(matches!(e.plan(&s).await, Err(Error::State(_))));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rerun_skips_unchanged_files_and_refetches_changed_ones() {
    guarded(async {
        let fake = Fake::start().await;
        fake.add_file(&key("a.txt"), data(1, 30));
        fake.add_file(&key("b.txt"), data(2, 40));
        fake.add_file(&key("big.bin"), data(3, 300 * KIB));
        let dest = tempfile::tempdir().unwrap();
        let e = engine(&fake);
        run(&e, spec(dest.path())).await;

        let plan = e.plan(&spec(dest.path())).await.unwrap();
        assert!(
            plan.items
                .iter()
                .all(|i| i.action == PlannedAction::SkipUnchanged)
        );
        assert_eq!(
            (plan.totals.files, plan.totals.skipped, plan.totals.bytes),
            (0, 3, 0)
        );
        fake.clear_log();
        let (summary, events) = run(&e, spec(dest.path())).await;
        assert_eq!((summary.downloaded, summary.skipped, summary.bytes), (0, 3, 0));
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, Event::FileSkipped { .. }))
                .count(),
            3
        );
        assert!(fake.downloads().is_empty(), "nothing was fetched");

        // One file changes on the server, one local file disappears.
        fake.replace_file(&key("a.txt"), data(9, 35), "\"changed\"");
        std::fs::remove_file(dest.path().join("b.txt")).unwrap();
        let plan = e.plan(&spec(dest.path())).await.unwrap();
        let action = |p: &str| {
            plan.items
                .iter()
                .find(|i| i.file.rel_path.as_str() == p)
                .unwrap()
                .action
        };
        assert_eq!(action("a.txt"), PlannedAction::Download);
        assert_eq!(action("b.txt"), PlannedAction::Download);
        assert_eq!(action("big.bin"), PlannedAction::SkipUnchanged);
        let (summary, _) = run(&e, spec(dest.path())).await;
        assert_eq!((summary.downloaded, summary.skipped), (2, 1));
        assert_eq!(read(dest.path(), "a.txt"), data(9, 35));

        let mut s = spec(dest.path());
        s.existing = ExistingPolicy::Redownload;
        let plan = e.plan(&s).await.unwrap();
        assert!(plan.items.iter().all(|i| i.action == PlannedAction::Download));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn throttling_pauses_all_lanes_for_retry_after() {
    guarded(async {
        let fake = Fake::start().await;
        for i in 0..4u8 {
            fake.add_file(&key(&format!("f{i}.bin")), data(i, 50 * KIB));
        }
        fake.throttle_downloads(1);
        let dest = tempfile::tempdir().unwrap();
        let (summary, events) = run(&engine(&fake), spec(dest.path())).await;
        assert_eq!((summary.downloaded, summary.failed), (4, 0));
        let throttled: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Event::Throttled { retry_after } => Some(*retry_after),
                _ => None,
            })
            .collect();
        assert_eq!(throttled, [Duration::from_secs(1)]);
        assert!(
            summary.elapsed >= Duration::from_millis(900),
            "{:?}",
            summary.elapsed
        );
        for i in 0..4u8 {
            assert_eq!(read(dest.path(), &format!("f{i}.bin")), data(i, 50 * KIB));
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transient_server_errors_are_retried_then_fail_after_max_retries() {
    guarded(async {
        let fake = Fake::start().await;
        fake.add_file(&key("f.bin"), data(1, 20 * KIB));
        let dest = tempfile::tempdir().unwrap();

        fake.fail_downloads(2);
        let (summary, events) = run(&engine(&fake), spec(dest.path())).await;
        assert_eq!((summary.downloaded, summary.failed), (1, 0));
        let retries = events
            .iter()
            .filter(|e| matches!(e, Event::FileFailed { will_retry: true, .. }))
            .count();
        assert_eq!(retries, 2);
        assert_eq!(read(dest.path(), "f.bin"), data(1, 20 * KIB));

        // A different folder so nothing is skipped; the server never recovers (max_retries = 3).
        let dest2 = tempfile::tempdir().unwrap();
        fake.fail_downloads(1000);
        let (summary, events) = run(&engine(&fake), spec(dest2.path())).await;
        assert_eq!((summary.downloaded, summary.failed), (0, 1));
        assert_eq!(summary.failures[0].rel_path.as_str(), "f.bin");
        assert!(summary.failures[0].error.contains("500"));
        let retries = events
            .iter()
            .filter(|e| matches!(e, Event::FileFailed { will_retry: true, .. }))
            .count();
        assert_eq!(retries, 3);
        assert!(matches!(
            events
                .iter()
                .rev()
                .find(|e| matches!(e, Event::FileFailed { .. })),
            Some(Event::FileFailed {
                will_retry: false,
                ..
            })
        ));
        assert!(!exists(dest2.path(), "f.bin"));
        let db = StateDb::open(dest2.path()).unwrap();
        assert_eq!(
            db.file(&rel("f.bin")).unwrap().unwrap().status,
            FileStatus::Failed
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expired_session_pauses_the_job_and_resumes_with_a_new_session() {
    guarded(async {
        let fake = Fake::start().await;
        let big = data(5, 6 * 256 * KIB);
        fake.add_file(&key("big.bin"), big.clone());
        fake.expire_session_after(2);
        let dest = tempfile::tempdir().unwrap();
        let e = Engine::with_tuning(
            remote(&fake, COOKIE_OLD),
            Settings {
                parallel_files: 2,
                ..settings()
            },
            tuning(),
        );
        let s = spec(dest.path());
        let plan = e.plan(&s).await.unwrap();
        let handle = e.start(s, plan);
        let mut rx = handle.events();
        let mut seen = Vec::new();
        until(&mut rx, &mut seen, |ev| matches!(ev, Event::Paused)).await;
        assert!(
            seen.iter().any(|e| matches!(e, Event::AuthExpired { .. })),
            "AuthExpired precedes Paused: {seen:?}"
        );
        let before = fake.download_count();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(fake.download_count(), before, "no requests while paused");
        assert!(!exists(dest.path(), "big.bin"));

        handle.replace_session(fake.session(COOKIE_FRESH));
        let summary = handle.wait().await.unwrap();
        drain(&mut rx, &mut seen);
        assert_eq!((summary.downloaded, summary.failed), (1, 0));
        assert_eq!(read(dest.path(), "big.bin"), big);
        assert!(seen.iter().any(|e| matches!(e, Event::Resumed)));
        assert_eq!(
            seen.iter()
                .filter(|e| matches!(e, Event::AuthExpired { .. }))
                .count(),
            1
        );
        assert_eq!(terminal_events(&seen), 1);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manual_pause_and_resume() {
    guarded(async {
        let fake = Fake::start().await;
        for i in 0..3u8 {
            fake.add_file(&key(&format!("f{i}.bin")), data(i, 10 * KIB));
        }
        let dest = tempfile::tempdir().unwrap();
        let e = engine(&fake);
        let s = spec(dest.path());
        let plan = e.plan(&s).await.unwrap();
        fake.clear_log();
        let handle = e.start(s, plan);
        let mut rx = handle.events();
        handle.pause();
        handle.pause(); // idempotent: one Paused event
        tokio::time::sleep(Duration::from_millis(150)).await;
        let in_flight_at_most = fake.downloads().len();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            fake.downloads().len(),
            in_flight_at_most,
            "no new requests while paused"
        );
        handle.resume();
        let summary = handle.wait().await.unwrap();
        let mut seen = Vec::new();
        drain(&mut rx, &mut seen);
        assert_eq!(summary.downloaded, 3);
        assert_eq!(seen.iter().filter(|e| matches!(e, Event::Paused)).count(), 1);
        assert_eq!(seen.iter().filter(|e| matches!(e, Event::Resumed)).count(), 1);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn etag_change_mid_download_is_detected_and_the_file_refetched() {
    guarded(async {
        let fake = Fake::start().await;
        let v1 = data(1, 3 * 256 * KIB);
        let v2 = data(2, 3 * 256 * KIB);
        fake.add_file(&key("doc.bin"), v1);
        fake.swap_after_first_range(&key("doc.bin"), v2.clone(), "\"v2\"");
        let dest = tempfile::tempdir().unwrap();
        let e = engine_with(
            &fake,
            Settings {
                parallel_files: 1,
                ..settings()
            },
        );
        let (summary, events) = run(&e, spec(dest.path())).await;
        assert_eq!((summary.downloaded, summary.failed), (1, 0));
        assert_eq!(
            read(dest.path(), "doc.bin"),
            v2,
            "mixed content must never be kept"
        );
        assert!(events.iter().any(|e| matches!(
            e,
            Event::FileFailed { error, will_retry: true, .. } if error.contains("changed")
        )));
        assert_eq!(fake.downloads().len(), 6, "3 ranges, then all 3 again");
        let db = StateDb::open(dest.path()).unwrap();
        assert_eq!(db.file(&rel("doc.bin")).unwrap().unwrap().etag, "\"v2\"");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_file_that_keeps_changing_fails_after_one_retry() {
    guarded(async {
        let fake = Fake::start().await;
        fake.add_file(&key("moving.bin"), data(1, 40 * KIB));
        fake.add_file(&key("steady.bin"), data(2, 40 * KIB));
        fake.flap_etags(true);
        let dest = tempfile::tempdir().unwrap();
        let (summary, events) = run(&engine(&fake), spec(dest.path())).await;
        // `steady.bin` flaps too (every download bumps its eTag), so both fail; what matters is
        // that each failure is final, reported once, and leaves nothing at the destination.
        assert_eq!(summary.failed, 2);
        assert_eq!(summary.downloaded, 0);
        for name in ["moving.bin", "steady.bin"] {
            assert!(!exists(dest.path(), name));
            let finals = events
                .iter()
                .filter(|e| matches!(e, Event::FileFailed { rel_path, will_retry: false, .. } if rel_path.as_str() == name))
                .count();
            assert_eq!(finals, 1);
        }
        assert!(no_part_files(dest.path()));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_that_ignores_range_fails_the_file_without_writing_it() {
    guarded(async {
        let fake = Fake::start().await;
        fake.add_file(&key("big.bin"), data(1, 3 * 256 * KIB));
        fake.add_file(&key("ok.txt"), data(2, 10));
        fake.ignore_range(true);
        let dest = tempfile::tempdir().unwrap();
        let (summary, _) = run(&engine(&fake), spec(dest.path())).await;
        assert_eq!(summary.failed, 2);
        assert!(
            summary.failures.iter().all(|f| f.error.contains("Range")),
            "{:?}",
            summary.failures
        );
        assert!(!exists(dest.path(), "big.bin") && !exists(dest.path(), "ok.txt"));
        // The part file holds only zeros: ignored-range bodies were never written.
        let part = dest.path().join("big.bin.ampsync-part");
        if part.exists() {
            assert!(std::fs::read(part).unwrap().iter().all(|b| *b == 0));
        }
        let db = StateDb::open(dest.path()).unwrap();
        assert!(db.completed_ranges(&rel("big.bin")).unwrap().is_empty());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_then_rerun_fetches_only_the_missing_ranges() {
    guarded(async {
        let fake = Fake::start().await;
        let step = 256 * KIB as u64;
        let big = data(6, 8 * 256 * KIB);
        fake.add_file(&key("big.bin"), big.clone());
        fake.hang_after(3); // three ranges are served, the fourth request never answers
        let dest = tempfile::tempdir().unwrap();
        let one_lane = Settings {
            parallel_files: 1,
            ..settings()
        };

        let e = engine_with(&fake, one_lane.clone());
        let s = spec(dest.path());
        let plan = e.plan(&s).await.unwrap();
        let handle = e.start(s, plan);
        let mut rx = handle.events();
        while fake.download_count() < 4 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        handle.cancel();
        let result = handle.wait().await;
        assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
        let mut seen = Vec::new();
        drain(&mut rx, &mut seen);
        assert!(matches!(seen.last(), Some(Event::Cancelled)), "{seen:?}");
        assert_eq!(terminal_events(&seen), 1);
        assert!(!exists(dest.path(), "big.bin"));
        assert!(dest.path().join("big.bin.ampsync-part").exists());
        {
            let db = StateDb::open(dest.path()).unwrap();
            assert_eq!(db.completed_ranges(&rel("big.bin")).unwrap(), vec![0..3 * step]);
        }

        // Rerun with a fresh engine (a new process would look the same).
        fake.hang_after(usize::MAX);
        fake.clear_log();
        let e = engine_with(&fake, one_lane);
        let plan = e.plan(&spec(dest.path())).await.unwrap();
        assert_eq!(plan.items[0].action, PlannedAction::Resume { have: 3 * step });
        assert_eq!(plan.totals.bytes, 5 * step);
        let (summary, _) = run(&e, spec(dest.path())).await;
        assert_eq!((summary.downloaded, summary.failed), (1, 0));
        assert_eq!(summary.bytes, 5 * step);
        let mut fetched: Vec<(u64, u64)> = fake.downloads().into_iter().map(|(_, s, e)| (s, e)).collect();
        fetched.sort();
        let expected: Vec<(u64, u64)> = (3..8).map(|i| (i * step, (i + 1) * step)).collect();
        assert_eq!(fetched, expected, "only the missing ranges were requested");
        assert_eq!(read(dest.path(), "big.bin"), big);
        assert!(no_part_files(dest.path()));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_part_file_of_an_older_version_is_not_resumed() {
    guarded(async {
        let fake = Fake::start().await;
        let step = 256 * KIB as u64;
        fake.add_file(&key("big.bin"), data(1, 4 * 256 * KIB));
        fake.hang_after(2);
        let dest = tempfile::tempdir().unwrap();
        let one_lane = Settings {
            parallel_files: 1,
            ..settings()
        };
        let e = engine_with(&fake, one_lane.clone());
        let s = spec(dest.path());
        let plan = e.plan(&s).await.unwrap();
        let handle = e.start(s, plan);
        while fake.download_count() < 3 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        handle.cancel();
        let _ = handle.wait().await;

        let v2 = data(2, 4 * 256 * KIB);
        fake.replace_file(&key("big.bin"), v2.clone(), "\"new\"");
        fake.hang_after(usize::MAX);
        fake.clear_log();
        let e = engine_with(&fake, one_lane);
        let plan = e.plan(&spec(dest.path())).await.unwrap();
        assert_eq!(plan.items[0].action, PlannedAction::Download);
        let (summary, _) = run(&e, spec(dest.path())).await;
        assert_eq!(summary.downloaded, 1);
        assert_eq!(fake.downloads().len(), 4);
        assert_eq!(fake.downloads()[0].1, 0);
        let _ = step;
        assert_eq!(read(dest.path(), "big.bin"), v2);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn names_that_windows_cannot_store_are_downloaded_under_safe_names() {
    guarded(async {
        let fake = Fake::start().await;
        let names = ["a:b?.txt", "CON.txt", "dir.:x/in?side.txt", "plain.txt"];
        for (i, n) in names.iter().enumerate() {
            fake.add_file(&key(n), data(i as u8, 64));
        }
        let dest = tempfile::tempdir().unwrap();
        let (summary, _) = run(&engine(&fake), spec(dest.path())).await;
        assert_eq!((summary.downloaded, summary.failed), (4, 0));
        let db = StateDb::open(dest.path()).unwrap();
        for (i, n) in names.iter().enumerate() {
            let r = rel(n);
            let local = if cfg!(windows) {
                names::sanitize_rel_path(&r)
            } else {
                r.clone()
            };
            assert_eq!(
                std::fs::read(local.to_path(dest.path())).unwrap(),
                data(i as u8, 64),
                "{n}"
            );
            let rec = db.file(&r).unwrap().unwrap();
            if cfg!(windows) && local != r {
                assert_eq!(
                    rec.local_name.as_deref(),
                    Some(local.as_str()),
                    "mapping recorded for {n}"
                );
            } else {
                assert_eq!(rec.local_name, None);
            }
        }
        // A rerun finds the sanitised files again.
        let plan = engine(&fake).plan(&spec(dest.path())).await.unwrap();
        assert!(
            plan.items
                .iter()
                .all(|i| i.action == PlannedAction::SkipUnchanged)
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bandwidth_limit_slows_the_transfer() {
    guarded(async {
        let fake = Fake::start().await;
        fake.add_file(&key("f.bin"), data(1, 1024 * KIB));
        let dest = tempfile::tempdir().unwrap();
        let e = engine_with(
            &fake,
            Settings {
                bandwidth_limit: Some(512 * 1024),
                ..settings()
            },
        );
        let (summary, _) = run(&e, spec(dest.path())).await;
        assert_eq!(summary.downloaded, 1);
        // One second of burst, then 512 KiB/s: 1 MiB takes about a second.
        assert!(
            summary.elapsed >= Duration::from_millis(800),
            "{:?}",
            summary.elapsed
        );
        assert!(summary.elapsed < Duration::from_secs(10));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prune_removes_only_stale_files_inside_the_job_scope() {
    guarded(async {
        let fake = Fake::start().await;
        fake.add_file(&key("keep.txt"), data(1, 10));
        fake.add_file(&key("sub/also.txt"), data(2, 10));
        let dest = tempfile::tempdir().unwrap();
        let e = engine(&fake);
        run(&e, spec(dest.path())).await;

        std::fs::write(dest.path().join("stray.txt"), b"x").unwrap();
        std::fs::create_dir_all(dest.path().join("old/nested")).unwrap();
        std::fs::write(dest.path().join("old/nested/gone.txt"), b"x").unwrap();
        std::fs::write(dest.path().join("mine.pdf"), b"x").unwrap();

        // Without --prune nothing is deleted (D1).
        run(&e, spec(dest.path())).await;
        assert!(exists(dest.path(), "stray.txt"));

        let mut s = spec(dest.path());
        s.prune = true;
        s.exclude = vec!["*.pdf".into()];
        let (summary, _) = run(&e, s).await;
        assert_eq!(summary.failed, 0);
        assert!(!exists(dest.path(), "stray.txt"));
        assert!(!exists(dest.path(), "old"), "emptied directories go too");
        assert!(
            exists(dest.path(), "mine.pdf"),
            "outside the include/exclude scope"
        );
        assert!(exists(dest.path(), "keep.txt") && exists(dest.path(), "sub/also.txt"));
        assert!(dest.path().join(".ampsync").join("state.db").exists());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_terminal_event_is_always_emitted() {
    guarded(async {
        let fake = Fake::start().await;
        fake.add_file(&key("a.txt"), data(1, 10));
        let e = engine(&fake);

        // Success: Finished.
        let ok_dest = tempfile::tempdir().unwrap();
        let (_, events) = run(&e, spec(ok_dest.path())).await;
        assert!(matches!(events.last(), Some(Event::Finished(_))));

        // Failure: the destination cannot hold a state database.
        let plan = e.plan(&spec(ok_dest.path())).await.unwrap();
        let blocker = ok_dest.path().join("a.txt");
        let handle = e.start(JobSpec::new(FOLDER_LINK, &blocker), plan);
        let mut rx = handle.events();
        let result = handle.wait().await;
        assert!(result.is_err());
        let mut events = Vec::new();
        drain(&mut rx, &mut events);
        assert_eq!(terminal_events(&events), 1, "{events:?}");
        assert!(matches!(events.last(), Some(Event::Failed { error }) if !error.is_empty()));
        let json = serde_json::to_string(events.last().unwrap()).unwrap();
        assert!(json.starts_with(r#"{"type":"failed","error":"#), "{json}");

        // Cancel: Cancelled.
        fake.add_file(&key("slow.bin"), data(2, 600 * KIB));
        fake.hang_after(0);
        let dest = tempfile::tempdir().unwrap();
        let s = spec(dest.path());
        let plan = e.plan(&s).await.unwrap();
        let handle = e.start(s, plan);
        let mut rx = handle.events();
        while fake.download_count() < 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        handle.cancel();
        assert!(matches!(handle.wait().await, Err(Error::Cancelled)));
        let mut events = Vec::new();
        drain(&mut rx, &mut events);
        assert_eq!(terminal_events(&events), 1);
        assert!(matches!(events.last(), Some(Event::Cancelled)));
        assert_eq!(
            serde_json::to_string(&Event::Cancelled).unwrap(),
            r#"{"type":"cancelled"}"#
        );
    })
    .await;
}
