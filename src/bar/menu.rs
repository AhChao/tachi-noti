//! Platform-independent menu content for tachi-bar. Front ends (AppKit,
//! tray-icon + muda) only render an `Item` tree and route clicks back through
//! `Action`, so both OSes show the same menu with the same behaviour.

use super::{Row, build_snapshot, title_segments, usage_alert, usage_lines};
use crate::state::{SessionState, Status};
use crate::{config, history, notify, platform, usage};

pub enum Item {
    /// Section header (macOS sectionHeader; Windows disabled text).
    Header(String),
    /// A live session row.
    Session(SessionItem),
    /// Disabled informational line.
    Text(String),
    Separator,
    Submenu { title: String, items: Vec<Item> },
    /// Radio / check style entry.
    Choice { title: String, checked: bool, action: Action },
    Command { title: String, action: Action, key: Option<char> },
}

pub struct SessionItem {
    pub row: Row,
    pub session_id: String,
    pub group_key: String,
    /// None = the host app is unknown, so the row is shown disabled.
    pub action: Option<Action>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Focus { host_id: String, open_path: Option<String> },
    SetStopSound(String),
    SetAttentionSound(String),
    ToggleLaunchAtLogin,
    Restart,
    Quit,
}

pub struct Menu {
    pub items: Vec<Item>,
}

/// Facts only the front end knows.
pub struct Env {
    pub launch_at_login: bool,
}

/// What `dispatch_shared` could not do itself.
#[derive(Debug, PartialEq, Eq)]
pub enum Dispatched {
    Done,
    ToggleLaunchAtLogin,
    Restart,
    Quit,
}

/// Menu from the live on-disk state (history, usage, config).
pub fn build(states: Vec<SessionState>, now: u64, env: &Env) -> Menu {
    let history = history::tail(5);
    let usage = usage::load();
    let sounds = config::load().sounds;
    build_with(states, now, env, &history, usage.as_ref(), &sounds)
}

/// Pure core of `build`; `history` is oldest-first, as `history::tail` returns.
pub fn build_with(
    states: Vec<SessionState>,
    now: u64,
    env: &Env,
    history: &[history::Entry],
    usage: Option<&usage::Usage>,
    sounds: &config::Sounds,
) -> Menu {
    let snap = build_snapshot(states, now);
    let mut items = Vec::new();

    if snap.groups.is_empty() {
        items.push(Item::Text("No active sessions".into()));
    }
    for group in snap.groups {
        items.push(Item::Header(group.header));
        for row in group.rows {
            let action = match (&row.bundle_id, row.enabled) {
                (Some(host_id), true) => {
                    Some(Action::Focus { host_id: host_id.clone(), open_path: row.open_path.clone() })
                }
                _ => None,
            };
            items.push(Item::Session(SessionItem {
                session_id: row.session_id.clone(),
                group_key: group.key.clone(),
                action,
                row,
            }));
        }
    }

    items.push(Item::Separator);
    items.push(Item::Header("Usage".into()));
    let lines = usage.map(|u| usage_lines(u, now)).unwrap_or_default();
    if lines.is_empty() {
        items.push(Item::Text("no data yet \u{2014} run a Claude Code turn".into()));
    }
    items.extend(lines.into_iter().map(Item::Text));

    items.push(Item::Separator);
    items.push(history_submenu(history, now));
    // Completion = Stop notifications, Attention = permission / question /
    // plan ones — distinct on purpose, so they're tellable apart by ear.
    let options = platform::sound::options();
    items.push(sound_submenu("Completion Sound", &sounds.stop, &options, Action::SetStopSound));
    items.push(sound_submenu("Attention Sound", &sounds.attention, &options, Action::SetAttentionSound));

    items.push(Item::Separator);
    items.push(Item::Choice {
        title: "Launch at Login".into(),
        checked: env.launch_at_login,
        action: Action::ToggleLaunchAtLogin,
    });
    items.push(Item::Command { title: "Restart Tachi Bar".into(), action: Action::Restart, key: None });
    items.push(Item::Command { title: "Quit Tachi Bar".into(), action: Action::Quit, key: Some('q') });
    Menu { items }
}

fn history_submenu(entries: &[history::Entry], now: u64) -> Item {
    let mut items = Vec::new();
    if entries.is_empty() {
        items.push(Item::Text("None yet".into()));
    }
    for e in entries.iter().rev() {
        let body: String = e.body.chars().take(60).collect();
        items.push(Item::Text(format!("{} \u{00B7} {} \u{00B7} {}", history::rel_time(now, e.ts), e.repo, body)));
    }
    Item::Submenu { title: "Recent notifications".into(), items }
}

fn sound_submenu(title: &str, current: &str, options: &[(String, String)], action: fn(String) -> Action) -> Item {
    let items = options
        .iter()
        .map(|(label, value)| Item::Choice {
            title: label.clone(),
            checked: value == current,
            action: action(value.clone()),
        })
        .collect();
    Item::Submenu { title: title.into(), items }
}

