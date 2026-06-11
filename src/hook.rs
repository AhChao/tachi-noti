use crate::{config, focus, gitinfo, history, notify, settings, state, transcript};
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
    pub source: Option<String>,
    pub reason: Option<String>,
    pub tool_name: Option<String>,
    pub tool_input: Option<serde_json::Value>,
    pub permission_mode: Option<String>,
}

pub fn run() -> Result<()> {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw)?;
    let input: HookInput = serde_json::from_str(&raw).unwrap_or_default();
    let Some(event) = input.hook_event_name.as_deref() else { return Ok(()) };
    let cfg = config::load();
    let ctx = build_ctx(&input);

    match event {
        "SessionStart" => {
            transition(&ctx, state::Event::SessionStart { source: input.source.as_deref() });
            state::cleanup_stale();
            Ok(())
        }
        "UserPromptSubmit" => {
            transition(&ctx, state::Event::PromptSubmit);
            Ok(())
        }
        "Stop" => on_stop(&input, &cfg, &ctx),
        "Notification" => on_notification(&input, &cfg, &ctx),
        "PermissionRequest" => {
            transition(&ctx, state::Event::Waiting(permission_wait_info(&input)));
            Ok(())
        }
        "PreToolUse" => on_pre_tool_use(&input, &cfg, &ctx),
        "PostToolUse" => {
            transition(&ctx, state::Event::PostToolUse { tool_name: input.tool_name.as_deref() });
            Ok(())
        }
        "SessionEnd" => {
            state::remove(&ctx.session_id);
            Ok(())
        }
        _ => Ok(()),
    }
}

fn build_ctx(input: &HookInput) -> state::Ctx {
    let cwd = input
        .cwd
        .as_ref()
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let repo = gitinfo::detect(&cwd);
    state::Ctx {
        session_id: input.session_id.clone().unwrap_or_else(|| "unknown".into()),
        repo_name: repo.name,
        repo_root: repo.toplevel.map(|p| p.to_string_lossy().into_owned()),
        branch: repo.branch,
        bundle_id: focus::session_bundle_id(),
        cwd: cwd.to_string_lossy().into_owned(),
    }
}

fn transition(ctx: &state::Ctx, ev: state::Event) -> state::Transition {
    let prev = state::load(&ctx.session_id);
    let age = state::file_age_secs(&ctx.session_id);
    let t = state::apply_event(prev, ctx, ev, state::now_epoch(), age);
    if let Some(s) = &t.save {
        state::save(s);
    }
    t
}

fn on_stop(input: &HookInput, cfg: &config::Config, ctx: &state::Ctx) -> Result<()> {
    // State first: the transition must happen even if the popup is suppressed.
    let t = transition(ctx, state::Event::Stop);
    let duration = t.task_duration;

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
        body = format!("{body} ({})", state::format_duration(d));
    }

    let notice = build_notice(cfg, ctx, body, &cfg.sounds.stop);
    notify::send(&notify::detect(cfg), &notice)?;
    record_history("stop", ctx, &notice.body);
    Ok(())
}

fn on_notification(input: &HookInput, cfg: &config::Config, ctx: &state::Ctx) -> Result<()> {
    // The installed matcher already filters, but stay defensive in case the
    // hook is registered with a broader matcher.
    let info = match input.notification_type.as_deref() {
        Some("permission_prompt") | None => {
            // In bypass mode the prompt resolves itself — a popup and a
            // Waiting state would both be false alarms.
            if input.permission_mode.as_deref() == Some("bypassPermissions") {
                return Ok(());
            }
            state::WaitingInfo { kind: state::WaitKind::Permission, detail: input.message.clone() }
        }
        // idle_prompt = the turn already finished and the user just hasn't
        // replied. That's ⚪ free capacity, not 🟡 blocked — turning it yellow
        // (plus a popup) made every finished session scream for attention.
        Some(_) => return Ok(()),
    };
    // State first: suppressing the popup must not suppress the Waiting state.
    transition(ctx, state::Event::Waiting(info));

    if cfg.focus_suppression && focus::session_is_frontmost() {
        return Ok(());
    }
    let body = input
        .message
        .clone()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| "Claude needs your input".to_string());
    let body = transcript::squash(&body, cfg.max_body_len);

    let notice = build_notice(cfg, ctx, body, &cfg.sounds.attention);
    notify::send(&notify::detect(cfg), &notice)?;
    record_history("notification", ctx, &notice.body);
    Ok(())
}

