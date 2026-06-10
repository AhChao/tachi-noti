//! Pure model layer for the tachi-bar menu: turns session state files into a
//! renderable snapshot. No AppKit here so it stays unit-testable.

use crate::notify;
use crate::state::{SessionState, Status, format_duration};
use std::time::Duration;

/// Sessions silent this long disappear from the menu (died without SessionEnd).
pub const HIDE_AFTER_SECS: u64 = 12 * 3600;

pub struct Snapshot {
    pub groups: Vec<Group>,
    pub running: usize,
    pub waiting: usize,
}

pub struct Group {
    pub header: String,
    pub rows: Vec<Row>,
}

pub struct Row {
    pub glyph: &'static str,
    pub label: String,
    pub bundle_id: Option<String>,
    pub open_path: Option<String>,
    pub enabled: bool,
}

fn glyph(status: Status) -> &'static str {
    match status {
        Status::Running => "\u{1F7E2}", // 🟢
        Status::Waiting => "\u{1F7E1}", // 🟡
        Status::Idle => "\u{26AA}",     // ⚪
    }
}

fn status_word(status: Status) -> &'static str {
    match status {
        Status::Running => "running",
        Status::Waiting => "waiting",
        Status::Idle => "idle",
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
        .collect();
    live.sort_by_key(|s| std::cmp::Reverse(s.last_event_at));

    let running = live.iter().filter(|s| s.status == Status::Running).count();
    let waiting = live.iter().filter(|s| s.status == Status::Waiting).count();

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

    Snapshot { groups, running, waiting }
}

pub fn build_row(s: &SessionState, group_key: &str, now: u64) -> Row {
    let anchor = match s.status {
        Status::Running => s.task_started_at.unwrap_or(s.status_since),
        _ => s.status_since,
    };
    let elapsed = format_duration(Duration::from_secs(now.saturating_sub(anchor)));
    let id6: String = s.session_id.chars().take(6).collect();
    let mut label = format!("{} {elapsed} \u{00B7} {id6}", status_word(s.status));
    if s.cwd != *group_key {
        label.push_str(&format!(" \u{00B7} {}", basename(&s.cwd)));
    }
    let open_path = s
        .bundle_id
        .as_deref()
        .filter(|b| notify::vscode_family_bundle(b))
        .map(|_| group_key.to_string());
    Row {
        glyph: glyph(s.status),
        label,
        bundle_id: s.bundle_id.clone(),
        open_path,
        enabled: s.bundle_id.is_some(),
    }
}

pub fn title(s: &Snapshot) -> String {
    match (s.waiting, s.running) {
        (0, 0) => "\u{1F415}".into(), // 🐕
        (0, r) => format!("\u{1F7E2}{r}"),
        (w, 0) => format!("\u{1F7E1}{w}"),
        (w, r) => format!("\u{1F7E1}{w} \u{1F7E2}{r}"),
    }
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
    fn title_summarizes() {
        let mk = |w, r| Snapshot { groups: vec![], running: r, waiting: w };
        assert_eq!(title(&mk(0, 0)), "\u{1F415}");
        assert_eq!(title(&mk(0, 2)), "\u{1F7E2}2");
        assert_eq!(title(&mk(1, 0)), "\u{1F7E1}1");
        assert_eq!(title(&mk(1, 2)), "\u{1F7E1}1 \u{1F7E2}2");
    }
}
