//! Syslog format plugin. Parses RFC3164 and RFC5424 syslog messages.
//!
//! RFC3164: `<PRI>MMM DD HH:MM:SS HOST TAG: message`
//! RFC5424: `<PRI>VERSION TIMESTAMP HOSTNAME APP_NAME PROCID MSGID [SD] msg`

use crate::pipeline::parser::{FieldValue, ParsedLine};
use crate::plugin::Plugin;
use crate::plugin::Severity;
use crate::plugin::rust::timestamp;

pub struct Syslog;

impl Plugin for Syslog {
    fn name(&self) -> &str {
        "syslog"
    }

    fn detect(&self, sample: &[&str]) -> f32 {
        // `<PRI>timestamp host tag: message` — RFC3164/5424.
        let re = regex::Regex::new(r"^<\d{1,3}>").unwrap();
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

        // Parse PRI: <NNN>
        if !raw.starts_with('<') {
            return;
        }
        let close = match raw.find('>') {
            Some(p) => p,
            None => return,
        };
        let pri_str = &raw[1..close];
        let pri: i32 = match pri_str.parse() {
            Ok(v) => v,
            Err(_) => return,
        };
        let rest = &raw[close + 1..];

        // PRI = facility * 8 + severity
        let facility = pri / 8;
        let syslog_severity = pri % 8;

        // Map syslog severity to our Severity enum.
        // 0=emerg, 1=alert, 2=crit, 3=err, 4=warning, 5=notice, 6=info, 7=debug
        if line.severity.is_none() {
            line.severity = match syslog_severity {
                0..=3 => Some(Severity::Error),
                4 | 5 => Some(Severity::Warn),
                6 => Some(Severity::Info),
                7 => Some(Severity::Debug),
                _ => None,
            };
        }

        line.fields.insert("facility".into(), FieldValue::Int(facility as i64));
        line.fields.insert("syslog_priority".into(), FieldValue::Int(pri as i64));

        // Check for RFC5424: starts with a version number (e.g. "1 ")
        let rest_owned = rest.to_string();
        if let Some(space_pos) = rest_owned.find(' ')
            && rest_owned[..space_pos].chars().all(|c| c.is_ascii_digit())
            && !rest_owned[..space_pos].is_empty()
        {
            parse_rfc5424(line, &rest_owned, space_pos);
        } else {
            parse_rfc3164(line, &rest_owned);
        }
    }
}

/// Parse RFC3164: `MMM DD HH:MM:SS HOST TAG: message`
fn parse_rfc3164(line: &mut ParsedLine, rest: &str) {
    // Try to parse the timestamp from the start.
    if let Some(ns) = timestamp::parse_one(rest) {
        line.timestamp_ns = Some(ns);
    }

    // Skip past the timestamp (find the host after the time).
    // Format: "Nov 15 12:30:45 hostname tag: message"
    // The time is always 15 chars: "Nov 15 12:30:45"
    if rest.len() > 15 {
        let after_time = rest[15..].trim_start();
        // Host is up to the next space.
        let host_end = after_time.find(' ').unwrap_or(after_time.len());
        let host = &after_time[..host_end];
        if !host.is_empty() {
            line.fields.insert("host".into(), FieldValue::Str(host.to_string()));
        }

        // After host: "tag: message" or "tag[pid]: message" or just "message"
        let after_host = after_time[host_end..].trim_start();
        if let Some(colon_pos) = after_host.find(": ") {
            let tag_part = &after_host[..colon_pos];
            let message = after_host[colon_pos + 2..].trim();

            // Check for tag[pid] format.
            if let Some(bracket_start) = tag_part.find('[')
                && let Some(bracket_end) = tag_part.find(']')
            {
                let tag = &tag_part[..bracket_start];
                let pid_str = &tag_part[bracket_start + 1..bracket_end];
                line.fields.insert("tag".into(), FieldValue::Str(tag.to_string()));
                if let Ok(pid) = pid_str.parse::<i64>() {
                    line.fields.insert("pid".into(), FieldValue::Int(pid));
                }
            } else {
                line.fields.insert("tag".into(), FieldValue::Str(tag_part.to_string()));
            }
            line.fields.insert("message".into(), FieldValue::Str(message.to_string()));
        } else {
            // No tag, just message.
            line.fields.insert("message".into(), FieldValue::Str(after_host.to_string()));
        }
    }
}

