//! Common Log Format / NCSA plugin.
//!
//! CLF: `host ident authuser [date] "request" status bytes`
//! NCSA extended: `host ident authuser [date] "request" status bytes "referer" "user-agent"`
//! With duration: `... "request" status bytes duration`

use crate::pipeline::parser::{FieldValue, ParsedLine};
use crate::plugin::Plugin;

pub struct Clf;

impl Plugin for Clf {
    fn name(&self) -> &str {
        "clf"
    }

    fn detect(&self, sample: &[&str]) -> f32 {
        // `host ident authuser [date] "request" status bytes`
        let re = regex::Regex::new(r#"^\S+ \S+ \S+ \[.+?\] ".+?" \d{3} "#).unwrap();
        let hits = sample.iter().filter(|l| re.is_match(l.trim_start())).count();
        let f = hits as f32 / sample.len().max(1) as f32;
        if f > 0.5 {
            f
        } else {
            0.0
        }
    }

    fn parse(&self, line: &mut ParsedLine) {
        let raw = line.raw.trim_start();

        // Parse: host ident authuser [date] "request" status bytes ...
        // Token 1: host (up to first space)
        let host_end = match raw.find(' ') {
            Some(p) => p,
            None => return,
        };
        let host = &raw[..host_end];
        line.fields.insert("remote_host".into(), FieldValue::Str(host.to_string()));

        let rest = &raw[host_end + 1..];

        // Token 2: ident (usually -)
        let ident_end = match rest.find(' ') {
            Some(p) => p,
            None => return,
        };
        let ident = &rest[..ident_end];
        line.fields.insert("ident".into(), FieldValue::Str(ident.to_string()));

        let rest = &rest[ident_end + 1..];

        // Token 3: authuser (usually -)
        let user_end = match rest.find(' ') {
            Some(p) => p,
            None => return,
        };
        let user = &rest[..user_end];
        if user != "-" {
            line.fields.insert("authuser".into(), FieldValue::Str(user.to_string()));
        }

        let rest = &rest[user_end + 1..];

        // Token 4: [date] — enclosed in square brackets
        if !rest.starts_with('[') {
            return;
        }
        let date_end = match rest.find(']') {
            Some(p) => p,
            None => return,
        };
        let date_str = &rest[1..date_end];
        line.fields.insert("timestamp".into(), FieldValue::Str(date_str.to_string()));

        // Try to parse the CLF date format: "15/Nov/2023:12:30:45 +0000"
        if let Some(ns) = parse_clf_date(date_str) {
            line.timestamp_ns = Some(ns);
        }

        let rest = &rest[date_end + 1..].trim_start();

        // Token 5: "request" — enclosed in quotes
        if !rest.starts_with('"') {
            return;
        }
        let req_end = match rest[1..].find('"') {
            Some(p) => p + 1,
            None => return,
        };
        let request = &rest[1..req_end];
        line.fields.insert("request".into(), FieldValue::Str(request.to_string()));

        // Parse method, path, protocol from request.
        let req_parts: Vec<&str> = request.splitn(3, ' ').collect();
        if !req_parts.is_empty() {
            line.fields.insert("method".into(), FieldValue::Str(req_parts[0].to_string()));
        }
        if req_parts.len() >= 2 {
            line.fields.insert("path".into(), FieldValue::Str(req_parts[1].to_string()));
        }
        if req_parts.len() >= 3 {
            line.fields.insert("protocol".into(), FieldValue::Str(req_parts[2].to_string()));
        }

        let rest = &rest[req_end + 1..].trim_start();

        // Token 6: status (3-digit HTTP status)
        let status_end = match rest.find(' ') {
            Some(p) => p,
            None => {
                // Maybe status is the last token
                if let Ok(status) = rest.trim().parse::<i64>() {
                    line.fields.insert("status".into(), FieldValue::Int(status));
                }
                return;
            }
        };
        let status_str = &rest[..status_end];
        if let Ok(status) = status_str.parse::<i64>() {
            line.fields.insert("status".into(), FieldValue::Int(status));
        }

        let rest = &rest[status_end + 1..].trim_start();

        // Token 7: bytes (or -)
        let bytes_end = match rest.find(' ') {
            Some(p) => p,
            None => {
                let bytes_str = rest.trim();
                if bytes_str != "-"
                    && let Ok(bytes) = bytes_str.parse::<i64>()
                {
                    line.fields.insert("bytes".into(), FieldValue::Int(bytes));
                }
                return;
            }
        };
        let bytes_str = &rest[..bytes_end];
        if bytes_str != "-"
            && let Ok(bytes) = bytes_str.parse::<i64>()
        {
            line.fields.insert("bytes".into(), FieldValue::Int(bytes));
        }

        let rest = &rest[bytes_end + 1..].trim_start();

        // Optional: "referer" "user-agent" (NCSA extended)
        if let Some(rest) = rest.strip_prefix('"') {
            if let Some(ref_end) = rest.find('"') {
                let referer = &rest[..ref_end];
                if !referer.is_empty() && referer != "-" {
                    line.fields.insert("referer".into(), FieldValue::Str(referer.to_string()));
                }
                let after_ref = rest[ref_end + 1..].trim_start();
                if let Some(after_ref) = after_ref.strip_prefix('"')
                    && let Some(ua_end) = after_ref.find('"')
                {
                    let ua = &after_ref[..ua_end];
                    line.fields.insert("user_agent".into(), FieldValue::Str(ua.to_string()));
                }
            }
        } else {
            // Maybe a duration field (some custom CLF variants).
            let dur_end = rest.find(' ').unwrap_or(rest.len());
            let dur_str = &rest[..dur_end];
            if let Ok(dur) = dur_str.parse::<i64>() {
                line.fields.insert("duration_ms".into(), FieldValue::Int(dur));
            }
        }
    }
}

/// Parse CLF date format: "15/Nov/2023:12:30:45 +0000"
fn parse_clf_date(s: &str) -> Option<i64> {
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun",
        "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];

    // Format: DD/Mon/YYYY:HH:MM:SS +ZZZZ
    // Split on ':' to get date and time parts
    let first_colon = s.find(':')?;
    let date_part = &s[..first_colon];
    let time_and_tz = &s[first_colon + 1..];

    // Parse date: DD/Mon/YYYY
    let date_parts: Vec<&str> = date_part.split('/').collect();
    if date_parts.len() != 3 {
        return None;
    }
    let day: i32 = date_parts[0].parse().ok()?;
    let month_str = date_parts[1];
    let month_idx = months.iter().position(|m| *m == month_str)?;
    let month = (month_idx + 1) as i32;
    let year: i32 = date_parts[2].parse().ok()?;

    // Parse time: HH:MM:SS +ZZZZ
    let time_parts: Vec<&str> = time_and_tz.splitn(2, ' ').collect();
    let time_str = time_parts[0];
    let time_components: Vec<&str> = time_str.split(':').collect();
    if time_components.len() != 3 {
        return None;
    }
    let hour: i32 = time_components[0].parse().ok()?;
    let minute: i32 = time_components[1].parse().ok()?;
    let second: i32 = time_components[2].parse().ok()?;

    // Parse timezone: +HHMM or -HHMM
    let mut tz_offset_secs: i64 = 0;
    if time_parts.len() > 1 {
        let tz_str = time_parts[1];
        if tz_str.len() >= 5 {
            let sign = if tz_str.starts_with('-') { -1 } else { 1 };
            let tz_hour: i64 = tz_str[1..3].parse().ok()?;
            let tz_min: i64 = tz_str[3..5].parse().ok()?;
            tz_offset_secs = sign * (tz_hour * 3600 + tz_min * 60);
        }
    }

    let epoch = civil_to_epoch(year, month, day, hour, minute, second);
    Some((epoch - tz_offset_secs) * 1_000_000_000)
}

/// Convert civil (year, month, day, hour, minute, second) to Unix epoch seconds.
fn civil_to_epoch(year: i32, month: i32, day: i32, hour: i32, minute: i32, second: i32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) as i64 + 2) / 5 + (day - 1) as i64;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era as i64 * 146_097 + doe - 719_468;
    days * 86400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_clf() {
        let s = Clf;
        let sample = [
            r#"127.0.0.1 - - [15/Nov/2023:12:30:45 +0000] "GET /index.html HTTP/1.1" 200 2326"#,
            r#"127.0.0.1 - - [15/Nov/2023:12:30:46 +0000] "POST /api HTTP/1.1" 404 128"#,
        ];
        assert!(s.detect(&sample) > 0.5);
    }

    #[test]
    fn detect_non_clf() {
        let s = Clf;
        let sample = ["hello world", "just text"];
        assert_eq!(s.detect(&sample), 0.0);
    }

    #[test]
    fn parse_basic_clf() {
        let mut line = ParsedLine::stub(
            r#"127.0.0.1 - alice [15/Nov/2023:12:30:45 +0000] "GET /index.html HTTP/1.1" 200 2326"#,
        );
        Clf.parse(&mut line);

        assert_eq!(line.fields.get("remote_host").and_then(|v| v.as_str()), Some("127.0.0.1"));
        assert_eq!(line.fields.get("authuser").and_then(|v| v.as_str()), Some("alice"));
        assert_eq!(line.fields.get("method").and_then(|v| v.as_str()), Some("GET"));
        assert_eq!(line.fields.get("path").and_then(|v| v.as_str()), Some("/index.html"));
        assert_eq!(line.fields.get("protocol").and_then(|v| v.as_str()), Some("HTTP/1.1"));
        assert_eq!(line.fields.get("status").and_then(|v| v.as_int()), Some(200));
        assert_eq!(line.fields.get("bytes").and_then(|v| v.as_int()), Some(2326));
        assert!(line.timestamp_ns.is_some());
    }

    #[test]
    fn parse_clf_no_user() {
        let mut line = ParsedLine::stub(
            r#"10.0.0.1 - - [15/Nov/2023:12:30:45 +0000] "POST /api HTTP/1.1" 404 128"#,
        );
        Clf.parse(&mut line);

        assert_eq!(line.fields.get("remote_host").and_then(|v| v.as_str()), Some("10.0.0.1"));
        assert!(!line.fields.contains_key("authuser"));
        assert_eq!(line.fields.get("status").and_then(|v| v.as_int()), Some(404));
    }

    #[test]
    fn parse_ncsa_extended() {
        let mut line = ParsedLine::stub(
            r#"127.0.0.1 - - [15/Nov/2023:12:30:45 +0000] "GET / HTTP/1.1" 200 2326 "http://example.com" "Mozilla/5.0""#,
        );
        Clf.parse(&mut line);

        assert_eq!(line.fields.get("referer").and_then(|v| v.as_str()), Some("http://example.com"));
        assert_eq!(line.fields.get("user_agent").and_then(|v| v.as_str()), Some("Mozilla/5.0"));
    }

    #[test]
    fn parse_clf_no_bytes() {
        let mut line = ParsedLine::stub(
            r#"127.0.0.1 - - [15/Nov/2023:12:30:45 +0000] "GET / HTTP/1.1" 200 -"#,
        );
        Clf.parse(&mut line);

        assert_eq!(line.fields.get("status").and_then(|v| v.as_int()), Some(200));
        assert!(!line.fields.contains_key("bytes"));
    }

    #[test]
    fn parses_clf_date() {
        // 15/Nov/2023:12:30:45 +0000 = 1700051445 epoch
        let ns = parse_clf_date("15/Nov/2023:12:30:45 +0000").unwrap();
        assert_eq!(ns, 1700051445 * 1_000_000_000);
    }

    #[test]
    fn parses_clf_date_with_timezone() {
        // 15/Nov/2023:14:30:45 +0200 = 1700051445 epoch (2h ahead)
        let ns = parse_clf_date("15/Nov/2023:14:30:45 +0200").unwrap();
        assert_eq!(ns, 1700051445 * 1_000_000_000);
    }

    #[test]
    fn parse_clf_500_error() {
        let mut line = ParsedLine::stub(
            r#"10.0.0.1 - - [15/Nov/2023:12:30:45 +0000] "GET /broken HTTP/1.1" 500 0"#,
        );
        Clf.parse(&mut line);
        assert_eq!(line.fields.get("status").and_then(|v| v.as_int()), Some(500));
        assert_eq!(line.fields.get("bytes").and_then(|v| v.as_int()), Some(0));
    }
}
