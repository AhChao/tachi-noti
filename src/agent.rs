//! Coding-agent identity, capabilities, and host-process signatures.
//!
//! tachi-noti began Claude-only, but the state machine (`state`), persistence,
//! menu bar (`bar`), and notifier (`notify`) were already agent-agnostic — they
//! operate on a generic `SessionState`. The Claude-specific knowledge that
//! *remained* was at the edges: which process hosts a session, and what signals
//! an agent can emit. This module is the seam that turns that knowledge into a
//! small registry so other agents (Codex, Antigravity CLI) plug in without
//! touching the spine.
//!
//! A session's `AgentId` comes from the *ingest source* — the Claude hook, the
//! Codex `notify`, etc. — not from sniffing the process tree. Host signatures
//! are used only to find a pid for liveness reaping.

use serde::{Deserialize, Serialize};
use std::path::Path;

pub mod antigravity;
pub mod codex;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum AgentId {
    /// Anthropic Claude Code — the original, full-fidelity integration.
    #[default]
    Claude,
    /// OpenAI Codex CLI — thin `notify` only (turn-complete), low fidelity.
    Codex,
    /// Google Antigravity CLI (not the IDE) — rich JSON hooks, like Claude.
    Antigravity,
}

impl AgentId {
    /// Stable lowercase token used in CLI flags and state files.
    pub fn as_str(self) -> &'static str {
        match self {
            AgentId::Claude => "claude",
            AgentId::Codex => "codex",
            AgentId::Antigravity => "antigravity",
        }
    }

    /// Human-facing name for notification copy and the menu.
    pub fn display_name(self) -> &'static str {
        match self {
            AgentId::Claude => "Claude",
            AgentId::Codex => "Codex",
            AgentId::Antigravity => "Antigravity",
        }
    }

    /// Which signals this agent's integration can actually emit. The menu and
    /// notifier read this to degrade gracefully: a low-fidelity agent must
    /// never be made to *look* like it reported a permission wait it can't see.
    pub fn capabilities(self) -> Capabilities {
        match self {
            // Claude's hook set covers the whole lifecycle, tool-level, plus
            // statusLine usage data.
            AgentId::Claude => Capabilities {
                detect_running_start: true,
                detect_permission_wait: true,
                detect_plan: true,
                detect_question: true,
                emits_usage: true,
            },
            // Codex's `notify` fires only on `agent-turn-complete`: we learn a
            // turn *ended* (→ Idle) but never that one began, nor any wait.
            AgentId::Codex => Capabilities {
                detect_running_start: false,
                detect_permission_wait: false,
                detect_plan: false,
                detect_question: false,
                emits_usage: false,
            },
            // Antigravity CLI's JSON hooks give turn boundaries
            // (PreInvocation/Stop) and tool events, so we know when a turn
            // starts — better than Codex. But its PreToolUse fires for
            // auto-approved tools too, with no distinct "user must respond"
            // gate, so we don't claim wait detection (that would be a false
            // Waiting on every normal tool call). Usage stays off until its
            // quota surface is wired in.
            AgentId::Antigravity => Capabilities {
                detect_running_start: true,
                detect_permission_wait: false,
                detect_plan: false,
                detect_question: false,
                emits_usage: false,
            },
        }
    }

    /// Executable signatures that identify this agent's host process while
    /// walking the ancestor chain (for liveness). The kernel process *name*
    /// can't be trusted — Claude's native install runs a binary named after its
    /// version — so we match the executable path instead: a known basename, or
    /// a marker directory/file component anywhere in the path.
    fn host_patterns(self) -> HostPatterns {
        match self {
            // npm installs run the CLI under a JS runtime; the native install
            // lives under a `claude` directory.
            AgentId::Claude => HostPatterns {
                exec_names: &["claude", "node", "bun", "deno"],
                path_markers: &["claude"],
            },
            // Codex ships a native `codex` binary (npm wrapper also spawns it).
            AgentId::Codex => HostPatterns {
                exec_names: &["codex"],
                path_markers: &["codex"],
            },
            AgentId::Antigravity => HostPatterns {
                exec_names: &["antigravity"],
                path_markers: &["antigravity"],
            },
        }
    }

    /// Does this executable path look like this agent's host process?
    pub fn matches_host(self, path: &Path) -> bool {
        let hp = self.host_patterns();
        let base = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if hp.exec_names.contains(&base) {
            return true;
        }
        path.components()
            .any(|c| hp.path_markers.iter().any(|m| c.as_os_str() == *m))
    }
}

