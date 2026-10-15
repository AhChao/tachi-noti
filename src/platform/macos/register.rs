//! macOS needs no OS-level registration: notifications are attributed to
//! terminal-notifier / Script Editor, and click targets are bundle ids.

pub fn install() -> Result<Vec<String>, String> {
    Ok(Vec::new())
}

pub fn uninstall() -> Result<Vec<String>, String> {
    Ok(Vec::new())
}

pub fn status() -> Vec<String> {
    Vec::new()
}

/// Notification clicks are handled by terminal-notifier on macOS; the
/// `activate` entry point exists only for Windows' URL protocol.
pub fn handle_activation(_uri: &str) {}
