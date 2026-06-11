//! Claude Code usage (rate-limit) capture. The data comes from the official
//! statusLine stdin payload — `tachi-noti statusline` sits at the front of
//! the statusLine pipeline, persists `rate_limits`, and hands the bytes to
//! whatever statusline the user already had.

use crate::state;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Window {
    pub used_percentage: f64,
    /// Unix seconds; 0 when the payload didn't carry one.
    pub resets_at: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Usage {
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
    pub updated_at: u64,
}

pub fn usage_path() -> PathBuf {
    state::data_dir().join("usage.json")
}

pub fn load() -> Option<Usage> {
    load_from(&usage_path())
}

fn load_from(path: &Path) -> Option<Usage> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Extract rate limits from a statusLine payload and persist them. A payload
/// without usable rate_limits (API-key accounts, older Claude Code) leaves
/// the previous snapshot untouched.
pub fn save_from_statusline(payload: &Value) {
    save_from_statusline_at(&usage_path(), payload, state::now_epoch());
}

/// Every live session's statusline re-pushes ITS last-known snapshot each
/// second — idle sessions push hours-old numbers. Accept a window only when
/// it advances: a later reset boundary, or a higher percentage within the
/// same boundary. Stale writers can never clobber fresh data.
fn advance(prev: Option<Window>, new: Option<Window>) -> (Option<Window>, bool) {
    match (prev, new) {
        (None, n) => (n, n.is_some()),
        (p, None) => (p, false),
        (Some(p), Some(n)) => {
            if n.resets_at > p.resets_at || (n.resets_at == p.resets_at && n.used_percentage >= p.used_percentage) {
                (Some(n), true)
            } else {
                (Some(p), false)
            }
        }
    }
}

fn save_from_statusline_at(path: &Path, payload: &Value, now: u64) {
    let limits = &payload["rate_limits"];
    let new_five = window(&limits["five_hour"]);
    let new_seven = window(&limits["seven_day"]);
    if new_five.is_none() && new_seven.is_none() {
        return;
    }
    let prev = load_from(path).unwrap_or_default();
    let (five_hour, adv5) = advance(prev.five_hour, new_five);
    let (seven_day, adv7) = advance(prev.seven_day, new_seven);
    if !adv5 && !adv7 {
        return; // pure stale re-push — don't even touch the file
    }
    let usage = Usage { five_hour, seven_day, updated_at: now };
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let Ok(json) = serde_json::to_string(&usage) else { return };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.tmp.{}", std::process::id()));
    if std::fs::write(&tmp, json).is_ok() && std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn window(v: &Value) -> Option<Window> {
    let pct = v["used_percentage"].as_f64()?;
    // resets_at arrives as unix seconds; tolerate string ISO forms by ignoring.
    let resets_at = v["resets_at"].as_u64().unwrap_or(0);
    Some(Window { used_percentage: pct, resets_at })
}

/// Entry point for `tachi-noti statusline --chain CMD`: persist usage, then
/// hand the untouched bytes to the chained command and mirror its output.
pub fn run_statusline(chain: Option<&str>) -> i32 {
    use std::io::{Read, Write};
    let mut raw = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut raw);
    if let Ok(v) = serde_json::from_slice::<Value>(&raw) {
        // Keep the last raw payload for doctor/debugging (schema drift).
        let last = state::data_dir().join("statusline-last.json");
        let tmp = state::data_dir().join(format!(".statusline-last.tmp.{}", std::process::id()));
        if std::fs::write(&tmp, &raw).is_ok() {
            let _ = std::fs::rename(&tmp, &last);
        }
        save_from_statusline(&v);
    }
    let Some(cmd) = chain else { return 0 };
    let child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(cmd)
        .stdin(std::process::Stdio::piped())
        .spawn();
    let Ok(mut child) = child else { return 0 };
    if let Some(stdin) = child.stdin.take() {
        let mut stdin = stdin;
        let _ = stdin.write_all(&raw);
    }
    child.wait().ok().and_then(|s| s.code()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmppath(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tachi-noti-test-usage-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(format!("{tag}.json"))
    }

    #[test]
    fn extracts_and_persists_rate_limits() {
        let p = tmppath("basic");
        let payload = json!({
            "model": {"id": "claude-fable-5"},
            "rate_limits": {
                "five_hour": {"used_percentage": 37.0, "resets_at": 1707384399},
                "seven_day": {"used_percentage": 26.5, "resets_at": 1707645599}
            }
        });
        save_from_statusline_at(&p, &payload, 1000);
        let u = load_from(&p).unwrap();
        assert_eq!(u.five_hour.unwrap().used_percentage, 37.0);
        assert_eq!(u.seven_day.unwrap().resets_at, 1707645599);
        assert_eq!(u.updated_at, 1000);
    }

    #[test]
    fn missing_rate_limits_keeps_previous_snapshot() {
        let p = tmppath("keep");
        save_from_statusline_at(
            &p,
            &json!({"rate_limits": {"five_hour": {"used_percentage": 50.0, "resets_at": 7}}}),
            1000,
        );
        save_from_statusline_at(&p, &json!({"model": {"id": "x"}, "rate_limits": null}), 2000);
        let u = load_from(&p).unwrap();
        assert_eq!(u.updated_at, 1000, "null payload must not clobber data");
        assert_eq!(u.five_hour.unwrap().used_percentage, 50.0);
    }

    #[test]
    fn stale_session_repush_cannot_clobber_fresh_data() {
        let p = tmppath("stale");
        let _ = std::fs::remove_file(&p);
        // Active session writes fresh data (current window, 20%).
        save_from_statusline_at(
            &p,
            &json!({"rate_limits": {
                "five_hour": {"used_percentage": 20.0, "resets_at": 2000},
                "seven_day": {"used_percentage": 29.0, "resets_at": 9000}
            }}),
            1000,
        );
        // Idle session re-pushes an hours-old snapshot (earlier window, lower %).
        save_from_statusline_at(
            &p,
            &json!({"rate_limits": {
                "five_hour": {"used_percentage": 10.0, "resets_at": 500},
                "seven_day": {"used_percentage": 9.0, "resets_at": 9000}
            }}),
            1001,
        );
        let u = load_from(&p).unwrap();
        assert_eq!(u.five_hour.unwrap().used_percentage, 20.0, "older window rejected");
        assert_eq!(u.seven_day.unwrap().used_percentage, 29.0, "same window, lower pct rejected");
        assert_eq!(u.updated_at, 1000, "pure stale push doesn't touch the file");
        // Window rollover: later resets_at wins even with a lower percentage.
        save_from_statusline_at(
            &p,
            &json!({"rate_limits": {"five_hour": {"used_percentage": 1.0, "resets_at": 20000}}}),
            1002,
        );
        let u = load_from(&p).unwrap();
        assert_eq!(u.five_hour.unwrap().used_percentage, 1.0);
        assert_eq!(u.seven_day.unwrap().used_percentage, 29.0, "untouched window survives");
        assert_eq!(u.updated_at, 1002);
    }

    #[test]
    fn partial_windows_are_tolerated() {
        let p = tmppath("partial");
        save_from_statusline_at(
            &p,
            &json!({"rate_limits": {"five_hour": {"used_percentage": 12.0}}}),
            500,
        );
        let u = load_from(&p).unwrap();
        assert_eq!(u.five_hour.unwrap().resets_at, 0);
        assert!(u.seven_day.is_none());
    }
}