/// Parse RFC5424: `VERSION TIMESTAMP HOSTNAME APP_NAME PROCID MSGID [SD] msg`
fn parse_rfc5424(line: &mut ParsedLine, rest: &str, version_end: usize) {
    let version: i64 = rest[..version_end].parse().unwrap_or(1);
    line.fields.insert("syslog_version".into(), FieldValue::Int(version));

    let after_version = rest[version_end + 1..].trim_start();

    // Parse fields separated by spaces. RFC5424 has fixed positions:
    // TIMESTAMP HOSTNAME APP_NAME PROCID MSGID [STRUCTURED-DATA] MSG
    let parts: Vec<&str> = after_version.splitn(7, ' ').collect();
    if parts.is_empty() {
        return;
    }

    // Timestamp
    if let Some(ns) = timestamp::parse_one(parts[0]) {
        line.timestamp_ns = Some(ns);
    }
    line.fields.insert("timestamp".into(), FieldValue::Str(parts[0].to_string()));

    if parts.len() > 1 {
        line.fields.insert("host".into(), FieldValue::Str(parts[1].to_string()));
    }
    if parts.len() > 2 {
        line.fields.insert("app_name".into(), FieldValue::Str(parts[2].to_string()));
    }
    if parts.len() > 3 {
        if let Ok(pid) = parts[3].parse::<i64>() {
            line.fields.insert("procid".into(), FieldValue::Int(pid));
        } else {
            line.fields.insert("procid".into(), FieldValue::Str(parts[3].to_string()));
        }
    }
    if parts.len() > 4 {
        line.fields.insert("msgid".into(), FieldValue::Str(parts[4].to_string()));
    }
    // parts[5] is structured data, parts[6] is the message (if we split into 7)
    // But structured data can contain spaces if it's `[id key="val"]`...
    // For simplicity, we just store the rest as message.
    if parts.len() > 6 {
        line.fields.insert("message".into(), FieldValue::Str(parts[6].to_string()));
    } else if parts.len() > 5 {
        line.fields.insert("message".into(), FieldValue::Str(parts[5].to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_rfc3164() {
        let s = Syslog;
        let sample = [
            "<134>Nov 15 12:30:45 host app[123]: hello",
            "<134>Nov 15 12:30:46 host app[123]: world",
        ];
        assert!(s.detect(&sample) > 0.5);
    }

    #[test]
    fn detect_non_syslog() {
        let s = Syslog;
        let sample = ["hello world", "just some text"];
        assert_eq!(s.detect(&sample), 0.0);
    }

    #[test]
    fn parse_rfc3164_basic() {
        let mut line = ParsedLine::stub("<134>Nov 15 12:30:45 myhost app[123]: something happened");
        Syslog.parse(&mut line);

        assert_eq!(line.severity, Some(Severity::Info));
        assert_eq!(line.fields.get("host").and_then(|v| v.as_str()), Some("myhost"));
        assert_eq!(line.fields.get("tag").and_then(|v| v.as_str()), Some("app"));
        assert_eq!(line.fields.get("pid").and_then(|v| v.as_int()), Some(123));
        assert_eq!(line.fields.get("message").and_then(|v| v.as_str()), Some("something happened"));
        assert!(line.timestamp_ns.is_some());
    }

    #[test]
    fn parse_rfc3164_no_pid() {
        let mut line = ParsedLine::stub("<131>Nov 15 12:30:45 host myapp: error occurred");
        Syslog.parse(&mut line);

        // PRI=131: facility=16, severity=3 (err)
        assert_eq!(line.severity, Some(Severity::Error));
        assert_eq!(line.fields.get("tag").and_then(|v| v.as_str()), Some("myapp"));
        assert_eq!(line.fields.get("message").and_then(|v| v.as_str()), Some("error occurred"));
    }

    #[test]
    fn parse_rfc3164_no_tag() {
        let mut line = ParsedLine::stub("<134>Nov 15 12:30:45 host just a message here");
        Syslog.parse(&mut line);

        assert_eq!(line.fields.get("host").and_then(|v| v.as_str()), Some("host"));
        assert_eq!(line.fields.get("message").and_then(|v| v.as_str()), Some("just a message here"));
    }

    #[test]
    fn parse_rfc5424() {
        let mut line = ParsedLine::stub(
            "<165>1 2023-11-15T12:30:45Z myhost app 1234 ID47 [exampleSDID@32473 iut=\"3\"] BOMAn application event log entry",
        );
        Syslog.parse(&mut line);

        // PRI=165: facility=20, severity=5 (notice/warning)
        assert_eq!(line.severity, Some(Severity::Warn));
        assert_eq!(line.fields.get("host").and_then(|v| v.as_str()), Some("myhost"));
        assert_eq!(line.fields.get("app_name").and_then(|v| v.as_str()), Some("app"));
        assert_eq!(line.fields.get("procid").and_then(|v| v.as_int()), Some(1234));
        assert_eq!(line.fields.get("msgid").and_then(|v| v.as_str()), Some("ID47"));
        assert!(line.timestamp_ns.is_some());
    }

    #[test]
    fn parse_severity_mapping() {
        // PRI=8: facility=1, severity=0 (emerg) -> Error
        let mut l1 = ParsedLine::stub("<8>Jan  1 00:00:00 h t: m");
        Syslog.parse(&mut l1);
        assert_eq!(l1.severity, Some(Severity::Error));

        // PRI=12: facility=1, severity=4 (warning) -> Warn
        let mut l2 = ParsedLine::stub("<12>Jan  1 00:00:00 h t: m");
        Syslog.parse(&mut l2);
        assert_eq!(l2.severity, Some(Severity::Warn));

        // PRI=15: facility=1, severity=7 (debug) -> Debug
        let mut l3 = ParsedLine::stub("<15>Jan  1 00:00:00 h t: m");
        Syslog.parse(&mut l3);
        assert_eq!(l3.severity, Some(Severity::Debug));
    }
}
