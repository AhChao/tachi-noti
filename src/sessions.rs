use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

pub fn sessions_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("tachi-noti/sessions")
}

/// Session ids are UUIDs in practice, but never trust input used as a filename.
fn sanitize(id: &str) -> String {
    let s: String = id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    if s.is_empty() { "unknown".into() } else { s }
}

/// Record "user prompt submitted" time for duration tracking on Stop.
pub fn record_start(session_id: &str) {
    let dir = sessions_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let _ = std::fs::write(dir.join(sanitize(session_id)), now.to_string());
}

/// Read and remove the start marker; None if missing or the duration is
/// implausible (negative clock skew or older than a day).
pub fn take_start(session_id: &str) -> Option<Duration> {
    let path = sessions_dir().join(sanitize(session_id));
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let start = text.trim().parse::<u64>().ok()?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let elapsed = now.checked_sub(start)?;
    if elapsed > STALE_AFTER.as_secs() {
        return None;
    }
    Some(Duration::from_secs(elapsed))
}

/// Remove markers older than a day (sessions that never reached Stop).
pub fn cleanup_stale() {
    let Ok(entries) = std::fs::read_dir(sessions_dir()) else { return };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .map(|age| age > STALE_AFTER)
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

pub fn pending_count() -> usize {
    std::fs::read_dir(sessions_dir()).map(|d| d.count()).unwrap_or(0)
}

pub fn format_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_formatting() {
        assert_eq!(format_duration(Duration::from_secs(42)), "42s");
        assert_eq!(format_duration(Duration::from_secs(192)), "3m12s");
        assert_eq!(format_duration(Duration::from_secs(3780)), "1h03m");
    }

    #[test]
    fn sanitize_strips_path_separators() {
        assert_eq!(sanitize("../../etc/passwd"), "etcpasswd");
        assert_eq!(sanitize("abc-123_DEF"), "abc-123_DEF");
        assert_eq!(sanitize("///"), "unknown");
    }

    #[test]
    fn record_and_take_roundtrip() {
        let id = format!("test-roundtrip-{}", std::process::id());
        record_start(&id);
        let d = take_start(&id).expect("duration should exist");
        assert!(d.as_secs() < 5);
        assert!(take_start(&id).is_none(), "marker should be consumed");
    }
}
