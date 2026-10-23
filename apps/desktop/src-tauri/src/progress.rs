//! Progress outside the window (F5): the taskbar button's progress bar and the tray icon's ring
//! and tooltip.
//!
//! What to show is decided by [`Core`], plain data with no Tauri types, so the rules are unit
//! tested: the phase of the running job (from the queue's status plus a few engine events), the
//! byte counters, and what has already been painted. The functions at the bottom apply a
//! [`Plan`] to the real windows and tray.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use ampsync_core::engine::Event;
use tauri::image::Image;
use tauri::window::{ProgressBarState, ProgressBarStatus};
use tauri::{AppHandle, Manager};
use tiny_skia::{
    BlendMode, Color, FilterQuality, IntSize, LineCap, LineJoin, Paint, PathBuilder, Pixmap, PixmapPaint,
    PremultipliedColorU8, Stroke, Transform,
};

use crate::AppState;
use crate::queue::ItemStatus;
use crate::tray::TRAY_ID;

/// The tray icon is redrawn at most this often while a download is running.
const TRAY_REPAINT: Duration = Duration::from_secs(2);
/// Same rule as the Transfers view: no time left below this speed, or above this many seconds.
const MIN_RATE_FOR_ETA: u64 = 256 * 1024;
const MAX_ETA_SECS: u64 = 30 * 24 * 3600;

// ---------------------------------------------------------------- what to show

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// No download running.
    Idle,
    /// Signing in and listing files; no percentage yet.
    Preparing,
    Running,
    Paused,
    /// Paused until the user signs in again.
    SignIn,
    /// Paused because the disk is full.
    DiskFull,
    /// The last download stopped with an error; stays until the window is looked at.
    Failed,
}

/// The taskbar button's progress bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bar {
    Hidden,
    Indeterminate,
    Normal(u8),
    /// Yellow on Windows.
    Paused(u8),
    /// Red on Windows.
    Error(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tint {
    Active,
    Paused,
}

/// What the tray should show. `ring` is `None` for the plain icon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayView {
    pub ring: Option<(u8, Tint)>,
    pub tooltip: String,
}

pub fn percent(done: u64, total: u64) -> Option<u8> {
    if total == 0 {
        return None;
    }
    let p = (u128::from(done.min(total)) * 100) / u128::from(total);
    u8::try_from(p).ok()
}

/// Seconds left, or `None` when it would be a guess (slow or stalled, absurdly long, nothing
/// left to do).
pub fn eta_secs(done: u64, total: u64, rate_bps: u64) -> Option<u64> {
    if rate_bps < MIN_RATE_FOR_ETA || done >= total {
        return None;
    }
    let secs = (total - done) / rate_bps;
    (secs <= MAX_ETA_SECS).then_some(secs)
}

pub fn eta_text(secs: u64) -> String {
    let mins = secs.div_ceil(60);
    if secs < 45 {
        "under a minute left".to_string()
    } else if mins < 60 {
        format!("{mins} min left")
    } else if mins < 24 * 60 {
        let (h, m) = (mins / 60, mins % 60);
        if m == 0 {
            format!("{h} h left")
        } else {
            format!("{h} h {m:02} min left")
        }
    } else {
        let days = mins.div_ceil(24 * 60);
        format!("{days} {} left", if days == 1 { "day" } else { "days" })
    }
}

pub fn bar_for(phase: Phase, pct: Option<u8>) -> Bar {
    let p = pct.unwrap_or(0);
    match phase {
        Phase::Idle => Bar::Hidden,
        Phase::Preparing => Bar::Indeterminate,
        Phase::Running => Bar::Normal(p),
        Phase::Paused | Phase::SignIn | Phase::DiskFull => Bar::Paused(p),
        Phase::Failed => Bar::Error(p),
    }
}

