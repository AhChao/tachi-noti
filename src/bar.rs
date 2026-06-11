//! Pure model layer for the tachi-bar menu: turns session state files into a
//! renderable snapshot. No AppKit here so it stays unit-testable.

use crate::notify;
use crate::state::{SessionState, Status, WaitKind, format_duration};
use std::time::Duration;

/// Sessions silent this long disappear from the menu (died without SessionEnd).
pub const HIDE_AFTER_SECS: u64 = 12 * 3600;

/// A genuinely running task heartbeats via PostToolUse at least every minute;
/// a "running" session silent this long was killed mid-task — show it idle.
pub const RUNNING_STALE_SECS: u64 = 30 * 60;

pub struct Snapshot {
    pub groups: Vec<Group>,
    pub running: usize,
    pub waiting: usize,
    pub idle: usize,
}

pub struct Group {
    pub header: String,
    pub rows: Vec<Row>,
}

pub struct Row {
    pub status: Status,
    pub label: String,
    /// Full waiting detail for hover (label only carries a truncated hint).
    pub tooltip: Option<String>,
    pub bundle_id: Option<String>,
    pub open_path: Option<String>,
    pub enabled: bool,
}

fn status_word(s: &SessionState) -> &'static str {
    match s.status {
        Status::Running => "running",
        Status::Idle => "idle",
        Status::Waiting => match s.waiting.as_ref().map(|w| w.kind) {
            Some(WaitKind::Plan) => "plan ready?",
            Some(WaitKind::Question) => "question?",
            Some(WaitKind::Permission) => "permission",
            Some(WaitKind::Idle) | None => "waiting",
        },
    }
}

/// Waiting outranks running (it needs the user), idle last.
fn status_rank(status: Status) -> u8 {
    match status {
        Status::Waiting => 0,
        Status::Running => 1,
        Status::Idle => 2,
    }
}

fn basename(p: &str) -> &str {
    p.trim_end_matches('/').rsplit('/').next().unwrap_or(p)
}

pub fn build_snapshot(states: Vec<SessionState>, now: u64) -> Snapshot {
    let mut live: Vec<SessionState> = states
        .into_iter()
        .filter(|s| now.saturating_sub(s.last_event_at) <= HIDE_AFTER_SECS)
        .map(|mut s| {
            if s.status == Status::Running && now.saturating_sub(s.last_event_at) > RUNNING_STALE_SECS {
                s.status = Status::Idle;
                s.status_since = s.last_event_at;
                s.task_started_at = None;
            }
            s
        })
        .collect();
    live.sort_by_key(|s| std::cmp::Reverse(s.last_event_at));

    let running = live.iter().filter(|s| s.status == Status::Running).count();
    let waiting = live.iter().filter(|s| s.status == Status::Waiting).count();
    let idle = live.iter().filter(|s| s.status == Status::Idle).count();

    // Group by repo root (fallback cwd), preserving most-recent-first group order.
    let mut groups: Vec<(String, String, Vec<SessionState>)> = Vec::new(); // (key, header, members)
    for s in live {
        let key = s.repo_root.clone().unwrap_or_else(|| s.cwd.clone());
        let header = match &s.branch {
            Some(b) => format!("{} @ {b}", s.repo_name),
            None => s.repo_name.clone(),
        };
        match groups.iter_mut().find(|(k, _, _)| *k == key) {
            Some((_, _, members)) => members.push(s),
            None => groups.push((key, header, vec![s])),
        }
    }

    let groups = groups
        .into_iter()
        .map(|(key, header, mut members)| {
            members.sort_by_key(|s| (status_rank(s.status), std::cmp::Reverse(s.last_event_at)));
            let rows = members.iter().map(|s| build_row(s, &key, now)).collect();
            Group { header, rows }
        })
        .collect();

    Snapshot { groups, running, waiting, idle }
}

pub fn build_row(s: &SessionState, group_key: &str, now: u64) -> Row {
    let anchor = match s.status {
        Status::Running => s.task_started_at.unwrap_or(s.status_since),
        _ => s.status_since,
    };
    let elapsed = format_duration(Duration::from_secs(now.saturating_sub(anchor)));
    let id6: String = s.session_id.chars().take(6).collect();
    let mut label = format!("{} {elapsed} \u{00B7} {id6}", status_word(s));
    if s.cwd != *group_key {
        label.push_str(&format!(" \u{00B7} {}", basename(&s.cwd)));
    }
    let waiting_detail = s.waiting.as_ref().and_then(|w| w.detail.clone());
    // A permission wait names the exact command — surface a hint inline.
    if s.waiting.as_ref().map(|w| w.kind) == Some(WaitKind::Permission) {
        if let Some(d) = &waiting_detail {
            let hint: String = d.chars().take(24).collect();
            label.push_str(&format!(" \u{00B7} {hint}{}", if d.chars().count() > 24 { "\u{2026}" } else { "" }));
        }
    }
    let open_path = s
        .bundle_id
        .as_deref()
        .filter(|b| notify::vscode_family_bundle(b))
        .map(|_| group_key.to_string());
    Row {
        status: s.status,
        label,
        tooltip: waiting_detail,
        bundle_id: s.bundle_id.clone(),
        open_path,
        enabled: s.bundle_id.is_some(),
    }
}

