use std::process::Command;

/// Bundle id of the app hosting this Claude Code session. macOS sets
/// __CFBundleIdentifier on GUI-launched processes and hooks inherit it; it is
/// more truthful than TERM_PROGRAM (e.g. VS Code forks report TERM_PROGRAM=vscode).
pub fn session_host_id() -> Option<String> {
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
pub fn frontmost_host_id() -> Option<String> {
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

pub fn same_host(a: &str, b: &str) -> bool {
    a == b
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
/// focuses that exact window.
pub fn window_per_folder(host_id: &str) -> bool {
    VSCODE_FAMILY.contains(&host_id)
}

/// Hook-path evidence: TERM_PROGRAM=vscode catches unlisted forks, but is only
/// meaningful inside the hook process (inherited from the hosting IDE).
pub fn window_per_folder_hook_hint() -> bool {
    std::env::var("TERM_PROGRAM").map(|t| t == "vscode").unwrap_or(false)
}

/// macOS has no public, permission-free handle for another app's window, so
/// sessions carry no window token; focus is per app (or per folder).
pub fn current_window_token(_host_id: Option<&str>) -> Option<String> {
    None
}

pub fn window_frontmost(_token: &str) -> Option<bool> {
    None
}

/// Focus the window hosting a session. VS Code-family apps get the folder path
/// (window-precise); other apps a plain activation. argv-only, no shell.
pub fn focus_host(host_id: &str, open_path: Option<&str>, _window: Option<&str>) {
    let mut cmd = Command::new("/usr/bin/open");
    cmd.arg("-b").arg(host_id);
    if let Some(p) = open_path {
        cmd.arg(p);
    }
    let _ = cmd.spawn();
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

    #[test]
    fn vscode_family_knows_forks() {
        assert!(window_per_folder("com.google.antigravity-ide"));
        assert!(window_per_folder("com.microsoft.VSCode"));
        assert!(!window_per_folder("com.apple.Terminal"));
    }
}
