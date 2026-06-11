use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const STALE_AFTER_SECS: u64 = 48 * 3600;
const MAX_TASK_SECS: u64 = 24 * 3600;
/// PostToolUse only rewrites the file as a freshness heartbeat this often.
const HEARTBEAT_SECS: u64 = 60;

pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("tachi-noti")
}

pub fn state_dir() -> PathBuf {
    data_dir().join("state")
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SessionState {
    pub version: u32,
    pub session_id: String,
    pub repo_name: String,
    pub repo_root: Option<String>,
    pub branch: Option<String>,
    pub bundle_id: Option<String>,
    pub cwd: String,
    pub status: Status,
    pub status_since: u64,
    pub task_started_at: Option<u64>,
    pub last_event_at: u64,
    /// What the session is waiting on, when status == Waiting.
    #[serde(default)]
    pub waiting: Option<WaitingInfo>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct WaitingInfo {
    pub kind: WaitKind,
    pub detail: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum WaitKind {
    Permission,
    Plan,
    Question,
    Idle,
}

impl WaitKind {
    /// Signal specificity: a weaker signal must never overwrite a stronger one.
    fn rank(self) -> u8 {
        match self {
            WaitKind::Plan | WaitKind::Question => 3,
            WaitKind::Permission => 2,
            WaitKind::Idle => 1,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Running,
    Waiting,
    Idle,
}

/// Session context captured by the hook at event time.
pub struct Ctx {
    pub session_id: String,
    pub repo_name: String,
    pub repo_root: Option<String>,
    pub branch: Option<String>,
    pub bundle_id: Option<String>,
    pub cwd: String,
}

pub enum Event<'a> {
    SessionStart { source: Option<&'a str> },
    PromptSubmit,
    Stop,
    Waiting(WaitingInfo),
    PostToolUse { tool_name: Option<&'a str> },
}

pub struct Transition {
    pub save: Option<SessionState>,
    /// Completed task duration, only on Stop with a plausible start marker.
    pub task_duration: Option<Duration>,
}

pub fn now_epoch() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

/// Pure state transition; `file_age_secs` is the state file's age (for the
/// PostToolUse heartbeat decision). Never panics.
pub fn apply_event(prev: Option<SessionState>, ctx: &Ctx, ev: Event, now: u64, file_age_secs: Option<u64>) -> Transition {
    let fresh = |status: Status| SessionState {
        version: 1,
        session_id: ctx.session_id.clone(),
        repo_name: ctx.repo_name.clone(),
        repo_root: ctx.repo_root.clone(),
        branch: ctx.branch.clone(),
        bundle_id: ctx.bundle_id.clone(),
        cwd: ctx.cwd.clone(),
        status,
        status_since: now,
        task_started_at: None,
        last_event_at: now,
        waiting: None,
    };
    // Refresh identity fields on every event (branch may change mid-session).
    let carry = |mut s: SessionState| {
        s.repo_name = ctx.repo_name.clone();
        s.repo_root = ctx.repo_root.clone();
        s.branch = ctx.branch.clone();
        if ctx.bundle_id.is_some() {
            s.bundle_id = ctx.bundle_id.clone();
        }
        s.cwd = ctx.cwd.clone();
        s.last_event_at = now;
        s
    };
    let set_status = |mut s: SessionState, status: Status| {
        if s.status != status {
            s.status_since = now;
        }
        s.status = status;
        if status != Status::Waiting {
            s.waiting = None;
        }
        s
    };

    match ev {
        Event::SessionStart { source } => {
            if source == Some("compact") {
                // Mid-task compaction: keep status/task, just refresh liveness.
                let s = prev.map(carry).unwrap_or_else(|| fresh(Status::Idle));
                return Transition { save: Some(s), task_duration: None };
            }
            Transition { save: Some(fresh(Status::Idle)), task_duration: None }
        }
        Event::PromptSubmit => {
            let mut s = set_status(prev.map(carry).unwrap_or_else(|| fresh(Status::Running)), Status::Running);
            s.task_started_at = Some(now);
            Transition { save: Some(s), task_duration: None }
        }
        Event::Stop => {
            let mut s = set_status(prev.map(carry).unwrap_or_else(|| fresh(Status::Idle)), Status::Idle);
            let duration = s
                .task_started_at
                .and_then(|start| now.checked_sub(start))
                .filter(|secs| *secs <= MAX_TASK_SECS)
                .map(Duration::from_secs);
            s.task_started_at = None;
            Transition { save: Some(s), task_duration: duration }
        }
        Event::Waiting(info) => {
            // Keep task_started_at — the task is still in flight.
            let mut s = set_status(prev.map(carry).unwrap_or_else(|| fresh(Status::Waiting)), Status::Waiting);
            // Upgrade, never downgrade: a weaker signal (e.g. the generic
            // Notification message) must not clobber a richer one (e.g. the
            // exact command from PermissionRequest).
            s.waiting = match s.waiting.take() {
                Some(old) if old.kind.rank() > info.kind.rank() => Some(old),
                Some(old) if old.kind.rank() == info.kind.rank() && old.detail.is_some() => Some(old),
                _ => Some(info),
            };
            Transition { save: Some(s), task_duration: None }
        }
        Event::PostToolUse { tool_name } => match prev {
            Some(s) if s.status == Status::Waiting => {
                // Plan/question waits end only when their own tool completes
                // (a parallel subagent's tool result must not clear them);
                // permission waits end on any tool completing.
                let resolved = match s.waiting.as_ref().map(|w| w.kind) {
                    Some(WaitKind::Plan) => tool_name == Some("ExitPlanMode"),
                    Some(WaitKind::Question) => tool_name == Some("AskUserQuestion"),
                    _ => true,
                };
                if resolved {
                    let s = set_status(carry(s), Status::Running);
                    Transition { save: Some(s), task_duration: None }
                } else {
                    Transition { save: Some(carry(s)), task_duration: None }
                }
            }
            Some(s) if file_age_secs.map(|a| a >= HEARTBEAT_SECS).unwrap_or(true) => {
                Transition { save: Some(carry(s)), task_duration: None }
            }
            Some(_) => Transition { save: None, task_duration: None },
            None => Transition { save: Some(fresh(Status::Running)), task_duration: None },
        },
    }
}

fn path_for(session_id: &str) -> PathBuf {
    state_dir().join(format!("{}.json", sanitize(session_id)))
}

/// Session ids are UUIDs in practice, but never trust input used as a filename.
pub fn sanitize(id: &str) -> String {
    let s: String = id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    if s.is_empty() { "unknown".into() } else { s }
}

pub fn load(session_id: &str) -> Option<SessionState> {
    let text = std::fs::read_to_string(path_for(session_id)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Age of the session's state file in seconds (None if missing).
pub fn file_age_secs(session_id: &str) -> Option<u64> {
    let modified = std::fs::metadata(path_for(session_id)).ok()?.modified().ok()?;
    SystemTime::now().duration_since(modified).ok().map(|d| d.as_secs())
}

pub fn save(s: &SessionState) {
    let dir = state_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let Ok(json) = serde_json::to_string(s) else { return };
    let tmp = dir.join(format!(".tmp.{}.{}", std::process::id(), sanitize(&s.session_id)));
    if std::fs::write(&tmp, json).is_ok() && std::fs::rename(&tmp, path_for(&s.session_id)).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

pub fn remove(session_id: &str) {
    let _ = std::fs::remove_file(path_for(session_id));
}

pub fn load_all() -> Vec<SessionState> {
    let Ok(entries) = std::fs::read_dir(state_dir()) else { return vec![] };
    entries
        .flatten()
        .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
        .filter_map(|e| serde_json::from_str(&std::fs::read_to_string(e.path()).ok()?).ok())
        .collect()
}

pub fn live_count() -> usize {
    load_all().len()
}

/// Remove state (and stray tmp) files untouched for 48h — sessions that died
/// without a SessionEnd.
pub fn cleanup_stale() {
    let Ok(entries) = std::fs::read_dir(state_dir()) else { return };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .map(|age| age.as_secs() > STALE_AFTER_SECS)
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
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

    fn ctx(id: &str) -> Ctx {
        Ctx {
            session_id: id.into(),
            repo_name: "myrepo".into(),
            repo_root: Some("/tmp/myrepo".into()),
            branch: Some("main".into()),
            bundle_id: Some("com.example.ide".into()),
            cwd: "/tmp/myrepo".into(),
        }
    }

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
    fn full_lifecycle() {
        let c = ctx("s1");
        let t = apply_event(None, &c, Event::SessionStart { source: Some("startup") }, 100, None);
        let s = t.save.unwrap();
        assert_eq!(s.status, Status::Idle);

        let t = apply_event(Some(s), &c, Event::PromptSubmit, 110, Some(0));
        let s = t.save.unwrap();
        assert_eq!(s.status, Status::Running);
        assert_eq!(s.task_started_at, Some(110));
        assert_eq!(s.status_since, 110);

        let t = apply_event(Some(s), &c, Event::Waiting(wait(WaitKind::Permission, Some("cargo test"))), 150, Some(0));
        let s = t.save.unwrap();
        assert_eq!(s.status, Status::Waiting);
        assert_eq!(s.task_started_at, Some(110), "waiting keeps the task in flight");
        assert_eq!(s.waiting.as_ref().unwrap().detail.as_deref(), Some("cargo test"));

        let t = apply_event(Some(s), &c, Event::PostToolUse { tool_name: Some("Bash") }, 160, Some(0));
        let s = t.save.unwrap();
        assert_eq!(s.status, Status::Running, "answered permission resumes running");
        assert_eq!(s.waiting, None, "leaving Waiting clears the reason");

        let t = apply_event(Some(s), &c, Event::Stop, 310, Some(0));
        let s = t.save.unwrap();
        assert_eq!(s.status, Status::Idle);
        assert_eq!(t.task_duration, Some(Duration::from_secs(200)));
        assert_eq!(s.task_started_at, None);
    }

    #[test]
    fn compact_preserves_running_state() {
        let c = ctx("s2");
        let s = apply_event(None, &c, Event::PromptSubmit, 100, None).save.unwrap();
        let t = apply_event(Some(s), &c, Event::SessionStart { source: Some("compact") }, 200, Some(0));
        let s = t.save.unwrap();
        assert_eq!(s.status, Status::Running);
        assert_eq!(s.task_started_at, Some(100));
        assert_eq!(s.last_event_at, 200);
    }

    #[test]
    fn clear_resets_state() {
        let c = ctx("s3");
        let s = apply_event(None, &c, Event::PromptSubmit, 100, None).save.unwrap();
        let t = apply_event(Some(s), &c, Event::SessionStart { source: Some("clear") }, 200, Some(0));
        let s = t.save.unwrap();
        assert_eq!(s.status, Status::Idle);
        assert_eq!(s.task_started_at, None);
    }

    #[test]
    fn post_tool_use_heartbeat_throttles() {
        let c = ctx("s4");
        let s = apply_event(None, &c, Event::PromptSubmit, 100, None).save.unwrap();
        let t = apply_event(Some(s.clone()), &c, Event::PostToolUse { tool_name: Some("Read") }, 130, Some(10));
        assert!(t.save.is_none(), "fresh file: no write");
        let t = apply_event(Some(s), &c, Event::PostToolUse { tool_name: Some("Read") }, 200, Some(90));
        assert!(t.save.is_some(), "old file: heartbeat write");
    }

    fn wait(kind: WaitKind, detail: Option<&str>) -> WaitingInfo {
        WaitingInfo { kind, detail: detail.map(str::to_string) }
    }

    #[test]
    fn weaker_signal_never_downgrades_waiting() {
        let c = ctx("s6");
        let s = apply_event(None, &c, Event::Waiting(wait(WaitKind::Permission, Some("rm -rf target"))), 100, None)
            .save
            .unwrap();
        // Generic Notification arrives after the rich PermissionRequest.
        let s = apply_event(
            Some(s),
            &c,
            Event::Waiting(wait(WaitKind::Permission, Some("Claude needs your permission"))),
            101,
            Some(0),
        )
        .save
        .unwrap();
        assert_eq!(s.waiting.as_ref().unwrap().detail.as_deref(), Some("rm -rf target"));
        // Idle signal must not clobber a plan wait.
        let s = apply_event(Some(s), &c, Event::Waiting(wait(WaitKind::Plan, None)), 102, Some(0)).save.unwrap();
        let s = apply_event(Some(s), &c, Event::Waiting(wait(WaitKind::Idle, None)), 103, Some(0)).save.unwrap();
        assert_eq!(s.waiting.as_ref().unwrap().kind, WaitKind::Plan);
    }

    #[test]
    fn plan_wait_survives_unrelated_tool_results() {
        let c = ctx("s7");
        let s = apply_event(None, &c, Event::Waiting(wait(WaitKind::Plan, None)), 100, None).save.unwrap();
        // A parallel subagent's Bash result must not clear the plan-approval wait.
        let s = apply_event(Some(s), &c, Event::PostToolUse { tool_name: Some("Bash") }, 110, Some(0)).save.unwrap();
        assert_eq!(s.status, Status::Waiting);
        assert_eq!(s.waiting.as_ref().unwrap().kind, WaitKind::Plan);
        // Its own tool completing does clear it.
        let s = apply_event(Some(s), &c, Event::PostToolUse { tool_name: Some("ExitPlanMode") }, 120, Some(0))
            .save
            .unwrap();
        assert_eq!(s.status, Status::Running);
    }

    #[test]
    fn old_state_files_without_waiting_field_deserialize() {
        let json = r#"{"version":1,"session_id":"x","repo_name":"r","repo_root":null,"branch":null,
            "bundle_id":null,"cwd":"/tmp","status":"idle","status_since":1,"task_started_at":null,"last_event_at":1}"#;
        let s: SessionState = serde_json::from_str(json).unwrap();
        assert_eq!(s.waiting, None);
    }

    #[test]
    fn stop_discards_implausible_duration() {
        let c = ctx("s5");
        let mut s = apply_event(None, &c, Event::PromptSubmit, 100, None).save.unwrap();
        s.task_started_at = Some(999_999); // future start (clock skew)
        let t = apply_event(Some(s), &c, Event::Stop, 200, Some(0));
        assert_eq!(t.task_duration, None);
    }

    #[test]
    fn save_load_roundtrip() {
        let c = ctx(&format!("roundtrip-{}", std::process::id()));
        let s = apply_event(None, &c, Event::PromptSubmit, now_epoch(), None).save.unwrap();
        save(&s);
        let loaded = load(&c.session_id).expect("state should exist");
        assert_eq!(loaded.status, Status::Running);
        assert_eq!(loaded.repo_name, "myrepo");
        remove(&c.session_id);
        assert!(load(&c.session_id).is_none());
    }
}
