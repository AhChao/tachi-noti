//! The OS seam. Everything that differs between macOS and Windows lives
//! behind this module, and nothing outside it uses `cfg(target_os)`.
//!
//! The selection is compile-time: a binary only ever runs on one OS, so the
//! implementation is a `cfg`-chosen module, not a runtime trait object. Both
//! implementations expose the same items; `cargo check` on each target (CI
//! builds macOS and `x86_64-pc-windows-msvc`) is what enforces the contract.
//!
//! # Contract
//!
//! Every implementation module provides:
//!
//! **Process introspection** (`proc`) — used for liveness reaping and for
//! finding the agent / host process above a hook:
//! - `proc::parent_pid(pid: u32) -> Option<u32>`
//! - `proc::self_parent_pid() -> u32`
//! - `proc::exe_path(pid: u32) -> Option<PathBuf>` (resolved image path)
//! - `proc::pid_alive(pid: u32) -> bool` (exists and is ours)
//!
//! **Host app** (`host`) — the app hosting a session (terminal / IDE). The id
//! is opaque to callers and persisted in state files as `bundle_id`: a bundle
//! identifier on macOS, the host executable's full path on Windows.
//! - `host::session_host_id() -> Option<String>` (called inside a hook)
//! - `host::frontmost_host_id() -> Option<String>`
//! - `host::same_host(a: &str, b: &str) -> bool`
//! - `host::window_per_folder(host_id: &str) -> bool` — VS Code-family apps,
//!   where opening the project folder focuses the exact window
//! - `host::window_per_folder_hook_hint() -> bool` — extra hook-process-only
//!   evidence (macOS: TERM_PROGRAM=vscode catches unlisted forks)
//! - `host::focus_host(host_id: &str, open_path: Option<&str>)` — bring the
//!   session's window forward (menu click, notification click)
//!
//! **Notifications** (`notify`):
//! - `notify::Backend` with `name(&self) -> &'static str`,
//!   `detail(&self) -> String` (doctor line) and
//!   `setup_hint(&self) -> Option<&'static str>` (printed after `test`)
//! - `notify::detect(&Config) -> Backend`
//! - `notify::send(&Backend, &Notice) -> Result<(), Box<dyn Error>>`
//!
//! **Sounds** (`sound`) — names are what config.toml stores:
//! - `sound::DEFAULT_STOP`, `sound::DEFAULT_ATTENTION`: `&str`
//! - `sound::options() -> Vec<(String, String)>` (label, config value)
//! - `sound::preview_path(value: &str) -> Option<PathBuf>`
//! - `sound::play_file(path: &Path)` (non-blocking preview)
//! - `sound::ensure_bark()` (materialize Tachi's embedded bark)
//! - `sound::MISSING_NOTE: &str` (what the OS does with an unknown name)
//!
//! **Shell** (`shell`) — how the agent runs our hook / statusLine commands:
//! - `shell::command(cmd: &str) -> std::process::Command` (run a command line)
//! - `shell::exe_for_command_line(path: &Path) -> String` (an executable path
//!   spelled the way that shell accepts it, unquoted)
//! - `shell::is_executable(path: &Path) -> bool`
//! - `shell::EXE_SUFFIXES: &[&str]` (tried by `find_in_path`)
//! - `shell::CASE_INSENSITIVE_PATHS: bool`
//!
//! **App registration** (`register`) — OS-level identity that `install`
//! creates and `uninstall` removes (Windows: toast identity, Start menu
//! shortcut, URL protocol; macOS: nothing to do):
//! - `register::install() -> Result<Vec<String>, String>` (lines describing
//!   what was done)
//! - `register::uninstall() -> Result<Vec<String>, String>`
//! - `register::status() -> Vec<String>` (doctor lines)
//! - `register::handle_activation(uri: &str)` — `tachi-noti activate <uri>`,
//!   the target of a notification click on Windows (`tachi-noti://` URLs
//!   built by `activation_uri`); a no-op on macOS

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(not(any(target_os = "macos", windows)))]
compile_error!("tachi-noti supports macOS and Windows");

