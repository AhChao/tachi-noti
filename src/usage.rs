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

/// Last-seen activity fingerprint of one statusline-pushing stream — one
/// Claude Code process. A session_id is NOT a process: resuming a session in
/// a new process while the old one still runs yields two streams under one
/// id, each with its own cost counters, and they must be tracked apart.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Writer {
    pub fp: String,
    /// When the fingerprint last changed (unix seconds) — prune key.
    pub at: u64,
    /// Owning session_id (the map key also carries the process start).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub session: String,
    /// Process start (unix seconds) derived from total_duration_ms; None for
    /// payloads that don't carry it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Usage {
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
    pub updated_at: u64,
    /// stream key → activity fingerprint. A stream whose fingerprint just
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

/// Unix second the pushing process started: `now − total_duration_ms`. This
/// is stable across one process's renders (only rounding jitter) and differs
/// between two processes sharing a session_id, so it tells them apart.
fn process_start(payload: &Value, now: u64) -> Option<u64> {
    let dur_ms = payload["cost"]["total_duration_ms"].as_f64()?;
    Some(now.saturating_sub((dur_ms / 1000.0) as u64))
}

/// Two derived starts this close are the same process (second rounding of
/// `now`, render latency, a slow hook spawn).
const START_SLOP_SECS: u64 = 30;

