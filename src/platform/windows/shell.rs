use std::path::{Path, PathBuf};
use std::process::Command;

pub const EXE_SUFFIXES: &[&str] = &[".exe", ""];

/// NTFS lookups are case-insensitive and process image paths come back in
/// whatever casing the launcher used (`C:\Program Files\nodejs\node.exe` vs
/// `NODE.EXE`), so host signatures must compare without case.
pub const CASE_INSENSITIVE_PATHS: bool = true;

/// Claude Code on Windows runs hook and statusLine commands through Git
/// Bash, so chained statusLine commands (POSIX-quoted by `settings`) go to
/// the same bash. `CLAUDE_CODE_GIT_BASH_PATH` is Claude Code's own override;
/// without bash anywhere we fall back to cmd, which still runs a plain path.
pub fn command(cmd: &str) -> Command {
    match git_bash() {
        Some(bash) => {
            let mut c = Command::new(bash);
            c.arg("-c").arg(cmd);
            c
        }
        None => {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(cmd);
            c
        }
    }
}

fn git_bash() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CLAUDE_CODE_GIT_BASH_PATH").map(PathBuf::from) {
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(p) = crate::notify::find_in_path("bash") {
        return Some(p);
    }
    let program_files = std::env::var_os("ProgramFiles").map(PathBuf::from)?;
    let p = program_files.join("Git").join("bin").join("bash.exe");
    p.is_file().then_some(p)
}

/// `canonicalize` yields verbatim paths (`\\?\C:\...`) that neither bash nor
/// cmd accept as a command; strip the prefix and use forward slashes, which
/// both shells take for an executable path.
pub fn exe_for_command_line(path: &Path) -> String {
    normalize_command_path(&path.display().to_string())
}

pub fn normalize_command_path(raw: &str) -> String {
    let s = if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else if let Some(rest) = raw.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        raw.to_string()
    };
    s.replace('\\', "/")
}

pub fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_prefixes_are_stripped() {
        assert_eq!(
            normalize_command_path(r"\\?\C:\Users\me\.cargo\bin\tachi-noti.exe"),
            "C:/Users/me/.cargo/bin/tachi-noti.exe"
        );
        assert_eq!(normalize_command_path(r"\\?\UNC\server\share\t.exe"), "//server/share/t.exe");
        assert_eq!(normalize_command_path(r"C:\plain\t.exe"), "C:/plain/t.exe");
    }
}
