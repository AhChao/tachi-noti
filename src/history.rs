use crate::state;
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const MAX_BYTES: u64 = 1024 * 1024;
const TAIL_WINDOW: u64 = 256 * 1024;

#[derive(Serialize, Deserialize, Debug)]
pub struct Entry {
    pub ts: u64,
    pub event: String, // "stop" | "notification" | "test"
    pub repo: String,
    pub branch: Option<String>,
    pub body: String,
    pub session_id: Option<String>,
}

pub fn history_path() -> PathBuf {
    state::data_dir().join("history.jsonl")
}

/// Append one entry; never fails out of the hook path.
pub fn append(entry: &Entry) {
    append_to(&history_path(), entry);
}

fn append_to(path: &Path, entry: &Entry) {
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    if std::fs::metadata(path).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
        rotate_half(path);
    }
    let Ok(line) = serde_json::to_string(entry) else { return };
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
    }
}

/// Keep roughly the newer half of the file, cutting on a line boundary.
fn rotate_half(path: &Path) {
    let Ok(content) = std::fs::read(path) else { return };
    let mid = content.len() / 2;
    let Some(cut) = content[mid..].iter().position(|b| *b == b'\n').map(|p| mid + p + 1) else { return };
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    if std::fs::write(&tmp, &content[cut..]).is_ok() && std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Last `n` entries, oldest first. Tail-window read like transcript.rs.
pub fn tail(n: usize) -> Vec<Entry> {
    tail_from(&history_path(), n)
}

fn tail_from(path: &Path, n: usize) -> Vec<Entry> {
    let Ok(mut file) = std::fs::File::open(path) else { return vec![] };
    let Ok(len) = file.metadata().map(|m| m.len()) else { return vec![] };
    let offset = len.saturating_sub(TAIL_WINDOW);
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return vec![];
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return vec![];
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines: &str = &text;
    if offset > 0 {
        lines = text.split_once('\n').map(|(_, rest)| rest).unwrap_or("");
    }
    let mut entries: Vec<Entry> = lines
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str(l).ok())
        .take(n)
        .collect();
    entries.reverse();
    entries
}

pub fn rel_time(now: u64, ts: u64) -> String {
    let Some(d) = now.checked_sub(ts) else { return "now".into() };
    if d < 60 {
        "now".into()
    } else if d < 3600 {
        format!("{}m ago", d / 60)
    } else if d < 24 * 3600 {
        format!("{}h ago", d / 3600)
    } else {
        format!("{}d ago", d / (24 * 3600))
    }
}

pub fn entry_count() -> usize {
    std::fs::read_to_string(history_path()).map(|t| t.lines().count()).unwrap_or(0)
}

pub fn print_log(n: usize) {
    let entries = tail(n);
    if entries.is_empty() {
        println!("No notification history yet.");
        return;
    }
    let now = state::now_epoch();
    for e in entries {
        let repo = match &e.branch {
            Some(b) => format!("{} @ {b}", e.repo),
            None => e.repo.clone(),
        };
        println!("{:>8}  {:<13} {:<30} {}", rel_time(now, e.ts), e.event, repo, e.body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmppath(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tachi-noti-test-history-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(format!("{tag}.jsonl"))
    }

    fn entry(ts: u64, body: &str) -> Entry {
        Entry {
            ts,
            event: "stop".into(),
            repo: "r".into(),
            branch: Some("main".into()),
            body: body.into(),
            session_id: Some("s".into()),
        }
    }

    #[test]
    fn append_and_tail_roundtrip() {
        let p = tmppath("roundtrip");
        let _ = std::fs::remove_file(&p);
        for i in 0..5 {
            append_to(&p, &entry(i, &format!("msg{i}")));
        }
        let got = tail_from(&p, 3);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].body, "msg2");
        assert_eq!(got[2].body, "msg4");
    }

    #[test]
    fn tail_skips_garbage_lines() {
        let p = tmppath("garbage");
        std::fs::write(&p, "not json\n").unwrap();
        append_to(&p, &entry(1, "ok"));
        let got = tail_from(&p, 10);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].body, "ok");
    }

    #[test]
    fn rotate_keeps_newer_half_valid() {
        let p = tmppath("rotate");
        let _ = std::fs::remove_file(&p);
        for i in 0..100 {
            append_to(&p, &entry(i, &format!("entry-{i}")));
        }
        let before = std::fs::metadata(&p).unwrap().len();
        rotate_half(&p);
        let after = std::fs::metadata(&p).unwrap().len();
        assert!(after < before);
        // Every surviving line must still parse, newest preserved.
        let got = tail_from(&p, 1000);
        assert!(!got.is_empty());
        assert_eq!(got.last().unwrap().body, "entry-99");
        let raw = std::fs::read_to_string(&p).unwrap();
        assert_eq!(raw.lines().count(), got.len(), "no partial lines after rotation");
    }

    #[test]
    fn rel_time_buckets() {
        assert_eq!(rel_time(100, 90), "now");
        assert_eq!(rel_time(1000, 100), "15m ago");
        assert_eq!(rel_time(10_000, 100), "2h ago");
        assert_eq!(rel_time(1_000_000, 100), "11d ago");
        assert_eq!(rel_time(100, 200), "now", "future ts (clock skew) renders as now");
    }
}