/// Map key of the stream that pushed `payload`: an existing writer of the
/// same session whose start is within the slop, else a new
/// `session@start` key. Payloads without a duration fall back to the bare
/// session_id.
fn stream_key(writers: &HashMap<String, Writer>, session: &str, start: Option<u64>) -> String {
    let Some(start) = start else { return session.to_string() };
    writers
        .iter()
        .find(|(_, w)| w.session == session && w.start.is_some_and(|s| s.abs_diff(start) <= START_SLOP_SECS))
        .map(|(k, _)| k.clone())
        .unwrap_or_else(|| format!("{session}@{start}"))
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
///    First sighting of a stream is recorded but NOT trusted: it may be a
///    long-idle re-pusher we simply hadn't tracked yet.
///    Streams are keyed per PROCESS (session_id + derived process start),
///    not per session_id: a session resumed in a second process while the
///    first still runs pushes two alternating frozen fingerprints under one
///    id, and keyed by session_id alone every alternation looked like a
///    fresh API response — a 17-hour-old 88% kept overwriting a live 94%.
/// 3. Anything else may only fill an empty slot or replace one whose own
///    boundary expired — never overwrite live data.
fn save_from_statusline_at(path: &Path, payload: &Value, now: u64) {
    let limits = &payload["rate_limits"];
    let new_five = window(&limits["five_hour"]).filter(|w| !is_expired(w, now));
    let new_seven = window(&limits["seven_day"]).filter(|w| !is_expired(w, now));
    if new_five.is_none() && new_seven.is_none() {
        return;
    }
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    // Every live session pushes every second: serialize load → modify →
    // rename, or a writers-only update built from a stale read clobbers a
    // fresh value another process just wrote.
    let _lock = lock(path);
    let mut prev = load_from(path).unwrap_or_default();

    let session = payload["session_id"].as_str().unwrap_or("");
    let mut authoritative = false;
    let mut writers_changed = false;
    if let (false, Some(fp)) = (session.is_empty(), fingerprint(payload)) {
        let start = process_start(payload, now);
        let key = stream_key(&prev.writers, session, start);
        match prev.writers.get(&key) {
            Some(w) if w.fp == fp => {} // unchanged → plain re-push
            seen => {
                authoritative = seen.is_some();
                writers_changed = true;
                let session = session.to_string();
                prev.writers.insert(key, Writer { fp, at: now, session, start });
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
    let Ok(json) = serde_json::to_string(&usage) else { return };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.tmp.{}", std::process::id()));
    if std::fs::write(&tmp, json).is_ok() && std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Exclusive advisory lock beside `path`, released on drop. Best-effort: if
/// the lock file can't be opened we proceed unlocked rather than drop data.
fn lock(path: &Path) -> Option<std::fs::File> {
    use std::os::fd::AsRawFd;
    let name = path.file_name()?.to_string_lossy().into_owned();
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_file_name(format!(".{name}.lock")))
        .ok()?;
    let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) };
    (rc == 0).then_some(f)
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

    /// One statusline push from a process started at `start` (unix secs).
    fn push_proc(p: &Path, sid: &str, start: u64, cost: f64, pct: f64, now: u64) {
        save_from_statusline_at(
            p,
            &json!({"session_id": sid,
                "cost": {"total_cost_usd": cost, "total_duration_ms": ((now - start) * 1000) as f64},
                "rate_limits": {"seven_day": {"used_percentage": pct, "resets_at": 2_000_000}}}),
            now,
        );
    }

    #[test]
    fn forked_session_streams_cannot_clobber_live_data() {
        // Observed: session b2420100 pushed from two processes — a 9-day-old
        // one frozen at cost 90.724 / 88%, and a resumed one frozen at
        // 54.014 / 93% — alternating every few seconds, while other sessions
        // were live at 94%.
        let p = tmppath("forked");
        let _ = std::fs::remove_file(&p);
        let t0 = 1_000_000;
        push_proc(&p, "live", t0 - 60, 1.0, 93.0, t0);
        push_proc(&p, "live", t0 - 60, 2.0, 94.0, t0 + 1); // fresh API turn
        assert_eq!(load_from(&p).unwrap().seven_day.unwrap().used_percentage, 94.0);
        let (old_start, resumed_start) = (t0 - 9 * 86_400, t0 - 7_000);
        for i in 0..10 {
            let now = t0 + 2 + i * 2;
            push_proc(&p, "b2420100", old_start, 90.724, 88.0, now);
            push_proc(&p, "b2420100", resumed_start, 54.014, 93.0, now + 1);
        }
        let u = load_from(&p).unwrap();
        assert_eq!(u.seven_day.unwrap().used_percentage, 94.0, "frozen streams must not overwrite");
        assert_eq!(u.writers.values().filter(|w| w.session == "b2420100").count(), 2);
    }

    #[test]
    fn resumed_process_regains_authority_despite_lower_counters() {
        // A resumed process restarts its cost counter below the old
        // process's; it must still earn authority on its own first API turn.
        let p = tmppath("resumed");
        let _ = std::fs::remove_file(&p);
        let t0 = 1_000_000;
        push_proc(&p, "s", t0 - 86_400, 90.0, 50.0, t0); // old process
        push_proc(&p, "s", t0 - 86_400, 91.0, 60.0, t0 + 1); // live → 60
        push_proc(&p, "s", t0 - 5, 0.5, 70.0, t0 + 2); // resume: first sighting
        assert_eq!(load_from(&p).unwrap().seven_day.unwrap().used_percentage, 60.0);
        push_proc(&p, "s", t0 - 5, 0.8, 55.0, t0 + 3); // resumed process's API turn
        push_proc(&p, "s", t0 - 86_400, 91.0, 60.0, t0 + 4); // old one re-pushes
        let u = load_from(&p).unwrap();
        assert_eq!(u.seven_day.unwrap().used_percentage, 55.0, "decrease from new process lands");
    }

    #[test]
    fn start_jitter_stays_one_stream() {
        let p = tmppath("jitter");
        let _ = std::fs::remove_file(&p);
        push_proc(&p, "s", 1000, 1.0, 10.0, 5000);
        push_proc(&p, "s", 1003, 1.0, 10.0, 5001); // rounding/latency drift
        assert_eq!(load_from(&p).unwrap().writers.len(), 1);
    }

    #[test]
    fn legacy_writer_entries_still_load() {
        let p = tmppath("legacy");
        std::fs::write(&p, r#"{"updated_at":1,"writers":{"s1":{"fp":"1:0","at":1}}}"#).unwrap();
        assert!(load_from(&p).unwrap().writers["s1"].start.is_none());
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
