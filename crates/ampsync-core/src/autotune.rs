//! Automatic tuning of the number of parallel lanes (D22).
//!
//! AIMD, evaluated once per window of active transfer: add a lane while throughput keeps
//! improving, take it back (and hold) when it does not, halve on throttling (429/503), and stay
//! under SharePoint's per-user request budget. A new lane must beat the window before it twice in a
//! row to be kept, and the hold after a lane that did not help doubles with every miss in a row, so
//! window-to-window noise on a saturated link does not ratchet the count up over a long job. [`Controller`] is a pure state machine driven with
//! explicit timestamps so it can be tested without a clock; the engine feeds it from its tuner task.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

const MIB: u64 = 1024 * 1024;

/// Why the lane count changed (`Event::LanesChanged`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LanesReason {
    /// Throughput improved enough, so one more lane is being tried.
    ProbeUp,
    /// The last added lane did not help; it was taken back and the count is held.
    NoGain,
    /// SharePoint answered 429/503; the lane count was halved.
    Throttled,
    /// The rolling request count passed the soft budget: no more lanes, bigger ranges for new
    /// files. The lane count itself may be unchanged.
    Budget,
}

/// Every threshold and window of the controller. Production uses the defaults; tests shrink the
/// durations so a simulation takes seconds instead of minutes.
#[derive(Debug, Clone)]
pub struct AutoTuneConfig {
    /// Lanes at the start of a job. Default 4.
    pub start_lanes: usize,
    /// Never fewer than this. Default 2.
    pub min_lanes: usize,
    /// Never more than this. Default 16.
    pub max_lanes: usize,
    /// How often the tuner looks at the byte and request counters. Default 250 ms.
    pub sample: Duration,
    /// Length of one evaluation window, counted in active transfer time (not while paused or
    /// waiting out a `Retry-After`). Default 10 s.
    pub window: Duration,
    /// Relative throughput gain a new lane must bring to be kept. Default 0.05 (5%).
    pub min_gain: f64,
    /// No growth for this long after a lane was taken back. Default 60 s. Each further probe in a
    /// row that does not help doubles it, up to 8 times as long, so noise on a saturated link
    /// cannot ratchet the count upwards; a clear gain (twice `min_gain`) starts over.
    pub hold_no_gain: Duration,
    /// No growth for this long after the `Retry-After` wait that follows a throttle. Default 2 min.
    pub hold_throttled: Duration,
    /// A burst of 429s from several lanes halves the lanes once: further throttles within this
    /// time (or the `Retry-After`, if longer) of a halving only extend the hold. Default 1 s.
    pub throttle_guard: Duration,
    /// Rolling window over which requests are counted. Default 5 min.
    pub request_window: Duration,
    /// SharePoint's per-user request limit per `request_window`. Default 3,000.
    pub request_limit: u64,
    /// Fraction of `request_limit` above which growth stops and ranges grow. Default 0.8 (2,400).
    pub budget_fraction: f64,
    /// Range size used for files started while over budget (never smaller than the configured
    /// `range_size`). Default 32 MiB.
    pub budget_range_size: u64,
}

impl Default for AutoTuneConfig {
    fn default() -> Self {
        AutoTuneConfig {
            start_lanes: 4,
            min_lanes: 2,
            max_lanes: 16,
            sample: Duration::from_millis(250),
            window: Duration::from_secs(10),
            min_gain: 0.05,
            hold_no_gain: Duration::from_secs(60),
            hold_throttled: Duration::from_secs(120),
            throttle_guard: Duration::from_secs(1),
            request_window: Duration::from_secs(300),
            request_limit: 3000,
            budget_fraction: 0.8,
            budget_range_size: 32 * MIB,
        }
    }
}

impl AutoTuneConfig {
    /// Request count in `request_window` above which the budget rules apply.
    pub fn soft_limit(&self) -> u64 {
        (self.request_limit as f64 * self.budget_fraction) as u64
    }

    fn bounds(&self) -> (usize, usize) {
        let min = self.min_lanes.max(1);
        (min, self.max_lanes.max(min))
    }

    /// The most lanes a job can ever use.
    pub(crate) fn max(&self) -> usize {
        self.bounds().1
    }