/// Title segments + usage-alert flag for the 2s heartbeat, shared by both
/// front ends.
pub fn title(states: &[SessionState], now: u64) -> (Vec<(usize, Status)>, bool) {
    let usage = usage::load();
    // Config is only needed (and read) once there is usage to compare.
    let threshold = usage.as_ref().map(|_| config::load().usage_alert_pct).unwrap_or(0);
    title_with(states, now, usage.as_ref(), threshold)
}

pub fn title_with(
    states: &[SessionState],
    now: u64,
    usage: Option<&usage::Usage>,
    threshold_pct: u8,
) -> (Vec<(usize, Status)>, bool) {
    let snap = build_snapshot(states.to_vec(), now);
    let alert = usage.map(|u| usage_alert(u, threshold_pct)).unwrap_or(false);
    (title_segments(&snap), alert)
}

/// Run the OS-neutral part of an action; the front end handles the rest.
pub fn dispatch_shared(action: &Action) -> Dispatched {
    match action {
        Action::Focus { host_id, open_path } => {
            platform::host::focus_host(host_id, open_path.as_deref());
            Dispatched::Done
        }
        Action::SetStopSound(v) => {
            pick_sound(v, config::set_stop_sound);
            Dispatched::Done
        }
        Action::SetAttentionSound(v) => {
            pick_sound(v, config::set_attention_sound);
            Dispatched::Done
        }
        Action::ToggleLaunchAtLogin => Dispatched::ToggleLaunchAtLogin,
        Action::Restart => Dispatched::Restart,
        Action::Quit => Dispatched::Quit,
    }
}

