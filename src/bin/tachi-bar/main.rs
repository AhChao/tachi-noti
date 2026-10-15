//! tachi-bar — menu bar / system tray monitor for live Claude Code sessions.
//! Reads the state files written by `tachi-noti hook`; no daemon, no IPC.
//!
//! The menu content comes from `tachi_noti::bar::menu` on both OSes; this
//! binary only owns the CLI, the single-instance lock, restart, and the
//! per-OS front end (`macos_ui` / `windows_tray`).

// Windows: a tray app must not pop a console window.
#![cfg_attr(windows, windows_subsystem = "windows")]

use clap::{Parser, Subcommand};

#[cfg(target_os = "macos")]
mod macos_agent;
#[cfg(target_os = "macos")]
mod macos_app;
#[cfg(target_os = "macos")]
mod macos_ui;
#[cfg(windows)]
mod windows_tray;

#[derive(Parser)]
#[command(name = "tachi-bar", version, about = "Menu bar monitor for Claude Code sessions")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start tachi-bar at login (macOS: launchd agent; Windows: autostart entry)
    InstallAgent,
    /// Remove the login autostart
    UninstallAgent,
    /// Install "Tachi Bar.app" in ~/Applications so Spotlight can launch it (macOS)
    InstallApp,
    /// Remove the app wrapper from ~/Applications (macOS)
    UninstallApp,
    /// Handle a `tachi-noti://` notification click (Windows toast target)
    #[command(hide = true)]
    Activate { uri: String },
}

fn main() {
    match Cli::parse().cmd {
        Some(Cmd::Activate { uri }) => tachi_noti::platform::register::handle_activation(&uri),
        Some(cmd) => run_os_command(cmd),
        None => run_ui(),
    }
}

#[cfg(target_os = "macos")]
fn run_os_command(cmd: Cmd) {
    match cmd {
        Cmd::InstallAgent => macos_agent::install(),
        Cmd::UninstallAgent => macos_agent::uninstall(),
        Cmd::InstallApp => macos_app::install(),
        Cmd::UninstallApp => macos_app::uninstall(),
        Cmd::Activate { .. } => unreachable!("handled in main"),
    }
}

#[cfg(windows)]
fn run_os_command(cmd: Cmd) {
    match cmd {
        Cmd::InstallAgent => windows_tray::install_autostart(),
        Cmd::UninstallAgent => windows_tray::uninstall_autostart(),
        Cmd::InstallApp | Cmd::UninstallApp => {
            println!("not needed on Windows \u{2014} `tachi-noti install` creates the Start menu entry")
        }
        Cmd::Activate { .. } => unreachable!("handled in main"),
    }
}

fn run_ui() {
    // Single instance: a manual launch racing the launchd agent (its
    // KeepAlive respawns on kill) must not stack a second status item.
    // Exiting 0 keeps launchd's KeepAlive=SuccessfulExit:false from
    // respawn-looping the loser.
    let Some(_lock) = acquire_single_instance_lock() else {
        eprintln!("tachi-bar is already running \u{2014} exiting.");
        return;
    };
    #[cfg(target_os = "macos")]
    {
        // Launched from the .app wrapper with an agent installed: hand off
        // to launchd instead of running un-managed, so KeepAlive (and a
        // fresh code requirement) stay attached without the user having to
        // remember to hit Restart. Our lock releases on return; the
        // reload's sleep outlasts it.
        if std::env::var_os(FROM_APP_ENV).is_some()
            && macos_agent::is_installed()
            && macos_agent::reload_detached()
        {
            return;
        }
        macos_ui::run(); // never returns; the lock lives as long as the process
    }
    #[cfg(windows)]
    {
        windows_tray::run();
    }
}

/// A restart successor sets this to outwait its predecessor's instance lock
/// (released when the old process exits); a plain second launch gives up
/// immediately.
const WAIT_LOCK_ENV: &str = "TACHI_BAR_WAIT_LOCK";

/// Set by the .app wrapper's stub so a Spotlight launch can be told apart
/// from a terminal one (and handed off to launchd when the agent exists).
#[cfg(target_os = "macos")]
const FROM_APP_ENV: &str = "TACHI_BAR_LAUNCHED_FROM_APP";

/// Exclusive advisory lock; the returned file must stay alive for the
/// process lifetime.
fn acquire_single_instance_lock() -> Option<std::fs::File> {
    let dir = tachi_noti::state::data_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dir.join("tachi-bar.lock"))
        .ok()?;
    let attempts = if std::env::var_os(WAIT_LOCK_ENV).is_some() { 50 } else { 1 };
    for i in 0..attempts {
        if f.try_lock().is_ok() {
            return Some(f);
        }
        if i + 1 < attempts {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    None
}

/// Replace this process with a fresh one (menu: Restart Tachi Bar).
/// When the launch agent is installed, fully reload it (bootout + bootstrap)
/// so the successor stays launchd-managed; otherwise hand over to a spawned
/// copy that waits for our instance lock.
pub(crate) fn restart() {
    #[cfg(target_os = "macos")]
    if macos_agent::is_installed() && macos_agent::reload_detached() {
        // Exit 0 releases the instance lock without tripping KeepAlive
        // (SuccessfulExit=false); the reload's bootstrap respawns us.
        std::process::exit(0);
    }
    match std::env::current_exe() {
        Ok(exe) => {
            let _ = std::process::Command::new(exe).env(WAIT_LOCK_ENV, "1").spawn();
            std::process::exit(0); // releases the lock; the successor takes over
        }
        Err(e) => eprintln!("tachi-bar: cannot restart (no exe path): {e}"),
    }
}