/// Basename of a path string written with either separator — state files
/// carry native paths, and Windows paths use backslashes.
pub fn path_basename(p: &str) -> &str {
    p.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or(p)
}

/// URL scheme a notification click launches on Windows (registered by
/// `register::install`, handled by `tachi-noti activate`).
pub const ACTIVATION_SCHEME: &str = "tachi-noti";

/// What a notification click should bring forward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activation {
    pub host_id: String,
    pub open_path: Option<String>,
}

/// `tachi-noti://focus?host=<pct>&path=<pct>` — every byte outside the
/// unreserved set is percent-encoded, so arbitrary paths survive the shell's
/// `"%1"` substitution and cannot inject arguments.
pub fn activation_uri(a: &Activation) -> String {
    let mut uri = format!("{ACTIVATION_SCHEME}://focus?host={}", pct_encode(&a.host_id));
    if let Some(p) = &a.open_path {
        uri.push_str("&path=");
        uri.push_str(&pct_encode(p));
    }
    uri
}

/// Inverse of `activation_uri`; anything malformed is rejected (None), since
/// the URI arrives from outside the process.
pub fn parse_activation_uri(uri: &str) -> Option<Activation> {
    let rest = uri.strip_prefix(ACTIVATION_SCHEME)?.strip_prefix("://focus")?;
    // Shells may hand the URI over with a trailing slash before the query.
    let query = rest.strip_prefix("/?").or_else(|| rest.strip_prefix('?'))?;
    let mut host_id = None;
    let mut open_path = None;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=')?;
        match k {
            "host" => host_id = Some(pct_decode(v)?),
            "path" => open_path = Some(pct_decode(v)?),
            _ => {}
        }
    }
    let host_id = host_id.filter(|h| !h.is_empty())?;
    Some(Activation { host_id, open_path: open_path.filter(|p| !p.is_empty()) })
}

fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn pct_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_uri_roundtrips_hostile_paths() {
        let a = Activation {
            host_id: r"C:\Users\me\AppData\Local\Programs\Microsoft VS Code\Code.exe".into(),
            open_path: Some(r#"C:\work\a b&c=d"%1 '$(x)'\repo 中文"#.into()),
        };
        let uri = activation_uri(&a);
        assert!(uri.starts_with("tachi-noti://focus?host="));
        assert!(!uri.contains(' ') && !uri.contains('"') && !uri.contains('\\'));
        assert_eq!(parse_activation_uri(&uri), Some(a.clone()));
        // Trailing-slash form some launchers produce.
        let slashed = uri.replacen("focus?", "focus/?", 1);
        assert_eq!(parse_activation_uri(&slashed), Some(a));
    }

    #[test]
    fn activation_uri_without_path() {
        let a = Activation { host_id: "com.apple.Terminal".into(), open_path: None };
        assert_eq!(parse_activation_uri(&activation_uri(&a)), Some(a));
    }

    #[test]
    fn malformed_activation_uris_are_rejected() {
        for bad in [
            "",
            "tachi-noti://focus",
            "tachi-noti://other?host=x",
            "http://focus?host=x",
            "tachi-noti://focus?host=",
            "tachi-noti://focus?host=%zz",
            "tachi-noti://focus?host=%4",
            "tachi-noti://focus?path=C%3A",
            "tachi-noti://focus?hostx",
        ] {
            assert_eq!(parse_activation_uri(bad), None, "{bad:?} must be rejected");
        }
    }

    #[test]
    fn basename_handles_both_separators() {
        assert_eq!(path_basename("/Users/me/repo"), "repo");
        assert_eq!(path_basename("/Users/me/repo/"), "repo");
        assert_eq!(path_basename(r"C:\Users\me\repo"), "repo");
        assert_eq!(path_basename(r"C:\Users\me\repo\"), "repo");
        assert_eq!(path_basename("repo"), "repo");
    }
}
