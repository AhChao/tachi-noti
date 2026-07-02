//! Google Antigravity CLI (`agy`) adapter — rich JSON hooks.
//!
//! Antigravity's hook engine (Go package `jsonhook`) reads
//! `~/.gemini/antigravity-cli/hooks.json`, a map of *named* hook specs. Each
//! spec carries an optional `enabled` plus event keys: tool events
//! (`PreToolUse`/`PostToolUse`) take `{matcher, hooks:[…]}`; lifecycle events
//! (`PreInvocation`/`PostInvocation`/`Stop`) list command handlers directly.
//! The hook receives a JSON payload on stdin (camelCase: `workspacePaths`,
//! `transcriptPath`, `toolCall.name/args`).
//!
//! Because *we* author the hooks.json, we tag the event in the command args
//! (`--event start|tool|stop`) instead of parsing it out of the payload — more
//! robust, and it sidesteps fields we haven't pinned down. We register
//! PreInvocation→start (turn begins), PostToolUse→tool (heartbeat), and
//! Stop→idle+notify. We do NOT map PreToolUse to a wait: it fires for
//! auto-approved tools too, with no distinct user-must-respond gate, so a
//! Waiting state there would be a false alarm (see `Capabilities`).

use crate::agent::AgentId;
use crate::{config, focus, gitinfo, hook, notify, state};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Our named entry inside the shared hooks.json.
const HOOK_NAME: &str = "tachi-noti";

/// Confirmed payload fields (stdin JSON, camelCase). Everything is optional so
/// a partial or unreadable payload degrades instead of failing.
#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Payload {
    transcript_path: Option<String>,
    workspace_paths: Option<Vec<String>>,
    tool_call: Option<ToolCall>,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ToolCall {
    name: Option<String>,
}

/// Entry for `tachi-noti ingest --agent antigravity --event <start|tool|stop>`.
/// Reads the stdin payload best-effort (hard timeout — a hook that blocks would
/// hang `agy`), then applies the matching transition.
pub fn ingest(event: Option<&str>) -> Result<()> {
    let payload: Payload = serde_json::from_str(&read_stdin_brief()).unwrap_or_default();
    let ctx = build_ctx(&payload);
    match event {
        Some("start") => {
            hook::transition(&ctx, state::Event::PromptSubmit);
        }
        Some("tool") => {
            let tool = payload.tool_call.as_ref().and_then(|t| t.name.as_deref());
            hook::transition(&ctx, state::Event::PostToolUse { tool_name: tool });
        }
        Some("stop") => {
            hook::transition(&ctx, state::Event::Stop { background_running: false });
            let cfg = config::load();
            if cfg.focus_suppression && focus::session_is_frontmost() {
                return Ok(());
            }
            let notice = hook::build_notice(&cfg, &ctx, "Turn complete".to_string(), &cfg.sounds.stop);
            let _ = notify::send(&notify::detect(&cfg), &notice);
            hook::record_history("stop", &ctx, &notice.body);
        }
        _ => {}
    }
    Ok(())
}

/// Read stdin but never block the agent: a background reader with a short
/// deadline. The reader thread dies with the process if stdin never EOFs.
fn read_stdin_brief() -> String {
    use std::io::Read;
    use std::sync::mpsc;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::stdin().read_to_string(&mut s);
        let _ = tx.send(s);
    });
    rx.recv_timeout(std::time::Duration::from_millis(300)).unwrap_or_default()
}

fn build_ctx(p: &Payload) -> state::Ctx {
    let agent = AgentId::Antigravity;
    let pid = focus::agent_ancestor_pid(agent);
    let cwd = p
        .workspace_paths
        .as_ref()
        .and_then(|w| w.first())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let repo = gitinfo::detect(&cwd);
    // Key the session by transcript path (stable per conversation); fall back to
    // the agy pid so all of one session's hooks share a state file.
    let session_id = p
        .transcript_path
        .clone()
        .or_else(|| pid.map(|p| format!("antigravity-{p}")))
        .unwrap_or_else(|| "antigravity-unknown".into());
    state::Ctx {
        session_id,
        agent,
        repo_name: repo.name,
        repo_root: repo.toplevel.map(|p| p.to_string_lossy().into_owned()),
        branch: repo.branch,
        bundle_id: focus::session_bundle_id(),
        cwd: cwd.to_string_lossy().into_owned(),
        pid,
    }
}

// ── install / uninstall (~/.gemini/antigravity-cli/hooks.json) ───────────────

fn hooks_path() -> Result<PathBuf> {
    Ok(dirs::home_dir().ok_or("cannot resolve home directory")?.join(".gemini/antigravity-cli/hooks.json"))
}

fn exe_for_shell() -> String {
    std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map(|p| {
            let path = p.display().to_string();
            if path.contains(' ') || path.contains('\'') || path.contains('"') {
                crate::notify::sh_quote(&path)
            } else {
                path
            }
        })
        .unwrap_or_else(|_| "tachi-noti".to_string())
}

fn command_for(exe: &str, event: &str) -> String {
    format!("{exe} ingest --agent antigravity --event {event}")
}

/// Our named spec: PreInvocation→start, PostToolUse→tool, Stop→stop.
pub fn build_spec(exe: &str) -> Value {
    let lifecycle = |event: &str| json!([{ "type": "command", "command": command_for(exe, event) }]);
    let tool = |event: &str| json!([{ "matcher": ".*", "hooks": [{ "type": "command", "command": command_for(exe, event) }] }]);
    json!({
        "PreInvocation": lifecycle("start"),
        "PostToolUse": tool("tool"),
        "Stop": lifecycle("stop"),
    })
}

