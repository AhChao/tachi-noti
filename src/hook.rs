use crate::{config, focus, gitinfo, notify, sessions, settings, transcript};
use notify::Notice;
use serde::Deserialize;
use std::io::Read;
use std::path::{Path, PathBuf};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Deserialize, Default)]
pub struct HookInput {
    pub session_id: Option<String>,
    pub transcript_path: Option<String>,
    pub cwd: Option<String>,
    pub hook_event_name: Option<String>,
    pub notification_type: Option<String>,
    pub message: Option<String>,
}

pub fn run() -> Result<()> {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw)?;
    let input: HookInput = serde_json::from_str(&raw).unwrap_or_default();
    let Some(event) = input.hook_event_name.as_deref() else { return Ok(()) };
    let cfg = config::load();

    match event {
        "UserPromptSubmit" => {
            if let Some(id) = &input.session_id {
                sessions::record_start(id);
            }
            sessions::cleanup_stale();
            Ok(())
        }
        "Stop" => on_stop(&input, &cfg),
        "Notification" => on_notification(&input, &cfg),
        _ => Ok(()),
    }
}

fn on_stop(input: &HookInput, cfg: &config::Config) -> Result<()> {
    let duration = input.session_id.as_deref().and_then(sessions::take_start);
    if cfg.min_duration_secs > 0 {
        // Only suppress when we positively know the turn was quick.
        if let Some(d) = duration {
            if d.as_secs() < cfg.min_duration_secs {
                return Ok(());
            }
        }
    }
    if cfg.focus_suppression && focus::session_is_frontmost() {
        return Ok(());
    }

    let mut body = input
        .transcript_path
        .as_deref()
        .and_then(|p| transcript::last_assistant_text(Path::new(p)))
        .map(|t| transcript::squash(&t, cfg.max_body_len))
        .unwrap_or_else(|| "Task complete".to_string());
    if let Some(d) = duration {
        body = format!("{body} ({})", sessions::format_duration(d));
    }

    let notice = build_notice(input, cfg, body, &cfg.sounds.stop);
    notify::send(&notify::detect(cfg), &notice)
}

fn on_notification(input: &HookInput, cfg: &config::Config) -> Result<()> {
    // The installed matcher already filters, but stay defensive in case the
    // hook is registered with a broader matcher.
    match input.notification_type.as_deref() {
        Some("permission_prompt") | Some("idle_prompt") | None => {}
        Some(_) => return Ok(()),
    }
    if cfg.focus_suppression && focus::session_is_frontmost() {
        return Ok(());
    }
    let body = input
        .message
        .clone()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| "Claude needs your input".to_string());
    let body = transcript::squash(&body, cfg.max_body_len);

    let notice = build_notice(input, cfg, body, &cfg.sounds.attention);
    notify::send(&notify::detect(cfg), &notice)
}

fn build_notice(input: &HookInput, cfg: &config::Config, body: String, sound: &str) -> Notice {
    let cwd = input
        .cwd
        .as_ref()
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let repo = gitinfo::detect(&cwd);
    let subtitle = repo
        .branch
        .as_deref()
        .map(|b| format!("{} @ {b}", repo.name))
        .unwrap_or_default();
    Notice {
        title: repo.name.clone(),
        subtitle,
        body,
        sound: if sound.is_empty() { None } else { Some(sound.to_string()) },
        group: input.session_id.clone().unwrap_or(repo.name),
        activate: focus::session_bundle_id(),
        icon: notify::resolve_icon(cfg),
    }
}

pub fn run_test() -> Result<()> {
    let cfg = config::load();
    let backend = notify::detect(&cfg);
    let input = HookInput { session_id: Some("tachi-noti-test".into()), ..Default::default() };
    let notice = build_notice(&input, &cfg, "Tachi Noti is working — woof.".into(), &cfg.sounds.stop);

    println!("backend:  {}", backend.name());
    println!("title:    {}", notice.title);
    if !notice.subtitle.is_empty() {
        println!("subtitle: {}", notice.subtitle);
    }
    if let Some(bid) = &notice.activate {
        println!("click-to-focus: {bid}");
    }
    if cfg.focus_suppression && focus::session_is_frontmost() {
        println!("note: focus suppression would normally skip this (host app is frontmost); sending anyway for the test.");
    }
    notify::send(&backend, &notice)?;
    println!("notification sent.");
    if matches!(backend, notify::Backend::OsaScript) {
        println!(
            "\nIf nothing appeared: macOS attributes osascript notifications to \"Script Editor\" —\n\
             check System Settings → Notifications → Script Editor is allowed.\n\
             Tip: `brew install terminal-notifier` upgrades to grouping + click-to-focus."
        );
    }
    Ok(())
}

pub fn run_doctor() -> Result<()> {
    let cfg = config::load();
    let backend = notify::detect(&cfg);
    println!("tachi-noti doctor");
    println!("--------------");
    if let Ok(exe) = std::env::current_exe() {
        println!("binary:           {}", exe.display());
    }
    println!("backend:          {}{}", backend.name(), match &backend {
        notify::Backend::TerminalNotifier(p) => format!(" ({})", p.display()),
        notify::Backend::OsaScript => " (install terminal-notifier for grouping + click-to-focus)".into(),
    });
    println!("TERM_PROGRAM:     {}", std::env::var("TERM_PROGRAM").unwrap_or_else(|_| "(unset)".into()));
    println!("__CFBundleIdent:  {}", std::env::var("__CFBundleIdentifier").unwrap_or_else(|_| "(unset)".into()));
    println!("session bundle:   {}", focus::session_bundle_id().unwrap_or_else(|| "(unknown — no click-to-focus / suppression)".into()));
    println!("frontmost now:    {}", focus::frontmost_bundle_id().unwrap_or_else(|| "(unknown)".into()));
    match config::config_path() {
        Some(p) if p.exists() => println!("config:           {} (loaded)", p.display()),
        Some(p) => println!("config:           {} (missing, using defaults)", p.display()),
        None => println!("config:           (no home dir?)"),
    }
    println!(
        "sounds:           stop={} attention={}  focus_suppression={}  min_duration={}s",
        cfg.sounds.stop, cfg.sounds.attention, cfg.focus_suppression, cfg.min_duration_secs
    );
    for (scope, path) in settings::doctor_paths() {
        if path.exists() {
            let events = settings::installed_events(&path);
            if events.is_empty() {
                println!("settings {scope:7}: {} (no tachi-noti hooks)", path.display());
            } else {
                println!("settings {scope:7}: {} [{}]", path.display(), events.join(", "));
            }
        } else {
            println!("settings {scope:7}: {} (missing)", path.display());
        }
    }
    println!("sessions dir:     {} ({} pending)", sessions::sessions_dir().display(), sessions::pending_count());
    Ok(())
}

pub fn debug_log(msg: &str) {
    if std::env::var("TACHI_NOTI_DEBUG").map(|v| v == "1").unwrap_or(false) {
        let dir = dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("tachi-noti");
        if std::fs::create_dir_all(&dir).is_ok() {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("debug.log")) {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let _ = writeln!(f, "[{ts}] {msg}");
            }
        }
    }
}
