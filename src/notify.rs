use crate::config::Config;
use std::path::PathBuf;
use std::process::Command;

/// Tachi's portrait, embedded so the binary stays self-contained.
const TACHI_ICON: &[u8] = include_bytes!("../assets/tachi-head.png");

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub struct Notice {
    pub title: String,
    pub subtitle: String,
    pub body: String,
    pub sound: Option<String>,
    /// Notifications with the same group replace each other (terminal-notifier only).
    pub group: String,
    /// Bundle id to activate on click (terminal-notifier only).
    pub activate: Option<String>,
    /// Project folder for window-precise click focus (terminal-notifier only).
    pub click_path: Option<String>,
    /// Image path shown on the notification (terminal-notifier only).
    pub icon: Option<String>,
}

#[derive(Debug)]
pub enum Backend {
    TerminalNotifier(PathBuf),
    OsaScript,
}

impl Backend {
    pub fn name(&self) -> &'static str {
        match self {
            Backend::TerminalNotifier(_) => "terminal-notifier",
            Backend::OsaScript => "osascript",
        }
    }
}

pub fn detect(config: &Config) -> Backend {
    match config.backend.as_deref() {
        Some("osascript") => return Backend::OsaScript,
        Some("terminal-notifier") => {
            if let Some(p) = find_in_path("terminal-notifier") {
                return Backend::TerminalNotifier(p);
            }
            return Backend::OsaScript;
        }
        _ => {}
    }
    match find_in_path("terminal-notifier") {
        Some(p) => Backend::TerminalNotifier(p),
        None => Backend::OsaScript,
    }
}

/// Resolve the notification image: None in config = bundled Tachi portrait
/// (materialized into the cache dir), "" = disabled, anything else = a path.
pub fn resolve_icon(config: &Config) -> Option<String> {
    match config.icon.as_deref() {
        Some("") => None,
        Some(path) => Some(path.to_string()),
        None => default_icon().map(|p| p.to_string_lossy().into_owned()),
    }
}

fn default_icon() -> Option<PathBuf> {
    let dir = dirs::cache_dir()?.join("tachi-noti");
    let path = dir.join("tachi-head.png");
    let stale = std::fs::metadata(&path).map(|m| m.len() != TACHI_ICON.len() as u64).unwrap_or(true);
    if stale {
        std::fs::create_dir_all(&dir).ok()?;
        std::fs::write(&path, TACHI_ICON).ok()?;
    }
    Some(path)
}

