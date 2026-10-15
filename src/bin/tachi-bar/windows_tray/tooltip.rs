//! Tray tooltip text (the macOS title's counterpart).

use tachi_noti::state::Status;

/// Shell_NotifyIcon's szTip holds 128 UTF-16 units including the terminator.
const MAX_UNITS: usize = 127;

fn word(status: Status) -> &'static str {
    match status {
        Status::Waiting => "waiting",
        Status::Running => "running",
        Status::Idle => "idle",
    }
}

/// "Tachi", or "Tachi — 1 waiting · 2 running · 3 idle" (segments arrive
/// ordered by urgency), with " · usage alert" appended when a usage window is
/// over its threshold.
pub fn text(segments: &[(usize, Status)], alert: bool) -> String {
    let mut parts: Vec<String> = segments.iter().map(|(n, s)| format!("{n} {}", word(*s))).collect();
    if alert {
        parts.push("usage alert".into());
    }
    let full = if parts.is_empty() { "Tachi".to_string() } else { format!("Tachi \u{2014} {}", parts.join(" \u{00B7} ")) };
    truncate_utf16(&full, MAX_UNITS)
}

/// Longest prefix that fits `max` UTF-16 units without splitting a character.
fn truncate_utf16(s: &str, max: usize) -> String {
    let mut units = 0;
    let mut out = String::new();
    for c in s.chars() {
        units += c.len_utf16();
        if units > max {
            break;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_sessions_is_just_the_name() {
        assert_eq!(text(&[], false), "Tachi");
    }

    #[test]
    fn segments_keep_their_order() {
        let segs = [(1, Status::Waiting), (2, Status::Running), (3, Status::Idle)];
        assert_eq!(text(&segs, false), "Tachi \u{2014} 1 waiting \u{00B7} 2 running \u{00B7} 3 idle");
    }

    #[test]
    fn alert_is_appended_even_without_sessions() {
        assert_eq!(text(&[(2, Status::Idle)], true), "Tachi \u{2014} 2 idle \u{00B7} usage alert");
        assert_eq!(text(&[], true), "Tachi \u{2014} usage alert");
    }

    #[test]
    fn truncation_counts_utf16_units_and_never_splits_a_character() {
        let s = "a".repeat(126) + "\u{1F600}"; // astral char = 2 units -> 128 total
        let t = truncate_utf16(&s, MAX_UNITS);
        assert_eq!(t.encode_utf16().count(), 126);
        assert_eq!(truncate_utf16("short", MAX_UNITS), "short");
        let long = "x".repeat(300);
        assert_eq!(truncate_utf16(&long, MAX_UNITS).encode_utf16().count(), MAX_UNITS);
    }
}
