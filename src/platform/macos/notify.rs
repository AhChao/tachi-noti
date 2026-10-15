use crate::config::Config;
use crate::notify::{Notice, find_in_path, guard_leading_dash, sh_quote};
use std::path::PathBuf;
use std::process::Command;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug)]
pub enum Backend {
    TerminalNotifier(PathBuf),
    OsaScript,
}

impl Backend {
    pub fn name(&self) -> &'static str {
        match self {
            Backend::TerminalNotifier(_) => "terminal-notifier",
            Backend::OsaScript => "osascript",
        }
    }

    /// Doctor line suffix: where the backend lives / how to upgrade.
    pub fn detail(&self) -> String {
        match self {
            Backend::TerminalNotifier(p) => format!(" ({})", p.display()),
            Backend::OsaScript => " (install terminal-notifier for grouping + click-to-focus)".into(),
        }
    }

    /// Printed after `tachi-noti test` when the backend needs user setup.
    pub fn setup_hint(&self) -> Option<&'static str> {
        match self {
            Backend::TerminalNotifier(_) => None,
            Backend::OsaScript => Some(
                "If nothing appeared: macOS attributes osascript notifications to \"Script Editor\" —\n\
                 check System Settings → Notifications → Script Editor is allowed.\n\
                 Tip: `brew install terminal-notifier` upgrades to grouping + click-to-focus.",
            ),
        }
    }
}

pub fn detect(config: &Config) -> Backend {
    match config.backend.as_deref() {
        Some("osascript") => return Backend::OsaScript,
        Some("terminal-notifier") => {
            if let Some(p) = find_in_path("terminal-notifier") {
                return Backend::TerminalNotifier(p);
            }
            return Backend::OsaScript;
        }
        _ => {}
    }
    match find_in_path("terminal-notifier") {
        Some(p) => Backend::TerminalNotifier(p),
        None => Backend::OsaScript,
    }
}

pub fn send(backend: &Backend, n: &Notice) -> Result<()> {
    if n.sound.as_deref() == Some(crate::notify::BARK_SOUND_NAME) {
        super::sound::ensure_bark();
    }
    match backend {
        Backend::TerminalNotifier(path) => send_terminal_notifier(path, n),
        Backend::OsaScript => send_osascript(n),
    }
}

fn send_terminal_notifier(path: &PathBuf, n: &Notice) -> Result<()> {
    let mut cmd = Command::new(path);
    cmd.arg("-title").arg(&n.title);
    if !n.subtitle.is_empty() {
        cmd.arg("-subtitle").arg(&n.subtitle);
    }
    cmd.arg("-message").arg(guard_leading_dash(&n.body));
    if let Some(s) = &n.sound {
        cmd.arg("-sound").arg(s);
    }
    cmd.arg("-group").arg(&n.group);
    if let Some(icon) = &n.icon {
        // -appIcon is ignored on recent macOS (Big Sur+); -contentImage still
        // renders. Pass both so whichever the OS honors shows the image.
        cmd.arg("-appIcon").arg(icon);
        cmd.arg("-contentImage").arg(icon);
    }
    match &n.activate {
        // -sender and -activate conflict on Sequoia: -sender re-attributes the
        // notification and clicking activates the sender app instead. Prefer a
        // working click-to-focus over the nicer icon.
        Some(bid) => {
            // -activate only raises the app's last-used window. VS Code-family
            // apps keep one window per folder, so opening the project path
            // focuses the exact window hosting this session.
            let per_folder = super::host::window_per_folder(bid) || super::host::window_per_folder_hook_hint();
            match n.click_path.as_deref().filter(|_| per_folder) {
                Some(path) => {
                    cmd.arg("-execute")
                        .arg(format!("/usr/bin/open -b {} {}", sh_quote(bid), sh_quote(path)));
                }
                None => {
                    cmd.arg("-activate").arg(bid);
                }
            }
        }
        None => {
            cmd.arg("-sender").arg("com.apple.Terminal");
        }
    }
    let status = cmd.status()?;
    if !status.success() {
        return Err(format!("terminal-notifier exited with {status}").into());
    }
    Ok(())
}

fn send_osascript(n: &Notice) -> Result<()> {
    // Injection-proof by construction: user-controlled text only travels via
    // argv, never interpolated into the AppleScript source.
    let script = if n.sound.is_some() {
        "on run argv\ndisplay notification (item 1 of argv) with title (item 2 of argv) subtitle (item 3 of argv) sound name (item 4 of argv)\nend run"
    } else {
        "on run argv\ndisplay notification (item 1 of argv) with title (item 2 of argv) subtitle (item 3 of argv)\nend run"
    };
    let mut cmd = Command::new("/usr/bin/osascript");
    cmd.arg("-e").arg(script).arg(&n.body).arg(&n.title).arg(&n.subtitle);
    if let Some(s) = &n.sound {
        cmd.arg(s);
    }
    let status = cmd.status()?;
    if !status.success() {
        return Err(format!("osascript exited with {status}").into());
    }
    Ok(())
}