pub fn tray_view(phase: Phase, pct: Option<u8>, eta: Option<u64>) -> TrayView {
    let plain = |text: &str| TrayView {
        ring: None,
        tooltip: text.to_string(),
    };
    let Some(p) = pct else {
        return plain(match phase {
            Phase::Preparing => "AmpSync \u{b7} getting ready",
            _ => "AmpSync",
        });
    };
    match phase {
        Phase::Idle | Phase::Preparing | Phase::Failed => plain("AmpSync"),
        Phase::Running => TrayView {
            ring: Some((p, Tint::Active)),
            tooltip: match eta {
                Some(s) => format!("AmpSync \u{b7} {p}% \u{b7} {}", eta_text(s)),
                None => format!("AmpSync \u{b7} {p}%"),
            },
        },
        Phase::Paused | Phase::SignIn | Phase::DiskFull => {
            let why = match phase {
                Phase::SignIn => "waiting for sign-in",
                Phase::DiskFull => "disk full",
                _ => "paused",
            };
            TrayView {
                ring: Some((p, Tint::Paused)),
                tooltip: format!("AmpSync \u{b7} {p}% \u{b7} {why}"),
            }
        }
    }
}

/// What has to be done now. Each field is `Some` only when it differs from what is on screen.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub bar: Option<Bar>,
    pub tray: Option<TrayView>,
}

#[derive(Debug)]
pub struct Core {
    phase: Phase,
    done: u64,
    total: u64,
    rate: u64,
    disk_full: bool,
    applied_bar: Bar,
    applied_tray: Option<TrayView>,
    painted_at: Option<Instant>,
}

impl Core {
    pub fn new() -> Self {
        Core {
            phase: Phase::Idle,
            done: 0,
            total: 0,
            rate: 0,
            disk_full: false,
            applied_bar: Bar::Hidden,
            applied_tray: None,
            painted_at: None,
        }
    }

    /// The running job's queue status (`None` when nothing runs). A failure stays on show until
    /// the next job starts or the window is focused.
    pub fn set_current(&mut self, status: Option<ItemStatus>) {
        let next = match status {
            None if self.phase == Phase::Failed => Phase::Failed,
            None => Phase::Idle,
            Some(ItemStatus::Preparing | ItemStatus::Queued) => Phase::Preparing,
            Some(ItemStatus::Running) => Phase::Running,
            Some(ItemStatus::NeedsSignIn) => Phase::SignIn,
            Some(ItemStatus::Paused) if self.disk_full => Phase::DiskFull,
            Some(ItemStatus::Paused) => Phase::Paused,
            Some(ItemStatus::Done | ItemStatus::Cancelled) => Phase::Idle,
            Some(ItemStatus::Failed) => Phase::Failed,
        };
        // A new job starts from nothing.
        if next == Phase::Preparing && !matches!(self.phase, Phase::Preparing | Phase::Running) {
            self.done = 0;
            self.total = 0;
            self.rate = 0;
            self.disk_full = false;
        }
        if next == Phase::Idle {
            self.disk_full = false;
        }
        self.phase = next;
    }

    pub fn on_event(&mut self, ev: &Event) {
        match ev {
            Event::Progress {
                bytes_done,
                bytes_total,
                rate_bps,
            } => {
                self.done = *bytes_done;
                self.total = *bytes_total;
                self.rate = *rate_bps;
            }
            Event::DiskFull { .. } => self.disk_full = true,
            Event::Resumed => self.disk_full = false,
            _ => {}
        }
    }

    /// The job stopped with an error.
    pub fn fail(&mut self) {
        self.phase = Phase::Failed;
    }

    /// The window was focused: the user has seen a failure.
    pub fn acknowledge(&mut self) {
        if self.phase == Phase::Failed {
            self.phase = Phase::Idle;
        }
    }

