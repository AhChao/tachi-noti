use std::path::{Path, PathBuf};

pub const DEFAULT_STOP: &str = "Glass";
pub const DEFAULT_ATTENTION: &str = "Basso";

pub const MISSING_NOTE: &str =
    "not found in ~/Library/Sounds or /System/Library/Sounds — macOS will play the default sound";

/// Tachi's bark (one woof), embedded and materialized into ~/Library/Sounds
/// so macOS can play it by name like any system notification sound.
const TACHI_BARK: &[u8] = include_bytes!("../../../assets/tachi-bark.aiff");

/// Extensions macOS resolves for named notification sounds in Library/Sounds.
const SOUND_EXTS: &[&str] = &["aiff", "aif", "wav", "caf", "m4a", "mp3"];

/// Install the embedded bark into ~/Library/Sounds (macOS looks notification
/// sounds up by name there). Length check re-materializes on upgrades.
pub fn ensure_bark() {
    let Some(home) = dirs::home_dir() else { return };
    let dir = home.join("Library/Sounds");
    let path = dir.join(format!("{}.aiff", crate::notify::BARK_SOUND_NAME));
    let stale = std::fs::metadata(&path).map(|m| m.len() != TACHI_BARK.len() as u64).unwrap_or(true);
    if stale && std::fs::create_dir_all(&dir).is_ok() {
        let _ = std::fs::write(&path, TACHI_BARK);
    }
}

/// Sound names (basename without extension) found in a Library/Sounds dir.
fn sound_names_in(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    let (stem, ext) = name.rsplit_once('.')?;
                    SOUND_EXTS.contains(&ext.to_ascii_lowercase().as_str()).then(|| stem.to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Choices for the sound pickers: Tachi's bark, the user's own sounds
/// (~/Library/Sounds), the system sounds, and silence.
/// Returns (label, config value) pairs.
pub fn options() -> Vec<(String, String)> {
    let bark = crate::notify::BARK_SOUND_NAME;
    let mut opts = vec![("Tachi Bark".to_string(), bark.to_string())];
    let mut user: Vec<String> = dirs::home_dir()
        .map(|h| sound_names_in(&h.join("Library/Sounds")))
        .unwrap_or_default();
    user.retain(|n| n != bark);
    user.sort();
    user.dedup();
    let mut system = sound_names_in(Path::new("/System/Library/Sounds"));
    system.sort();
    opts.extend(user.into_iter().map(|n| (n.clone(), n)));
    opts.extend(system.into_iter().map(|n| (n.clone(), n)));
    opts.push(("Silent".to_string(), String::new()));
    opts
}

/// Resolve a sound value to a playable file (in-menu preview, doctor check).
/// Mirrors macOS's by-name lookup: ~/Library/Sounds first, then the system's.
pub fn preview_path(value: &str) -> Option<PathBuf> {
    if value.is_empty() {
        return None;
    }
    let dirs_to_try = [
        dirs::home_dir().map(|h| h.join("Library/Sounds")),
        Some(PathBuf::from("/System/Library/Sounds")),
    ];
    for dir in dirs_to_try.into_iter().flatten() {
        for ext in SOUND_EXTS {
            let p = dir.join(format!("{value}.{ext}"));
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

/// Audible feedback for a picker choice; afplay runs detached.
pub fn play_file(path: &Path) {
    let _ = std::process::Command::new("/usr/bin/afplay").arg(path).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sound_names_cover_macos_formats() {
        let dir = std::env::temp_dir().join(format!("tachi-noti-test-sounds-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["Glass.aiff", "custom.wav", "Voice.m4a", "notes.txt", "noext"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let mut names = sound_names_in(&dir);
        names.sort();
        assert_eq!(names, vec!["Glass", "Voice", "custom"], "audio files only, extension stripped");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
