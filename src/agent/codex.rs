//! OpenAI Codex CLI adapter — thin `notify` integration.
//!
//! Codex exposes a single `notify` program, invoked once per
//! `agent-turn-complete`, with the event JSON passed as one argv string (not on
//! stdin, the way Claude hooks arrive). That yields a turn-finished signal — we
//! flip the session to Idle and fire a completion notification — but never a
//! turn-start or any wait, which is exactly what `AgentId::Codex.capabilities()`
//! declares. `notify` is a single slot, so install *chains* whatever program
//! the user already had, mirroring the statusLine wrap in `settings.rs`.

use crate::agent::AgentId;
use crate::{config, focus, gitinfo, hook, notify, state, transcript};
use std::path::{Path, PathBuf};
use toml_edit::{Array, DocumentMut, Item, Value};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The subset of Codex's `notify` payload we use. Codex documents these as
/// "common fields"; unknown ones are ignored.
#[derive(serde::Deserialize, Default)]
struct Notify {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(rename = "thread-id")]
    thread_id: Option<String>,
    #[serde(rename = "turn-id")]
    turn_id: Option<String>,
    cwd: Option<String>,
    #[serde(rename = "last-assistant-message")]
    last_assistant_message: Option<String>,
}

/// Entry point for `tachi-noti ingest --agent codex [--chain <json>] <payload>`.
/// Best-effort: never fails the Codex turn. Always re-invokes the chained
/// program (if any) so the user's original notify still runs.
pub fn ingest(payload: Option<&str>, chain: Option<&str>) -> Result<()> {
    if let Some(raw) = payload
        && let Ok(n) = serde_json::from_str::<Notify>(raw)
    {
        handle(&n);
    }
    if let Some(chain) = chain {
        run_chained(chain, payload);
    }
    Ok(())
}

fn handle(n: &Notify) {
    // The only event Codex currently emits; stay defensive about the rest.
    if n.kind.as_deref() != Some("agent-turn-complete") {
        return;
    }
    let cfg = config::load();
    let ctx = build_ctx(n);
    // Turn finished → Idle. Codex gives no turn-start signal, so we never show
    // it Running; duration is unknown (no task_started_at) and stays absent.
    hook::transition(&ctx, state::Event::Stop);

    if cfg.focus_suppression && focus::session_is_frontmost() {
        return;
    }
    let body = n
        .last_assistant_message
        .as_deref()
        .filter(|m| !m.trim().is_empty())
        .map(|t| transcript::squash(t, cfg.max_body_len))
        .unwrap_or_else(|| "Turn complete".to_string());
    let notice = hook::build_notice(&cfg, &ctx, body, &cfg.sounds.stop);
    let _ = notify::send(&notify::detect(&cfg), &notice);
    hook::record_history("stop", &ctx, &notice.body);
}

fn build_ctx(n: &Notify) -> state::Ctx {
    let cwd = n
        .cwd
        .clone()
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let repo = gitinfo::detect(&cwd);
    let agent = AgentId::Codex;
    state::Ctx {
        // thread-id is stable across a Codex session, so it keys one state file;
        // fall back to turn-id, then a constant, so we never panic.
        session_id: n
            .thread_id
            .clone()
            .or_else(|| n.turn_id.clone())
            .unwrap_or_else(|| "codex-unknown".into()),
        agent,
        repo_name: repo.name,
        repo_root: repo.toplevel.map(|p| p.to_string_lossy().into_owned()),
        branch: repo.branch,
        bundle_id: focus::session_bundle_id(),
        cwd: cwd.to_string_lossy().into_owned(),
        pid: focus::agent_ancestor_pid(agent),
    }
}

/// Re-run the program the user's `notify` pointed at before we wrapped it,
/// preserving Codex's contract (the JSON payload is the final argv).
fn run_chained(chain_json: &str, payload: Option<&str>) {
    let Ok(argv) = serde_json::from_str::<Vec<String>>(chain_json) else { return };
    let Some((prog, args)) = argv.split_first() else { return };
    let mut cmd = std::process::Command::new(prog);
    cmd.args(args);
    if let Some(p) = payload {
        cmd.arg(p);
    }
    let _ = cmd.status();
}

// ── install / uninstall (~/.codex/config.toml) ───────────────────────────────

fn config_path() -> Result<PathBuf> {
    Ok(dirs::home_dir().ok_or("cannot resolve home directory")?.join(".codex/config.toml"))
}

/// Absolute binary path (unquoted — TOML array elements are argv, not a shell
/// string, so they must not be shell-quoted).
fn exe_path() -> String {
    std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "tachi-noti".to_string())
}

fn read_doc(path: &Path) -> Result<DocumentMut> {
    if !path.exists() {
        return Ok(DocumentMut::new());
    }
    let text = std::fs::read_to_string(path)?;
    text.parse::<DocumentMut>()
        .map_err(|e| format!("{} is not valid TOML ({e}); fix it manually first", path.display()).into())
}

fn notify_strings(doc: &DocumentMut) -> Option<Vec<String>> {
    let arr = doc.get("notify")?.as_array()?;
    Some(arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
}

/// A notify array is ours if it invokes our binary as an `ingest` stage.
fn is_ours(argv: &[String]) -> bool {
    argv.iter().any(|s| s.contains("tachi-noti")) && argv.iter().any(|s| s == "ingest")
}

fn our_array(exe: &str, chain: Option<&[String]>) -> Array {
    let mut a = Array::new();
    for s in [exe, "ingest", "--agent", "codex"] {
        a.push(s);
    }
    if let Some(prev) = chain.filter(|p| !p.is_empty()) {
        a.push("--chain");
        a.push(serde_json::to_string(prev).unwrap_or_default());
    }
    a
}

/// Recover the program we chained, from `--chain <json>` in our own array.
fn extract_chain(argv: &[String]) -> Vec<String> {
    argv.iter()
        .position(|s| s == "--chain")
        .and_then(|i| argv.get(i + 1))
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default()
}

fn backup(path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
    let bak = path.with_extension(format!("toml.bak.{ts}"));
    std::fs::copy(path, &bak)?;
    Ok(Some(bak))
}

fn write_atomic(path: &Path, doc: &DocumentMut) -> Result<()> {
    let dir = path.parent().ok_or("config path has no parent")?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".config.toml.tmp.{}", std::process::id()));
    std::fs::write(&tmp, doc.to_string())?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

