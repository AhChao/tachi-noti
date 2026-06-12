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

/// Parent pid of a process via libproc — no subprocess spawned.
fn proc_ppid(pid: u32) -> Option<u32> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let rc = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if rc != size {
        return None;
    }
    Some(info.pbi_ppid)
}

/// Executable path of a process (resolved, not argv[0]).
fn proc_path(pid: u32) -> Option<std::path::PathBuf> {
    let mut buf = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let rc = unsafe {
        libc::proc_pidpath(pid as libc::c_int, buf.as_mut_ptr() as *mut libc::c_void, buf.len() as u32)
    };
    if rc <= 0 {
        return None;
    }
    use std::os::unix::ffi::OsStrExt;
    Some(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..rc as usize])))
}

/// Does this executable path look like the Claude Code CLI? The kernel's
/// process name can't be trusted: the native install runs a binary literally
/// named after its version (~/.local/share/claude/versions/2.1.175), so match
/// the path instead — a `claude`-named file/directory, or a JS runtime
/// (npm installs run the CLI under node/bun/deno).
fn is_claude_host(path: &std::path::Path) -> bool {
    let base = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if ["claude", "node", "bun", "deno"].contains(&base) {
        return true;
    }
    path.components().any(|c| c.as_os_str() == "claude")
}

/// Pid of the Claude Code process hosting this hook: the nearest ancestor
/// whose executable looks like the CLI (hook → sh → claude → shell → IDE).
/// The nearest match also picks the right session for nested `claude`
/// invocations. None = couldn't tell (the session then only expires by time).
pub fn claude_ancestor_pid() -> Option<u32> {
    let mut pid = std::os::unix::process::parent_id();
    for _ in 0..15 {
        if pid <= 1 {
            return None;
        }
        if proc_path(pid).map(|p| is_claude_host(&p)).unwrap_or(false) {
            return Some(pid);
        }
        pid = proc_ppid(pid)?;
    }
    None
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
    fn proc_introspection_resolves_self_and_rejects_bogus_pid() {
        assert!(proc_ppid(std::process::id()).expect("own process must resolve") > 0);
        let path = proc_path(std::process::id()).expect("own path must resolve");
        assert!(path.is_absolute());
        // macOS pid_max is 99999 — far beyond it can never exist.
        assert!(proc_ppid(4_000_000).is_none());
        assert!(proc_path(4_000_000).is_none());
    }

    #[test]
    fn claude_host_path_shapes() {
        use std::path::Path;
        // Native install: versioned binary under a claude directory.
        assert!(is_claude_host(Path::new("/Users/x/.local/share/claude/versions/2.1.175")));
        // Plain binary or symlink target named claude.
        assert!(is_claude_host(Path::new("/opt/homebrew/bin/claude")));
        // npm install runs under a JS runtime.
        assert!(is_claude_host(Path::new("/usr/local/bin/node")));
        assert!(is_claude_host(Path::new("/Users/x/.bun/bin/bun")));
        // Ordinary ancestors must not match.
        assert!(!is_claude_host(Path::new("/bin/zsh")));
        assert!(!is_claude_host(Path::new("/Applications/Antigravity IDE.app/Contents/MacOS/Electron")));
        assert!(!is_claude_host(Path::new("/Users/x/.claude/something")));
    }

    #[test]
    fn term_program_map() {
        assert_eq!(term_program_to_bundle_id("Apple_Terminal"), Some("com.apple.Terminal"));
        assert_eq!(term_program_to_bundle_id("iTerm.app"), Some("com.googlecode.iterm2"));
        assert_eq!(term_program_to_bundle_id("unknown-thing"), None);
    }
}