/// PermissionRequest carries structured tool info — summarize what approval
/// is being asked for. ExitPlanMode's dialog is the plan-approval prompt.
fn permission_wait_info(input: &HookInput) -> state::WaitingInfo {
    if input.tool_name.as_deref() == Some("ExitPlanMode") {
        return state::WaitingInfo { kind: state::WaitKind::Plan, detail: Some("plan ready — approve?".into()) };
    }
    let detail = match (input.tool_name.as_deref(), &input.tool_input) {
        (Some("Bash"), Some(v)) => v["command"].as_str().map(|c| transcript::squash(c, 80)),
        (Some(t), Some(v)) => v["file_path"]
            .as_str()
            .map(|p| format!("{t}: {}", p.rsplit('/').next().unwrap_or(p)))
            .or_else(|| Some(t.to_string())),
        (Some(t), None) => Some(t.to_string()),
        _ => None,
    };
    state::WaitingInfo { kind: state::WaitKind::Permission, detail }
}

/// AskUserQuestion never fires a Notification (the session waits silently),
/// so this is both the state transition and the missing popup.
fn on_pre_tool_use(input: &HookInput, cfg: &config::Config, ctx: &state::Ctx) -> Result<()> {
    if input.tool_name.as_deref() != Some("AskUserQuestion") {
        return Ok(()); // defensive: matcher should already scope us
    }
    let question = input
        .tool_input
        .as_ref()
        .and_then(|v| v["questions"][0]["question"].as_str())
        .map(str::to_string);
    let info = state::WaitingInfo { kind: state::WaitKind::Question, detail: question.clone() };
    transition(ctx, state::Event::Waiting(info));

    if cfg.focus_suppression && focus::session_is_frontmost() {
        return Ok(());
    }
    let body = question.unwrap_or_else(|| "Claude has a question for you".to_string());
    let body = transcript::squash(&body, cfg.max_body_len);
    let notice = build_notice(cfg, ctx, body, &cfg.sounds.attention);
    notify::send(&notify::detect(cfg), &notice)?;
    record_history("question", ctx, &notice.body);
    Ok(())
}

fn record_history(event: &str, ctx: &state::Ctx, body: &str) {
    history::append(&history::Entry {
        ts: state::now_epoch(),
        event: event.to_string(),
        repo: ctx.repo_name.clone(),
        branch: ctx.branch.clone(),
        body: body.to_string(),
        session_id: Some(ctx.session_id.clone()),
    });
}

fn build_notice(cfg: &config::Config, ctx: &state::Ctx, body: String, sound: &str) -> Notice {
    let subtitle = ctx
        .branch
        .as_deref()
        .map(|b| format!("{} @ {b}", ctx.repo_name))
        .unwrap_or_default();
    Notice {
        title: ctx.repo_name.clone(),
        subtitle,
        body,
        sound: if sound.is_empty() { None } else { Some(sound.to_string()) },
        group: ctx.session_id.clone(),
        activate: ctx.bundle_id.clone(),
        click_path: Some(ctx.repo_root.clone().unwrap_or_else(|| ctx.cwd.clone())),
        icon: notify::resolve_icon(cfg),
    }
}

pub fn run_test() -> Result<()> {
    let cfg = config::load();
    let backend = notify::detect(&cfg);
    let input = HookInput { session_id: Some("tachi-noti-test".into()), ..Default::default() };
    let ctx = build_ctx(&input);
    let notice = build_notice(&cfg, &ctx, "Tachi Noti is working — woof.".into(), &cfg.sounds.stop);

    println!("backend:  {}", backend.name());
    println!("title:    {}", notice.title);
    if !notice.subtitle.is_empty() {
        println!("subtitle: {}", notice.subtitle);
    }
    if let Some(bid) = &notice.activate {
        match &notice.click_path {
            Some(p) => println!("click-to-focus: {bid} → window for {p}"),
            None => println!("click-to-focus: {bid}"),
        }
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
                let missing = settings::missing_events(&path);
                if !missing.is_empty() {
                    println!("                  run 'tachi-noti install' to add: {}", missing.join(", "));
                }
            }
        } else {
            println!("settings {scope:7}: {} (missing)", path.display());
        }
    }
    println!("state dir:        {} ({} live sessions)", state::state_dir().display(), state::live_count());
    let hp = history::history_path();
    let size = std::fs::metadata(&hp).map(|m| m.len()).unwrap_or(0);
    println!("history:          {} ({} entries, {} KB)", hp.display(), history::entry_count(), size / 1024);
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
