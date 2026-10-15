//! launchd agent: starts tachi-bar at login and relaunches it after Restart.

use std::path::PathBuf;
use std::process::Command;

pub const LABEL: &str = "com.tachi-noti.bar";

pub fn plist_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn plist(exe: &str) -> String {
    let exe = xml_escape(exe);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key><array><string>{exe}</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ProcessType</key><string>Interactive</string>
  <key>AbandonProcessGroup</key><true/>
</dict></plist>
"#
    )
}

pub fn uid() -> String {
    Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "501".into())
}

pub fn is_installed() -> bool {
    plist_path().exists()
}

/// Reload the launch agent (bootout + bootstrap) from a detached shell.
/// Not `kickstart -k`: that reuses launchd's cached code requirement,
/// which goes stale when the binary is replaced (cargo install), and the
/// respawn then fails with EX_CONFIG until the agent is reloaded. The
/// shell is detached because bootout kills this process when launchd
/// manages it; the `sleep 1` lets the old instance release its lock
/// before RunAtLoad spawns the successor. `process_group(0)` is what
/// keeps the shell alive at all: when a launchd job exits, launchd
/// SIGKILLs the job's whole process group (AbandonProcessGroup defaults
/// to false), so a same-group child dies before it can bootstrap.
pub fn reload_detached() -> bool {
    use std::os::unix::process::CommandExt;
    let u = uid();
    Command::new("/bin/sh")
        .arg("-c")
        .arg(format!(
            "/bin/launchctl bootout gui/{u}/{LABEL}; sleep 1; /bin/launchctl bootstrap gui/{u} \"$0\""
        ))
        .arg(plist_path())
        .process_group(0)
        .spawn()
        .is_ok()
}

pub fn install() {
    let exe = match std::env::current_exe().and_then(|p| p.canonicalize()) {
        Ok(p) => p.display().to_string(),
        Err(e) => {
            eprintln!("error: cannot resolve tachi-bar path: {e}");
            std::process::exit(1);
        }
    };
    let path = plist_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Unload a previous version first; failure is fine (not loaded).
    let _ = Command::new("/bin/launchctl")
        .args(["bootout", &format!("gui/{}/{LABEL}", uid())])
        .output();
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    if std::fs::write(&tmp, plist(&exe)).is_err() || std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        eprintln!("error: cannot write {}", path.display());
        std::process::exit(1);
    }
    let status = Command::new("/bin/launchctl")
        .args(["bootstrap", &format!("gui/{}", uid())])
        .arg(&path)
        .status();
    match status {
        Ok(s) if s.success() => println!("Installed launch agent {} ({})", LABEL, path.display()),
        _ => eprintln!("warning: wrote {} but launchctl bootstrap failed; it will load at next login", path.display()),
    }
}

pub fn uninstall() {
    let _ = Command::new("/bin/launchctl")
        .args(["bootout", &format!("gui/{}/{LABEL}", uid())])
        .output();
    let path = plist_path();
    if path.exists() {
        let _ = std::fs::remove_file(&path);
        println!("Removed launch agent {LABEL}");
    } else {
        println!("Launch agent not installed — nothing to do.");
    }
}