/// What an agent's integration is able to report. Anything `false` means the
/// menu/notifier must not invent that state for this agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// Knows when a turn *begins* (not just when it ends).
    pub detect_running_start: bool,
    pub detect_permission_wait: bool,
    pub detect_plan: bool,
    pub detect_question: bool,
    /// Emits rate-limit / usage data we can surface in the menu.
    pub emits_usage: bool,
}

struct HostPatterns {
    /// Match when the executable's basename is exactly one of these.
    exec_names: &'static [&'static str],
    /// Match when any path component equals one of these (marker dir/file).
    path_markers: &'static [&'static str],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_roundtrip_through_serde() {
        for id in [AgentId::Claude, AgentId::Codex, AgentId::Antigravity] {
            let json = serde_json::to_string(&id).unwrap();
            assert_eq!(json, format!("\"{}\"", id.as_str()));
            let back: AgentId = serde_json::from_str(&json).unwrap();
            assert_eq!(back, id);
        }
    }

    #[test]
    fn missing_agent_defaults_to_claude() {
        // Back-compat: state files written before the field existed.
        assert_eq!(AgentId::default(), AgentId::Claude);
    }

    #[test]
    fn claude_host_shapes() {
        // Native install: versioned binary under a `claude` directory.
        assert!(AgentId::Claude.matches_host(Path::new("/Users/x/.local/share/claude/versions/2.1.175")));
        // Plain binary or symlink target named claude.
        assert!(AgentId::Claude.matches_host(Path::new("/opt/homebrew/bin/claude")));
        // npm install runs under a JS runtime.
        assert!(AgentId::Claude.matches_host(Path::new("/usr/local/bin/node")));
        assert!(AgentId::Claude.matches_host(Path::new("/Users/x/.bun/bin/bun")));
        // Ordinary ancestors must not match.
        assert!(!AgentId::Claude.matches_host(Path::new("/bin/zsh")));
        // The `~/.claude` config dir component is not the host binary.
        assert!(!AgentId::Claude.matches_host(Path::new("/Users/x/.claude/something")));
        // The Antigravity *IDE* (Electron app) is not a CLI host.
        assert!(!AgentId::Claude.matches_host(Path::new("/Applications/Antigravity IDE.app/Contents/MacOS/Electron")));
        assert!(!AgentId::Antigravity.matches_host(Path::new("/Applications/Antigravity IDE.app/Contents/MacOS/Electron")));
    }

    #[test]
    fn codex_host_shapes() {
        // The shipped binary is named `codex` (Homebrew / npm symlink).
        assert!(AgentId::Codex.matches_host(Path::new("/opt/homebrew/bin/codex")));
        // A versioned binary under a `codex` marker dir, mirroring Claude's layout.
        assert!(AgentId::Codex.matches_host(Path::new("/Users/x/.local/share/codex/versions/0.5.0")));
        assert!(!AgentId::Codex.matches_host(Path::new("/usr/local/bin/node")), "node alone is Claude's runtime, not Codex");
        assert!(!AgentId::Codex.matches_host(Path::new("/bin/zsh")));
    }

    #[test]
    fn capabilities_reflect_fidelity() {
        assert!(AgentId::Claude.capabilities().detect_permission_wait);
        assert!(!AgentId::Codex.capabilities().detect_permission_wait);
        assert!(!AgentId::Codex.capabilities().detect_running_start);
        // Antigravity knows turn-start (PreInvocation) but has no reliable
        // user-must-respond gate, so no wait detection.
        assert!(AgentId::Antigravity.capabilities().detect_running_start);
        assert!(!AgentId::Antigravity.capabilities().detect_permission_wait);
        assert!(!AgentId::Antigravity.capabilities().emits_usage);
    }
}
