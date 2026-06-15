use clap::{Parser, Subcommand};
use tachi_noti::agent::{self, AgentId};
use tachi_noti::{Scope, hook, settings};

#[derive(Parser)]
#[command(name = "tachi-noti", version, about = "Tachi Noti — native macOS notifications for Claude Code hooks, delivered by Tachi the butler collie")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Read a Claude Code hook event from stdin and notify (used by hooks, never fails)
    Hook,
    /// Ingest a non-Claude agent's event (e.g. Codex `notify`, JSON as argv); never fails
    #[command(hide = true)]
    Ingest {
        #[arg(long, value_enum)]
        agent: AgentId,
        /// Event tag for agents we register per-event (Antigravity: start|tool|stop)
        #[arg(long)]
        event: Option<String>,
        /// Chained downstream program (JSON-encoded argv) to re-invoke after us (Codex)
        #[arg(long)]
        chain: Option<String>,
        /// The agent's event payload as a single JSON string (Codex; Antigravity uses stdin)
        payload: Option<String>,
    },
    /// Add tachi-noti hooks to a coding agent's settings (Claude by default)
    Install {
        #[arg(long, value_enum, default_value_t = AgentId::Claude)]
        agent: AgentId,
        /// User- or project-scoped settings (Claude only)
        #[arg(long, value_enum, default_value_t = Scope::User)]
        scope: Scope,
    },
    /// Remove tachi-noti hooks from a coding agent's settings (Claude by default)
    Uninstall {
        #[arg(long, value_enum, default_value_t = AgentId::Claude)]
        agent: AgentId,
        #[arg(long, value_enum, default_value_t = Scope::User)]
        scope: Scope,
    },
    /// statusLine pipeline stage: capture usage, pass stdin to --chain (used by settings)
    #[command(hide = true)]
    Statusline {
        #[arg(long)]
        chain: Option<String>,
    },
    /// Show recent notification history
    Log {
        #[arg(short = 'n', long, default_value_t = 20)]
        lines: usize,
    },
    /// Fire a sample notification to verify setup
    Test,
    /// Print diagnostic information
    Doctor,
}

fn main() {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Hook => {
            // Never block Claude Code: swallow panics and errors, always exit 0.
            let _ = std::panic::catch_unwind(|| {
                if let Err(e) = hook::run() {
                    hook::debug_log(&format!("hook error: {e}"));
                }
            });
            std::process::exit(0);
        }
        Cmd::Ingest { agent, event, chain, payload } => {
            // Never block the agent: swallow panics and errors, always exit 0.
            let _ = std::panic::catch_unwind(|| {
                if let Err(e) = run_ingest(agent, event.as_deref(), payload.as_deref(), chain.as_deref()) {
                    hook::debug_log(&format!("ingest error: {e}"));
                }
            });
            std::process::exit(0);
        }
        Cmd::Install { agent, scope } => exit_on_err(install(agent, scope)),
        Cmd::Uninstall { agent, scope } => exit_on_err(uninstall(agent, scope)),
        Cmd::Statusline { chain } => {
            std::process::exit(tachi_noti::usage::run_statusline(chain.as_deref()));
        }
        Cmd::Log { lines } => tachi_noti::history::print_log(lines),
        Cmd::Test => exit_on_err(hook::run_test()),
        Cmd::Doctor => exit_on_err(hook::run_doctor()),
    }
}

type CmdResult = Result<(), Box<dyn std::error::Error>>;

fn run_ingest(agent: AgentId, event: Option<&str>, payload: Option<&str>, chain: Option<&str>) -> CmdResult {
    match agent {
        AgentId::Codex => agent::codex::ingest(payload, chain),
        AgentId::Antigravity => agent::antigravity::ingest(event),
        // Claude arrives via `hook` (stdin), not `ingest`.
        AgentId::Claude => Ok(()),
    }
}

fn install(agent: AgentId, scope: Scope) -> CmdResult {
    match agent {
        AgentId::Claude => settings::install(scope),
        AgentId::Codex => agent::codex::install(),
        AgentId::Antigravity => agent::antigravity::install(),
    }
}

fn uninstall(agent: AgentId, scope: Scope) -> CmdResult {
    match agent {
        AgentId::Claude => settings::uninstall(scope),
        AgentId::Codex => agent::codex::uninstall(),
        AgentId::Antigravity => agent::antigravity::uninstall(),
    }
}

fn exit_on_err(r: Result<(), Box<dyn std::error::Error>>) {
    if let Err(e) = r {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
