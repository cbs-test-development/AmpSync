//! Progress display for `ampsync get`: one overall bar plus one line per lane on a terminal,
//! plain log lines otherwise. Progress goes to stderr; stdout is kept for results.

use std::collections::HashMap;
use std::io::IsTerminal;

use ampsync_core::engine::Event;
use ampsync_core::plan::RelPath;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};

use crate::human;

const LANE_PATH_WIDTH: usize = 48;

/// Text next to the overall bar: rate and time left.
pub fn overall_message(bytes_done: u64, bytes_total: u64, rate_bps: u64) -> String {
    let remaining = bytes_total.saturating_sub(bytes_done);
    match human::eta(remaining, rate_bps) {
        Some(eta) if remaining > 0 => format!("{}  ETA {}", human::rate(rate_bps), human::duration(eta)),
        _ => human::rate(rate_bps),
    }
}

/// One lane's line: `[2] path/to/file.bin  12.3 MiB / 1.2 GiB ( 1%)`.
pub fn lane_line(lane: u8, rel_path: &str, offset: u64, size: u64) -> String {
    let pct = (offset.min(size) * 100)
        .checked_div(size)
        .map_or(100, |p| p as u32);
    format!(
        "[{}] {:<width$}  {} / {} ({pct:>3}%)",
        u16::from(lane) + 1,
        human::truncate_left(rel_path, LANE_PATH_WIDTH),
        human::bytes(offset),
        human::bytes(size),
        width = LANE_PATH_WIDTH
    )
}

/// What `get` shows while the job runs.
pub struct Display {
    mb: MultiProgress,
    overall: ProgressBar,
    lanes: Vec<ProgressBar>,
    lane_of: HashMap<RelPath, u8>,
    sizes: HashMap<RelPath, u64>,
    files_total: u64,
    files_done: u64,
    interactive: bool,
}

impl Display {
    pub fn new(files_total: u64, bytes_total: u64) -> Self {
        let interactive = std::io::stderr().is_terminal();
        let mb = MultiProgress::with_draw_target(if interactive {
            ProgressDrawTarget::stderr_with_hz(8)
        } else {
            ProgressDrawTarget::hidden()
        });
        let overall = mb.add(ProgressBar::new(bytes_total));
        overall.set_style(
            ProgressStyle::with_template("{prefix} [{wide_bar}] {bytes}/{total_bytes}  {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_bar())
                .progress_chars("=> "),
        );
        overall.set_prefix(files_prefix(0, files_total));
        Display {
            mb,
            overall,
            lanes: Vec::new(),
            lane_of: HashMap::new(),
            sizes: HashMap::new(),
            files_total,
            files_done: 0,
            interactive,
        }
    }

    fn lane_bar(&mut self, lane: u8) -> &ProgressBar {
        while self.lanes.len() <= usize::from(lane) {
            let bar = self.mb.add(ProgressBar::new_spinner());
            bar.set_style(
                ProgressStyle::with_template("{msg}").unwrap_or_else(|_| ProgressStyle::default_spinner()),
            );
            bar.set_message(format!("[{}] idle", self.lanes.len() + 1));
            self.lanes.push(bar);
        }
        &self.lanes[usize::from(lane)]
    }

    /// A line of text that does not corrupt the bars.
    pub fn say(&self, text: &str) {
        if self.interactive {
            let _ = self.mb.println(text);
        } else {
            eprintln!("{text}");
        }
    }

    /// Hide the bars while something else (a sign-in window, a prompt) needs the terminal.
    pub fn suspend<R>(&self, f: impl FnOnce() -> R) -> R {
        self.mb.suspend(f)
    }

    fn file_done(&mut self, rel_path: &RelPath) {
        self.files_done += 1;
        self.overall
            .set_prefix(files_prefix(self.files_done, self.files_total));
        if let Some(lane) = self.lane_of.remove(rel_path) {
            self.lane_bar(lane)
                .set_message(format!("[{}] idle", u16::from(lane) + 1));
        }
    }

    pub fn on_event(&mut self, event: &Event) {
        match event {
            Event::FileStarted {
                rel_path, size, lane, ..
            } => {
                self.lane_of.insert(rel_path.clone(), *lane);
                self.sizes.insert(rel_path.clone(), *size);
                self.lane_bar(*lane)
                    .set_message(lane_line(*lane, rel_path.as_str(), 0, *size));
            }
            Event::LaneProgress {
                lane,
                rel_path,
                offset,
                size,
            } => {
                self.lane_of.insert(rel_path.clone(), *lane);
                self.lane_bar(*lane)
                    .set_message(lane_line(*lane, rel_path.as_str(), *offset, *size));
            }
            Event::Progress {
                bytes_done,
                bytes_total,
                rate_bps,
            } => {
                self.overall.set_length(*bytes_total);
                self.overall.set_position(*bytes_done);
                self.overall
                    .set_message(overall_message(*bytes_done, *bytes_total, *rate_bps));
            }
            Event::FileVerified { rel_path, .. } => {
                if !self.interactive {
                    let size = self.sizes.get(rel_path).copied().unwrap_or(0);
                    eprintln!("done  {rel_path} ({})", human::bytes(size));
                }
                self.sizes.remove(rel_path);
                self.file_done(rel_path);
            }
            Event::FileFailed {
                rel_path,
                error,
                will_retry,
            } => {
                if *will_retry {
                    self.say(&format!("Retrying {rel_path}: {error}"));
                } else {
                    self.say(&format!("Failed {rel_path}: {error}"));
                    self.sizes.remove(rel_path);
                    self.file_done(rel_path);
                }
            }
            Event::Throttled { retry_after } => self.say(&format!(
                "SharePoint asked us to slow down; waiting {}.",
                human::duration(*retry_after)
            )),
            Event::AuthExpired { host } => {
                self.say(&format!("Sign-in expired for {host}; transfers are paused."))
            }
            Event::Paused => self.say("Paused."),
            Event::Resumed => self.say("Resumed."),
            Event::Failed { error } => self.say(&format!("Download stopped: {error}")),
            // Plan-time skips arrive as counts in `Planned`, not as per-file events.
            _ => {}
        }
    }

    /// Remove the bars from the screen.
    pub fn finish(&self) {
        for lane in &self.lanes {
            lane.finish_and_clear();
        }
        self.overall.finish_and_clear();
        let _ = self.mb.clear();
    }
}

fn files_prefix(done: u64, total: u64) -> String {
    format!("files {done}/{total}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overall_message_shows_rate_and_eta() {
        let m = overall_message(0, 100 * 1024 * 1024, 10 * 1024 * 1024);
        assert!(m.contains("10.0 MiB/s"), "{m}");
        assert!(m.contains("ETA 10s"), "{m}");
    }

    #[test]
    fn overall_message_without_rate_or_work() {
        assert_eq!(overall_message(10, 10, 5), "5 B/s");
        assert!(!overall_message(1, 100, 0).contains("ETA"));
    }

    #[test]
    fn lane_line_shape() {
        let l = lane_line(1, "a/b.bin", 512 * 1024, 1024 * 1024);
        assert!(l.starts_with("[2] a/b.bin"), "{l}");
        assert!(l.contains("512.0 KiB / 1.0 MiB"), "{l}");
        assert!(l.ends_with(" 50%)"), "{l}");
        let long = lane_line(0, &"x/".repeat(60), 0, 0);
        assert!(long.contains("..."), "{long}");
        assert!(long.contains("100%"), "{long}");
    }

    #[test]
    fn prefix() {
        assert_eq!(files_prefix(3, 10), "files 3/10");
    }
}