pub fn find_in_path(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(bin);
        if let Ok(meta) = candidate.metadata() {
            if meta.is_file() {
                use std::os::unix::fs::PermissionsExt;
                if meta.permissions().mode() & 0o111 != 0 {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

pub fn send(backend: &Backend, n: &Notice) -> Result<()> {
    match backend {
        Backend::TerminalNotifier(path) => send_terminal_notifier(path, n),
        Backend::OsaScript => send_osascript(n),
    }
}

fn send_terminal_notifier(path: &PathBuf, n: &Notice) -> Result<()> {
    let mut cmd = Command::new(path);
    cmd.arg("-title").arg(&n.title);
    if !n.subtitle.is_empty() {
        cmd.arg("-subtitle").arg(&n.subtitle);
    }
    cmd.arg("-message").arg(guard_leading_dash(&n.body));
    if let Some(s) = &n.sound {
        cmd.arg("-sound").arg(s);
    }
    cmd.arg("-group").arg(&n.group);
    if let Some(icon) = &n.icon {
        // -appIcon is ignored on recent macOS (Big Sur+); -contentImage still
        // renders. Pass both so whichever the OS honors shows the image.
        cmd.arg("-appIcon").arg(icon);
        cmd.arg("-contentImage").arg(icon);
    }
    match &n.activate {
        // -sender and -activate conflict on Sequoia: -sender re-attributes the
        // notification and clicking activates the sender app instead. Prefer a
        // working click-to-focus over the nicer icon.
        Some(bid) => {
            // -activate only raises the app's last-used window. VS Code-family
            // apps keep one window per folder, so opening the project path
            // focuses the exact window hosting this session.
            match n.click_path.as_deref().filter(|_| vscode_family(bid)) {
                Some(path) => {
                    cmd.arg("-execute")
                        .arg(format!("/usr/bin/open -b {} {}", sh_quote(bid), sh_quote(path)));
                }
                None => {
                    cmd.arg("-activate").arg(bid);
                }
            }
        }
        None => {
            cmd.arg("-sender").arg("com.apple.Terminal");
        }
    }
    let status = cmd.status()?;
    if !status.success() {
        return Err(format!("terminal-notifier exited with {status}").into());
    }
    Ok(())
}

const VSCODE_FAMILY: &[&str] = &[
    "com.microsoft.VSCode",
    "com.microsoft.VSCodeInsiders",
    "com.vscodium",
    "com.google.antigravity-ide",
    "com.todesktop.230313mzl4w4u92", // Cursor
    "com.exafunction.windsurf",
];

/// Apps that keep one window per opened folder, where `open -b <bundle> <dir>`
/// focuses that exact window. TERM_PROGRAM=vscode catches unlisted forks.
fn vscode_family(bundle_id: &str) -> bool {
    VSCODE_FAMILY.contains(&bundle_id)
        || std::env::var("TERM_PROGRAM").map(|t| t == "vscode").unwrap_or(false)
}

/// POSIX single-quote escaping for the -execute command string.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// terminal-notifier misparses messages starting with '-' or '[' as flags;
/// a zero-width space neutralizes that without visible change.
pub fn guard_leading_dash(body: &str) -> String {
    if body.starts_with('-') || body.starts_with('[') {
        format!("\u{200B}{body}")
    } else {
        body.to_string()
    }
}

fn send_osascript(n: &Notice) -> Result<()> {
    // Injection-proof by construction: user-controlled text only travels via
    // argv, never interpolated into the AppleScript source.
    let script = if n.sound.is_some() {
        "on run argv\ndisplay notification (item 1 of argv) with title (item 2 of argv) subtitle (item 3 of argv) sound name (item 4 of argv)\nend run"
    } else {
        "on run argv\ndisplay notification (item 1 of argv) with title (item 2 of argv) subtitle (item 3 of argv)\nend run"
    };
    let mut cmd = Command::new("/usr/bin/osascript");
    cmd.arg("-e").arg(script).arg(&n.body).arg(&n.title).arg(&n.subtitle);
    if let Some(s) = &n.sound {
        cmd.arg(s);
    }
    let status = cmd.status()?;
    if !status.success() {
        return Err(format!("osascript exited with {status}").into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leading_dash_is_guarded() {
        assert!(guard_leading_dash("-rf something").starts_with('\u{200B}'));
        assert!(guard_leading_dash("[ok] done").starts_with('\u{200B}'));
        assert_eq!(guard_leading_dash("normal"), "normal");
    }

    #[test]
    fn sh_quote_escapes_safely() {
        assert_eq!(sh_quote("/plain/path"), "'/plain/path'");
        assert_eq!(sh_quote("a'b"), "'a'\\''b'");
        assert_eq!(sh_quote("$(rm -rf /); `boom`"), "'$(rm -rf /); `boom`'");
    }

    #[test]
    fn vscode_family_knows_forks() {
        assert!(VSCODE_FAMILY.contains(&"com.google.antigravity-ide"));
        assert!(VSCODE_FAMILY.contains(&"com.microsoft.VSCode"));
        assert!(!VSCODE_FAMILY.contains(&"com.apple.Terminal"));
    }

    #[test]
    fn find_in_path_finds_sh() {
        assert!(find_in_path("sh").is_some());
        assert!(find_in_path("definitely-not-a-real-binary-xyz").is_none());
    }
}
