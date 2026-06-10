use clap::{Parser, Subcommand};
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
    /// Add tachi-noti hooks to Claude Code settings.json
    Install {
        #[arg(long, value_enum, default_value_t = Scope::User)]
        scope: Scope,
    },
    /// Remove tachi-noti hooks from Claude Code settings.json
    Uninstall {
        #[arg(long, value_enum, default_value_t = Scope::User)]
        scope: Scope,
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
        Cmd::Install { scope } => exit_on_err(settings::install(scope)),
        Cmd::Uninstall { scope } => exit_on_err(settings::uninstall(scope)),
        Cmd::Log { lines } => tachi_noti::history::print_log(lines),
        Cmd::Test => exit_on_err(hook::run_test()),
        Cmd::Doctor => exit_on_err(hook::run_doctor()),
    }
}

fn exit_on_err(r: Result<(), Box<dyn std::error::Error>>) {
    if let Err(e) = r {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