/// Title segments next to the dog icon, ordered by urgency: waiting (needs
/// you), running, idle (free capacity — visible so spare sessions register at
/// a glance). Empty = no sessions, the dog stands alone.
pub fn title_segments(s: &Snapshot) -> Vec<(usize, Status)> {
    let mut parts = Vec::new();
    if s.waiting > 0 {
        parts.push((s.waiting, Status::Waiting));
    }
    if s.running > 0 {
        parts.push((s.running, Status::Running));
    }
    if s.idle > 0 {
        parts.push((s.idle, Status::Idle));
    }
    parts
}

/// Choices for the completion-sound picker: Tachi's bark, the system sounds,
/// and silence. Returns (label, config value) pairs.
pub fn sound_options() -> Vec<(String, String)> {
    let mut opts = vec![("Tachi Bark".to_string(), "TachiBark".to_string())];
    let mut system: Vec<String> = std::fs::read_dir("/System/Library/Sounds")
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    name.strip_suffix(".aiff").map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    system.sort();
    opts.extend(system.into_iter().map(|n| (n.clone(), n)));
    opts.push(("Silent".to_string(), String::new()));
    opts
}

/// Resolve a sound value to a playable file for the in-menu preview.
pub fn sound_preview_path(value: &str) -> Option<std::path::PathBuf> {
    if value.is_empty() {
        return None;
    }
    let candidates = [
        dirs::home_dir()?.join(format!("Library/Sounds/{value}.aiff")),
        std::path::PathBuf::from(format!("/System/Library/Sounds/{value}.aiff")),
    ];
    candidates.into_iter().find(|p| p.exists())
}

