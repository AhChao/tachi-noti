use std::path::{Path, PathBuf};
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME, SND_NODEFAULT, SND_SYNC};
use windows::core::HSTRING;

pub const DEFAULT_STOP: &str = "Windows Notify System Generic";
pub const DEFAULT_ATTENTION: &str = "Windows Exclamation";

pub const MISSING_NOTE: &str =
    "not found in %WINDIR%\\Media — Windows will play its default notification sound";

/// Tachi's bark (one woof), embedded and materialized into the cache dir.
/// Toasts from an unpackaged app cannot reference a custom sound, so the bark
/// is played by us (`play_blocking`) rather than by the toast.
const TACHI_BARK: &[u8] = include_bytes!("../../../assets/tachi-bark.wav");

fn bark_file_name() -> String {
    format!("{}.wav", crate::notify::BARK_SOUND_NAME)
}

fn bark_path() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("tachi-noti").join("sounds").join(bark_file_name()))
}

fn media_dir() -> PathBuf {
    let windir = std::env::var_os("WINDIR")
        .or_else(|| std::env::var_os("SystemRoot"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    windir.join("Media")
}

/// Materialize the embedded bark into the cache dir. Length check
/// re-materializes on upgrades.
pub fn ensure_bark() {
    let Some(path) = bark_path() else { return };
    let stale = std::fs::metadata(&path).map(|m| m.len() != TACHI_BARK.len() as u64).unwrap_or(true);
    if !stale {
        return;
    }
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_ok() {
            let _ = std::fs::write(&path, TACHI_BARK);
        }
    }
}

/// Sound names (file stem) of the .wav files in a directory.
fn sound_names_in(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    let (stem, ext) = name.rsplit_once('.')?;
                    (ext.eq_ignore_ascii_case("wav") && !stem.is_empty()).then(|| stem.to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Choices for the sound pickers: Tachi's bark, the system sounds in
/// %WINDIR%\Media, and silence. Returns (label, config value) pairs.
pub fn options() -> Vec<(String, String)> {
    let bark = crate::notify::BARK_SOUND_NAME;
    let mut opts = vec![("Tachi Bark".to_string(), bark.to_string())];
    let mut system = sound_names_in(&media_dir());
    system.retain(|n| n != bark);
    system.sort();
    system.dedup();
    opts.extend(system.into_iter().map(|n| (n.clone(), n)));
    opts.push(("Silent".to_string(), String::new()));
    opts
}

/// Resolve a sound value to a playable file: the bark from our cache, else a
/// .wav in %WINDIR%\Media.
pub fn preview_path(value: &str) -> Option<PathBuf> {
    if value.is_empty() {
        return None;
    }
    if value == crate::notify::BARK_SOUND_NAME {
        ensure_bark();
        return bark_path().filter(|p| p.exists());
    }
    // A name is a bare file stem; refuse anything that could walk out of the
    // Media dir (config.toml is user-editable).
    if value.contains(['/', '\\', ':']) {
        return None;
    }
    let p = media_dir().join(format!("{value}.wav"));
    p.is_file().then_some(p)
}

fn play(path: &Path, mode: windows::Win32::Media::Audio::SND_FLAGS) {
    let wide = HSTRING::from(path.as_os_str());
    // SAFETY: `wide` is a null-terminated UTF-16 string that outlives the call.
    unsafe {
        let _ = PlaySoundW(&wide, None, SND_FILENAME | SND_NODEFAULT | mode);
    }
}

/// Audible feedback for a picker choice; returns immediately.
pub fn play_file(path: &Path) {
    play(path, SND_ASYNC);
}

/// Play to completion. A hook process exits right after sending, and an async
/// PlaySound would be cut off with it.
pub fn play_blocking(path: &Path) {
    play(path, SND_SYNC);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sound_names_are_wav_only_case_insensitive() {
        let dir = std::env::temp_dir().join(format!("tachi-noti-test-sounds-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["Alarm01.wav", "Chord.WAV", "Ding.Wav", "song.mp3", "notes.txt", "noext", ".wav"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let mut names = sound_names_in(&dir);
        names.sort();
        assert_eq!(names, vec!["Alarm01", "Chord", "Ding"], "wav files only, extension stripped");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preview_path_rejects_path_traversal() {
        assert_eq!(preview_path(r"..\..\evil"), None);
        assert_eq!(preview_path("C:\\x"), None);
        assert_eq!(preview_path(""), None);
    }
}
