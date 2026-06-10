use std::process::Command;

/// Bundle id of the app hosting this Claude Code session. macOS sets
/// __CFBundleIdentifier on GUI-launched processes and hooks inherit it; it is
/// more truthful than TERM_PROGRAM (e.g. VS Code forks report TERM_PROGRAM=vscode).
pub fn session_bundle_id() -> Option<String> {
    if let Ok(bid) = std::env::var("__CFBundleIdentifier") {
        if !bid.is_empty() {
            return Some(bid);
        }
    }
    let term = std::env::var("TERM_PROGRAM").ok()?;
    term_program_to_bundle_id(&term).map(str::to_string)
}

pub fn term_program_to_bundle_id(term_program: &str) -> Option<&'static str> {
    match term_program {
        "Apple_Terminal" => Some("com.apple.Terminal"),
        "iTerm.app" => Some("com.googlecode.iterm2"),
        "vscode" => Some("com.microsoft.VSCode"),
        "WezTerm" => Some("com.github.wez.wezterm"),
        "ghostty" => Some("com.mitchellh.ghostty"),
        "kitty" => Some("net.kovidgoyal.kitty"),
        _ => None,
    }
}

/// Frontmost app's bundle id via lsappinfo (fast, no Automation permission).
pub fn frontmost_bundle_id() -> Option<String> {
    let front = Command::new("/usr/bin/lsappinfo").arg("front").output().ok()?;
    let asn = String::from_utf8_lossy(&front.stdout).trim().to_string();
    if asn.is_empty() {
        return None;
    }
    let info = Command::new("/usr/bin/lsappinfo")
        .args(["info", "-only", "bundleid", &asn])
        .output()
        .ok()?;
    parse_bundleid_output(&String::from_utf8_lossy(&info.stdout))
}

/// Parses `"CFBundleIdentifier"="com.example.app"` (key in output differs from
/// the `-only bundleid` query name).
pub fn parse_bundleid_output(out: &str) -> Option<String> {
    let value = out.trim().rsplit('=').next()?;
    let value = value.trim().trim_matches('"').trim();
    if value.is_empty() { None } else { Some(value.to_string()) }
}

/// True when the session's host app is frontmost — the user is already looking
/// at it, so skip the notification. Any uncertainty means "don't suppress".
pub fn session_is_frontmost() -> bool {
    match (session_bundle_id(), frontmost_bundle_id()) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lsappinfo_output() {
        assert_eq!(
            parse_bundleid_output("\"CFBundleIdentifier\"=\"com.google.antigravity-ide\"\n"),
            Some("com.google.antigravity-ide".to_string())
        );
        assert_eq!(parse_bundleid_output(""), None);
        assert_eq!(parse_bundleid_output("garbage"), Some("garbage".to_string()));
    }

    #[test]
    fn term_program_map() {
        assert_eq!(term_program_to_bundle_id("Apple_Terminal"), Some("com.apple.Terminal"));
        assert_eq!(term_program_to_bundle_id("iTerm.app"), Some("com.googlecode.iterm2"));
        assert_eq!(term_program_to_bundle_id("unknown-thing"), None);
    }
}