pub fn install() -> Result<()> {
    let path = config_path()?;
    let mut doc = read_doc(&path)?;
    let exe = exe_path();

    let chain: Option<Vec<String>> = match notify_strings(&doc) {
        Some(argv) if is_ours(&argv) => {
            println!("Already installed in {} — nothing to do.", path.display());
            return Ok(());
        }
        // A foreign notify already exists: chain it so it keeps running.
        Some(argv) if !argv.is_empty() => Some(argv),
        _ => None,
    };

    let arr = our_array(&exe, chain.as_deref());
    set_notify(&mut doc, arr);

    let bak = backup(&path)?;
    write_atomic(&path, &doc)?;
    println!("Installed Codex notify into {}", path.display());
    if let Some(prev) = &chain {
        println!("  chained your existing notify: {prev:?}");
    }
    if let Some(b) = bak {
        println!("  backup: {}", b.display());
    }
    println!("Restart Codex to pick up the change.");
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let path = config_path()?;
    if !path.exists() {
        println!("{} does not exist — nothing to do.", path.display());
        return Ok(());
    }
    let mut doc = read_doc(&path)?;
    let restore = match notify_strings(&doc) {
        Some(argv) if is_ours(&argv) => extract_chain(&argv),
        _ => {
            println!("No tachi-noti notify found in {} — nothing to do.", path.display());
            return Ok(());
        }
    };

    if restore.is_empty() {
        doc.as_table_mut().remove("notify");
    } else {
        let mut a = Array::new();
        for s in &restore {
            a.push(s.as_str());
        }
        set_notify(&mut doc, a);
    }

    let bak = backup(&path)?;
    write_atomic(&path, &doc)?;
    if restore.is_empty() {
        println!("Removed Codex notify from {} (we had created it)", path.display());
    } else {
        println!("Restored your original Codex notify in {}: {restore:?}", path.display());
    }
    if let Some(b) = bak {
        println!("  backup: {}", b.display());
    }
    Ok(())
}

fn set_notify(doc: &mut DocumentMut, arr: Array) {
    doc["notify"] = Item::Value(Value::Array(arr));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(argv: &Array) -> Vec<String> {
        argv.iter().filter_map(|v| v.as_str().map(String::from)).collect()
    }

    #[test]
    fn parses_turn_complete_payload() {
        let raw = r#"{"type":"agent-turn-complete","thread-id":"t1","turn-id":"u9",
            "cwd":"/tmp/repo","last-assistant-message":"done","extra":"ignored"}"#;
        let n: Notify = serde_json::from_str(raw).unwrap();
        assert_eq!(n.kind.as_deref(), Some("agent-turn-complete"));
        assert_eq!(n.thread_id.as_deref(), Some("t1"));
        assert_eq!(n.cwd.as_deref(), Some("/tmp/repo"));
        assert_eq!(n.last_assistant_message.as_deref(), Some("done"));
    }

    #[test]
    fn ownership_detection() {
        assert!(is_ours(&["/usr/local/bin/tachi-noti".into(), "ingest".into(), "--agent".into(), "codex".into()]));
        assert!(!is_ours(&["python3".into(), "/home/me/notify.py".into()]));
        assert!(!is_ours(&["tachi-noti".into(), "hook".into()]), "the hook command is not the ingest stage");
    }

    #[test]
    fn install_into_empty_has_no_chain() {
        let arr = our_array("/bin/tachi-noti", None);
        assert_eq!(strs(&arr), vec!["/bin/tachi-noti", "ingest", "--agent", "codex"]);
        assert!(extract_chain(&strs(&arr)).is_empty());
    }

    #[test]
    fn foreign_notify_is_chained_and_restored() {
        let prev = vec!["python3".to_string(), "/home/me/notify.py".to_string()];
        let arr = our_array("/bin/tachi-noti", Some(&prev));
        let argv = strs(&arr);
        assert!(is_ours(&argv));
        assert_eq!(argv[4], "--chain");
        // Uninstall recovers exactly what was there before.
        assert_eq!(extract_chain(&argv), prev);
    }

    #[test]
    fn install_uninstall_roundtrip_preserves_foreign_notify() {
        let mut doc: DocumentMut = "notify = [\"python3\", \"/home/me/notify.py\"]\nmodel = \"o3\"\n"
            .parse()
            .unwrap();
        // install: capture + wrap
        let prev = notify_strings(&doc).unwrap();
        assert!(!is_ours(&prev));
        set_notify(&mut doc, our_array("/bin/tachi-noti", Some(&prev)));
        assert!(is_ours(&notify_strings(&doc).unwrap()));
        assert_eq!(doc["model"].as_str(), Some("o3"), "unrelated keys untouched");
        // uninstall: restore
        let chain = extract_chain(&notify_strings(&doc).unwrap());
        let mut a = Array::new();
        for s in &chain {
            a.push(s.as_str());
        }
        set_notify(&mut doc, a);
        assert_eq!(notify_strings(&doc).unwrap(), vec!["python3", "/home/me/notify.py"]);
        assert_eq!(doc["model"].as_str(), Some("o3"));
    }
}
