//! Which app hosts this session, and is the user already looking at it.
//! OS specifics live in `platform::{host, proc}`.

use crate::platform::{host, proc};

/// Host app id of this Claude Code session (bundle id on macOS, host
/// executable path on Windows). Only meaningful inside a hook process.
pub fn session_bundle_id() -> Option<String> {
    host::session_host_id()
}

pub fn frontmost_bundle_id() -> Option<String> {
    host::frontmost_host_id()
}

/// Pid of the agent process hosting this event: the nearest ancestor whose
/// executable matches the agent's host signature (event → sh → agent → shell →
/// IDE). Matching the path (not the kernel process name) is required because
/// e.g. Claude's native install runs a binary named after its version. The
/// nearest match also picks the right session for nested invocations.
/// None = couldn't tell (the session then only expires by time).
pub fn agent_ancestor_pid(agent: crate::agent::AgentId) -> Option<u32> {
    let mut pid = proc::self_parent_pid();
    for _ in 0..15 {
        if pid <= 1 {
            return None;
        }
        if proc::exe_path(pid).map(|p| agent.matches_host(&p)).unwrap_or(false) {
            return Some(pid);
        }
        pid = proc::parent_pid(pid)?;
    }
    None
}

/// True when the session's host app is frontmost — the user is already looking
/// at it, so skip the notification. Any uncertainty means "don't suppress".
pub fn session_is_frontmost() -> bool {
    match (session_bundle_id(), frontmost_bundle_id()) {
        (Some(a), Some(b)) => host::same_host(&a, &b),
        _ => false,
    }
}
