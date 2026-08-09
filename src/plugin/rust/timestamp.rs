//! Timestamp normalization. Detects common formats and converts to epoch
//! nanoseconds.

use crate::pipeline::parser::ParsedLine;
use crate::plugin::Plugin;

pub struct Timestamp;

impl Plugin for Timestamp {
    fn name(&self) -> &str {
        "timestamp"
    }

    fn detect(&self, _sample: &[&str]) -> f32 {
        0.1
    }

    fn parse(&self, line: &mut ParsedLine) {
        if line.timestamp_ns.is_some() {
            return;
        }
        // Honor a `ts`/`time`/`timestamp` field set by an earlier plugin.
        if let Some(crate::pipeline::parser::FieldValue::Str(s)) =
            line.fields.get("ts").or_else(|| line.fields.get("time")).or_else(|| line.fields.get("timestamp"))
            && let Some(ns) = parse_one(s)
        {
            line.timestamp_ns = Some(ns);
            return;
        }
        // Otherwise try the start of the raw line.
        if let Some(ns) = parse_one(line.raw.trim_start()) {
            line.timestamp_ns = Some(ns);
        }
    }
}

/// Try to parse a timestamp from the start of `s`. Returns epoch nanoseconds
/// on success. Supports epoch (s/ms/us/ns), ISO8601/RFC3339, and syslog
/// date formats.
pub fn parse_one(s: &str) -> Option<i64> {
    // Pure epoch seconds / millis / micros / nanos.
    if let Some(ns) = parse_epoch(s) {
        return Some(ns);
    }
    // ISO8601 / RFC3339: 2023-11-15T12:30:45(.123)?(Z|+HH:MM)?
    if let Some(ns) = parse_iso(s) {
        return Some(ns);
    }
    // Syslog date: "Nov 15 12:30:45" or "2023-11-15T12:30:45"
    if let Some(ns) = parse_syslog_date(s) {
        return Some(ns);
    }
    None
}

fn parse_epoch(s: &str) -> Option<i64> {
    let end = s
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(s.len());
    let tok = &s[..end];
    if tok.is_empty() {
        return None;
    }
    let v: f64 = tok.parse().ok()?;
    // Heuristic: choose unit by magnitude. Only treat as epoch if the
    // value is >= 1e9 (year 2001 in seconds). Smaller numbers are likely
    // part of a date string (e.g. "2023-11-15..."), not an epoch.
    let ns = if v >= 1e18 {
        v // already ns
    } else if v >= 1e15 {
        v * 1e3 // micros
    } else if v >= 1e12 {
        v * 1e6 // millis
    } else if v >= 1e9 {
        v * 1e9 // seconds
    } else {
        return None;
    };
    Some(ns as i64)
}