/// Persist a picked sound, then play it as audible feedback.
fn pick_sound(value: &str, save: fn(&str) -> Result<(), String>) {
    if let Err(e) = save(value) {
        eprintln!("tachi-bar: cannot save sound choice: {e}");
        return;
    }
    if value == notify::BARK_SOUND_NAME {
        platform::sound::ensure_bark();
    }
    // Audible feedback so the pick can be judged on the spot.
    if let Some(path) = platform::sound::preview_path(value) {
        platform::sound::play_file(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, root: &str, status: Status, last_event_at: u64, bundle: Option<&str>) -> SessionState {
        SessionState {
            version: 1,
            session_id: id.into(),
            agent: crate::agent::AgentId::Claude,
            repo_name: platform::path_basename(root).into(),
            repo_root: Some(root.into()),
            branch: Some("main".into()),
            bundle_id: bundle.map(Into::into),
            cwd: root.into(),
            status,
            status_since: last_event_at.saturating_sub(60),
            task_started_at: None,
            last_event_at,
            waiting: None,
            pid: None,
        }
    }

    fn env(launch: bool) -> Env {
        Env { launch_at_login: launch }
    }

    fn sounds() -> config::Sounds {
        config::Sounds { stop: "TachiBark".into(), attention: String::new() }
    }

    fn kind(i: &Item) -> &'static str {
        match i {
            Item::Header(_) => "header",
            Item::Session(_) => "session",
            Item::Text(_) => "text",
            Item::Separator => "sep",
            Item::Submenu { .. } => "submenu",
            Item::Choice { .. } => "choice",
            Item::Command { .. } => "command",
        }
    }

    fn kinds(m: &Menu) -> Vec<&'static str> {
        m.items.iter().map(kind).collect()
    }

    fn submenu<'a>(m: &'a Menu, name: &str) -> &'a [Item] {
        m.items
            .iter()
            .find_map(|i| match i {
                Item::Submenu { title, items } if title == name => Some(items.as_slice()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no submenu {name}"))
    }

    #[test]
    fn empty_state_top_level_order() {
        let m = build_with(vec![], 100, &env(false), &[], None, &sounds());
        assert_eq!(
            kinds(&m),
            ["text", "sep", "header", "text", "sep", "submenu", "submenu", "submenu", "sep", "choice", "command", "command"]
        );
        assert!(matches!(&m.items[0], Item::Text(t) if t == "No active sessions"));
        assert!(matches!(&m.items[2], Item::Header(t) if t == "Usage"));
        assert!(matches!(&m.items[3], Item::Text(t) if t.starts_with("no data yet")));
        assert!(matches!(&m.items[11], Item::Command { title, key: Some('q'), .. } if title == "Quit Tachi Bar"));
    }

    #[test]
    fn sessions_follow_their_group_header() {
        let states = vec![
            session("aaa111", "/r/alpha", Status::Idle, 100, Some("com.apple.Terminal")),
            session("bbb222", "/r/alpha", Status::Waiting, 90, Some("com.apple.Terminal")),
            session("ccc333", "/r/beta", Status::Running, 80, Some("com.apple.Terminal")),
        ];
        let m = build_with(states, 200, &env(false), &[], None, &sounds());
        assert_eq!(&kinds(&m)[..6], ["header", "session", "session", "header", "session", "sep"]);
        let Item::Session(first) = &m.items[1] else { panic!() };
        assert_eq!(first.session_id, "bbb222", "waiting outranks idle");
        assert_eq!(first.group_key, "/r/alpha");
        assert_eq!(first.row.session_id, "bbb222");
    }

    #[test]
    fn disabled_session_has_no_action() {
        let states = vec![
            session("aaa111", "/r/alpha", Status::Idle, 100, None),
            session("bbb222", "/r/alpha", Status::Idle, 90, Some("com.apple.Terminal")),
        ];
        let m = build_with(states, 200, &env(false), &[], None, &sounds());
        let Item::Session(off) = &m.items[1] else { panic!() };
        assert!(off.action.is_none() && !off.row.enabled);
        let Item::Session(on) = &m.items[2] else { panic!() };
        assert_eq!(
            on.action,
            Some(Action::Focus { host_id: "com.apple.Terminal".into(), open_path: None })
        );
    }

    // VS Code bundle ids are only recognised by the macOS host module.
    #[cfg(target_os = "macos")]
    #[test]
    fn focus_action_carries_open_path() {
        let states = vec![session("aaa111", "/r/alpha", Status::Idle, 100, Some("com.microsoft.VSCode"))];
        let m = build_with(states, 200, &env(false), &[], None, &sounds());
        let Item::Session(s) = &m.items[1] else { panic!() };
        assert_eq!(
            s.action,
            Some(Action::Focus { host_id: "com.microsoft.VSCode".into(), open_path: Some("/r/alpha".into()) })
        );
    }

    #[test]
    fn sound_choices_reflect_config() {
        let m = build_with(vec![], 100, &env(false), &[], None, &sounds());
        let stop = submenu(&m, "Completion Sound");
        let Item::Choice { title, checked, action } = &stop[0] else { panic!() };
        assert_eq!(title, "Tachi Bark");
        assert!(*checked);
        assert_eq!(*action, Action::SetStopSound("TachiBark".into()));
        assert_eq!(stop.iter().filter(|i| matches!(i, Item::Choice { checked: true, .. })).count(), 1);

        let attention = submenu(&m, "Attention Sound");
        let Item::Choice { title, checked, action } = attention.last().unwrap() else { panic!() };
        assert_eq!(title, "Silent");
        assert!(*checked, "empty config value selects Silent");
        assert_eq!(*action, Action::SetAttentionSound(String::new()));
        assert!(matches!(&attention[0], Item::Choice { checked: false, .. }));
    }

    #[test]
    fn launch_at_login_reflects_env() {
        for on in [true, false] {
            let m = build_with(vec![], 100, &env(on), &[], None, &sounds());
            let found = m.items.iter().any(|i| {
                matches!(i, Item::Choice { title, checked, action: Action::ToggleLaunchAtLogin }
                    if title == "Launch at Login" && *checked == on)
            });
            assert!(found, "launch_at_login={on}");
        }
    }

    #[test]
    fn history_newest_first_and_empty_placeholder() {
        let m = build_with(vec![], 1000, &env(false), &[], None, &sounds());
        let none = submenu(&m, "Recent notifications");
        assert!(matches!(&none[0], Item::Text(t) if t == "None yet"));

        let entry = |ts, body: &str| history::Entry {
            ts,
            event: "stop".into(),
            repo: "alpha".into(),
            branch: None,
            body: body.into(),
            session_id: None,
        };
        let m = build_with(vec![], 1000, &env(false), &[entry(100, "older"), entry(900, "newer")], None, &sounds());
        let recent = submenu(&m, "Recent notifications");
        assert_eq!(recent.len(), 2);
        assert!(matches!(&recent[0], Item::Text(t) if t.ends_with("alpha \u{00B7} newer")), "newest first");
        assert!(matches!(&recent[1], Item::Text(t) if t.ends_with("alpha \u{00B7} older")));
    }

    #[test]
    fn usage_lines_replace_placeholder() {
        use crate::usage::{Usage, Window};
        let u = Usage {
            five_hour: Some(Window { used_percentage: 37.4, resets_at: 10_000 }),
            seven_day: None,
            updated_at: 1_000,
            writers: Default::default(),
        };
        let m = build_with(vec![], 1_180, &env(false), &[], Some(&u), &sounds());
        assert_eq!(&kinds(&m)[2..6], ["header", "text", "text", "sep"]);
    }

    #[test]
    fn title_with_counts_and_alert() {
        use crate::usage::{Usage, Window};
        let states = vec![session("a", "/r/p", Status::Waiting, 100, None)];
        let u = Usage {
            five_hour: Some(Window { used_percentage: 90.0, resets_at: 10_000 }),
            seven_day: None,
            updated_at: 100,
            writers: Default::default(),
        };
        let (segs, alert) = title_with(&states, 150, Some(&u), 80);
        assert_eq!(segs, vec![(1, Status::Waiting)]);
        assert!(alert);
        let (_, alert) = title_with(&states, 150, None, 80);
        assert!(!alert);
    }

    #[test]
    fn toggle_restart_quit_are_left_to_the_front_end() {
        assert_eq!(dispatch_shared(&Action::ToggleLaunchAtLogin), Dispatched::ToggleLaunchAtLogin);
        assert_eq!(dispatch_shared(&Action::Restart), Dispatched::Restart);
        assert_eq!(dispatch_shared(&Action::Quit), Dispatched::Quit);
    }
}
