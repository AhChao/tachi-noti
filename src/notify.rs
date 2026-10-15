//! OS-independent notification model. Delivery lives in
//! `platform::notify` (terminal-notifier / osascript on macOS, WinRT toasts
//! on Windows); this module owns what a notification *is*.

use crate::config::Config;
use std::path::PathBuf;

pub use crate::platform::notify::{Backend, detect, send};

/// Tachi's portrait, embedded so the binary stays self-contained.
const TACHI_ICON: &[u8] = include_bytes!("../assets/tachi-head.png");

/// Config value selecting Tachi's own bark (embedded in the binary).
pub const BARK_SOUND_NAME: &str = "TachiBark";

pub struct Notice {
    pub title: String,
    pub subtitle: String,
    pub body: String,
    pub sound: Option<String>,
    /// Notifications with the same group replace each other.
    pub group: String,
    /// Host app id (see `platform::host`) to focus on click.
    pub activate: Option<String>,
    /// Project folder for window-precise click focus.
    pub click_path: Option<String>,
    /// Window token of the session's own window (see `state::SessionState::window`).
    pub window: Option<String>,
    /// Image path shown on the notification.
    pub icon: Option<String>,
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

pub fn default_icon() -> Option<PathBuf> {
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
        for suffix in crate::platform::shell::EXE_SUFFIXES {
            let candidate = dir.join(format!("{bin}{suffix}"));
            if crate::platform::shell::is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// POSIX single-quote escaping (Claude Code runs commands through sh on
/// macOS and Git Bash on Windows).
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Inverse of sh_quote — only accepts strings in exactly that format.
pub fn sh_unquote(s: &str) -> Option<String> {
    let s = s.trim();
    if s.len() < 2 || !s.starts_with('\'') || !s.ends_with('\'') {
        return None;
    }
    Some(s[1..s.len() - 1].replace("'\\''", "'"))
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
        assert_eq!(sh_unquote(&sh_quote("a'b c")).as_deref(), Some("a'b c"));
    }

    #[test]
    #[cfg(unix)]
    fn find_in_path_finds_sh() {
        assert!(find_in_path("sh").is_some());
        assert!(find_in_path("definitely-not-a-real-binary-xyz").is_none());
    }
}
