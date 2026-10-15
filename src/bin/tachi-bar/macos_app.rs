//! "Tachi Bar.app" wrapper in ~/Applications: Spotlight only indexes app
//! bundles, not bare executables, so this is what makes tachi-bar launchable
//! by name. The bundle's executable is a shell stub that execs the real
//! binary, so `cargo install` updates apply without reinstalling the app.

use std::path::PathBuf;
use std::process::Command;

/// Tachi's portrait; converted to the bundle icon at install time.
const PORTRAIT: &[u8] = include_bytes!("../../../assets/tachi.png");

const BUNDLE_ID: &str = "com.tachi-noti.bar";

pub fn bundle_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Applications/Tachi Bar.app")
}

fn info_plist() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Tachi Bar</string>
  <key>CFBundleDisplayName</key><string>Tachi Bar</string>
  <key>CFBundleIdentifier</key><string>{BUNDLE_ID}</string>
  <key>CFBundleExecutable</key><string>tachi-bar</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleIconFile</key><string>tachi</string>
  <key>CFBundleShortVersionString</key><string>{version}</string>
  <key>LSUIElement</key><true/>
</dict></plist>
"#,
        version = env!("CARGO_PKG_VERSION")
    )
}

/// Escape for interpolation inside a double-quoted sh string.
fn sh_escape(s: &str) -> String {
    s.replace('\\', r"\\").replace('"', "\\\"").replace('$', "\\$").replace('`', "\\`")
}

pub fn install() {
    let exe = match std::env::current_exe().and_then(|p| p.canonicalize()) {
        Ok(p) => p.display().to_string(),
        Err(e) => {
            eprintln!("error: cannot resolve tachi-bar path: {e}");
            std::process::exit(1);
        }
    };
    let bundle = bundle_path();
    if bundle.exists() && !is_ours(&bundle) {
        eprintln!(
            "error: {} exists but does not look like tachi-bar's bundle — not touching it.",
            bundle.display()
        );
        std::process::exit(1);
    }

    // Stage in a dot-prefixed sibling and rename into place: Spotlight
    // classifies a bundle the moment its directory appears, and one built
    // in place gets stuck indexed as a plain folder (never as an app).
    // Dot-paths are ignored by Spotlight, and the rename lands complete.
    let staging = bundle.with_file_name(format!(".Tachi Bar.app.staging.{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let macos = staging.join("Contents/MacOS");
    let resources = staging.join("Contents/Resources");
    for dir in [&macos, &resources] {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("error: cannot create {}: {e}", dir.display());
            std::process::exit(1);
        }
    }
    let stub = format!(
        "#!/bin/sh\n{}=1 exec \"{}\" \"$@\"\n",
        crate::FROM_APP_ENV,
        sh_escape(&exe)
    );
    let stub_path = macos.join("tachi-bar");
    if std::fs::write(staging.join("Contents/Info.plist"), info_plist()).is_err()
        || std::fs::write(staging.join("Contents/PkgInfo"), "APPL????").is_err()
        || std::fs::write(&stub_path, stub).is_err()
    {
        let _ = std::fs::remove_dir_all(&staging);
        eprintln!("error: cannot write into {}", staging.display());
        std::process::exit(1);
    }
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&stub_path, std::fs::Permissions::from_mode(0o755));

    // Icon: png → icns via sips. Best-effort — the app works without it.
    let png = resources.join("tachi-portrait.png");
    if std::fs::write(&png, PORTRAIT).is_ok() {
        let _ = Command::new("/usr/bin/sips")
            .args(["-s", "format", "icns"])
            .arg(&png)
            .arg("--out")
            .arg(resources.join("tachi.icns"))
            .output();
        let _ = std::fs::remove_file(&png);
    }

    if bundle.exists() {
        if let Err(e) = std::fs::remove_dir_all(&bundle) {
            let _ = std::fs::remove_dir_all(&staging);
            eprintln!("error: cannot replace {}: {e}", bundle.display());
            std::process::exit(1);
        }
    }
    if let Err(e) = std::fs::rename(&staging, &bundle) {
        let _ = std::fs::remove_dir_all(&staging);
        eprintln!("error: cannot move bundle into {}: {e}", bundle.display());
        std::process::exit(1);
    }

    // Nudge LaunchServices so Spotlight picks the app up immediately.
    let _ = Command::new(
        "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister",
    )
    .arg("-f")
    .arg(&bundle)
    .output();

    println!("Installed {} — Spotlight can now launch \u{201c}Tachi Bar\u{201d}.", bundle.display());
}

/// True when the bundle's Info.plist carries our bundle identifier.
fn is_ours(bundle: &std::path::Path) -> bool {
    std::fs::read_to_string(bundle.join("Contents/Info.plist"))
        .map(|s| s.contains(BUNDLE_ID))
        .unwrap_or(false)
}

pub fn uninstall() {
    let bundle = bundle_path();
    // Only delete a bundle that is verifiably ours.
    if !is_ours(&bundle) {
        if bundle.exists() {
            eprintln!(
                "error: {} exists but does not look like tachi-bar's bundle — not touching it.",
                bundle.display()
            );
            std::process::exit(1);
        }
        println!("App wrapper not installed — nothing to do.");
        return;
    }
    match std::fs::remove_dir_all(&bundle) {
        Ok(()) => println!("Removed {}", bundle.display()),
        Err(e) => {
            eprintln!("error: cannot remove {}: {e}", bundle.display());
            std::process::exit(1);
        }
    }
}
