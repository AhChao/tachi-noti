use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Scope {
    User,
    Project,
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const EVENTS: [(&str, Option<&str>); 8] = [
    ("Stop", None),
    ("Notification", Some("permission_prompt|idle_prompt")),
    ("UserPromptSubmit", None),
    ("SessionStart", None),
    ("SessionEnd", None),
    ("PostToolUse", None),
    ("PermissionRequest", None),
    ("PreToolUse", Some("AskUserQuestion")),
];

fn settings_path(scope: Scope) -> Result<PathBuf> {
    Ok(match scope {
        Scope::User => dirs::home_dir().ok_or("cannot resolve home directory")?.join(".claude/settings.json"),
        Scope::Project => std::env::current_dir()?.join(".claude/settings.json"),
    })
}

fn hook_command() -> String {
    std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map(|p| {
            let path = p.display().to_string();
            // Hook commands run through a shell; quote paths that need it.
            if path.contains(' ') || path.contains('\'') || path.contains('"') {
                format!("'{}' hook", path.replace('\'', "'\\''"))
            } else {
                format!("{path} hook")
            }
        })
        .unwrap_or_else(|_| "tachi-noti hook".to_string())
}

fn is_ours(group: &Value) -> bool {
    group["hooks"]
        .as_array()
        .map(|hooks| {
            hooks.iter().any(|h| {
                h["command"]
                    .as_str()
                    .map(|c| c.contains("tachi-noti") && c.contains("hook"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn read_settings(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let text = std::fs::read_to_string(path)?;
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    // Never clobber a file we cannot parse.
    serde_json::from_str(&text).map_err(|e| format!("{} is not valid JSON ({e}); fix it manually first", path.display()).into())
}

fn backup(path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let ts = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let bak = path.with_extension(format!("json.bak.{ts}"));
    std::fs::copy(path, &bak)?;
    Ok(Some(bak))
}

fn write_atomic(path: &Path, root: &Value) -> Result<()> {
    let dir = path.parent().ok_or("settings path has no parent")?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".settings.json.tmp.{}", std::process::id()));
    let text = format!("{}\n", serde_json::to_string_pretty(root)?);
    std::fs::write(&tmp, text)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

/// Append our hook groups for each event, preserving everything already there.
/// Returns the list of events that were newly added (empty = already installed).
pub fn merge_install(root: &mut Value, command: &str) -> Result<Vec<String>> {
    if !root.is_object() {
        return Err("settings root is not a JSON object".into());
    }
    let hooks = root
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| json!({}));
    if !hooks.is_object() {
        return Err("\"hooks\" key exists but is not an object".into());
    }
    let mut added = Vec::new();
    for (event, matcher) in EVENTS {
        let arr = hooks
            .as_object_mut()
            .unwrap()
            .entry(event)
            .or_insert_with(|| json!([]));
        let Some(arr) = arr.as_array_mut() else {
            return Err(format!("hooks.{event} exists but is not an array").into());
        };
        if arr.iter().any(is_ours) {
            continue;
        }
        let mut group = json!({
            "hooks": [{ "type": "command", "command": command, "timeout": 10 }]
        });
        if let Some(m) = matcher {
            group["matcher"] = json!(m);
        }
        arr.push(group);
        added.push(event.to_string());
    }
    Ok(added)
}

/// Remove only our entries; drop groups/events that become empty.
/// Returns the list of events we removed entries from.
pub fn merge_uninstall(root: &mut Value) -> Vec<String> {
    let mut removed = Vec::new();
    let Some(hooks) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return removed;
    };
    for (event, _) in EVENTS {
        let Some(arr) = hooks.get_mut(event).and_then(|a| a.as_array_mut()) else { continue };
        let before = arr.len();
        arr.retain(|group| !is_ours(group));
        if arr.len() != before {
            removed.push(event.to_string());
        }
        if arr.is_empty() {
            hooks.remove(event);
        }
    }
    removed
}

pub fn install(scope: Scope) -> Result<()> {
    let path = settings_path(scope)?;
    let mut root = read_settings(&path)?;
    let command = hook_command();
    let added = merge_install(&mut root, &command)?;
    if added.is_empty() {
        println!("Already installed in {} — nothing to do.", path.display());
        return Ok(());
    }
    let bak = backup(&path)?;
    write_atomic(&path, &root)?;
    println!("Installed hooks ({}) into {}", added.join(", "), path.display());
    println!("  command: {command}");
    if let Some(b) = bak {
        println!("  backup:  {}", b.display());
    }
    println!("Restart your Claude Code session to pick up the hooks.");
    Ok(())
}

pub fn uninstall(scope: Scope) -> Result<()> {
    let path = settings_path(scope)?;
    if !path.exists() {
        println!("{} does not exist — nothing to do.", path.display());
        return Ok(());
    }
    let mut root = read_settings(&path)?;
    let removed = merge_uninstall(&mut root);
    if removed.is_empty() {
        println!("No tachi-noti hooks found in {} — nothing to do.", path.display());
        return Ok(());
    }
    let bak = backup(&path)?;
    write_atomic(&path, &root)?;
    println!("Removed hooks ({}) from {}", removed.join(", "), path.display());
    if let Some(b) = bak {
        println!("  backup: {}", b.display());
    }
    Ok(())
}

/// For `doctor`: which of our events are present in this settings file.
pub fn installed_events(path: &Path) -> Vec<String> {
    let Ok(root) = read_settings(path) else { return vec![] };
    EVENTS
        .iter()
        .filter(|(event, _)| {
            root["hooks"][event]
                .as_array()
                .map(|arr| arr.iter().any(is_ours))
                .unwrap_or(false)
        })
        .map(|(event, _)| event.to_string())
        .collect()
}

/// Events from the current EVENTS set not yet present in this settings file
/// (after an upgrade, prompts the user to re-run install).
pub fn missing_events(path: &Path) -> Vec<String> {
    let installed = installed_events(path);
    if installed.is_empty() {
        return vec![]; // not installed at all — different message
    }
    EVENTS
        .iter()
        .filter(|(event, _)| !installed.iter().any(|e| e == event))
        .map(|(event, _)| event.to_string())
        .collect()
}

pub fn doctor_paths() -> Vec<(String, PathBuf)> {
    let mut v = Vec::new();
    if let Ok(p) = settings_path(Scope::User) {
        v.push(("user".to_string(), p));
    }
    if let Ok(p) = settings_path(Scope::Project) {
        v.push(("project".to_string(), p));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    const CMD: &str = "/usr/local/bin/tachi-noti hook";

    fn existing_settings() -> Value {
        // Mirrors a real-world file: third-party hooks + unknown top-level keys.
        json!({
            "statusLine": { "type": "command", "command": "buddy statusline" },
            "permissions": { "allow": ["Bash(ls:*)"] },
            "hooks": {
                "Stop": [
                    { "hooks": [{ "type": "command", "command": "claude-buddy stop-hook" }] }
                ],
                "UserPromptSubmit": [
                    { "hooks": [{ "type": "command", "command": "claude-buddy prompt-hook" }] }
                ]
            }
        })
    }

    const ALL_EVENTS: [&str; 8] = [
        "Stop", "Notification", "UserPromptSubmit", "SessionStart", "SessionEnd", "PostToolUse",
        "PermissionRequest", "PreToolUse",
    ];

    #[test]
    fn install_into_empty() {
        let mut root = json!({});
        let added = merge_install(&mut root, CMD).unwrap();
        assert_eq!(added, ALL_EVENTS.to_vec());
        assert_eq!(root["hooks"]["Notification"][0]["matcher"], "permission_prompt|idle_prompt");
        assert_eq!(root["hooks"]["Stop"][0]["hooks"][0]["command"], CMD);
        assert!(root["hooks"]["Stop"][0].get("matcher").is_none());
    }

    #[test]
    fn install_preserves_existing() {
        let mut root = existing_settings();
        merge_install(&mut root, CMD).unwrap();
        // Third-party groups untouched, ours appended after.
        assert_eq!(root["hooks"]["Stop"][0]["hooks"][0]["command"], "claude-buddy stop-hook");
        assert_eq!(root["hooks"]["Stop"][1]["hooks"][0]["command"], CMD);
        // Unknown top-level keys survive.
        assert_eq!(root["statusLine"]["command"], "buddy statusline");
        assert_eq!(root["permissions"]["allow"][0], "Bash(ls:*)");
    }

    #[test]
    fn install_is_idempotent() {
        let mut root = json!({});
        merge_install(&mut root, CMD).unwrap();
        let snapshot = root.clone();
        let added = merge_install(&mut root, CMD).unwrap();
        assert!(added.is_empty());
        assert_eq!(root, snapshot);
    }

    #[test]
    fn install_detects_old_binary_path() {
        let mut root = json!({});
        merge_install(&mut root, "/old/path/tachi-noti hook").unwrap();
        let added = merge_install(&mut root, "/new/path/tachi-noti hook").unwrap();
        assert!(added.is_empty(), "different absolute path still counts as installed");
    }

    #[test]
    fn uninstall_removes_only_ours() {
        let mut root = existing_settings();
        merge_install(&mut root, CMD).unwrap();
        let removed = merge_uninstall(&mut root);
        assert_eq!(removed, ALL_EVENTS.to_vec());
        // claude-buddy intact, Notification array dropped entirely.
        assert_eq!(root["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert_eq!(root["hooks"]["Stop"][0]["hooks"][0]["command"], "claude-buddy stop-hook");
        assert!(root["hooks"].get("Notification").is_none());
        // Uninstall again is a no-op.
        assert!(merge_uninstall(&mut root).is_empty());
    }

    #[test]
    fn install_adds_new_events_to_existing() {
        // A v0.1 install only had the first three events; upgrading must add
        // exactly the new ones without touching the old groups.
        let mut root = json!({
            "hooks": {
                "Stop": [{ "hooks": [{ "type": "command", "command": CMD }] }],
                "Notification": [{ "matcher": "permission_prompt|idle_prompt", "hooks": [{ "type": "command", "command": CMD }] }],
                "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": CMD }] }]
            }
        });
        let added = merge_install(&mut root, CMD).unwrap();
        assert_eq!(added, vec!["SessionStart", "SessionEnd", "PostToolUse", "PermissionRequest", "PreToolUse"]);
        assert_eq!(root["hooks"]["Stop"].as_array().unwrap().len(), 1, "old group untouched");
        assert_eq!(root["hooks"]["PreToolUse"][0]["matcher"], "AskUserQuestion");
    }

    #[test]
    fn rejects_wrong_types() {
        let mut root = json!({ "hooks": "oops" });
        assert!(merge_install(&mut root, CMD).is_err());
        let mut root = json!({ "hooks": { "Stop": {} } });
        assert!(merge_install(&mut root, CMD).is_err());
    }
}
