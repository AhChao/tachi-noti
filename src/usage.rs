//! Claude Code usage (rate-limit) capture. The data comes from the official
//! statusLine stdin payload — `tachi-noti statusline` sits at the front of
//! the statusLine pipeline, persists `rate_limits`, and hands the bytes to
//! whatever statusline the user already had.

use crate::state;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Window {
    pub used_percentage: f64,
    /// Unix seconds; 0 when the payload didn't carry one.
    pub resets_at: u64,
}

/// Last-seen activity fingerprint of one statusline-pushing session.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Writer {
    pub fp: String,
    /// When the fingerprint last changed (unix seconds) — prune key.
    pub at: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Usage {
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
    pub updated_at: u64,
    /// session_id → activity fingerprint. A session whose fingerprint just
    /// changed got a fresh API response, so its rate_limits are current.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub writers: HashMap<String, Writer>,
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

/// API-activity counters: they move exactly when the session receives an API
/// response (wall-clock total_duration_ms ticks every render and must NOT be
/// in here — a two-week-old zombie session ticks it every second while
/// re-pushing rate limits whose reset boundary passed days ago). None when
/// the payload has no usable cost block (older Claude Code).
fn fingerprint(payload: &Value) -> Option<String> {
    let cost = &payload["cost"];
    let usd = cost["total_cost_usd"].as_f64()?;
    let api_ms = cost["total_api_duration_ms"].as_f64().unwrap_or(0.0);
    Some(format!("{usd}:{api_ms}"))
}

fn is_expired(w: &Window, now: u64) -> bool {
    // resets_at == 0 = payload carried no boundary; tolerated, can't be gated.
    w.resets_at != 0 && w.resets_at <= now
}

const WRITER_TTL_SECS: u64 = 48 * 3600;

/// Gate order for pushed windows — there is deliberately NO percentage
/// ratchet: idle sessions re-push hours-old snapshots every second, and a
/// percentage comparison cannot tell a stale high from a fresh low (the
/// weekly quota provably gets re-graded downward mid-window).
///
/// 1. A window whose reset boundary already passed is dropped outright —
///    a genuine current window always resets in the future. This also makes
///    official rollovers self-cleaning: pre-rollover re-pushes expire.
/// 2. A session whose API-activity fingerprint changed since its previous
///    push ("authoritative") just got an API response, so its rate_limits
///    are at most one render old — accepted wholesale, decreases included.
///    First sighting of a session is recorded but NOT trusted: it may be a
///    long-idle re-pusher we simply hadn't tracked yet.
/// 3. Anything else may only fill an empty slot or replace one whose own
///    boundary expired — never overwrite live data.
fn save_from_statusline_at(path: &Path, payload: &Value, now: u64) {
    let limits = &payload["rate_limits"];
    let new_five = window(&limits["five_hour"]).filter(|w| !is_expired(w, now));
    let new_seven = window(&limits["seven_day"]).filter(|w| !is_expired(w, now));
    if new_five.is_none() && new_seven.is_none() {
        return;
    }
    let mut prev = load_from(path).unwrap_or_default();

    let session = payload["session_id"].as_str().unwrap_or("");
    let mut authoritative = false;
    let mut writers_changed = false;
    if let (false, Some(fp)) = (session.is_empty(), fingerprint(payload)) {
        match prev.writers.get(session) {
            Some(w) if w.fp == fp => {} // unchanged → plain re-push
            seen => {
                authoritative = seen.is_some();
                writers_changed = true;
                prev.writers.insert(session.to_string(), Writer { fp, at: now });
                prev.writers.retain(|_, w| now.saturating_sub(w.at) < WRITER_TTL_SECS);
            }
        }
    }

    let place = |slot: Option<Window>, new: Option<Window>| -> (Option<Window>, bool) {
        match (slot, new) {
            (s, None) => (s, false),
            (_, n) if authoritative => (n, true), // fresh data, even when lower
            (None, n) => (n, true),               // bootstrap an empty slot
            (Some(s), n) if is_expired(&s, now) => (n, true),
            (s, _) => (s, false),                 // live data beats a re-push
        }
    };
    let (five_hour, acc5) = place(prev.five_hour, new_five);
    let (seven_day, acc7) = place(prev.seven_day, new_seven);
    if !acc5 && !acc7 && !writers_changed {
        return; // nothing accepted — don't even touch the file
    }
    let updated_at = if acc5 || acc7 { now } else { prev.updated_at };
    let usage = Usage { five_hour, seven_day, updated_at, writers: prev.writers };
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
            &json!({"rate_limits": {"five_hour": {"used_percentage": 50.0, "resets_at": 7000}}}),
            1000,
        );
        save_from_statusline_at(&p, &json!({"model": {"id": "x"}, "rate_limits": null}), 2000);
        let u = load_from(&p).unwrap();
        assert_eq!(u.updated_at, 1000, "null payload must not clobber data");
        assert_eq!(u.five_hour.unwrap().used_percentage, 50.0);
    }

    #[test]
    fn expired_windows_never_land() {
        let p = tmppath("zombie");
        let _ = std::fs::remove_file(&p);
        // A zombie session re-pushes rate limits whose boundaries passed days
        // ago — even into an EMPTY file this must not land.
        save_from_statusline_at(
            &p,
            &json!({"session_id": "zombie", "cost": {"total_cost_usd": 0.0},
                "rate_limits": {
                    "five_hour": {"used_percentage": 20.0, "resets_at": 500},
                    "seven_day": {"used_percentage": 60.0, "resets_at": 800}
                }}),
            1000,
        );
        assert!(load_from(&p).is_none(), "expired-only push must not create the file");
    }

    #[test]
    fn repush_cannot_clobber_live_data_but_replaces_expired() {
        let p = tmppath("gate");
        let _ = std::fs::remove_file(&p);
        // Active session bootstraps fresh data.
        save_from_statusline_at(
            &p,
            &json!({"rate_limits": {
                "five_hour": {"used_percentage": 20.0, "resets_at": 2000},
                "seven_day": {"used_percentage": 4.0, "resets_at": 9000}
            }}),
            1000,
        );
        // A woken idle session's first push carries yesterday's cached weekly
        // HIGH for the same window — live data must win (no ratchet).
        save_from_statusline_at(
            &p,
            &json!({"session_id": "idle", "cost": {"total_cost_usd": 9.0},
                "rate_limits": {"seven_day": {"used_percentage": 32.0, "resets_at": 9000}}}),
            1001,
        );
        let u = load_from(&p).unwrap();
        assert_eq!(u.seven_day.unwrap().used_percentage, 4.0, "stale high rejected");
        // Rollover: once the stored 5h window expires, a re-push carrying the
        // NEW window replaces it even without authority.
        save_from_statusline_at(
            &p,
            &json!({"rate_limits": {"five_hour": {"used_percentage": 1.0, "resets_at": 20000}}}),
            2500,
        );
        let u = load_from(&p).unwrap();
        assert_eq!(u.five_hour.unwrap().used_percentage, 1.0, "expired slot replaced");
        assert_eq!(u.seven_day.unwrap().used_percentage, 4.0, "untouched window survives");
        assert_eq!(u.updated_at, 2500);
    }

    #[test]
    fn wall_clock_tick_is_not_api_activity() {
        let p = tmppath("wallclock");
        let _ = std::fs::remove_file(&p);
        let push = |pct: f64, dur_ms: f64, at: u64| {
            save_from_statusline_at(
                &p,
                &json!({"session_id": "s1",
                    "cost": {"total_cost_usd": 1.0, "total_api_duration_ms": 500.0,
                             "total_duration_ms": dur_ms},
                    "rate_limits": {"seven_day": {"used_percentage": pct, "resets_at": 9000}}}),
                at,
            );
        };
        push(30.0, 100.0, 100); // bootstrap
        push(2.0, 200.0, 101); // only wall-clock moved → NOT authoritative
        let u = load_from(&p).unwrap();
        assert_eq!(u.seven_day.unwrap().used_percentage, 30.0, "wall tick grants no authority");
    }

    #[test]
    fn active_session_decrease_is_accepted() {
        let p = tmppath("regrade");
        let _ = std::fs::remove_file(&p);
        let push = |pct: f64, cost: f64, at: u64| {
            save_from_statusline_at(
                &p,
                &json!({
                    "session_id": "s1",
                    "cost": {"total_cost_usd": cost},
                    "rate_limits": {"seven_day": {"used_percentage": pct, "resets_at": 9000}}
                }),
                at,
            );
        };
        push(32.0, 1.0, 100); // first sighting: registered, value lands (empty file)
        push(4.0, 1.0, 101); // same cost → re-push → ratchet keeps 32
        let u = load_from(&p).unwrap();
        assert_eq!(u.seven_day.unwrap().used_percentage, 32.0);
        push(4.0, 2.0, 102); // cost moved → fresh API turn → decrease accepted
        let u = load_from(&p).unwrap();
        assert_eq!(u.seven_day.unwrap().used_percentage, 4.0, "quota re-grade must land");
        assert_eq!(u.updated_at, 102);
    }

    #[test]
    fn unseen_session_is_recorded_but_not_trusted() {
        let p = tmppath("unseen");
        let _ = std::fs::remove_file(&p);
        save_from_statusline_at(
            &p,
            &json!({
                "session_id": "fresh",
                "cost": {"total_cost_usd": 5.0},
                "rate_limits": {"seven_day": {"used_percentage": 30.0, "resets_at": 9000}}
            }),
            100,
        );
        // A long-idle session's first observed push must not lower fresh data.
        save_from_statusline_at(
            &p,
            &json!({
                "session_id": "idle-old",
                "cost": {"total_cost_usd": 9.0},
                "rate_limits": {"seven_day": {"used_percentage": 2.0, "resets_at": 9000}}
            }),
            101,
        );
        let u = load_from(&p).unwrap();
        assert_eq!(u.seven_day.unwrap().used_percentage, 30.0);
        assert_eq!(u.writers.len(), 2, "both sessions tracked");
    }

    #[test]
    fn writers_are_pruned_after_ttl() {
        let p = tmppath("prune");
        let _ = std::fs::remove_file(&p);
        let push = |sid: &str, cost: f64, at: u64| {
            save_from_statusline_at(
                &p,
                &json!({
                    "session_id": sid,
                    "cost": {"total_cost_usd": cost},
                    "rate_limits": {"five_hour": {"used_percentage": 10.0, "resets_at": at + 1000}}
                }),
                at,
            );
        };
        push("old", 1.0, 100);
        push("new", 1.0, 100 + WRITER_TTL_SECS + 1);
        let u = load_from(&p).unwrap();
        assert!(!u.writers.contains_key("old"), "expired writer dropped");
        assert!(u.writers.contains_key("new"));
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