    pub fn plan(&mut self, now: Instant) -> Plan {
        let pct = percent(self.done, self.total);
        let stalled = !matches!(self.phase, Phase::Running);
        let eta = if stalled {
            None
        } else {
            eta_secs(self.done, self.total, self.rate)
        };
        let mut out = Plan::default();

        let bar = bar_for(self.phase, pct);
        if bar != self.applied_bar {
            self.applied_bar = bar;
            out.bar = Some(bar);
        }

        let view = tray_view(self.phase, pct, eta);
        if self.applied_tray.as_ref() != Some(&view) {
            // A change of look (ring on or off, amber to grey) shows at once; the percentage and
            // the time left follow at most every two seconds.
            let look = |v: &TrayView| v.ring.map(|(_, t)| t);
            let look_changed = self.applied_tray.as_ref().map(look) != Some(look(&view));
            let due = self
                .painted_at
                .is_none_or(|t| now.duration_since(t) >= TRAY_REPAINT);
            if look_changed || due {
                self.painted_at = Some(now);
                self.applied_tray = Some(view.clone());
                out.tray = Some(view);
            }
        }
        out
    }
}

impl Default for Core {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------- the ring

const AMBER: (u8, u8, u8) = (0xf0, 0xa4, 0x3a);
const GREY: (u8, u8, u8) = (0x8a, 0x93, 0xa2);

/// Icon edge in pixels for a display scale: 16, 24 or 32.
pub fn ring_px(scale_factor: f64) -> u32 {
    if scale_factor < 1.25 {
        16
    } else if scale_factor < 1.75 {
        24
    } else {
        32
    }
}

/// A square RGBA (straight alpha) image: `base` scaled into the middle, a faint track around it
/// and an arc over the track from 12 o'clock, clockwise, covering `percent` percent.
pub fn render_ring(
    base: &[u8],
    base_w: u32,
    base_h: u32,
    px: u32,
    percent: u8,
    tint: Tint,
) -> Option<Vec<u8>> {
    let mut pix = Pixmap::new(px, px)?;
    let size = f64::from(px);
    let width = (size * 0.11).max(2.0);
    let gap = (size * 0.04).max(1.0);
    let radius = size / 2.0 - width / 2.0;

    // The icon sits inside the ring.
    if let Some(src) = premultiplied(base, base_w, base_h)
        && base_w > 0
        && base_h > 0
    {
        let inner = ((radius - width / 2.0 - gap) * 2.0 * 0.92).max(4.0);
        let scale = inner / f64::from(base_w.max(base_h));
        let tx = (size - f64::from(base_w) * scale) / 2.0;
        let ty = (size - f64::from(base_h) * scale) / 2.0;
        let paint = PixmapPaint {
            quality: FilterQuality::Bicubic,
            ..PixmapPaint::default()
        };
        pix.draw_pixmap(
            0,
            0,
            src.as_ref(),
            &paint,
            Transform::from_row(scale as f32, 0.0, 0.0, scale as f32, tx as f32, ty as f32),
            None,
        );
    }

    let centre = (size / 2.0) as f32;
    let r = radius as f32;
    let stroke = Stroke {
        width: width as f32,
        line_cap: LineCap::Butt,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    let mut track = Paint {
        anti_alias: true,
        blend_mode: BlendMode::SourceOver,
        ..Paint::default()
    };
    track.set_color(Color::from_rgba8(GREY.0, GREY.1, GREY.2, 140));
    if let Some(circle) = PathBuilder::from_circle(centre, centre, r) {
        pix.stroke_path(&circle, &track, &stroke, Transform::identity(), None);
    }

    let (cr, cg, cb) = match tint {
        Tint::Active => AMBER,
        Tint::Paused => GREY,
    };
    let mut arc_paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    arc_paint.set_color(Color::from_rgba8(cr, cg, cb, 255));
    let sweep = f64::from(percent.min(100)) / 100.0 * std::f64::consts::TAU;
    if percent >= 100 {
        if let Some(circle) = PathBuilder::from_circle(centre, centre, r) {
            pix.stroke_path(&circle, &arc_paint, &stroke, Transform::identity(), None);
        }
    } else if percent > 0 {
        let steps = ((sweep / 0.1).ceil() as u32).max(2);
        let mut pb = PathBuilder::new();
        for i in 0..=steps {
            let a = -std::f64::consts::FRAC_PI_2 + sweep * f64::from(i) / f64::from(steps);
            let (x, y) = (centre + r * a.cos() as f32, centre + r * a.sin() as f32);
            if i == 0 {
                pb.move_to(x, y);
            } else {
                pb.line_to(x, y);
            }
        }
        if let Some(path) = pb.finish() {
            pix.stroke_path(&path, &arc_paint, &stroke, Transform::identity(), None);
        }
    }
    Some(straight(&pix))
}

fn premultiplied(rgba: &[u8], w: u32, h: u32) -> Option<Pixmap> {
    let size = IntSize::from_wh(w, h)?;
    if rgba.len() != (w as usize) * (h as usize) * 4 {
        return None;
    }
    let mut data = Vec::with_capacity(rgba.len());
    for p in rgba.as_chunks::<4>().0 {
        let a = u32::from(p[3]);
        let mul = |c: u8| ((u32::from(c) * a + 127) / 255) as u8;
        data.extend_from_slice(&[mul(p[0]), mul(p[1]), mul(p[2]), p[3]]);
    }
    Pixmap::from_vec(data, size)
}

fn straight(pix: &Pixmap) -> Vec<u8> {
    let mut out = Vec::with_capacity(pix.data().len());
    for p in pix.pixels() {
        let c = PremultipliedColorU8::demultiply(p);
        out.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    out
}

// ---------------------------------------------------------------- applying it

/// Managed state: the rules' memory and the plain icon to restore.
pub struct Tracker {
    core: Mutex<Core>,
    base: Option<(Vec<u8>, u32, u32)>,
}

pub fn setup(app: &AppHandle) {
    let base = app
        .default_window_icon()
        .map(|i| (i.rgba().to_vec(), i.width(), i.height()));
    app.manage(Tracker {
        core: Mutex::new(Core::new()),
        base,
    });
}

fn with_core(app: &AppHandle, f: impl FnOnce(&mut Core)) {
    let tracker = app.state::<Tracker>();
    let plan = {
        let mut core = tracker.core.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut core);
        core.plan(Instant::now())
    };
    apply(app, &tracker, plan);
}

/// The queue changed: follow the running job's status.
pub fn sync(app: &AppHandle) {
    let current = app.state::<AppState>().queue.current_status();
    with_core(app, |c| c.set_current(current));
}

/// Every engine event of the running job.
pub fn on_event(app: &AppHandle, ev: &Event) {
    if matches!(
        ev,
        Event::Progress { .. } | Event::DiskFull { .. } | Event::Resumed
    ) {
        with_core(app, |c| c.on_event(ev));
    }
}

/// The running job stopped with an error (call before its status changes).
pub fn failed(app: &AppHandle) {
    with_core(app, Core::fail);
}

/// The main window got the focus.
pub fn on_focus(app: &AppHandle) {
    with_core(app, Core::acknowledge);
}

fn apply(app: &AppHandle, tracker: &Tracker, plan: Plan) {
    if let Some(bar) = plan.bar
        && let Some(w) = app.get_webview_window("main")
    {
        let (status, progress) = match bar {
            Bar::Hidden => (ProgressBarStatus::None, None),
            Bar::Indeterminate => (ProgressBarStatus::Indeterminate, None),
            Bar::Normal(p) => (ProgressBarStatus::Normal, Some(u64::from(p))),
            Bar::Paused(p) => (ProgressBarStatus::Paused, Some(u64::from(p))),
            Bar::Error(p) => (ProgressBarStatus::Error, Some(u64::from(p))),
        };
        // Desktops without a taskbar progress API (most Linux ones) just ignore this.
        let _ = w.set_progress_bar(ProgressBarState {
            status: Some(status),
            progress,
        });
    }
    if let Some(view) = plan.tray
        && let Some(tray) = app.tray_by_id(TRAY_ID)
    {
        let _ = tray.set_tooltip(Some(view.tooltip));
        match view.ring {
            Some((p, tint)) => {
                let scale = app
                    .get_webview_window("main")
                    .and_then(|w| w.scale_factor().ok())
                    .unwrap_or(1.0);
                let px = ring_px(scale);
                let icon = tracker.base.as_ref().and_then(|(rgba, w, h)| {
                    render_ring(rgba, *w, *h, px, p, tint).map(|d| Image::new_owned(d, px, px))
                });
                if let Some(icon) = icon {
                    let _ = tray.set_icon(Some(icon));
                }
            }
            None => {
                let _ = tray.set_icon(app.default_window_icon().cloned());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64, base: Instant) -> Instant {
        base + Duration::from_millis(ms)
    }

    fn prog(core: &mut Core, done: u64, total: u64, rate: u64) {
        core.on_event(&Event::Progress {
            bytes_done: done,
            bytes_total: total,
            rate_bps: rate,
        });
    }

    #[test]
    fn percent_rounds_down_and_never_passes_100() {
        assert_eq!(percent(0, 0), None);
        assert_eq!(percent(0, 100), Some(0));
        assert_eq!(percent(419, 1000), Some(41));
        assert_eq!(percent(1000, 1000), Some(100));
        assert_eq!(percent(2000, 1000), Some(100));
    }

    #[test]
    fn eta_follows_the_transfers_view_rule() {
        let mb = 1024 * 1024;
        assert_eq!(eta_secs(0, 600 * mb, 256 * 1024 - 1), None);
        assert_eq!(eta_secs(0, 600 * mb, 10 * mb), Some(60));
        assert_eq!(eta_secs(600 * mb, 600 * mb, 10 * mb), None);
        // Over 30 days of work at the minimum speed is no estimate at all.
        assert_eq!(eta_secs(0, 1024 * 1024 * 1024 * 1024, 256 * 1024), None);
    }

    #[test]
    fn eta_text_reads_naturally() {
        assert_eq!(eta_text(10), "under a minute left");
        assert_eq!(eta_text(60), "1 min left");
        assert_eq!(eta_text(9 * 60), "9 min left");
        assert_eq!(eta_text(9 * 60 + 1), "10 min left");
        assert_eq!(eta_text(3600), "1 h left");
        assert_eq!(eta_text(3600 + 14 * 60), "1 h 14 min left");
        assert_eq!(eta_text(30 * 3600), "2 days left");
        assert_eq!(eta_text(24 * 3600), "1 day left");
    }

    #[test]
    fn tooltip_shows_percent_and_time_left_only_while_running() {
        let running = tray_view(Phase::Running, Some(42), Some(9 * 60));
        assert_eq!(running.tooltip, "AmpSync \u{b7} 42% \u{b7} 9 min left");
        assert_eq!(running.ring, Some((42, Tint::Active)));
        assert_eq!(
            tray_view(Phase::Running, Some(42), None).tooltip,
            "AmpSync \u{b7} 42%"
        );
        let paused = tray_view(Phase::Paused, Some(42), Some(60));
        assert_eq!(paused.tooltip, "AmpSync \u{b7} 42% \u{b7} paused");
        assert_eq!(paused.ring, Some((42, Tint::Paused)));
        assert!(
            tray_view(Phase::SignIn, Some(7), None)
                .tooltip
                .ends_with("waiting for sign-in")
        );
        assert!(
            tray_view(Phase::DiskFull, Some(7), None)
                .tooltip
                .ends_with("disk full")
        );
    }

    #[test]
    fn plain_icon_when_idle_failed_or_unknown_size() {
        assert_eq!(
            tray_view(Phase::Idle, None, None),
            TrayView {
                ring: None,
                tooltip: "AmpSync".into()
            }
        );
        assert_eq!(tray_view(Phase::Failed, Some(40), None).ring, None);
        assert_eq!(tray_view(Phase::Running, None, None).ring, None);
        assert_eq!(
            tray_view(Phase::Preparing, None, None).tooltip,
            "AmpSync \u{b7} getting ready"
        );
    }

    #[test]
    fn taskbar_states() {
        assert_eq!(bar_for(Phase::Idle, Some(10)), Bar::Hidden);
        assert_eq!(bar_for(Phase::Preparing, None), Bar::Indeterminate);
        assert_eq!(bar_for(Phase::Running, Some(42)), Bar::Normal(42));
        assert_eq!(bar_for(Phase::Paused, Some(42)), Bar::Paused(42));
        assert_eq!(bar_for(Phase::SignIn, Some(42)), Bar::Paused(42));
        assert_eq!(bar_for(Phase::DiskFull, Some(42)), Bar::Paused(42));
        assert_eq!(bar_for(Phase::Failed, Some(42)), Bar::Error(42));
    }

    #[test]
    fn queue_statuses_map_to_phases() {
        let mut c = Core::new();
        c.set_current(Some(ItemStatus::Preparing));
        assert_eq!(c.phase, Phase::Preparing);
        c.set_current(Some(ItemStatus::Running));
        assert_eq!(c.phase, Phase::Running);
        c.set_current(Some(ItemStatus::NeedsSignIn));
        assert_eq!(c.phase, Phase::SignIn);
        c.set_current(Some(ItemStatus::Paused));
        assert_eq!(c.phase, Phase::Paused);
        c.on_event(&Event::DiskFull {
            path: "x".into(),
            needed: 2,
            free: 1,
        });
        c.set_current(Some(ItemStatus::Paused));
        assert_eq!(c.phase, Phase::DiskFull);
        c.on_event(&Event::Resumed);
        c.set_current(Some(ItemStatus::Running));
        assert_eq!(c.phase, Phase::Running);
        c.set_current(None);
        assert_eq!(c.phase, Phase::Idle);
    }

    #[test]
    fn a_failure_stays_until_the_next_job_or_a_focus() {
        let mut c = Core::new();
        c.set_current(Some(ItemStatus::Running));
        c.fail();
        c.set_current(None);
        assert_eq!(c.phase, Phase::Failed);
        assert_eq!(c.plan(Instant::now()).bar, Some(Bar::Error(0)));
        c.acknowledge();
        assert_eq!(c.phase, Phase::Idle);

        c.fail();
        c.set_current(Some(ItemStatus::Preparing));
        assert_eq!(c.phase, Phase::Preparing);
    }

    #[test]
    fn a_new_job_starts_from_zero() {
        let mut c = Core::new();
        c.set_current(Some(ItemStatus::Running));
        prog(&mut c, 50, 100, 1 << 20);
        c.set_current(None);
        c.set_current(Some(ItemStatus::Preparing));
        assert_eq!(c.plan(Instant::now()).tray.unwrap().ring, None);
    }

    #[test]
    fn taskbar_updates_only_on_change() {
        let t0 = Instant::now();
        let mut c = Core::new();
        c.set_current(Some(ItemStatus::Running));
        prog(&mut c, 10, 100, 0);
        assert_eq!(c.plan(t0).bar, Some(Bar::Normal(10)));
        prog(&mut c, 10, 100, 5);
        assert_eq!(c.plan(at(250, t0)).bar, None);
        prog(&mut c, 11, 100, 5);
        assert_eq!(c.plan(at(500, t0)).bar, Some(Bar::Normal(11)));
        c.set_current(None);
        assert_eq!(c.plan(at(600, t0)).bar, Some(Bar::Hidden));
    }

    #[test]
    fn tray_repaints_at_most_every_two_seconds_but_look_changes_are_immediate() {
        let t0 = Instant::now();
        let mut c = Core::new();
        c.set_current(Some(ItemStatus::Running));
        prog(&mut c, 1, 100, 1 << 20);
        let first = c.plan(t0).tray.expect("first paint");
        assert_eq!(first.ring, Some((1, Tint::Active)));

        // Four updates a second for two seconds: nothing until the interval is up.
        let mut painted = 0;
        for i in 1..8u64 {
            prog(&mut c, 1 + i, 100, 1 << 20);
            if c.plan(at(i * 250, t0)).tray.is_some() {
                painted += 1;
            }
        }
        assert_eq!(painted, 0);
        prog(&mut c, 20, 100, 1 << 20);
        assert_eq!(c.plan(at(2000, t0)).tray.unwrap().ring, Some((20, Tint::Active)));

        // Pausing changes the look: shown at once, inside the interval.
        c.set_current(Some(ItemStatus::Paused));
        assert_eq!(c.plan(at(2100, t0)).tray.unwrap().ring, Some((20, Tint::Paused)));
        // The same view again is not repainted.
        assert_eq!(c.plan(at(9000, t0)).tray, None);

        // Done: back to the plain icon right away.
        c.set_current(None);
        let plain = c.plan(at(9100, t0)).tray.unwrap();
        assert_eq!(plain.ring, None);
        assert_eq!(plain.tooltip, "AmpSync");
    }

    #[test]
    fn ring_sizes() {
        assert_eq!(ring_px(1.0), 16);
        assert_eq!(ring_px(1.25), 24);
        assert_eq!(ring_px(1.5), 24);
        assert_eq!(ring_px(2.0), 32);
        assert_eq!(ring_px(3.0), 32);
    }

    fn px_at(img: &[u8], size: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * size + x) * 4) as usize;
        [img[i], img[i + 1], img[i + 2], img[i + 3]]
    }

    fn base_icon() -> Vec<u8> {
        // A solid blue square, fully opaque.
        std::iter::repeat_n([0x20u8, 0x40, 0xe0, 0xff], 64 * 64)
            .flatten()
            .collect()
    }

    #[test]
    fn the_ring_fills_clockwise_from_the_top() {
        let size = 32;
        let half = render_ring(&base_icon(), 64, 64, size, 50, Tint::Active).unwrap();
        assert_eq!(half.len(), (size * size * 4) as usize);
        let mid = size / 2;
        // 3 o'clock is inside the first half: amber.
        let right = px_at(&half, size, size - 2, mid);
        assert!(
            right[3] > 200 && right[0] > 200 && right[2] < 120,
            "right was {right:?}"
        );
        // 9 o'clock is beyond it: only the faint grey track.
        let left = px_at(&half, size, 1, mid);
        assert!(left[3] > 60 && left[3] < 220, "left was {left:?}");
        assert!(left[2] > left[0] - 20 && left[0] < 190, "left was {left:?}");
        // The base icon shows in the middle.
        let centre = px_at(&half, size, mid, mid);
        assert_eq!(centre, [0x20, 0x40, 0xe0, 0xff]);
        // The corners stay transparent.
        assert_eq!(px_at(&half, size, 0, 0)[3], 0);
    }

    #[test]
    fn empty_and_full_rings() {
        let size = 24;
        let mid = size / 2;
        let none = render_ring(&base_icon(), 64, 64, size, 0, Tint::Active).unwrap();
        let all = render_ring(&base_icon(), 64, 64, size, 100, Tint::Active).unwrap();
        assert!(px_at(&none, size, size - 2, mid)[0] < 190); // grey track only
        for (x, y) in [(size - 2, mid), (1, mid), (mid, 1), (mid, size - 2)] {
            let p = px_at(&all, size, x, y);
            assert!(p[0] > 200 && p[3] > 200, "({x},{y}) was {p:?}");
        }
    }

    #[test]
    fn paused_ring_is_grey_not_amber() {
        let size = 32;
        let img = render_ring(&base_icon(), 64, 64, size, 100, Tint::Paused).unwrap();
        let p = px_at(&img, size, size - 2, size / 2);
        assert!(p[3] > 200 && p[2] > 140 && p[0] < 180, "was {p:?}");
    }

    #[test]
    fn a_bad_base_icon_still_gives_a_ring() {
        let img = render_ring(&[1, 2, 3], 64, 64, 16, 30, Tint::Active).unwrap();
        assert_eq!(img.len(), 16 * 16 * 4);
    }
}