    /// The lane count a job starts with, inside the bounds.
    pub(crate) fn initial_lanes(&self) -> usize {
        let (min, max) = self.bounds();
        self.start_lanes.clamp(min, max)
    }
}

/// A decision of the controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Change {
    pub lanes: usize,
    pub reason: LanesReason,
}

/// Once over budget, stay there until the count falls to this fraction of the soft limit.
const BUDGET_EXIT: f64 = 0.75;

/// A lane being tried: `reference` is the throughput of the window before it was added, `first`
/// the throughput of its first window once that beat the reference.
#[derive(Debug, Clone, Copy)]
struct Probe {
    reference: f64,
    first: Option<f64>,
}

/// The hold after a probe that did not help grows to at most `hold_no_gain << MAX_HOLD_DOUBLINGS`.
const MAX_HOLD_DOUBLINGS: u32 = 3;

pub(crate) struct Controller {
    cfg: AutoTuneConfig,
    lanes: usize,
    /// The lane being tried, while it is judged.
    probe: Option<Probe>,
    hold_until: Option<Instant>,
    guard_until: Option<Instant>,
    over_budget: bool,
    /// Probes in a row that were taken back (lengthens the next hold).
    misses: u32,
}

impl Controller {
    pub(crate) fn new(cfg: AutoTuneConfig) -> Self {
        Controller {
            lanes: cfg.initial_lanes(),
            cfg,
            probe: None,
            hold_until: None,
            guard_until: None,
            over_budget: false,
            misses: 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn lanes(&self) -> usize {
        self.lanes
    }

    /// Over the soft request budget: new files should use the larger range size.
    pub(crate) fn over_budget(&self) -> bool {
        self.over_budget
    }

    /// One evaluation window ended. `throughput` is bytes per second of active transfer, `requests`
    /// the number of requests in the rolling request window.
    pub(crate) fn on_window(&mut self, now: Instant, throughput: f64, requests: u64) -> Option<Change> {
        let (min, max) = self.cfg.bounds();
        let soft = self.cfg.soft_limit();
        let mut entered_budget = false;
        if !self.over_budget && requests > soft {
            self.over_budget = true;
            entered_budget = true;
        } else if self.over_budget && requests as f64 <= soft as f64 * BUDGET_EXIT {
            self.over_budget = false;
        }

        if let Some(probe) = self.probe.take() {
            let gain = self.cfg.min_gain;
            let improved = throughput > 0.0 && throughput >= probe.reference * (1.0 + gain);
            match (improved, probe.first) {
                (false, _) if self.lanes > min => {
                    self.lanes -= 1;
                    let hold = self.cfg.hold_no_gain * (1 << self.misses.min(MAX_HOLD_DOUBLINGS));
                    self.misses += 1;
                    self.hold_until = Some(now + hold);
                    return Some(Change {
                        lanes: self.lanes,
                        reason: LanesReason::NoGain,
                    });
                }
                (false, _) => {}
                // One good window can be luck: keep the lane and judge it once more.
                (true, None) => {
                    self.probe = Some(Probe {
                        first: Some(throughput),
                        ..probe
                    });
                    return entered_budget.then_some(Change {
                        lanes: self.lanes,
                        reason: LanesReason::Budget,
                    });
                }
                // Kept. A clear gain in both windows also forgives earlier misses.
                (true, Some(first)) => {
                    if first.min(throughput) >= probe.reference * (1.0 + 2.0 * gain) {
                        self.misses = 0;
                    }
                }
            }
        }
        if entered_budget {
            return Some(Change {
                lanes: self.lanes,
                reason: LanesReason::Budget,
            });
        }
        if self.over_budget || self.lanes >= max || self.hold_until.is_some_and(|h| now < h) {
            return None;
        }
        self.probe = Some(Probe {
            reference: throughput,
            first: None,
        });
        self.lanes += 1;
        Some(Change {
            lanes: self.lanes,
            reason: LanesReason::ProbeUp,
        })
    }

    /// A request was answered 429/503 with this `Retry-After`.
    pub(crate) fn on_throttle(&mut self, now: Instant, retry_after: Duration) -> Option<Change> {
        let (min, _) = self.cfg.bounds();
        self.probe = None;
        let until = now + retry_after + self.cfg.hold_throttled;
        if self.hold_until.is_none_or(|h| until > h) {
            self.hold_until = Some(until);
        }
        if self.guard_until.is_some_and(|g| now < g) {
            return None;
        }
        self.guard_until = Some(now + retry_after.max(self.cfg.throttle_guard));
        let halved = (self.lanes / 2).max(min);
        if halved == self.lanes {
            return None;
        }
        self.lanes = halved;
        Some(Change {
            lanes: halved,
            reason: LanesReason::Throttled,
        })
    }
}

/// Requests seen in a rolling window.
pub(crate) struct RequestLog {
    window: Duration,
    stamps: Mutex<VecDeque<Instant>>,
    total: AtomicU64,
}

impl RequestLog {
    pub(crate) fn new(window: Duration) -> Self {
        RequestLog {
            window,
            stamps: Mutex::default(),
            total: AtomicU64::new(0),
        }
    }

    pub(crate) fn record(&self) {
        self.total.fetch_add(1, Ordering::SeqCst);
        let now = Instant::now();
        let mut s = self.stamps.lock().unwrap_or_else(|p| p.into_inner());
        s.push_back(now);
        Self::trim(&mut s, now, self.window);
    }

    /// Requests since the job started (never trimmed).
    pub(crate) fn total(&self) -> u64 {
        self.total.load(Ordering::SeqCst)
    }

    /// Requests in the rolling window.
    pub(crate) fn count(&self) -> u64 {
        let now = Instant::now();
        let mut s = self.stamps.lock().unwrap_or_else(|p| p.into_inner());
        Self::trim(&mut s, now, self.window);
        s.len() as u64
    }

    fn trim(s: &mut VecDeque<Instant>, now: Instant, window: Duration) {
        while s.front().is_some_and(|t| now.duration_since(*t) > window) {
            s.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: f64 = 1_000_000.0;

    /// Drive the controller through `windows` evaluation windows against a link that carries
    /// `per_lane` bytes/s per lane up to `link` bytes/s in total.
    fn simulate(
        c: &mut Controller,
        start: Instant,
        windows: usize,
        per_lane: f64,
        link: f64,
        window: Duration,
    ) -> Vec<usize> {
        let mut trace = Vec::new();
        for i in 1..=windows {
            let now = start + window * i as u32;
            let tput = (c.lanes() as f64 * per_lane).min(link);
            c.on_window(now, tput, 0);
            trace.push(c.lanes());
        }
        trace
    }

    #[test]
    fn climbs_to_the_bandwidth_optimum_and_stays_within_one_lane_of_it() {
        let cfg = AutoTuneConfig::default();
        let window = cfg.window;
        let mut c = Controller::new(cfg);
        // The link saturates at 9 lanes of 2 MB/s.
        let trace = simulate(&mut c, Instant::now(), 120, 2.0 * MB, 18.0 * MB, window);
        assert_eq!(
            trace[..9],
            [5, 5, 6, 6, 7, 7, 8, 8, 9],
            "one lane per two windows while it helps"
        );
        assert!(
            trace.iter().all(|l| *l <= 10),
            "one probe above the optimum at most: {trace:?}"
        );
        assert!(trace[20..].iter().all(|l| (9..=10).contains(l)), "{trace:?}");
        assert_eq!(*trace.iter().max().unwrap(), 10);
    }

    #[test]
    fn a_probe_that_does_not_help_is_taken_back_and_held() {
        let cfg = AutoTuneConfig::default();
        let (window, hold) = (cfg.window, cfg.hold_no_gain);
        let mut c = Controller::new(cfg);
        let t0 = Instant::now();
        // 4 lanes already saturate the link.
        assert_eq!(
            c.on_window(t0 + window, 40.0 * MB, 0),
            Some(Change {
                lanes: 5,
                reason: LanesReason::ProbeUp
            })
        );
        assert_eq!(
            c.on_window(t0 + window * 2, 41.0 * MB, 0), // +2.5% is not enough
            Some(Change {
                lanes: 4,
                reason: LanesReason::NoGain
            })
        );
        // Held: nothing changes until the hold is over.
        let end = t0 + window * 2 + hold;
        let mut now = t0 + window * 2;
        while now + window < end {
            now += window;
            assert_eq!(c.on_window(now, 40.0 * MB, 0), None);
        }
        assert_eq!(
            c.on_window(end, 40.0 * MB, 0).map(|c| c.reason),
            Some(LanesReason::ProbeUp)
        );
    }

    #[test]
    fn noise_on_a_saturated_link_does_not_ratchet_the_lanes_up() {
        let cfg = AutoTuneConfig::default();
        let window = cfg.window;
        let mut c = Controller::new(cfg);
        let mut rng = fastrand::Rng::with_seed(7);
        let t0 = Instant::now();
        // 6 lanes of 2 MB/s fill the link, and every window is off by up to 5% either way. An hour.
        let mut trace = Vec::new();
        for i in 1..=360u32 {
            let ideal = (c.lanes() as f64 * 2.0 * MB).min(12.0 * MB);
            c.on_window(t0 + window * i, ideal * (0.95 + rng.f64() * 0.1), 0);
            trace.push(c.lanes());
        }
        let peak = *trace.iter().max().unwrap();
        assert!((6..=8).contains(&peak), "peaked at {peak}: {trace:?}");
        let above = trace[60..].iter().filter(|l| **l > 7).count();
        assert!(
            above * 10 < 300,
            "{above} of 300 windows more than one lane above: {trace:?}"
        );
    }

    #[test]
    fn misses_in_a_row_lengthen_the_hold_up_to_eight_times() {
        let cfg = AutoTuneConfig::default();
        let (window, hold) = (cfg.window, cfg.hold_no_gain);
        let mut c = Controller::new(cfg);
        let t0 = Instant::now();
        // Already saturated: every probe misses. Record when each probe starts.
        let mut probes = Vec::new();
        for i in 1..=400u32 {
            let now = t0 + window * i;
            if let Some(Change {
                reason: LanesReason::ProbeUp,
                ..
            }) = c.on_window(now, 40.0 * MB, 0)
            {
                probes.push(now);
            }
        }
        // Between two probes: the probe's own window, then the hold.
        let holds: Vec<Duration> = probes.windows(2).map(|w| w[1] - w[0] - window).collect();
        assert!(holds.len() >= 5, "{holds:?}");
        assert!(holds[0] >= hold && holds[0] < hold + window, "{holds:?}");
        assert!(holds[1] >= hold * 2 && holds[2] >= hold * 4, "{holds:?}");
        assert!(holds.iter().all(|h| *h < hold * 8 + window), "capped: {holds:?}");
    }

    #[test]
    fn a_clear_gain_forgives_earlier_misses() {
        let cfg = AutoTuneConfig::default();
        let (window, hold) = (cfg.window, cfg.hold_no_gain);
        let mut c = Controller::new(cfg);
        let mut now = Instant::now();
        let mut step = |c: &mut Controller, tput: f64| {
            now += window;
            (now, c.on_window(now, tput, 0).map(|c| c.reason))
        };
        // Two misses in a row: the next hold would be four times the base.
        assert_eq!(step(&mut c, 40.0 * MB).1, Some(LanesReason::ProbeUp));
        assert_eq!(step(&mut c, 40.0 * MB).1, Some(LanesReason::NoGain));
        while step(&mut c, 40.0 * MB).1.is_none() {}
        assert_eq!(step(&mut c, 40.0 * MB).1, Some(LanesReason::NoGain));
        while step(&mut c, 40.0 * MB).1.is_none() {}
        // The probe that just started brings +25% twice: kept, and the misses are forgiven.
        assert_eq!(step(&mut c, 50.0 * MB).1, None);
        assert_eq!(step(&mut c, 50.0 * MB).1, Some(LanesReason::ProbeUp));
        let (missed_at, r) = step(&mut c, 50.0 * MB);
        assert_eq!(r, Some(LanesReason::NoGain));
        let next = loop {
            let (t, r) = step(&mut c, 50.0 * MB);
            if r.is_some() {
                break t;
            }
        };
        assert!(next - missed_at <= hold + window, "back to the base hold");
    }

    #[test]
    fn a_gain_of_exactly_the_threshold_counts() {
        let mut c = Controller::new(AutoTuneConfig::default());
        let t = Instant::now();
        c.on_window(t, 100.0, 0);
        assert_eq!(c.on_window(t + Duration::from_secs(10), 105.0, 0), None);
        let up = c.on_window(t + Duration::from_secs(20), 105.0, 0);
        assert_eq!(up.map(|c| (c.lanes, c.reason)), Some((6, LanesReason::ProbeUp)));
    }

    #[test]
    fn throttling_halves_holds_and_collapses_bursts() {
        let cfg = AutoTuneConfig::default();
        let (window, hold) = (cfg.window, cfg.hold_throttled);
        let mut c = Controller::new(cfg);
        let t0 = Instant::now();
        for i in 1..=16 {
            c.on_window(t0 + window * i, 100.0 * MB * f64::from(i), 0);
        }
        assert_eq!(c.lanes(), 12);
        let now = t0 + window * 17;
        let retry = Duration::from_secs(5);
        assert_eq!(
            c.on_throttle(now, retry),
            Some(Change {
                lanes: 6,
                reason: LanesReason::Throttled
            })
        );
        // The other lanes' 429s of the same burst do not halve again.
        assert_eq!(c.on_throttle(now + Duration::from_millis(300), retry), None);
        assert_eq!(c.lanes(), 6);
        // Held for the retry wait plus the hold.
        let free = now + retry + hold;
        assert_eq!(c.on_window(free - Duration::from_secs(1), 1.0, 0), None);
        assert_eq!(
            c.on_window(free + Duration::from_secs(1), 1.0, 0)
                .map(|c| c.reason),
            Some(LanesReason::ProbeUp)
        );
        // A later throttle halves again, but never below the minimum.
        let mut now = free + Duration::from_secs(60);
        for _ in 0..6 {
            c.on_throttle(now, Duration::ZERO);
            now += Duration::from_secs(10);
        }
        assert_eq!(c.lanes(), 2);
        assert_eq!(c.on_throttle(now, Duration::ZERO), None);
    }

    #[test]
    fn over_budget_stops_growth_and_signals_once() {
        let cfg = AutoTuneConfig::default();
        assert_eq!(cfg.soft_limit(), 2400);
        let mut c = Controller::new(cfg);
        let t = Instant::now();
        let w = Duration::from_secs(10);
        assert_eq!(c.on_window(t, 10.0, 2000).map(|c| c.lanes), Some(5));
        // Throughput keeps improving, but the request count crosses the soft limit.
        assert_eq!(
            c.on_window(t + w, 20.0, 2401),
            Some(Change {
                lanes: 5,
                reason: LanesReason::Budget
            })
        );
        assert!(c.over_budget());
        assert_eq!(
            c.on_window(t + w * 2, 30.0, 2500),
            None,
            "no more lanes and no repeated event"
        );
        assert_eq!(
            c.on_window(t + w * 3, 30.0, 2000),
            None,
            "hysteresis: still over budget"
        );
        assert!(c.over_budget());
        assert_eq!(c.on_window(t + w * 4, 30.0, 1700).map(|c| c.lanes), Some(6));
        assert!(!c.over_budget());
    }

    #[test]
    fn stays_inside_the_bounds() {
        let mut c = Controller::new(AutoTuneConfig::default());
        let t = Instant::now();
        for i in 1..=40 {
            c.on_window(t + Duration::from_secs(10 * i), 1e6 * 1.2f64.powi(i as i32), 0);
        }
        assert_eq!(c.lanes(), 16, "at most 16 lanes however much it helps");
        let cfg = AutoTuneConfig {
            start_lanes: 99,
            min_lanes: 0,
            ..AutoTuneConfig::default()
        };
        assert_eq!(Controller::new(cfg).lanes(), 16);
    }

    #[test]
    fn reasons_serialise_snake_case() {
        let json = serde_json::to_string(&[
            LanesReason::ProbeUp,
            LanesReason::NoGain,
            LanesReason::Throttled,
            LanesReason::Budget,
        ])
        .unwrap();
        assert_eq!(json, r#"["probe_up","no_gain","throttled","budget"]"#);
    }

    #[test]
    fn request_log_is_a_rolling_window() {
        let log = RequestLog::new(Duration::from_millis(60));
        for _ in 0..5 {
            log.record();
        }
        assert_eq!(log.count(), 5);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(log.count(), 0);
        log.record();
        assert_eq!(log.count(), 1);
    }
}
