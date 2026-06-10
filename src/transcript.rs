use serde_json::Value;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const TAIL_WINDOW: u64 = 256 * 1024;

/// Extract the text of the last assistant message from a Claude Code transcript
/// (JSONL). Reads only the last `TAIL_WINDOW` bytes so multi-MB transcripts stay
/// fast. Skips assistant lines that carry only tool_use blocks and subagent
/// (`isSidechain`) lines. Returns None if nothing suitable is in the window.
pub fn last_assistant_text(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let offset = len.saturating_sub(TAIL_WINDOW);
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = Vec::with_capacity((len - offset) as usize);
    file.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);

    let mut lines: &str = &text;
    if offset > 0 {
        // We may have landed mid-line; drop the first partial line.
        lines = text.split_once('\n').map(|(_, rest)| rest).unwrap_or("");
    }

    for line in lines.lines().rev() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v["type"] != "assistant" {
            continue;
        }
        if v["isSidechain"] == true {
            continue;
        }
        let Some(content) = v["message"]["content"].as_array() else { continue };
        // Last text block of the message is the most conclusive part.
        if let Some(t) = content
            .iter()
            .rev()
            .find(|b| b["type"] == "text")
            .and_then(|b| b["text"].as_str())
        {
            let t = t.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

/// Collapse newlines and truncate to `max` chars on a char boundary, appending …
pub fn squash(text: &str, max: usize) -> String {
    let one_line: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if one_line.chars().count() <= max {
        return one_line;
    }
    let cut: String = one_line.chars().take(max.saturating_sub(1)).collect();
    format!("{}\u{2026}", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn fixture(name: &str, content: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tachi-noti-test-transcript-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        let p = d.join(name);
        fs::write(&p, content).unwrap();
        p
    }

    fn assistant_line(text: &str) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{text}"}}]}}}}"#
        )
    }

    #[test]
    fn picks_last_assistant_text() {
        let content = format!(
            "{}\n{}\n{{\"type\":\"user\",\"message\":{{}}}}\n",
            assistant_line("first"),
            assistant_line("second answer")
        );
        let p = fixture("basic.jsonl", &content);
        assert_eq!(last_assistant_text(&p).as_deref(), Some("second answer"));
    }

    #[test]
    fn skips_tool_use_only_tail() {
        let content = format!(
            "{}\n{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"name\":\"Bash\"}}]}}}}\n",
            assistant_line("real text")
        );
        let p = fixture("tooluse.jsonl", &content);
        assert_eq!(last_assistant_text(&p).as_deref(), Some("real text"));
    }

    #[test]
    fn skips_sidechain() {
        let content = format!(
            "{}\n{{\"type\":\"assistant\",\"isSidechain\":true,\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"subagent noise\"}}]}}}}\n",
            assistant_line("main answer")
        );
        let p = fixture("sidechain.jsonl", &content);
        assert_eq!(last_assistant_text(&p).as_deref(), Some("main answer"));
    }

    #[test]
    fn tolerates_garbage_lines() {
        let content = format!("not json at all\n{}\n{{broken\n", assistant_line("ok"));
        let p = fixture("garbage.jsonl", &content);
        assert_eq!(last_assistant_text(&p).as_deref(), Some("ok"));
    }

    #[test]
    fn empty_file_is_none() {
        let p = fixture("empty.jsonl", "");
        assert_eq!(last_assistant_text(&p), None);
        assert_eq!(last_assistant_text(Path::new("/nonexistent/x.jsonl")), None);
    }

    #[test]
    fn squash_truncates_on_char_boundary() {
        assert_eq!(squash("short", 10), "short");
        assert_eq!(squash("line1\nline2", 50), "line1 line2");
        let s = squash(&"統一發票對獎號碼".repeat(40), 12);
        assert!(s.chars().count() <= 12);
        assert!(s.ends_with('\u{2026}'));
    }
}