/// Focus the window hosting a session. VS Code-family apps get the folder path
/// (window-precise); other apps a plain activation. argv-only, no shell.
pub fn focus(bundle_id: &str, open_path: Option<&str>) {
    let mut cmd = std::process::Command::new("/usr/bin/open");
    cmd.arg("-b").arg(bundle_id);
    if let Some(p) = open_path {
        cmd.arg(p);
    }
    let _ = cmd.spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, root: &str, status: Status, last_event_at: u64) -> SessionState {
        SessionState {
            version: 1,
            session_id: id.into(),
            repo_name: basename(root).into(),
            repo_root: Some(root.into()),
            branch: Some("main".into()),
            bundle_id: Some("com.microsoft.VSCode".into()),
            cwd: root.into(),
            status,
            status_since: last_event_at.saturating_sub(60),
            task_started_at: if status == Status::Running { Some(last_event_at.saturating_sub(200)) } else { None },
            last_event_at,
            waiting: None,
        }
    }

    #[test]
    fn groups_by_repo_root_most_recent_first() {
        let snap = build_snapshot(
            vec![
                session("aaa111", "/r/alpha", Status::Idle, 100),
                session("bbb222", "/r/beta", Status::Running, 300),
                session("ccc333", "/r/alpha", Status::Waiting, 200),
            ],
            400,
        );
        assert_eq!(snap.groups.len(), 2);
        assert_eq!(snap.groups[0].header, "beta @ main");
        assert_eq!(snap.groups[1].header, "alpha @ main");
        assert_eq!(snap.groups[1].rows.len(), 2);
        // Waiting outranks idle within the group.
        assert!(snap.groups[1].rows[0].label.starts_with("waiting"));
        assert_eq!(snap.running, 1);
        assert_eq!(snap.waiting, 1);
    }

    #[test]
    fn hides_stale_sessions() {
        let now = HIDE_AFTER_SECS + 1000;
        let snap = build_snapshot(
            vec![session("old", "/r/old", Status::Running, 1), session("new", "/r/new", Status::Idle, now - 10)],
            now,
        );
        assert_eq!(snap.groups.len(), 1);
        assert_eq!(snap.groups[0].header, "new @ main");
        assert_eq!(snap.running, 0, "stale sessions don't count");
    }

    #[test]
    fn row_label_and_click_target() {
        let mut s = session("abcdef123", "/r/proj", Status::Running, 1000);
        s.cwd = "/r/proj/sub".into();
        let snap = build_snapshot(vec![s], 1100);
        let row = &snap.groups[0].rows[0];
        assert_eq!(row.label, "running 5m00s \u{00B7} abcdef \u{00B7} sub");
        assert_eq!(row.open_path.as_deref(), Some("/r/proj"), "click opens repo root, not cwd");
        assert!(row.enabled);
    }

    #[test]
    fn non_vscode_bundle_gets_no_path() {
        let mut s = session("x", "/r/p", Status::Idle, 100);
        s.bundle_id = Some("com.apple.Terminal".into());
        let snap = build_snapshot(vec![s], 200);
        assert_eq!(snap.groups[0].rows[0].open_path, None);
        assert!(snap.groups[0].rows[0].enabled);
    }

    #[test]
    fn unknown_bundle_disables_row() {
        let mut s = session("x", "/r/p", Status::Idle, 100);
        s.bundle_id = None;
        let snap = build_snapshot(vec![s], 200);
        assert!(!snap.groups[0].rows[0].enabled);
    }

    #[test]
    fn waiting_rows_show_semantics() {
        use crate::state::{WaitKind, WaitingInfo};
        let mut s = session("abcdef", "/r/p", Status::Waiting, 100);
        s.waiting = Some(WaitingInfo { kind: WaitKind::Plan, detail: Some("plan ready — approve?".into()) });
        let snap = build_snapshot(vec![s.clone()], 160);
        let row = &snap.groups[0].rows[0];
        assert!(row.label.starts_with("plan ready? 2m00s"), "label was: {}", row.label);
        assert_eq!(row.tooltip.as_deref(), Some("plan ready — approve?"));

        s.waiting = Some(WaitingInfo {
            kind: WaitKind::Permission,
            detail: Some("cargo install --path . --features bar".into()),
        });
        let snap = build_snapshot(vec![s.clone()], 160);
        let row = &snap.groups[0].rows[0];
        assert!(row.label.starts_with("permission"), "label was: {}", row.label);
        assert!(row.label.contains("cargo install --path . -\u{2026}"), "inline hint truncated: {}", row.label);

        s.waiting = Some(WaitingInfo { kind: WaitKind::Question, detail: None });
        let snap = build_snapshot(vec![s], 160);
        assert!(snap.groups[0].rows[0].label.starts_with("question?"));
    }

    #[test]
    fn title_summarizes() {
        let mk = |w, r, i| Snapshot { groups: vec![], running: r, waiting: w, idle: i };
        assert!(title_segments(&mk(0, 0, 0)).is_empty(), "no sessions: dog stands alone");
        assert_eq!(title_segments(&mk(0, 0, 3)), vec![(3, Status::Idle)]);
        assert_eq!(
            title_segments(&mk(0, 2, 5)),
            vec![(2, Status::Running), (5, Status::Idle)],
            "idle always visible — it's usable capacity"
        );
        assert_eq!(title_segments(&mk(1, 0, 0)), vec![(1, Status::Waiting)]);
        assert_eq!(
            title_segments(&mk(1, 2, 4)),
            vec![(1, Status::Waiting), (2, Status::Running), (4, Status::Idle)],
            "waiting leads — it needs the user"
        );
    }

    #[test]
    fn multiple_sessions_same_repo_all_counted() {
        let snap = build_snapshot(
            vec![
                session("aaa", "/r/proj", Status::Running, 300),
                session("bbb", "/r/proj", Status::Running, 200),
                session("ccc", "/r/proj", Status::Waiting, 100),
            ],
            400,
        );
        assert_eq!(snap.groups.len(), 1, "one group for the repo");
        assert_eq!(snap.groups[0].rows.len(), 3, "every session gets a row");
        assert_eq!(snap.running, 2);
        assert_eq!(snap.waiting, 1);
        assert_eq!(title_segments(&snap), vec![(1, Status::Waiting), (2, Status::Running)]);
    }

    #[test]
    fn stale_running_demotes_to_idle() {
        let now = 100_000;
        let snap = build_snapshot(
            vec![
                session("dead", "/r/p", Status::Running, now - RUNNING_STALE_SECS - 10),
                session("live", "/r/p", Status::Running, now - 30),
            ],
            now,
        );
        assert_eq!(snap.running, 1, "killed-mid-task session no longer counts as running");
        assert_eq!(snap.idle, 1);
        let labels: Vec<&str> = snap.groups[0].rows.iter().map(|r| r.label.split(' ').next().unwrap()).collect();
        assert!(labels.contains(&"idle"));
    }
}