fn read_doc(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let text = std::fs::read_to_string(path)?;
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&text)
        .map_err(|e| format!("{} is not valid JSON ({e}); fix it manually first", path.display()).into())
}

fn backup(path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
    let bak = path.with_extension(format!("json.bak.{ts}"));
    std::fs::copy(path, &bak)?;
    Ok(Some(bak))
}

fn write_atomic(path: &Path, root: &Value) -> Result<()> {
    let dir = path.parent().ok_or("hooks path has no parent")?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".hooks.json.tmp.{}", std::process::id()));
    let text = format!("{}\n", serde_json::to_string_pretty(root)?);
    std::fs::write(&tmp, text)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

/// Insert/refresh our named spec, preserving any other named hooks.
/// Returns true if the document changed.
pub fn merge_install(root: &mut Value, exe: &str) -> Result<bool> {
    if !root.is_object() {
        return Err("hooks.json root is not a JSON object".into());
    }
    let spec = build_spec(exe);
    if root.get(HOOK_NAME) == Some(&spec) {
        return Ok(false);
    }
    root[HOOK_NAME] = spec;
    Ok(true)
}

/// Remove only our named spec. Returns true if it was present.
pub fn merge_uninstall(root: &mut Value) -> bool {
    root.as_object_mut().map(|o| o.remove(HOOK_NAME).is_some()).unwrap_or(false)
}

pub fn install() -> Result<()> {
    let path = hooks_path()?;
    let mut root = read_doc(&path)?;
    let exe = exe_for_shell();
    if !merge_install(&mut root, &exe)? {
        println!("Already installed in {} — nothing to do.", path.display());
        return Ok(());
    }
    let bak = backup(&path)?;
    write_atomic(&path, &root)?;
    println!("Installed Antigravity hooks into {} (named \"{HOOK_NAME}\")", path.display());
    if let Some(b) = bak {
        println!("  backup: {}", b.display());
    }
    println!("Restart agy to pick up the change.");
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let path = hooks_path()?;
    if !path.exists() {
        println!("{} does not exist — nothing to do.", path.display());
        return Ok(());
    }
    let mut root = read_doc(&path)?;
    if !merge_uninstall(&mut root) {
        println!("No \"{HOOK_NAME}\" hook found in {} — nothing to do.", path.display());
        return Ok(());
    }
    let bak = backup(&path)?;
    // Leave an empty object rather than deleting the file the user may own.
    write_atomic(&path, &root)?;
    println!("Removed \"{HOOK_NAME}\" hook from {}", path.display());
    if let Some(b) = bak {
        println!("  backup: {}", b.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "/usr/local/bin/tachi-noti";

    #[test]
    fn payload_parses_camelcase() {
        let raw = r#"{"transcriptPath":"/x/brain/abc/log.jsonl",
            "workspacePaths":["/tmp/repo","/tmp/other"],
            "toolCall":{"name":"run_command","args":{"cmd":"ls"}}}"#;
        let p: Payload = serde_json::from_str(raw).unwrap();
        assert_eq!(p.transcript_path.as_deref(), Some("/x/brain/abc/log.jsonl"));
        assert_eq!(p.workspace_paths.unwrap()[0], "/tmp/repo");
        assert_eq!(p.tool_call.unwrap().name.as_deref(), Some("run_command"));
    }

    #[test]
    fn empty_or_garbage_payload_is_tolerated() {
        let p: Payload = serde_json::from_str("").unwrap_or_default();
        assert!(p.transcript_path.is_none() && p.workspace_paths.is_none());
    }

    #[test]
    fn spec_matches_confirmed_schema_shape() {
        let spec = build_spec(EXE);
        // Lifecycle events: handlers listed directly (no matcher/hooks wrapper).
        assert_eq!(spec["Stop"][0]["type"], "command");
        assert!(spec["Stop"][0].get("matcher").is_none());
        assert!(spec["Stop"][0]["command"].as_str().unwrap().contains("--event stop"));
        assert!(spec["PreInvocation"][0]["command"].as_str().unwrap().contains("--event start"));
        // Tool events: matcher + nested hooks array.
        assert_eq!(spec["PostToolUse"][0]["matcher"], ".*");
        assert!(spec["PostToolUse"][0]["hooks"][0]["command"].as_str().unwrap().contains("--event tool"));
    }

    #[test]
    fn install_preserves_foreign_named_hooks() {
        let mut root = json!({
            "my-linter": { "PostToolUse": [ { "matcher": "run_command", "hooks": [ { "command": "./lint.sh" } ] } ] }
        });
        assert!(merge_install(&mut root, EXE).unwrap());
        // Foreign hook untouched, ours added under our name.
        assert_eq!(root["my-linter"]["PostToolUse"][0]["hooks"][0]["command"], "./lint.sh");
        assert!(root[HOOK_NAME]["Stop"][0]["command"].as_str().unwrap().contains("tachi-noti"));
        // Idempotent.
        assert!(!merge_install(&mut root, EXE).unwrap());
    }

    #[test]
    fn uninstall_removes_only_ours() {
        let mut root = json!({ "my-linter": { "Stop": [] } });
        merge_install(&mut root, EXE).unwrap();
        assert!(merge_uninstall(&mut root));
        assert!(root.get(HOOK_NAME).is_none());
        assert!(root.get("my-linter").is_some(), "foreign hook survives");
        // Second uninstall is a no-op.
        assert!(!merge_uninstall(&mut root));
    }
}