/// Parse ISO8601 / RFC3339 timestamps.
/// Examples: 2023-11-15T12:30:45Z, 2023-11-15T12:30:45.123Z,
///           2023-11-15T12:30:45+02:00, 2023-11-15 12:30:45
fn parse_iso(s: &str) -> Option<i64> {
    // Minimum: "2023-11-15T12:30:45" = 19 chars
    if s.len() < 19 {
        return None;
    }
    let b = s.as_bytes();

    // Year: 4 digits
    let year: i32 = parse_digits(s, 0, 4)?;
    if b[4] != b'-' {
        return None;
    }
    // Month: 2 digits
    let month: i32 = parse_digits(s, 5, 2)?;
    if b[7] != b'-' {
        return None;
    }
    // Day: 2 digits
    let day: i32 = parse_digits(s, 8, 2)?;

    // Separator: T or space
    if b[10] != b'T' && b[10] != b' ' {
        return None;
    }

    // Hour: 2 digits
    let hour: i32 = parse_digits(s, 11, 2)?;
    if b[13] != b':' {
        return None;
    }
    // Minute: 2 digits
    let minute: i32 = parse_digits(s, 14, 2)?;
    if b[16] != b':' {
        return None;
    }
    // Second: 2 digits
    let second: i32 = parse_digits(s, 17, 2)?;

    // Optional fractional seconds: .123...
    let mut pos = 19;
    let mut nanos: i64 = 0;
    if pos < s.len() && b[pos] == b'.' {
        pos += 1;
        let frac_start = pos;
        while pos < s.len() && b[pos].is_ascii_digit() {
            pos += 1;
        }
        let frac = &s[frac_start..pos];
        // Pad/truncate to 9 digits (nanoseconds)
        let mut ns_str = frac.to_string();
        ns_str.truncate(9);
        while ns_str.len() < 9 {
            ns_str.push('0');
        }
        nanos = ns_str.parse().unwrap_or(0);
    }

    // Optional timezone: Z or +HH:MM or -HH:MM
    let mut tz_offset_secs: i64 = 0;
    if pos < s.len() {
        match b[pos] {
            b'Z' | b'z' => {
                // UTC, no offset
            }
            b'+' | b'-' => {
                let sign = if b[pos] == b'+' { 1 } else { -1 };
                if pos + 6 > s.len() {
                    return None;
                }
                let tz_hour: i64 = parse_digits(s, pos + 1, 2)? as i64;
                if b[pos + 3] != b':' {
                    return None;
                }
                let tz_min: i64 = parse_digits(s, pos + 4, 2)? as i64;
                tz_offset_secs = sign * (tz_hour * 3600 + tz_min * 60);
            }
            _ => {
                // No timezone — assume UTC
            }
        }
    }

    let epoch = civil_to_epoch(year, month, day, hour, minute, second);
    let ns = (epoch - tz_offset_secs) * 1_000_000_000 + nanos;
    Some(ns)
}

/// Parse syslog-style date: "Nov 15 12:30:45" or "2023-11-15T12:30:45"
/// Also handles "Nov  1 12:30:45" (padded day).
fn parse_syslog_date(s: &str) -> Option<i64> {
    // Check for month abbreviation at start.
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun",
        "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let s_trimmed = s;
    if s_trimmed.len() < 15 {
        return None;
    }
    let month_str = &s_trimmed[..3];
    let month_idx = months.iter().position(|m| *m == month_str)?;
    let month = (month_idx + 1) as i32;

    // "Nov 15 12:30:45" — day may be space-padded
    // After "Nov" there's a space, then day (1 or 2 digits), then space
    let rest = &s_trimmed[3..].trim_start();
    let day_end = rest.find(' ').unwrap_or(rest.len());
    let day: i32 = rest[..day_end].trim().parse().ok()?;

    // Time: HH:MM:SS
    let time_part = rest[day_end..].trim_start();
    if time_part.len() < 8 {
        return None;
    }
    let hour: i32 = time_part[..2].parse().ok()?;
    if time_part.as_bytes()[2] != b':' {
        return None;
    }
    let minute: i32 = time_part[3..5].parse().ok()?;
    if time_part.as_bytes()[5] != b':' {
        return None;
    }
    let second: i32 = time_part[6..8].parse().ok()?;

    // Syslog dates have no year — assume current year.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    let now_secs = now.as_secs() as i64;
    let current_year = (now_secs / 31_536_000 + 1970) as i32;

    let epoch = civil_to_epoch(current_year, month, day, hour, minute, second);
    Some(epoch * 1_000_000_000)
}

/// Parse `len` digits from `s` starting at `offset`. Returns the integer.
fn parse_digits(s: &str, offset: usize, len: usize) -> Option<i32> {
    if offset + len > s.len() {
        return None;
    }
    let sub = &s[offset..offset + len];
    sub.parse().ok()
}

