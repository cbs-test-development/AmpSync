//! Human-readable formatting and parsing: sizes, durations, rates, event lines.

use std::time::Duration;

use ampsync_core::engine::Event;

const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];

/// `1536` -> `1.5 KiB`. Binary units, one decimal above 1 KiB.
pub fn bytes(n: u64) -> String {
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut i = 0;
    // 1023.95 would print as "1024.0", so bump the unit slightly early.
    while v >= 1023.95 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", UNITS[i])
}

/// `12.3 MiB/s`.
pub fn rate(bytes_per_sec: u64) -> String {
    format!("{}/s", bytes(bytes_per_sec))
}

/// `45s`, `3m 05s`, `2h 03m`, `1d 4h`.
pub fn duration(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else if s < 86_400 {
        format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("{}d {}h", s / 86_400, (s % 86_400) / 3600)
    }
}

/// Estimated time left, or `None` when the rate is unknown.
pub fn eta(remaining_bytes: u64, rate_bps: u64) -> Option<Duration> {
    (rate_bps > 0).then(|| Duration::from_secs(remaining_bytes.div_ceil(rate_bps)))
}

/// Parse a bandwidth limit: `500K`, `20M`, `1.5M`, `2GiB`, `100` (bytes), optionally ending in
/// `/s`. Multiples are powers of 1024, matching what [`bytes`] prints.
pub fn parse_rate(input: &str) -> Result<u64, String> {
    let bad = || {
        format!("`{input}` is not a valid rate; use a number with an optional K, M or G, e.g. 500K or 20M")
    };
    let mut s = input.trim().to_ascii_lowercase();
    if let Some(rest) = s.strip_suffix("/s") {
        s = rest.trim_end().to_string();
    }
    if let Some(rest) = s.strip_suffix("ib") {
        s = rest.to_string();
    } else if let Some(rest) = s.strip_suffix('b') {
        s = rest.to_string();
    }
    let (num, mult) = match s.chars().last() {
        Some('k') => (&s[..s.len() - 1], 1024.0),
        Some('m') => (&s[..s.len() - 1], 1024.0 * 1024.0),
        Some('g') => (&s[..s.len() - 1], 1024.0 * 1024.0 * 1024.0),
        Some('t') => (&s[..s.len() - 1], 1024.0f64.powi(4)),
        _ => (s.as_str(), 1.0),
    };
    let n: f64 = num.trim().parse().map_err(|_| bad())?;
    let total = n * mult;
    if !n.is_finite() || total < 1.0 || total > u64::MAX as f64 {
        return Err(bad());
    }
    Ok(total as u64)
}

/// Shorten `s` to at most `max` characters by replacing the start with `...`.
pub fn truncate_left(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    if max <= 3 {
        return s.chars().skip(n - max).collect();
    }
    let tail: String = s.chars().skip(n - (max - 3)).collect();
    format!("...{tail}")
}

/// One engine event as a single line of JSON (the `--json` output).
pub fn event_json_line(event: &Event) -> String {
    serde_json::to_string(event).unwrap_or_else(|e| {
        format!(
            "{{\"type\":\"error\",\"message\":{}}}",
            serde_json::json!(e.to_string())
        )
    })
}

/// `1 file` / `2 files`.
pub fn count(n: u64, singular: &str, plural: &str) -> String {
    format!("{n} {}", if n == 1 { singular } else { plural })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ampsync_core::engine::{Summary, VerifiedBy};
    use ampsync_core::plan::RelPath;

    #[test]
    fn sizes() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1024), "1.0 KiB");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(1_048_575), "1.0 MiB");
        assert_eq!(bytes(5 * 1024 * 1024 * 1024), "5.0 GiB");
        assert_eq!(bytes(u64::MAX), "16777216.0 TiB");
        assert_eq!(rate(2 * 1024 * 1024), "2.0 MiB/s");
    }

    #[test]
    fn durations() {
        assert_eq!(duration(Duration::from_secs(0)), "0s");
        assert_eq!(duration(Duration::from_secs(59)), "59s");
        assert_eq!(duration(Duration::from_secs(185)), "3m 05s");
        assert_eq!(duration(Duration::from_secs(7380)), "2h 03m");
        assert_eq!(duration(Duration::from_secs(100_800)), "1d 4h");
    }

    #[test]
    fn eta_needs_a_rate() {
        assert_eq!(eta(100, 0), None);
        assert_eq!(eta(100, 10), Some(Duration::from_secs(10)));
        assert_eq!(eta(101, 10), Some(Duration::from_secs(11)));
    }

    #[test]
    fn rates_parse() {
        assert_eq!(parse_rate("100"), Ok(100));
        assert_eq!(parse_rate("500K"), Ok(512_000));
        assert_eq!(parse_rate("20M"), Ok(20 * 1024 * 1024));
        assert_eq!(parse_rate("1.5m"), Ok(1_572_864));
        assert_eq!(parse_rate("2GiB"), Ok(2 * 1024 * 1024 * 1024));
        assert_eq!(parse_rate("3 MB/s"), Ok(3 * 1024 * 1024));
        assert_eq!(parse_rate("5b"), Ok(5));
        for bad in ["", "0", "abc", "-5", "K", "1.2.3", "nanM", "0.0001"] {
            assert!(parse_rate(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn truncation_keeps_the_tail() {
        assert_eq!(truncate_left("short.txt", 20), "short.txt");
        assert_eq!(
            truncate_left("a/very/long/folder/name/file.bin", 16),
            "...name/file.bin"
        );
        assert_eq!(truncate_left("abcdef", 3), "def");
        assert_eq!(truncate_left("ünïcödé-file-name", 10).chars().count(), 10);
    }

    #[test]
    fn event_lines_are_one_line_of_json() {
        let e = Event::FileVerified {
            rel_path: RelPath::new("a/b.txt").unwrap(),
            by: VerifiedBy::SizeAndEtag,
        };
        let line = event_json_line(&e);
        assert_eq!(
            line,
            r#"{"type":"file_verified","rel_path":"a/b.txt","by":"size_and_etag"}"#
        );
        assert!(!line.contains('\n'));
        let fin = Event::Finished(Summary {
            downloaded: 1,
            skipped: 0,
            failed: 0,
            bytes: 5,
            elapsed: Duration::from_millis(1200),
            failures: vec![],
        });
        let v: serde_json::Value = serde_json::from_str(&event_json_line(&fin)).unwrap();
        assert_eq!(v["type"], "finished");
        assert_eq!(v["elapsed_ms"], 1200);
        assert_eq!(event_json_line(&Event::Paused), r#"{"type":"paused"}"#);
    }

    #[test]
    fn plurals() {
        assert_eq!(count(1, "file", "files"), "1 file");
        assert_eq!(count(0, "file", "files"), "0 files");
    }
}
