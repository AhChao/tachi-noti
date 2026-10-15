use std::path::Path;
use std::process::Command;

pub const EXE_SUFFIXES: &[&str] = &[""];

/// APFS is case-insensitive by default, but process paths come back with
/// their real casing, and matching them exactly is what the host signatures
/// were written against.
pub const CASE_INSENSITIVE_PATHS: bool = false;

/// Claude Code runs hook and statusLine commands through /bin/sh.
pub fn command(cmd: &str) -> Command {
    let mut c = Command::new("/bin/sh");
    c.arg("-c").arg(cmd);
    c
}

pub fn exe_for_command_line(path: &Path) -> String {
    path.display().to_string()
}

pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}