/// Convert civil (year, month, day, hour, minute, second) to Unix epoch seconds.
/// Uses the standard algorithm (Howard Hinnant's days_from_civil).
fn civil_to_epoch(year: i32, month: i32, day: i32, hour: i32, minute: i32, second: i32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64; // [0, 399]
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) as i64 + 2) / 5 + (day - 1) as i64;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    let days = era as i64 * 146_097 + doe - 719_468;
    days * 86400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_epoch_seconds() {
        assert!(parse_one("1700000000 ").is_some());
    }

    #[test]
    fn parses_epoch_nanos() {
        let ns = parse_one("1700000000000000000").unwrap();
        assert!(ns > 1e18 as i64);
    }

    #[test]
    fn parses_iso_zulu() {
        // 2023-11-15T12:30:45Z = 1700051445 epoch
        let ns = parse_iso("2023-11-15T12:30:45Z").unwrap();
        assert_eq!(ns, 1700051445 * 1_000_000_000);
    }

    #[test]
    fn parses_iso_with_fractional() {
        let ns = parse_iso("2023-11-15T12:30:45.123Z").unwrap();
        assert_eq!(ns, 1700051445 * 1_000_000_000 + 123_000_000);
    }

    #[test]
    fn parses_iso_with_timezone() {
        // +02:00 = 2 hours ahead of UTC, so epoch should be 2h less
        let ns = parse_iso("2023-11-15T12:30:45+02:00").unwrap();
        assert_eq!(ns, (1700051445 - 7200) * 1_000_000_000);
    }

    #[test]
    fn parses_iso_space_separator() {
        let ns = parse_iso("2023-11-15 12:30:45").unwrap();
        assert_eq!(ns, 1700051445 * 1_000_000_000);
    }

    #[test]
    fn parses_iso_negative_timezone() {
        // -05:00 = 5 hours behind UTC
        let ns = parse_iso("2023-11-15T12:30:45-05:00").unwrap();
        assert_eq!(ns, (1700051445 + 18000) * 1_000_000_000);
    }

    #[test]
    fn parses_iso_nanosecond_precision() {
        let ns = parse_iso("2023-11-15T12:30:45.123456789Z").unwrap();
        assert_eq!(ns, 1700051445 * 1_000_000_000 + 123_456_789);
    }

    #[test]
    fn parses_syslog_date() {
        // "Nov 15 12:30:45" — month=11, day=15, time=12:30:45
        // Should produce a valid epoch for current year
        let ns = parse_syslog_date("Nov 15 12:30:45 something").unwrap();
        assert!(ns > 0);
        // Verify it's 12:30:45 UTC
        let secs = ns / 1_000_000_000;
        let time_of_day = secs % 86400;
        assert_eq!(time_of_day, 12 * 3600 + 30 * 60 + 45);
    }

    #[test]
    fn parses_syslog_date_padded_day() {
        let ns = parse_syslog_date("Nov  1 00:00:01 host").unwrap();
        assert!(ns > 0);
        let secs = ns / 1_000_000_000;
        let time_of_day = secs % 86400;
        assert_eq!(time_of_day, 1);
    }

    #[test]
    fn parse_one_iso() {
        let ns = parse_one("2023-11-15T12:30:45Z rest of line").unwrap();
        assert_eq!(ns, 1700051445 * 1_000_000_000);
    }

    #[test]
    fn parse_one_rejects_garbage() {
        assert!(parse_one("hello world").is_none());
        assert!(parse_one("").is_none());
    }

    #[test]
    fn civil_to_epoch_known() {
        // 2023-11-15T12:30:45Z = 1700051445
        assert_eq!(civil_to_epoch(2023, 11, 15, 12, 30, 45), 1700051445);
        // 1970-01-01T00:00:00 = 0
        assert_eq!(civil_to_epoch(1970, 1, 1, 0, 0, 0), 0);
        // 2000-01-01T00:00:00 = 946684800
        assert_eq!(civil_to_epoch(2000, 1, 1, 0, 0, 0), 946684800);
    }
}
